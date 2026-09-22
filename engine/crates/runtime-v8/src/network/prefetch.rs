//! Asset prefetch and DNS pre-resolve ops.
//!
//! Provides two ops:
//!
//! - `op_prefetch_dns`: Takes a JSON array of hostnames and pre-resolves them
//!   in the background, warming the OS resolver cache.
//!
//! - `op_prefetch_assets`: Takes a JSON array of URLs and fires off background
//!   HTTP GET requests purely to warm the HTTP connection pool and the OS DNS
//!   cache. Responses are NOT cached in-process — `fetch()` re-requests
//!   normally — so prefetch never pins asset bytes in memory.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use deno_core::OpState;
use deno_core::op2;
use deno_core::serde_json;
use deno_core::url::Url;
use deno_error::JsErrorBox;
use tracing::debug;

use super::fetch::get_or_create_client_from_state;
use migo_services::network::dns_cache;

/// Maximum number of concurrent prefetch requests.
const MAX_CONCURRENT_PREFETCH: usize = 6;

/// Maximum hostnames a single `prefetchDns` call will act on, so a game
/// passing thousands of names can't translate into an unbounded pile of
/// background DNS work.
const MAX_PREFETCH_DNS_HOSTS: usize = 32;

/// Largest response body we will drain during a prefetch. Draining a
/// small body lets the HTTP/1.1 connection return to the pool warm;
/// larger bodies are left unread (the TCP/TLS handshake is already warmed
/// and pulling a multi-megabyte asset we will not keep would only waste
/// bandwidth). Bodies are never stored — see the module note above.
const MAX_DRAIN_BODY: usize = 1024 * 1024;

// ---------------------------------------------------------------------------
// op_prefetch_dns
// ---------------------------------------------------------------------------

/// Pre-resolve a list of hostnames so subsequent fetch() calls are faster.
///
/// Accepts a JSON-encoded string array: `["api.example.com", "cdn.example.com"]`
///
/// Resolution happens in background Tokio tasks. This op returns immediately.
/// Invalid or private/loopback addresses are silently skipped.
#[op2(fast)]
pub fn op_prefetch_dns(
    state: &mut OpState,
    #[string] hosts_json: String,
) -> Result<(), JsErrorBox> {
    let hosts: Vec<String> = serde_json::from_str(&hosts_json)
        .map_err(|e| JsErrorBox::type_error(format!("prefetchDns: invalid JSON: {}", e)))?;

    if hosts.is_empty() {
        return Ok(());
    }

    // Apply the same domain whitelist as fetch/WebSocket: prefetch must
    // not warm the resolver (or leak hostnames to the DNS server) for
    // hosts the policy would refuse to connect to. Empty whitelist =
    // allow all, matching the gate.
    let policy = state
        .borrow::<shared::op_state::HostOpState>()
        .network_policy
        .clone();
    let mut allowed: Vec<String> = hosts
        .into_iter()
        .filter(|h| super::gate::is_host_whitelisted(h, &policy))
        .collect();

    if allowed.is_empty() {
        return Ok(());
    }
    // Bound the fan-out so one call can't schedule unbounded background work.
    allowed.truncate(MAX_PREFETCH_DNS_HOSTS);

    debug!("prefetchDns: pre-resolving {} hosts", allowed.len());
    dns_cache::pre_resolve(allowed);
    Ok(())
}

/// Partition `items` into sequential batches, each at most
/// `max_in_flight` long, preserving order and including **every** item.
///
/// `op_prefetch_assets` uses this to bound how many prefetch requests are
/// in flight at once without dropping URLs: it spawns one batch, awaits
/// it, then moves on to the next. A `max_in_flight` of `0` degrades to
/// one-at-a-time rather than panicking in `slice::chunks(0)`.
fn concurrency_batches<T: Clone>(items: &[T], max_in_flight: usize) -> Vec<Vec<T>> {
    if items.is_empty() {
        return Vec::new();
    }
    items
        .chunks(max_in_flight.max(1))
        .map(|c| c.to_vec())
        .collect()
}

// ---------------------------------------------------------------------------
// op_prefetch_assets
// ---------------------------------------------------------------------------

/// Prefetch a list of asset URLs in the background.
///
/// Accepts a JSON-encoded string array: `["https://cdn.example.com/a.png", ...]`
///
/// Each URL is fetched via HTTP GET using the shared connection pool. The
/// primary benefit is warming the TCP/TLS connection pool and DNS cache.
/// Small responses (under 1 MB) are also cached in memory.
///
/// Security: SSRF checks, domain whitelist, and HTTPS enforcement are applied
/// to every URL, same as regular `fetch()`.
#[op2(async(lazy), fast)]
pub async fn op_prefetch_assets(
    state: Rc<RefCell<OpState>>,
    #[string] urls_json: String,
) -> Result<(), JsErrorBox> {
    let urls: Vec<String> = serde_json::from_str(&urls_json)
        .map_err(|e| JsErrorBox::type_error(format!("prefetchAssets: invalid JSON: {}", e)))?;

    if urls.is_empty() {
        return Ok(());
    }

    // Get the shared HTTP/2 client from OpState
    let client = {
        let mut st = state.borrow_mut();
        get_or_create_client_from_state(&mut st, true)
            .map_err(|e| JsErrorBox::generic(format!("prefetchAssets: {}", e)))?
    };

    // Validate all URLs upfront and apply security checks
    let mut valid_urls: Vec<Url> = Vec::new();
    {
        let st = state.borrow();
        for url_str in &urls {
            let url = match Url::parse(url_str) {
                Ok(u) => u,
                Err(_) => continue, // skip invalid URLs
            };
            // Single gate call replaces the three-way SSRF + whitelist
            // + HTTPS check.  Any rule failure -> quietly skip (this
            // is a best-effort prefetch, errors must not leak out).
            if super::gate::enforce_from_state(&url, &st, super::gate::GateKind::Prefetch).is_err()
            {
                continue;
            }
            valid_urls.push(url);
        }
    }

    if valid_urls.is_empty() {
        return Ok(());
    }

    debug!(
        "prefetchAssets: fetching {} URLs (max {} in flight)",
        valid_urls.len(),
        MAX_CONCURRENT_PREFETCH
    );

    // Fetch EVERY valid URL, capping how many run at once. This path
    // used to truncate to the first MAX_CONCURRENT_PREFETCH URLs and
    // silently drop the rest; batching keeps the concurrency bound while
    // still warming all of the requested assets.
    for batch in concurrency_batches(&valid_urls, MAX_CONCURRENT_PREFETCH) {
        let mut handles = Vec::with_capacity(batch.len());
        for url in batch {
            handles.push(tokio::spawn(prefetch_one(client.clone(), url)));
        }
        for handle in handles {
            let _ = handle.await;
        }
    }

    Ok(())
}

/// Fetch a single prefetch URL and, when the response is small and
/// successful, store it in the in-process prefetch cache. Errors are
/// swallowed: prefetch is best-effort warmup and must never surface a
/// failure to the calling game.
async fn prefetch_one(client: reqwest::Client, url: Url) {
    let url_str = url.to_string();
    match client
        .get(url)
        .timeout(Duration::from_secs(30))
        .send()
        .await
    {
        Ok(mut resp) => {
            let status = resp.status().as_u16();
            // Only warm on success/redirect, and skip draining when a
            // declared Content-Length already exceeds the cap.
            let cl_too_big = resp
                .content_length()
                .is_some_and(|len| len as usize > MAX_DRAIN_BODY);
            if !(200..400).contains(&status) || cl_too_big {
                debug!(
                    "prefetchAssets: {} -> {} (warmed, body not drained)",
                    url_str, status
                );
                return;
            }
            // Drain up to MAX_DRAIN_BODY so an HTTP/1.1 connection returns
            // to the pool warm, then stop. The cap is enforced on the
            // stream itself (not via Content-Length), so a chunked /
            // Content-Length-less response can't pull an unbounded body
            // into memory — the previous `resp.bytes()` did exactly that
            // whenever the header was absent. Nothing is stored either way.
            let mut drained = 0usize;
            loop {
                match resp.chunk().await {
                    Ok(Some(chunk)) => {
                        drained += chunk.len();
                        if drained >= MAX_DRAIN_BODY {
                            break;
                        }
                    }
                    Ok(None) => break,
                    Err(e) => {
                        debug!("prefetchAssets: {} body read error: {}", url_str, e);
                        return;
                    }
                }
            }
            debug!(
                "prefetchAssets: {} -> {} ({} bytes drained, warmed)",
                url_str, status, drained
            );
        }
        Err(e) => {
            debug!("prefetchAssets: {} fetch error: {}", url_str, e);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn batches_cover_every_item_without_dropping() {
        // Regression: `op_prefetch_assets` used to fetch only the first
        // `MAX_CONCURRENT_PREFETCH` URLs and silently drop the rest. The
        // batching helper must schedule EVERY url, in order, while still
        // bounding how many run at once.
        let items: Vec<u32> = (0..20).collect();
        let batches = concurrency_batches(&items, 6);

        let flat: Vec<u32> = batches.iter().flatten().copied().collect();
        assert_eq!(flat, items, "no item may be dropped");
        assert!(
            batches.iter().all(|b| b.len() <= 6),
            "each batch must respect the concurrency bound"
        );
        assert_eq!(batches.len(), 4, "20 items / 6 per batch => 6+6+6+2");
    }

    #[test]
    fn batches_handle_empty_and_singletons() {
        assert!(concurrency_batches::<u32>(&[], 6).is_empty());
        assert_eq!(concurrency_batches(&[42u32], 6), vec![vec![42]]);
    }

    #[test]
    fn batches_never_divide_by_zero() {
        // A zero bound must not panic via `slice::chunks(0)`; it degrades
        // to one-at-a-time rather than dropping or crashing.
        let items: Vec<u32> = (0..3).collect();
        let batches = concurrency_batches(&items, 0);
        let flat: Vec<u32> = batches.iter().flatten().copied().collect();
        assert_eq!(flat, items);
        assert!(batches.iter().all(|b| b.len() <= 1));
    }
}
