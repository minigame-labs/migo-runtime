//! Uses Tokio's async TcpStream, managed as a deno_core Resource.
//! The JS layer creates instances, and each socket has its own read/write
//! halves protected by AsyncRefCell for single-threaded V8 access.
//!
//! ## Event model
//!
//! JS polls events via `op_tcp_next_event` (async loop), similar to WebSocket.
//! Connect, message, error, and close events are returned as tagged enum values.

use std::borrow::Cow;
use std::cell::RefCell;
use std::net::SocketAddr;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use crate::io_state::IoSchedulerState;
use deno_core::AsyncRefCell;
use deno_core::CancelFuture;
use deno_core::CancelHandle;
use deno_core::CancelTryFuture;
use deno_core::JsBuffer;
use deno_core::OpState;
use deno_core::RcRef;
use deno_core::Resource;
use deno_core::ResourceId;
use deno_core::ToJsBuffer;
use deno_core::op2;
use deno_error::JsErrorBox;
use migo_io::pools::ByteTicket;
use serde::Serialize;
use shared::op_state::HostOpState;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tracing::debug;

use super::common::{
    AddrMeta, BACKGROUND_THROTTLE, ReceiveScratch, checked_port, join_host_port, resolve_first,
};

// ── Resource ──

/// Max single TCP read (bytes). 64 KiB balances syscall overhead against the
/// per-socket retained receive scratch; not reduced without a device A/B.
const TCP_RECV_CAPACITY: usize = 65536;

/// Deadline for one TCP write, including waiting for the exclusive writer
/// guard and flushing bytes to the peer. A stalled reader must not retain the
/// writer indefinitely; this is a liveness bound, not a performance claim.
const TCP_WRITE_TIMEOUT: Duration = Duration::from_secs(30);

/// Read half + reusable 64 KiB receive scratch, kept together in one
/// `AsyncRefCell` so concurrent `op_tcp_next_event` calls serialize on the same
/// guard and a cancelled read drops it safely.
struct TcpReceiveState {
    reader: tokio::io::ReadHalf<TcpStream>,
    scratch: ReceiveScratch,
}

pub struct TcpSocketResource {
    rx: AsyncRefCell<TcpReceiveState>,
    writer: AsyncRefCell<tokio::io::WriteHalf<TcpStream>>,
    cancel: CancelHandle,
    write_timeout: Duration,
    local: AddrMeta,
    remote: AddrMeta,
}

// Safety: the deno runtime is single-threaded; all access is via AsyncRefCell.
unsafe impl Send for TcpSocketResource {}
unsafe impl Sync for TcpSocketResource {}

impl Resource for TcpSocketResource {
    fn name(&self) -> Cow<'_, str> {
        "tcpSocket".into()
    }

    fn close(self: Rc<Self>) {
        self.cancel.cancel();
    }
}

// ── Event types ──

/// Events returned by `op_tcp_next_event` to JS.
///
/// The JS polling loop maps these to TCPSocket event callbacks.
#[derive(Serialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum TcpEvent {
    /// Data received from the remote end.
    #[serde(rename = "message")]
    Message {
        /// Raw bytes received (external Uint8Array backing, exact length).
        data: ToJsBuffer,
        /// Remote address info (cached `Arc<str>`, cloned per event; serde's
        /// `rc` feature serializes it as a plain JS string).
        remote_address: Arc<str>,
        remote_family: &'static str,
        remote_port: u16,
        /// Local address info.
        local_address: Arc<str>,
        local_family: &'static str,
        local_port: u16,
    },
    /// An error occurred on the socket.
    #[serde(rename = "error")]
    Error { err_msg: String },
    /// The socket has been closed.
    #[serde(rename = "close")]
    Close,
}

// ── op_tcp_connect ──

/// Result of a successful TCP connect operation.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TcpConnectResult {
    pub rid: ResourceId,
    pub remote_address: Arc<str>,
    pub remote_family: &'static str,
    pub remote_port: u16,
    pub local_address: Arc<str>,
    pub local_family: &'static str,
    pub local_port: u16,
}

/// Connect to a TCP endpoint.
///
/// # Arguments
/// - `address`: IP or hostname to connect to
/// - `port`: Port number
/// - `timeout_secs`: Connection timeout in seconds (0 = default 2s)
///
/// # Returns
/// A `TcpConnectResult` with the resource ID and address info.
#[op2(async(lazy), fast)]
#[serde]
pub async fn op_tcp_connect(
    state: Rc<RefCell<OpState>>,
    #[string] address: String,
    #[smi] port: u32,
    #[smi] timeout_secs: u32,
) -> Result<TcpConnectResult, JsErrorBox> {
    let port = checked_port(port)?;
    let timeout = if timeout_secs == 0 {
        2
    } else {
        timeout_secs.min(300)
    };

    debug!("TCP connect: {}:{} (timeout={}s)", address, port, timeout);

    // Shared network-policy gate: raw TCP must obey the same domain
    // whitelist / IP-literal block as `fetch` and `WebSocket`. Without
    // this, a game that can't reach `evil.example` via `fetch()` could
    // still reach it via `createTCPSocket().connect(...)`.
    {
        let st = state.borrow();
        super::gate::enforce_host_from_state(
            &address,
            port,
            &st,
            super::gate::GateKind::TcpSocket,
        )?;
    }

    // Resolve address — supports both IP and hostname.
    // tokio::net::lookup_host handles DNS resolution asynchronously.
    let addr_str = join_host_port(&address, port);
    let sock_addr = tokio::time::timeout(
        std::time::Duration::from_secs(timeout as u64),
        resolve_first(&addr_str),
    )
    .await
    .map_err(|_| JsErrorBox::generic(format!("connect:fail timeout after {}s", timeout)))?
    .map_err(|e| JsErrorBox::generic(format!("connect:fail resolve error: {}", e)))?;

    // Connect with timeout.
    let stream = tokio::time::timeout(
        std::time::Duration::from_secs(timeout as u64),
        TcpStream::connect(sock_addr),
    )
    .await
    .map_err(|_| JsErrorBox::generic(format!("connect:fail timeout after {}s", timeout)))?
    .map_err(|e| JsErrorBox::generic(format!("connect:fail {}", e)))?;

    // Enable TCP_NODELAY for lower latency (common for game sockets).
    let _ = stream.set_nodelay(true);

    let local_addr = stream
        .local_addr()
        .unwrap_or_else(|_| SocketAddr::from(([0, 0, 0, 0], 0)));
    let remote_addr = stream.peer_addr().unwrap_or(sock_addr);

    let (reader, writer) = tokio::io::split(stream);

    // Format address metadata once at connect; events and the connect result
    // clone the `Arc<str>` (refcount bump) instead of re-formatting per event.
    let local = AddrMeta::new(&local_addr);
    let remote = AddrMeta::new(&remote_addr);
    let resource = TcpSocketResource {
        rx: AsyncRefCell::new(TcpReceiveState {
            reader,
            scratch: ReceiveScratch::new(TCP_RECV_CAPACITY),
        }),
        writer: AsyncRefCell::new(writer),
        cancel: CancelHandle::default(),
        write_timeout: TCP_WRITE_TIMEOUT,
        local: local.clone(),
        remote: remote.clone(),
    };

    let rid = state.borrow_mut().resource_table.add(resource);

    debug!(
        "TCP connected, rid={}, local={}, remote={}",
        rid, local_addr, remote_addr
    );

    Ok(TcpConnectResult {
        rid,
        remote_address: remote.address,
        remote_family: remote.family,
        remote_port: remote.port,
        local_address: local.address,
        local_family: local.family,
        local_port: local.port,
    })
}

// ── op_tcp_next_event ──

/// Poll for the next TCP event (blocking async).
///
/// Returns one of: Message (with data), Error, or Close.
/// The JS layer calls this in a loop to receive data.
#[op2(async(lazy), fast)]
#[serde]
pub async fn op_tcp_next_event(
    state: Rc<RefCell<OpState>>,
    #[smi] rid: ResourceId,
) -> Result<TcpEvent, JsErrorBox> {
    let (resource, backgrounded) = {
        let st = state.borrow();
        let res = st
            .resource_table
            .get::<TcpSocketResource>(rid)
            .map_err(|_| JsErrorBox::generic("TCPSocket not found"))?;
        let bg = st.borrow::<HostOpState>().backgrounded.clone();
        (res, bg)
    };

    let cancel = RcRef::map(&resource, |r| &r.cancel);

    let event = async {
        // Throttle polling when the app is in the background to save
        // CPU and battery.
        if backgrounded.load(Ordering::Relaxed) {
            tokio::time::sleep(BACKGROUND_THROTTLE).await;
        }

        // One `borrow_mut` over the reader+scratch receive state: concurrent
        // `op_tcp_next_event` calls serialize on this guard, and a cancelled
        // read drops it safely.
        let mut rx = RcRef::map(&resource, |r| &r.rx).borrow_mut().await;
        let rx = &mut *rx;

        // Read into the retained scratch. The disjoint field borrow lets the
        // read fill `scratch` while holding `&mut reader`; both borrows end
        // with this block so `scratch.copy_filled` can run afterward.
        let read = {
            let TcpReceiveState { reader, scratch } = rx;
            reader.read(scratch.as_mut_slice()).await
        };

        match read {
            Ok(0) => Ok::<TcpEvent, JsErrorBox>(TcpEvent::Close),
            // Copy exactly the filled prefix into an exact-length `Box<[u8]>`;
            // no trailing zeros from an earlier, longer read leak through.
            Ok(n) => Ok(TcpEvent::Message {
                data: ToJsBuffer::from(rx.scratch.copy_filled(n)),
                remote_address: resource.remote.address.clone(),
                remote_family: resource.remote.family,
                remote_port: resource.remote.port,
                local_address: resource.local.address.clone(),
                local_family: resource.local.family,
                local_port: resource.local.port,
            }),
            Err(e) => Ok(TcpEvent::Error {
                err_msg: e.to_string(),
            }),
        }
    };

    event.try_or_cancel(cancel).await
}

// ── op_tcp_write ──

/// Write data to the TCP socket.
///
/// Accepts either a string or binary buffer.
#[op2(async(lazy))]
pub async fn op_tcp_write(
    state: Rc<RefCell<OpState>>,
    #[smi] rid: ResourceId,
    #[string] data_str: Option<String>,
    #[buffer] data_buf: Option<JsBuffer>,
) -> Result<(), JsErrorBox> {
    let (resource, bytes, pools) = {
        let st = state.borrow();
        let resource = st
            .resource_table
            .get::<TcpSocketResource>(rid)
            .map_err(|_| JsErrorBox::generic("TCPSocket not found"))?;
        let bytes: &[u8] = if let Some(text) = &data_str {
            text.as_bytes()
        } else if let Some(buf) = &data_buf {
            buf
        } else {
            return Err(JsErrorBox::type_error("write:fail no data provided"));
        };
        let pools = st.borrow::<IoSchedulerState>().0.pools().clone();
        (resource, bytes, pools)
    };
    let ticket = reserve_tcp_write_bytes(&pools, bytes.len())?;

    write_tcp_payload(resource, bytes, ticket).await
}

fn reserve_tcp_write_bytes(
    pools: &migo_io::pools::IoPools,
    byte_count: usize,
) -> Result<ByteTicket, JsErrorBox> {
    pools
        .reserve_bytes(byte_count as u64)
        .map_err(|error| JsErrorBox::generic(format!("write:fail byte limit: {error}")))
}

async fn write_tcp_payload(
    resource: Rc<TcpSocketResource>,
    bytes: &[u8],
    ticket: ByteTicket,
) -> Result<(), JsErrorBox> {
    let cancel = RcRef::map(&resource, |r| &r.cancel);
    let write = async {
        let _ticket = ticket;
        tokio::time::timeout(resource.write_timeout, async {
            let mut writer = RcRef::map(&resource, |r| &r.writer).borrow_mut().await;
            writer
                .write_all(bytes)
                .await
                .map_err(|e| JsErrorBox::generic(format!("write:fail {e}")))
        })
        .await
        .map_err(|_| JsErrorBox::generic("write:fail deadline exceeded"))?
    };
    write.try_or_cancel(cancel).await
}
// ── op_tcp_close ──

/// Close the TCP socket, releasing the resource.
#[op2(fast)]
pub fn op_tcp_close(state: &mut OpState, #[smi] rid: ResourceId) -> Result<(), JsErrorBox> {
    // Taking the resource from the table and dropping it triggers
    // Resource::close(), which cancels the CancelHandle and drops
    // reader/writer halves, causing the underlying TcpStream to shut down.
    let resource = state
        .resource_table
        .take::<TcpSocketResource>(rid)
        .map_err(|_| JsErrorBox::generic("TCPSocket not found"))?;
    resource.close();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use deno_core::{JsRuntime, RuntimeOptions, v8};
    use migo_io::pools::IoPools;

    /// Real serde_v8/V8 regression (not just upstream serde_v8): a composite
    /// TCP message envelope must serialize its `data` as a `Uint8Array` backed
    /// by an exact-length external `ArrayBuffer` (offset 0, byteLength == n ==
    /// backing length), with exact contents — including the 0-length case.
    /// Fails on `Vec<u8>` (serde_v8 serializes it as a numeric `Array`).
    #[test]
    fn tcp_message_data_serializes_as_exact_uint8array() {
        let mut rt = JsRuntime::new(RuntimeOptions::default());
        let main_context = rt.main_context();
        let isolate = rt.v8_isolate();
        v8::scope_with_context!(scope, isolate, &main_context);

        for bytes in [Vec::<u8>::new(), vec![1u8, 2, 3, 4, 255, 0, 7]] {
            let n = bytes.len();
            let event = TcpEvent::Message {
                data: ToJsBuffer::from(bytes.clone().into_boxed_slice()),
                remote_address: Arc::from("1.2.3.4"),
                remote_family: "IPv4",
                remote_port: 1234,
                local_address: Arc::from("5.6.7.8"),
                local_family: "IPv4",
                local_port: 5678,
            };

            let v = deno_core::serde_v8::to_v8(scope, &event).expect("serialize TcpEvent");
            let obj = v8::Local::<v8::Object>::try_from(v).expect("event serializes to an object");
            let key: v8::Local<v8::Value> = v8::String::new(scope, "data").unwrap().into();
            let data_val = obj.get(scope, key).expect("data field present");

            assert!(
                data_val.is_uint8_array(),
                "TcpEvent data must serialize as a Uint8Array (external ArrayBuffer), not a \
                 numeric Array, for n={n}"
            );
            let ta = v8::Local::<v8::Uint8Array>::try_from(data_val).unwrap();
            assert_eq!(
                ta.byte_offset(),
                0,
                "external buffer view must start at offset 0"
            );
            assert_eq!(ta.byte_length(), n, "view length must equal payload length");
            let backing = ta.buffer(scope).expect("typed array has a backing buffer");
            assert_eq!(
                backing.byte_length(),
                n,
                "backing ArrayBuffer must be exact length (no slack)"
            );
            let mut out = vec![0u8; n];
            assert_eq!(ta.copy_contents(&mut out), n);
            assert_eq!(out, bytes, "exact contents preserved");
        }
    }

    /// Regression for FNET-01 (docs/audits/2026-09-09/full/io-network.md:57-61):
    /// TCPSocket.connect() had no connection-generation guard, so two rapid calls
    /// before either resolved would both proceed and the second would overwrite
    /// `_rid`, orphaning the first fd.
    ///
    /// The fix adds `_connectGen` to the JS class.  Each call increments the
    /// counter and remembers its own generation (myGen).  When the Rust op result
    /// arrives, if `_connectGen !== myGen` the result is stale: the fd must be
    /// closed immediately via `op_tcp_close`.
    ///
    /// The test executes the production module source with mocked ops, so it
    /// exercises the actual class rather than a copied state-machine model.
    #[test]
    fn fnet01_connect_generation_discards_stale_result_and_closes_orphan() {
        let source = include_str!("08_tcp_socket.js")
            .split("// -- TCPSocket class --")
            .nth(1)
            .expect("TCP socket class source")
            .split("// -- Factory function --")
            .next()
            .expect("TCP socket class body");
        let mut rt = JsRuntime::new(RuntimeOptions::default());
        let script = format!(
            r#"
            const core = {{}};
            const pending = [];
            const closedRids = [];
            function op_tcp_connect() {{
                return new Promise((resolve) => pending.push(resolve));
            }}
            function op_tcp_next_event() {{
                return Promise.resolve({{ type: "close" }});
            }}
            function op_tcp_write() {{ return Promise.resolve(); }}
            function op_tcp_close(rid) {{ closedRids.push(rid); }}
            function createListenerGroup() {{
                return {{ on() {{}}, off() {{}}, trigger() {{}} }};
            }}
            function toExactArrayBuffer(value) {{ return value; }}
            {source}
            const socket = new TCPSocket("ipv4");
            socket.connect({{ address: "mock", port: 1 }});
            socket.connect({{ address: "mock", port: 1 }});
            pending[0]({{ rid: 101 }});
            pending[1]({{ rid: 102 }});
            (async () => {{
                await Promise.resolve();
                await Promise.resolve();
                globalThis.fnetClosed = closedRids.slice();
                globalThis.fnetRid = socket._rid;
            }})();
            "#,
            source = source,
        );
        rt.execute_script("fnet01", script)
            .expect("mocked TCP source must execute");
        let executor = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test executor");
        executor
            .block_on(rt.run_event_loop(Default::default()))
            .expect("mocked TCP promises must settle");
        rt.execute_script(
            "fnet01-check",
            r#"
            if (fnetClosed.length === 0 || fnetClosed[0] !== 101) {
                throw new Error("stale rid was not closed first: " + JSON.stringify(fnetClosed));
            }
            if (fnetRid !== 102) throw new Error("winning rid was not published");
            "#,
        )
        .expect("FNET-01 stale connection must be closed");
    }
    /// NET-05 mock regression: cancellation covers both a pending writer/guard
    /// and the payload captured by that future.  This drives the same
    /// `CancelFuture` layer used by `op_tcp_write` without requiring a real
    /// loopback peer (the SSRF gate rejects loopback integration endpoints).
    #[test]
    fn net05_cancel_drops_blocked_writer_payload() {
        let payload = Arc::new(vec![0u8; 16 * 1024 * 1024]);
        let weak_payload = Arc::downgrade(&payload);
        let cancel = CancelHandle::new_rc();
        let cancel_for_future = cancel.clone();
        let future = async move {
            let _payload = payload;
            std::future::pending::<()>().await;
        }
        .or_cancel(cancel_for_future);

        cancel.cancel();
        let executor = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test executor");
        let result = executor.block_on(future);
        assert!(
            result.is_err(),
            "close cancellation must terminate blocked write"
        );
        assert!(
            weak_payload.upgrade().is_none(),
            "cancelled write must release its retained payload"
        );
    }
    #[test]
    fn io04_tcp_write_refuses_before_native_payload_work() {
        let pools = IoPools::new(9401);
        let error = match reserve_tcp_write_bytes(&pools, 256 * 1024 * 1024 + 1) {
            Ok(_) => panic!("request larger than the process byte limit must be refused"),
            Err(error) => error,
        };
        let message = error.to_string();
        assert!(
            message.contains("byte limit") && message.contains("requested"),
            "structured byte admission error: {message}"
        );
    }

    #[test]
    fn io04_tcp_write_ticket_returns_after_cancellation() {
        let pools = IoPools::new(9402);
        let ticket = reserve_tcp_write_bytes(&pools, 4096).unwrap();
        let cancel = CancelHandle::new_rc();
        let cancel_for_future = cancel.clone();
        let future = async move {
            let _ticket = ticket;
            std::future::pending::<()>().await;
        }
        .or_cancel(cancel_for_future);

        cancel.cancel();
        let executor = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test executor");
        assert!(executor.block_on(future).is_err());
        reserve_tcp_write_bytes(&pools, 4096)
            .expect("cancellation must return the byte credit exactly once");
    }

    #[test]
    fn net05_stalled_reader_deadline_reclaims_writer() {
        let executor = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test executor");
        executor.block_on(async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("loopback listener");
            let address = listener.local_addr().unwrap();
            let client = TcpStream::connect(address).await.expect("loopback connect");
            let (_server, _) = listener.accept().await.expect("accept client");
            let local_addr = client.local_addr().unwrap();
            let remote_addr = client.peer_addr().unwrap();
            let (reader, writer) = tokio::io::split(client);
            let resource = Rc::new(TcpSocketResource {
                rx: AsyncRefCell::new(TcpReceiveState {
                    reader,
                    scratch: ReceiveScratch::new(TCP_RECV_CAPACITY),
                }),
                writer: AsyncRefCell::new(writer),
                cancel: CancelHandle::default(),
                write_timeout: Duration::from_millis(100),
                local: AddrMeta::new(&local_addr),
                remote: AddrMeta::new(&remote_addr),
            });
            let payload = vec![0u8; 64 * 1024 * 1024];
            let pools = IoPools::new(9403);
            let ticket = reserve_tcp_write_bytes(&pools, payload.len()).unwrap();
            let result = tokio::time::timeout(
                Duration::from_secs(5),
                write_tcp_payload(Rc::clone(&resource), &payload, ticket),
            )
            .await
            .expect("deadline must reclaim a stalled write");
            let error = result.expect_err("stalled reader must hit the write deadline");
            assert!(
                error.to_string().contains("deadline exceeded"),
                "structured deadline error: {error}"
            );
            RcRef::map(&resource, |r| &r.writer).borrow_mut().await;
        });
    }

    #[test]
    fn net05_stalled_reader_close_reclaims_writer() {
        let executor = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test executor");
        let local = tokio::task::LocalSet::new();
        local.block_on(&executor, async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("loopback listener");
            let address = listener.local_addr().unwrap();
            let client = TcpStream::connect(address).await.expect("loopback connect");
            let (_server, _) = listener.accept().await.expect("accept client");
            let local_addr = client.local_addr().unwrap();
            let remote_addr = client.peer_addr().unwrap();
            let (reader, writer) = tokio::io::split(client);
            let resource = Rc::new(TcpSocketResource {
                rx: AsyncRefCell::new(TcpReceiveState {
                    reader,
                    scratch: ReceiveScratch::new(TCP_RECV_CAPACITY),
                }),
                writer: AsyncRefCell::new(writer),
                cancel: CancelHandle::default(),
                write_timeout: Duration::from_secs(30),
                local: AddrMeta::new(&local_addr),
                remote: AddrMeta::new(&remote_addr),
            });
            let payload = vec![0u8; 64 * 1024 * 1024];
            let pools = IoPools::new(9404);
            let ticket = reserve_tcp_write_bytes(&pools, payload.len()).unwrap();
            let to_close = Rc::clone(&resource);
            let cancel_task = tokio::task::spawn_local(async move {
                tokio::time::sleep(Duration::from_millis(100)).await;
                to_close.cancel.cancel();
            });
            let result = tokio::time::timeout(
                Duration::from_secs(5),
                write_tcp_payload(Rc::clone(&resource), &payload, ticket),
            )
            .await
            .expect("close must reclaim a stalled write");
            cancel_task.await.expect("cancel task");
            let error = result.expect_err("closed socket write must fail");
            assert!(
                !error.to_string().is_empty(),
                "close cancellation must report a structured error"
            );
            RcRef::map(&resource, |r| &r.writer).borrow_mut().await;
        });
    }
}
