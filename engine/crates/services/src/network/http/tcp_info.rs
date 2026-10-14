//! The round-trip time the kernel measures for a connected TCP socket.
//!
//! A response's `profile.rtt` is the connection's RTT when the response arrived. The kernel keeps a smoothed estimate
//! per socket: `TCP_INFO.tcpi_rtt` on Linux and Android, `TCP_CONNECTION_INFO.tcpi_srtt` on Apple's systems. Elsewhere
//! there is none to read, and the field is absent rather than invented.
//!
//! The socket is read by its descriptor, and a descriptor outlives nothing: once the connection closes it, the number
//! may name another file. So the probe and the connection share one slot -- the connection empties it before its socket
//! closes, and a read holds the slot's lock for the length of the system call -- and a probe whose connection is gone
//! answers nothing.

use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;

#[cfg(unix)]
type RawSocket = std::os::fd::RawFd;
#[cfg(windows)]
type RawSocket = std::os::windows::io::RawSocket;

type Slot = Arc<Mutex<Option<RawSocket>>>;

/// Reads the socket's RTT while its connection holds it open.
#[derive(Debug, Clone)]
pub(crate) struct SocketProbe(Slot);

/// Held by the connection: empties the probe's slot when the connection goes, before the socket closes.
#[derive(Debug)]
pub(crate) struct SocketGuard(Slot);

impl Drop for SocketGuard {
    fn drop(&mut self) {
        *self.0.lock() = None;
    }
}

/// A probe for `socket`, and the guard its connection holds. The guard must be dropped before the socket is closed:
/// declare it before the stream in the connection that owns both.
pub(crate) fn watch(socket: &tokio::net::TcpStream) -> (SocketProbe, SocketGuard) {
    #[cfg(unix)]
    let raw = std::os::fd::AsRawFd::as_raw_fd(socket);
    #[cfg(windows)]
    let raw = std::os::windows::io::AsRawSocket::as_raw_socket(socket);
    let slot: Slot = Arc::new(Mutex::new(Some(raw)));
    (SocketProbe(Arc::clone(&slot)), SocketGuard(slot))
}

impl SocketProbe {
    pub(crate) fn rtt(&self) -> Option<Duration> {
        let slot = self.0.lock();
        platform::rtt((*slot)?)
    }
}

#[cfg(any(target_os = "linux", target_os = "android"))]
mod platform {
    use std::time::Duration;

    /// The leading fields of the kernel's `struct tcp_info`, through `tcpi_rttvar`: eight bytes of state, then 32-bit
    /// counters. The kernel copies as much as the caller asks for, so a prefix is a complete answer; bionic's libc
    /// does not declare the struct, so it is declared here once for both systems.
    #[repr(C)]
    #[derive(Default)]
    struct TcpInfoPrefix {
        flags: [u8; 8],
        rto: u32,
        ato: u32,
        snd_mss: u32,
        rcv_mss: u32,
        unacked: u32,
        sacked: u32,
        lost: u32,
        retrans: u32,
        fackets: u32,
        last_data_sent: u32,
        last_ack_sent: u32,
        last_data_recv: u32,
        last_ack_recv: u32,
        pmtu: u32,
        rcv_ssthresh: u32,
        /// Smoothed RTT, microseconds.
        rtt: u32,
        rttvar: u32,
    }

    pub(super) fn rtt(fd: std::os::fd::RawFd) -> Option<Duration> {
        let mut info = TcpInfoPrefix::default();
        let mut len = std::mem::size_of::<TcpInfoPrefix>() as libc::socklen_t;
        // SAFETY: `fd` is a live socket (the caller holds the slot's lock, and the connection empties the slot before
        // closing it); `info` is a writable buffer of `len` bytes.
        let answered = unsafe {
            libc::getsockopt(
                fd,
                libc::IPPROTO_TCP,
                libc::TCP_INFO,
                (&mut info as *mut TcpInfoPrefix).cast(),
                &mut len,
            )
        };
        let through_rtt = std::mem::offset_of!(TcpInfoPrefix, rttvar) as libc::socklen_t;
        (answered == 0 && len >= through_rtt && info.rtt > 0)
            .then(|| Duration::from_micros(u64::from(info.rtt)))
    }
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
mod platform {
    use std::time::Duration;

    pub(super) fn rtt(fd: std::os::fd::RawFd) -> Option<Duration> {
        // SAFETY: an all-zero `tcp_connection_info` is a valid value of a plain C struct.
        let mut info: libc::tcp_connection_info = unsafe { std::mem::zeroed() };
        let mut len = std::mem::size_of::<libc::tcp_connection_info>() as libc::socklen_t;
        // SAFETY: as for Linux: a live socket, and a writable buffer of `len` bytes.
        let answered = unsafe {
            libc::getsockopt(
                fd,
                libc::IPPROTO_TCP,
                libc::TCP_CONNECTION_INFO,
                (&mut info as *mut libc::tcp_connection_info).cast(),
                &mut len,
            )
        };
        // Smoothed RTT, milliseconds.
        (answered == 0 && info.tcpi_srtt > 0)
            .then(|| Duration::from_millis(u64::from(info.tcpi_srtt)))
    }
}

#[cfg(not(any(
    target_os = "linux",
    target_os = "android",
    target_os = "macos",
    target_os = "ios"
)))]
mod platform {
    use std::time::Duration;

    pub(super) fn rtt(_socket: super::RawSocket) -> Option<Duration> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A connected socket reports an RTT where the platform measures one; a probe whose connection has gone answers
    /// nothing, rather than reading whatever the descriptor names by then.
    #[tokio::test]
    async fn a_probe_reads_its_socket_and_nothing_after_it_closes() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let accepting = tokio::spawn(async move { listener.accept().await.unwrap() });
        let socket = tokio::net::TcpStream::connect(address).await.unwrap();
        let _server = accepting.await.unwrap();

        let (probe, guard) = watch(&socket);
        let measured = probe.rtt();
        if cfg!(any(
            target_os = "linux",
            target_os = "android",
            target_os = "macos",
            target_os = "ios"
        )) {
            assert!(
                measured.is_some(),
                "a connected socket has a smoothed RTT here"
            );
        }
        drop(guard);
        drop(socket);
        assert_eq!(probe.rtt(), None);
    }
}
