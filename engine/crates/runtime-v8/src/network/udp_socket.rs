//! This runtime's UDP ops: adapters over the host's socket.
//!
//! The socket, its policy, its address filter and the scratch a datagram lands
//! in are the service's (`migo_services::network::udp`), which both executions
//! call. What is here is what only an op can do: take arguments out of V8, name
//! the service resource through deno's table, and hand the event back in the
//! shape the engine's `UDPSocket` facade reads.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use deno_core::JsBuffer;
use deno_core::OpState;
use deno_core::ResourceId;
use deno_core::ToJsBuffer;
use deno_core::op2;
use deno_error::JsErrorBox;
use migo_services::network::udp as service_udp;
use serde::Serialize;
use shared::op_state::HostOpState;

use super::fetch::{NetworkResources, ServiceHandle};

/// The events `op_udp_next_event` answers with.
#[derive(Serialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum UdpEvent {
    /// A datagram: an external Uint8Array backing of exact length, with the
    /// peer it came from and the socket it arrived on.
    #[serde(rename = "message")]
    Message {
        data: ToJsBuffer,
        remote_address: Arc<str>,
        remote_family: &'static str,
        remote_port: u16,
        size: usize,
        local_address: Arc<str>,
        local_family: &'static str,
        local_port: u16,
    },
    #[serde(rename = "error")]
    Error { err_msg: String },
    /// The socket has been closed.
    #[serde(rename = "close")]
    #[allow(dead_code)]
    Close,
}

impl From<service_udp::UdpEvent> for UdpEvent {
    fn from(event: service_udp::UdpEvent) -> Self {
        match event {
            service_udp::UdpEvent::Message {
                data,
                remote,
                local,
            } => Self::Message {
                size: data.len(),
                data: ToJsBuffer::from(data),
                remote_address: remote.address,
                remote_family: remote.family,
                remote_port: remote.port,
                local_address: local.address,
                local_family: local.family,
                local_port: local.port,
            },
            service_udp::UdpEvent::Error(err_msg) => Self::Error { err_msg },
        }
    }
}

/// What a bind answers.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UdpBindResult {
    pub rid: ResourceId,
    pub port: u16,
    pub address: Arc<str>,
    pub family: &'static str,
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
        .map_err(|_| JsErrorBox::generic("UDPSocket not found"))?;
    Ok((std::sync::Arc::clone(&handle.resources), handle.id))
}

/// Bind a local port. Synchronous, as the facade's `bind` is.
#[op2]
#[serde]
pub fn op_udp_bind(
    state: Rc<RefCell<OpState>>,
    #[smi] port: u32,
    #[string] socket_type: String,
) -> Result<UdpBindResult, JsErrorBox> {
    let resources = std::sync::Arc::clone(&state.borrow().borrow::<NetworkResources>().0);
    let bound = service_udp::bind(&resources, port, &socket_type).map_err(service_error)?;
    let rid = state.borrow_mut().resource_table.add(ServiceHandle::new(
        resources,
        bound.rid,
        "udpSocket",
    ));
    Ok(UdpBindResult {
        rid,
        port: bound.local.port,
        address: bound.local.address,
        family: bound.local.family,
    })
}

/// Set the socket's default destination, which an unaddressed write goes to.
#[op2(async(lazy), fast)]
pub async fn op_udp_connect(
    state: Rc<RefCell<OpState>>,
    #[smi] rid: ResourceId,
    #[string] address: String,
    #[smi] port: u32,
) -> Result<(), JsErrorBox> {
    let (resources, id) = socket(&state, rid)?;
    let policy = state
        .borrow()
        .borrow::<HostOpState>()
        .network_policy
        .clone();
    service_udp::connect(&policy, &resources, id, &address, port)
        .await
        .map_err(service_error)
}

#[op2(async(lazy))]
#[allow(clippy::too_many_arguments)]
pub async fn op_udp_send(
    state: Rc<RefCell<OpState>>,
    #[smi] rid: ResourceId,
    #[string] address: String,
    #[smi] port: u32,
    #[string] data_str: Option<String>,
    #[buffer] data_buf: Option<JsBuffer>,
    #[smi] offset: u32,
    #[smi] length: u32,
    set_broadcast: bool,
) -> Result<(), JsErrorBox> {
    let (resources, id) = socket(&state, rid)?;
    let policy = state
        .borrow()
        .borrow::<HostOpState>()
        .network_policy
        .clone();
    let bytes = data_buf.map(|buffer| buffer.to_vec());
    service_udp::send(
        &policy,
        &resources,
        id,
        &address,
        port,
        data_str,
        bytes,
        offset,
        length,
        set_broadcast,
    )
    .await
    .map_err(service_error)
}

#[op2(fast)]
pub fn op_udp_set_ttl(
    state: &mut OpState,
    #[smi] rid: ResourceId,
    #[smi] ttl: u32,
) -> Result<(), JsErrorBox> {
    let handle = state
        .resource_table
        .get::<ServiceHandle>(rid)
        .map_err(|_| JsErrorBox::generic("UDPSocket not found"))?;
    service_udp::set_ttl(&handle.resources, handle.id, ttl).map_err(service_error)
}

#[op2(async(lazy), fast)]
#[serde]
pub async fn op_udp_next_event(
    state: Rc<RefCell<OpState>>,
    #[smi] rid: ResourceId,
) -> Result<UdpEvent, JsErrorBox> {
    let (resources, id) = socket(&state, rid)?;
    let backgrounded = state
        .borrow()
        .borrow::<HostOpState>()
        .backgrounded
        .clone();
    service_udp::next_event(&resources, id, &backgrounded)
        .await
        .map(UdpEvent::from)
        .map_err(service_error)
}

/// Close the socket and release the handle content held.
#[op2(fast)]
pub fn op_udp_close(state: &mut OpState, #[smi] rid: ResourceId) -> Result<(), JsErrorBox> {
    let handle = state
        .resource_table
        .take::<ServiceHandle>(rid)
        .map_err(|_| JsErrorBox::generic("UDPSocket not found"))?;
    service_udp::close(&handle.resources, handle.id).map_err(service_error)
}
