//! This runtime's TCP ops: adapters over the host's socket.
//!
//! The connection, its policy, its deadlines and the scratch a read fills are
//! the service's (`migo_services::network::tcp`), which both executions call.
//! What is here is what only an op can do: take arguments out of V8, name the
//! service resource through deno's table, and hand the event back in the shape
//! the engine's `TCPSocket` facade reads.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use deno_core::JsBuffer;
use deno_core::OpState;
use deno_core::ResourceId;
use deno_core::ToJsBuffer;
use deno_core::op2;
use deno_error::JsErrorBox;
use migo_services::network::tcp as service_tcp;
use serde::Serialize;
use shared::op_state::HostOpState;

use crate::io_state::IoSchedulerState;

use super::fetch::{NetworkResources, ServiceHandle};

/// The events `op_tcp_next_event` answers with, which the engine's polling
/// loop maps onto the facade's callbacks.
#[derive(Serialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum TcpEvent {
    /// Bytes from the peer: an external Uint8Array backing of exact length,
    /// with both ends of the connection they came over.
    #[serde(rename = "message")]
    Message {
        data: ToJsBuffer,
        /// The cached `Arc<str>` cloned per event; serde's `rc` feature
        /// serializes it as a plain JS string.
        remote_address: Arc<str>,
        remote_family: &'static str,
        remote_port: u16,
        local_address: Arc<str>,
        local_family: &'static str,
        local_port: u16,
    },
    #[serde(rename = "error")]
    Error { err_msg: String },
    #[serde(rename = "close")]
    Close,
}

impl From<service_tcp::TcpEvent> for TcpEvent {
    fn from(event: service_tcp::TcpEvent) -> Self {
        match event {
            service_tcp::TcpEvent::Message { data, endpoints } => Self::Message {
                data: ToJsBuffer::from(data),
                remote_address: endpoints.remote.address,
                remote_family: endpoints.remote.family,
                remote_port: endpoints.remote.port,
                local_address: endpoints.local.address,
                local_family: endpoints.local.family,
                local_port: endpoints.local.port,
            },
            service_tcp::TcpEvent::Error(err_msg) => Self::Error { err_msg },
            service_tcp::TcpEvent::Close => Self::Close,
        }
    }
}

/// What a connect answers: the handle, and both ends of the connection.
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

fn service_error(error: migo_services::ServiceError) -> JsErrorBox {
    if error.class == "TypeError" {
        JsErrorBox::type_error(error.message)
    } else {
        JsErrorBox::generic(error.message)
    }
}

/// The service resource `rid` names, or the error a closed socket gives.
fn socket(
    state: &Rc<RefCell<OpState>>,
    rid: ResourceId,
) -> Result<
    (
        std::sync::Arc<migo_services::network::resources::ResourceTable>,
        migo_services::network::resources::ResourceId,
    ),
    JsErrorBox,
> {
    let state = state.borrow();
    let handle = state
        .resource_table
        .get::<ServiceHandle>(rid)
        .map_err(|_| JsErrorBox::generic("TCPSocket not found"))?;
    Ok((std::sync::Arc::clone(&handle.resources), handle.id))
}

#[op2(async(lazy), fast)]
#[serde]
pub async fn op_tcp_connect(
    state: Rc<RefCell<OpState>>,
    #[string] address: String,
    #[smi] port: u32,
    #[smi] timeout_secs: u32,
) -> Result<TcpConnectResult, JsErrorBox> {
    let (resources, policy) = {
        let state = state.borrow();
        (
            std::sync::Arc::clone(&state.borrow::<NetworkResources>().0),
            state.borrow::<HostOpState>().network_policy.clone(),
        )
    };
    let connected = service_tcp::connect(&policy, &resources, &address, port, timeout_secs)
        .await
        .map_err(service_error)?;

    // The id content holds is this runtime's, naming the service's.
    let rid = state.borrow_mut().resource_table.add(ServiceHandle::new(
        resources,
        connected.rid,
        "tcpSocket",
    ));
    let service_tcp::TcpEndpoints { local, remote } = connected.endpoints;
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

#[op2(async(lazy), fast)]
#[serde]
pub async fn op_tcp_next_event(
    state: Rc<RefCell<OpState>>,
    #[smi] rid: ResourceId,
) -> Result<TcpEvent, JsErrorBox> {
    let (resources, id) = socket(&state, rid)?;
    let backgrounded = state
        .borrow()
        .borrow::<HostOpState>()
        .backgrounded
        .clone();
    service_tcp::next_event(&resources, id, &backgrounded)
        .await
        .map(TcpEvent::from)
        .map_err(service_error)
}

#[op2(async(lazy))]
pub async fn op_tcp_write(
    state: Rc<RefCell<OpState>>,
    #[smi] rid: ResourceId,
    #[string] data_str: Option<String>,
    #[buffer] data_buf: Option<JsBuffer>,
) -> Result<(), JsErrorBox> {
    let (resources, id) = socket(&state, rid)?;
    let pools = state.borrow().borrow::<IoSchedulerState>().0.pools().clone();
    let bytes = data_buf.map(|buffer| buffer.to_vec());
    service_tcp::write(&resources, &pools, id, data_str, bytes)
        .await
        .map_err(service_error)
}

/// Close the socket and release the handle content held.
#[op2(fast)]
pub fn op_tcp_close(state: &mut OpState, #[smi] rid: ResourceId) -> Result<(), JsErrorBox> {
    // Taking the handle out drops it, and its `close` reaches the service's
    // resource: the read in flight is cancelled and both halves drop.
    let handle = state
        .resource_table
        .take::<ServiceHandle>(rid)
        .map_err(|_| JsErrorBox::generic("TCPSocket not found"))?;
    service_tcp::close(&handle.resources, handle.id).map_err(service_error)
}

#[cfg(test)]
mod tests {
    use super::*;
    use deno_core::{JsRuntime, RuntimeOptions, v8};

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
}
