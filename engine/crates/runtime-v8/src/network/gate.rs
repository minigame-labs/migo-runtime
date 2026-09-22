//! This runtime's side of the network policy gate.
//!
//! The rules -- scheme whitelist, IP-literal block, domain whitelist, HTTPS
//! enforcement -- are `migo_services::network::gate`'s, so both executions
//! allow and refuse exactly the same requests. What is here is the two things
//! only an op can do: read the policy out of this runtime's `OpState`, and
//! answer a refusal as the error class op call sites bubble up with `?`.

use deno_core::OpState;
use deno_core::url::Url;
use deno_error::JsErrorBox;

// Re-exported so this runtime's call sites name one gate: the rules and their
// vocabulary are the service's, and only the two adaptors below are here.
pub(crate) use migo_services::network::gate::{GateKind, is_host_whitelisted};
use shared::op_state::HostOpState;

/// Enforce the policy for a raw socket's host and port.
pub(crate) fn enforce_host_from_state(
    host: &str,
    port: u16,
    state: &OpState,
    kind: GateKind,
) -> Result<(), JsErrorBox> {
    let policy = &state.borrow::<HostOpState>().network_policy;
    migo_services::network::gate::enforce_host(host, port, policy, kind)
        .map_err(JsErrorBox::generic)
}

/// Enforce the policy using the runtime's `HostOpState`.
pub(crate) fn enforce_from_state(
    url: &Url,
    state: &OpState,
    kind: GateKind,
) -> Result<(), JsErrorBox> {
    let policy = &state.borrow::<HostOpState>().network_policy;
    migo_services::network::gate::enforce(url, policy, kind).map_err(JsErrorBox::generic)
}
