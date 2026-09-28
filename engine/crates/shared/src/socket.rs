//! Socket options the standard library leaves to the caller.

/// Make a write to `socket` after its peer has gone fail with `EPIPE` rather
/// than raise `SIGPIPE`.
///
/// A Rust program's runtime ignores `SIGPIPE` before `main`; a library linked
/// into a Swift or Objective-C app gets no such start-up, and the signal's
/// default action ends the process -- the app, for a producer that went away
/// mid-write. Linux and Android need nothing: the standard library passes
/// `MSG_NOSIGNAL` on every send. Apple platforms have no such flag. The socket
/// option is the documented way, and the standard library sets it on the
/// sockets it creates but not on one `accept` returns, which is where it is
/// set here rather than trusted to be inherited from the listener.
#[cfg(target_vendor = "apple")]
pub fn suppress_sigpipe(socket: &std::net::TcpStream) -> std::io::Result<()> {
    use std::os::fd::AsRawFd;

    let enabled: libc::c_int = 1;
    // SAFETY: a descriptor the stream owns for the call, a pointer to a live
    // `c_int`, and that `c_int`'s size.
    let result = unsafe {
        libc::setsockopt(
            socket.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_NOSIGPIPE,
            (&raw const enabled).cast(),
            size_of::<libc::c_int>() as libc::socklen_t,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

/// See the Apple version: elsewhere every send already carries the equivalent.
#[cfg(not(target_vendor = "apple"))]
pub fn suppress_sigpipe(_socket: &std::net::TcpStream) -> std::io::Result<()> {
    Ok(())
}

#[cfg(all(test, target_vendor = "apple"))]
mod tests {
    use std::net::{Ipv4Addr, TcpListener, TcpStream};
    use std::os::fd::AsRawFd;

    #[test]
    fn an_accepted_socket_reports_the_option_set() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let _client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (accepted, _) = listener.accept().unwrap();
        super::suppress_sigpipe(&accepted).unwrap();

        let mut value: libc::c_int = 0;
        let mut length = size_of::<libc::c_int>() as libc::socklen_t;
        // SAFETY: a live descriptor, and a buffer and length that match.
        let result = unsafe {
            libc::getsockopt(
                accepted.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_NOSIGPIPE,
                (&raw mut value).cast(),
                &mut length,
            )
        };
        assert_eq!(result, 0);
        assert_ne!(value, 0);
    }
}
