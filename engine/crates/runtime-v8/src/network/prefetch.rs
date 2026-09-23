//! Asset prefetch and DNS pre-resolve: this runtime's adapters.
//!
//! Both bodies are the service's (`migo_services::network::prefetch` and
//! `::dns_cache`), which both executions call. What is here is what only an op
//! can do: read this session's policy and client out of the runtime's state.

use std::cell::RefCell;
use std::rc::Rc;

use deno_core::OpState;
use deno_core::op2;
use deno_error::JsErrorBox;
use migo_services::network::{dns_cache, prefetch};

use super::fetch::get_or_create_client_from_state;

/// Pre-resolve a list of hostnames so subsequent fetch() calls are faster.
///
/// Accepts a JSON-encoded string array: `["api.example.com", "cdn.example.com"]`
///
/// The work is the service's (`migo_services::network::dns_cache`), which both
/// executions call: the policy filter, the cap and the background resolve are
/// one set of rules. Resolution happens in background Tokio tasks; this op
/// returns immediately.
#[op2(fast)]
pub fn op_prefetch_dns(
    state: &mut OpState,
    #[string] hosts_json: String,
) -> Result<(), JsErrorBox> {
    let policy = state
        .borrow::<shared::op_state::HostOpState>()
        .network_policy
        .clone();
    dns_cache::prefetch_dns(&policy, &hosts_json)
        .map_err(|error| JsErrorBox::type_error(error.message))
}

/// Prefetch a list of asset URLs in the background.
///
/// The work is the service's, which both executions call: the policy filter,
/// the concurrency bound and the drained-body cap are one set of rules. The
/// HTTP/2 client is this runtime's, because the connections it warms are the
/// ones its own `fetch` will use.
#[op2(async(lazy), fast)]
pub async fn op_prefetch_assets(
    state: Rc<RefCell<OpState>>,
    #[string] urls_json: String,
) -> Result<(), JsErrorBox> {
    let (policy, client) = {
        let mut state = state.borrow_mut();
        let policy = state
            .borrow::<shared::op_state::HostOpState>()
            .network_policy
            .clone();
        let client = get_or_create_client_from_state(&mut state, true)
            .map_err(|error| JsErrorBox::generic(format!("prefetchAssets: {error}")))?;
        (policy, client)
    };
    prefetch::prefetch_assets(&policy, &client, &urls_json)
        .await
        .map_err(|error| JsErrorBox::type_error(error.message))
}
