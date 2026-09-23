//! Warming what a game is about to ask for.
//!
//! Two calls with one shape: `prefetchDns` resolves hostnames and
//! `prefetchAssets` opens connections. Neither answers content anything -- the
//! benefit is that the TCP and TLS handshakes, and the DNS answers, are already
//! in hand when the request that needs them is made.
//!
//! Best effort, and silent: a URL the policy refuses is skipped rather than
//! reported, because a prefetch that failed has cost the game nothing and a
//! prefetch that reported would make a warm-up into an error path.

use std::time::Duration;

use reqwest::Client;
use tracing::debug;
use url::Url;

use shared::op_state::NetworkPolicy;

use crate::ServiceError;

use super::gate::{self, GateKind};

/// The most prefetch requests in flight at once.
const MAX_CONCURRENT_PREFETCH: usize = 6;

/// The most of a prefetched body that is drained.
///
/// Draining a small body lets an HTTP/1.1 connection go back to the pool warm;
/// a larger one is left unread, because the handshake is already warmed and
/// pulling megabytes nobody keeps would only spend the game's bandwidth.
/// Nothing is stored either way.
const MAX_DRAIN_BODY: usize = 1024 * 1024;

/// `op_prefetch_assets`'s body: the URLs content named, filtered by this
/// session's policy, fetched a bounded number at a time.
pub async fn prefetch_assets(
    policy: &NetworkPolicy,
    client: &Client,
    urls_json: &str,
) -> Result<(), ServiceError> {
    let urls: Vec<String> = serde_json::from_str(urls_json).map_err(|error| {
        ServiceError::classed("TypeError", format!("prefetchAssets: invalid JSON: {error}"))
    })?;
    if urls.is_empty() {
        return Ok(());
    }

    // One gate call per URL, the same one `fetch` makes: a rule this fails is a
    // URL skipped, because a warm-up must not surface a failure to the game.
    let allowed: Vec<Url> = urls
        .iter()
        .filter_map(|url| Url::parse(url).ok())
        .filter(|url| gate::enforce(url, policy, GateKind::Prefetch).is_ok())
        .collect();
    if allowed.is_empty() {
        return Ok(());
    }
    debug!(
        "prefetchAssets: fetching {} URLs (max {MAX_CONCURRENT_PREFETCH} in flight)",
        allowed.len()
    );

    // Every URL is fetched, a batch at a time: truncating to the concurrency
    // bound would silently drop the rest, which is how this path once behaved.
    for batch in batches(&allowed, MAX_CONCURRENT_PREFETCH) {
        let mut running = Vec::with_capacity(batch.len());
        for url in batch {
            running.push(tokio::spawn(warm(client.clone(), url)));
        }
        for handle in running {
            let _ = handle.await;
        }
    }
    Ok(())
}

/// Fetch one URL for its connection, draining a small body and stopping at
/// [`MAX_DRAIN_BODY`]. Failures are swallowed: this is a warm-up.
async fn warm(client: Client, url: Url) {
    let shown = url.to_string();
    let Ok(mut response) = client.get(url).timeout(Duration::from_secs(30)).send().await else {
        debug!("prefetchAssets: {shown} could not be fetched");
        return;
    };
    let status = response.status().as_u16();
    // Only a success or a redirect is worth draining, and a body that declares
    // itself past the cap is not drained at all.
    let too_large = response
        .content_length()
        .is_some_and(|length| length as usize > MAX_DRAIN_BODY);
    if !(200..400).contains(&status) || too_large {
        debug!("prefetchAssets: {shown} -> {status} (warmed, body not drained)");
        return;
    }
    // The cap is enforced on the stream rather than on `Content-Length`, so a
    // chunked answer with no declared length cannot pull an unbounded body into
    // memory.
    let mut drained = 0usize;
    loop {
        match response.chunk().await {
            Ok(Some(chunk)) => {
                drained += chunk.len();
                if drained >= MAX_DRAIN_BODY {
                    break;
                }
            }
            Ok(None) => break,
            Err(error) => {
                debug!("prefetchAssets: {shown} body read error: {error}");
                return;
            }
        }
    }
    debug!("prefetchAssets: {shown} -> {status} ({drained} bytes drained, warmed)");
}

/// `items` in sequential batches of at most `max_in_flight`, in order and
/// including every item. A bound of zero degrades to one at a time rather than
/// panicking in `chunks(0)`.
fn batches<T: Clone>(items: &[T], max_in_flight: usize) -> Vec<Vec<T>> {
    if items.is_empty() {
        return Vec::new();
    }
    items
        .chunks(max_in_flight.max(1))
        .map(<[T]>::to_vec)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn batches_cover_every_item_without_dropping() {
        // Regression: `op_prefetch_assets` once fetched only the first
        // `MAX_CONCURRENT_PREFETCH` URLs and silently drop the rest. The
        // batching helper must schedule EVERY url, in order, while still
        // bounding how many run at once.
        let items: Vec<u32> = (0..20).collect();
        let grouped = batches(&items, 6);

        let flat: Vec<u32> = grouped.iter().flatten().copied().collect();
        assert_eq!(flat, items, "no item may be dropped");
        assert!(
            grouped.iter().all(|batch| batch.len() <= 6),
            "each batch must respect the concurrency bound"
        );
        assert_eq!(grouped.len(), 4, "20 items / 6 per batch => 6+6+6+2");
    }

    #[test]
    fn batches_handle_empty_and_singletons() {
        assert!(batches::<u32>(&[], 6).is_empty());
        assert_eq!(batches(&[42u32], 6), vec![vec![42]]);
    }

    #[test]
    fn batches_never_divide_by_zero() {
        // A zero bound must not panic via `slice::chunks(0)`; it degrades
        // to one-at-a-time rather than dropping or crashing.
        let items: Vec<u32> = (0..3).collect();
        let grouped = batches(&items, 0);
        let flat: Vec<u32> = grouped.iter().flatten().copied().collect();
        assert_eq!(flat, items);
        assert!(grouped.iter().all(|batch| batch.len() <= 1));
    }
}
