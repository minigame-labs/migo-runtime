//! This runtime's WebSocket ops: adapters over the host's connection.
//!
//! The connection, its limits, its policy and its handshake are the service's
//! (`migo_services::network::websocket`), which both executions call. What is
//! here is what only an op can do: take arguments out of V8, name the service
//! resource through deno's table so `close()` on a rid still reaches it, and
//! hand the answer back in the shape the engine's `WebSocket` facade reads.

use std::cell::RefCell;
use std::rc::Rc;

use deno_core::JsBuffer;
use deno_core::OpState;
use deno_core::ResourceId;
use deno_core::ToJsBuffer;
use deno_core::op2;
use deno_error::JsErrorBox;
use migo_services::network::websocket as service_ws;
use serde::Serialize;
use shared::op_state::HostOpState;

use super::fetch::{NetworkResources, ServiceHandle};

/// The event envelope the engine's JavaScript reads. A message carries text or
/// bytes, never both.
#[derive(Serialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum WsEvent {
    #[serde(rename = "message")]
    Message {
        #[serde(skip_serializing_if = "Option::is_none")]
        data_str: Option<String>,
        /// Binary payload as an external exact-length Uint8Array backing.
        #[serde(skip_serializing_if = "Option::is_none")]
        data_bin: Option<ToJsBuffer>,
        is_binary: bool,
    },
    #[serde(rename = "error")]
    Error { err_msg: String },
    #[serde(rename = "close")]
    Close { code: u16, reason: String },
}

impl From<service_ws::WsEvent> for WsEvent {
    fn from(event: service_ws::WsEvent) -> Self {
        match event {
            service_ws::WsEvent::Text(text) => Self::Message {
                data_str: Some(text),
                data_bin: None,
                is_binary: false,
            },
            service_ws::WsEvent::Binary(data) => Self::Message {
                data_str: None,
                data_bin: Some(ToJsBuffer::from(data)),
                is_binary: true,
            },
            service_ws::WsEvent::Error(err_msg) => Self::Error { err_msg },
            service_ws::WsEvent::Close { code, reason } => Self::Close { code, reason },
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WsCreateResult {
    pub rid: ResourceId,
    pub protocol: String,
    pub extensions: String,
}

fn service_error(error: migo_services::ServiceError) -> JsErrorBox {
    if error.class == "TypeError" {
        JsErrorBox::type_error(error.message)
    } else {
        JsErrorBox::generic(error.message)
    }
}

/// What a socket call needs out of this runtime's state: the session's
/// resources, and the policy the handshake is held to.
fn session(
    state: &Rc<RefCell<OpState>>,
) -> (
    std::sync::Arc<migo_services::network::resources::ResourceTable>,
    shared::op_state::NetworkPolicy,
) {
    let state = state.borrow();
    (
        std::sync::Arc::clone(&state.borrow::<NetworkResources>().0),
        state.borrow::<HostOpState>().network_policy.clone(),
    )
}

#[op2(async(lazy))]
#[serde]
pub async fn op_ws_create(
    state: Rc<RefCell<OpState>>,
    #[string] url: String,
    #[serde] protocols: Vec<String>,
    #[serde] headers: Vec<(String, String)>,
    // `Option` rather than a bare u32 so a stale V8 snapshot, whose baked
    // JavaScript still calls the three-argument form, falls back to the default
    // timeout: deno coerces a missing `Option` smi to `None`, where a missing
    // required smi throws.
    #[smi] timeout_ms: Option<u32>,
) -> Result<WsCreateResult, JsErrorBox> {
    let (resources, policy) = session(&state);
    let handshake = service_ws::create(
        &policy,
        &resources,
        &url,
        &protocols,
        &headers,
        timeout_ms,
    )
    .await
    .map_err(service_error)?;

    // The id content holds is this runtime's, naming the service's.
    let rid = state.borrow_mut().resource_table.add(ServiceHandle::new(
        resources,
        handshake.rid,
        "webSocket",
    ));
    Ok(WsCreateResult {
        rid,
        protocol: handshake.protocol,
        extensions: handshake.extensions,
    })
}

/// The service resource `rid` names, or the error a closed socket gives.
fn connection(
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
        .map_err(|_| JsErrorBox::generic("WebSocket not found"))?;
    Ok((std::sync::Arc::clone(&handle.resources), handle.id))
}

#[op2(async(lazy), fast)]
#[serde]
pub async fn op_ws_next_event(
    state: Rc<RefCell<OpState>>,
    #[smi] rid: ResourceId,
) -> Result<WsEvent, JsErrorBox> {
    let (resources, id) = connection(&state, rid)?;
    let backgrounded = state
        .borrow()
        .borrow::<HostOpState>()
        .backgrounded
        .clone();
    service_ws::next_event(&resources, id, &backgrounded)
        .await
        .map(WsEvent::from)
        .map_err(service_error)
}

#[op2(async(lazy))]
pub async fn op_ws_send(
    state: Rc<RefCell<OpState>>,
    #[smi] rid: ResourceId,
    #[string] data_str: Option<String>,
    #[buffer] data_buf: Option<JsBuffer>,
) -> Result<(), JsErrorBox> {
    let (resources, id) = connection(&state, rid)?;
    let bytes = data_buf.map(|buffer| buffer.to_vec());
    service_ws::send(&resources, id, data_str, bytes)
        .await
        .map_err(service_error)
}

#[op2(async(lazy), fast)]
pub async fn op_ws_close(
    state: Rc<RefCell<OpState>>,
    #[smi] rid: ResourceId,
    #[smi] code: u16,
    #[string] reason: String,
) -> Result<(), JsErrorBox> {
    let (resources, id) = connection(&state, rid)?;
    service_ws::close(&resources, id, code, reason)
        .await
        .map_err(service_error)
}

#[cfg(test)]
mod tests {
    use super::*;
    use deno_core::{JsRuntime, RuntimeOptions, v8};

    /// Real serde_v8/V8 regression for the WebSocket event envelope, covering
    /// the shape the engine's facade reads: a text message, a binary one, an
    /// error and a close.
    #[test]
    fn an_event_serializes_as_the_facade_reads_it() {
        let mut runtime = JsRuntime::new(RuntimeOptions::default());
        deno_core::scope!(scope, runtime);
        let mut rendered = |event: WsEvent| {
            let value = deno_core::serde_v8::to_v8(scope, event).expect("serializes");
            let json = v8::json::stringify(scope, value).expect("JSON");
            json.to_rust_string_lossy(scope)
        };

        assert_eq!(
            rendered(WsEvent::from(service_ws::WsEvent::Text("hi".into()))),
            r#"{"type":"message","dataStr":"hi","isBinary":false}"#
        );
        assert_eq!(
            rendered(WsEvent::from(service_ws::WsEvent::Binary(vec![1, 2]))),
            r#"{"type":"message","dataBin":{"0":1,"1":2},"isBinary":true}"#
        );
        assert_eq!(
            rendered(WsEvent::from(service_ws::WsEvent::Error("boom".into()))),
            r#"{"type":"error","errMsg":"boom"}"#
        );
        assert_eq!(
            rendered(WsEvent::from(service_ws::WsEvent::Close {
                code: 1006,
                reason: String::new(),
            })),
            r#"{"type":"close","code":1006,"reason":""}"#
        );
    }
}
