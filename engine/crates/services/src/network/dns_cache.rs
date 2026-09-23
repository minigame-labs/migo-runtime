//! Background DNS pre-resolution.
//!
//! `op_prefetch_dns` calls [`pre_resolve`] to warm the **OS** resolver
//! cache so subsequent `fetch()` / socket connects skip the DNS round
//! trip.
//!
//! There used to be an in-process `Mutex<HashMap>` cache here as well,
//! but nothing ever read it: the SSRF-checking resolver deliberately
//! re-resolves per request (so a rebound A-record can't be served from a
//! stale entry), and `fetch` never consulted it. A write-only cache just
//! costs a global lock plus dead memory, so it was removed — warming the
//! OS resolver is the part that actually helps, and that lives on here.

use std::net::SocketAddr;

use tracing::debug;

use crate::ServiceError;

/// Max concurrent DNS resolutions run by the background supervisor.
const MAX_CONCURRENT_RESOLVES: usize = 6;

/// The most hostnames one `prefetchDns` call acts on, so a game passing
/// thousands of names cannot translate into an unbounded pile of background
/// DNS work.
pub const MAX_PREFETCH_DNS_HOSTS: usize = 32;

/// `op_prefetch_dns`'s body: the JSON array content passed, filtered by this
/// session's policy and bounded, warmed in the background.
///
/// The policy is applied here rather than at the resolver because a prefetch
/// must not leak a hostname the session would refuse to connect to: the
/// question this answers is "may content reach it at all", and the answer is
/// the same one `fetch` gets.
pub fn prefetch_dns(policy: &shared::op_state::NetworkPolicy, hosts_json: &str) -> Result<(), ServiceError> {
    let hosts: Vec<String> = serde_json::from_str(hosts_json)
        .map_err(|error| ServiceError::classed("TypeError", format!("prefetchDns: invalid JSON: {error}")))?;
    let mut allowed: Vec<String> = hosts
        .into_iter()
        .filter(|host| super::gate::is_host_whitelisted(host, policy))
        .collect();
    if allowed.is_empty() {
        return Ok(());
    }
    allowed.truncate(MAX_PREFETCH_DNS_HOSTS);
    debug!("prefetchDns: pre-resolving {} hosts", allowed.len());
    pre_resolve(allowed);
    Ok(())
}

/// Pre-resolve a list of hostnames in the background, warming the OS
/// resolver cache so later connects are faster.
///
/// SSRF filtering is applied for parity with the request-time resolver:
/// if a host resolves entirely outside the blocked ranges it is treated
/// as a successful warm; hosts that only resolve to private/loopback
/// ranges are logged and skipped. The op returns immediately.
///
/// A single supervising task processes the hosts in bounded-concurrency
/// batches instead of spawning one task per host — otherwise one JS call
/// could fan out into an unbounded number of background tasks. Callers
/// (`op_prefetch_dns`) have already applied the domain whitelist and
/// capped the list length.
pub fn pre_resolve(hosts: Vec<String>) {
    tokio::spawn(async move {
        for batch in hosts.chunks(MAX_CONCURRENT_RESOLVES) {
            let resolves = batch.iter().cloned().map(resolve_one);
            futures::future::join_all(resolves).await;
        }
    });
}

async fn resolve_one(host: String) {
    let lookup_addr = format!("{}:0", host);
    match tokio::net::lookup_host(&lookup_addr).await {
        Ok(addrs) => {
            let addrs: Vec<SocketAddr> = addrs.collect();
            let all_safe = addrs
                .iter()
                .all(|a| !super::address_filter::is_blocked_address(a));
            if all_safe && !addrs.is_empty() {
                debug!("dns_cache: pre-resolved {} -> {} addrs", host, addrs.len());
            } else {
                debug!("dns_cache: skipped {} (no safe public address)", host);
            }
        }
        Err(e) => {
            debug!("dns_cache: failed to pre-resolve {}: {}", host, e);
        }
    }
}
