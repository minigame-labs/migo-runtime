//! Fetching an `http(s)://` image source under a session's network policy.
//! Decoding what comes back is `image::inline`'s.
//!
//! The fetch is here rather than in an op because both executions make it: the
//! embedded runtime passes its own client, and the external session passes the
//! one its network service built from the same policy.

use std::time::Duration;

use reqwest::Client;
use shared::error::{EngineError, EngineResult, ErrorCode};
use shared::op_state::NetworkPolicy;
use url::Url;

use super::gate::{self, GateKind};
use crate::image::inline::MAX_HTTP_IMAGE_BYTES;

/// TCP connect timeout for HTTP image fetches.
const HTTP_IMAGE_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// Total request timeout for HTTP image fetches (connect + body).
const HTTP_IMAGE_TOTAL_TIMEOUT: Duration = Duration::from_secs(15);

/// Fetch an `http(s)://` image and answer its body bytes.
///
/// The preflight is `fetch()`'s: the same gate, under `ImageInlineSrc`. An
/// image source that skipped it would make the domain allow list, the HTTPS
/// rule and the IP-literal block optional for anything content can write into
/// `Image.src` -- which is the whole policy, by another name.
pub async fn fetch_http_image(
    policy: &NetworkPolicy,
    client: &Client,
    url: &str,
) -> EngineResult<Vec<u8>> {
    let parsed = Url::parse(url).map_err(|e| {
        EngineError::new(ErrorCode::InvalidArgument)
            .with_msg("invalid image URL")
            .with_detail(e.to_string())
    })?;

    // Enforced before the client is touched: the resolver's SSRF guard does not
    // cover an IP-literal host and says nothing about scheme, allow list or
    // HTTPS.
    gate::enforce(&parsed, policy, GateKind::ImageInlineSrc).map_err(|e| {
        EngineError::new(ErrorCode::PermissionDenied)
            .with_msg("image fetch blocked by network policy")
            .with_detail(e)
    })?;
    let send_fut = client
        .get(parsed.clone())
        .timeout(HTTP_IMAGE_TOTAL_TIMEOUT)
        .send();
    let resp = tokio::time::timeout(HTTP_IMAGE_TOTAL_TIMEOUT, send_fut)
        .await
        .map_err(|_| {
            EngineError::new(ErrorCode::IoError)
                .with_msg("image fetch timed out")
                .with_detail(format!("no response within {:?}", HTTP_IMAGE_TOTAL_TIMEOUT))
        })?
        .map_err(|e| {
            EngineError::new(ErrorCode::IoError)
                .with_msg("image fetch failed")
                .with_detail(e.to_string())
        })?;
    if !resp.status().is_success() {
        return Err(EngineError::new(ErrorCode::IoError)
            .with_msg("image fetch returned non-2xx")
            .with_detail(format!("url={}, status={}", url, resp.status())));
    }

    // Body-size cap: refuse before `bytes().await` allocates the full
    // response in one `Vec<u8>`. We rely on `Content-Length` when the
    // server provides it; for chunked responses, the streaming loop
    // below enforces the same bound byte-by-byte.
    if let Some(len) = resp.content_length() {
        if len > MAX_HTTP_IMAGE_BYTES {
            return Err(EngineError::new(ErrorCode::IoError)
                .with_msg("image body exceeds limit")
                .with_detail(format!(
                    "advertised {} bytes > limit {}",
                    len, MAX_HTTP_IMAGE_BYTES
                )));
        }
    }

    // Stream into a pre-reserved buffer so peak allocation is the
    // known Content-Length (or MAX_HTTP_IMAGE_BYTES at worst), not the
    // unbounded concat-on-demand inside `reqwest::Response::bytes`.
    let hint = resp
        .content_length()
        .map(|l| l.min(MAX_HTTP_IMAGE_BYTES) as usize)
        .unwrap_or(64 * 1024);
    let mut buf = Vec::with_capacity(hint);
    let mut resp = resp;
    loop {
        let chunk = tokio::time::timeout(HTTP_IMAGE_TOTAL_TIMEOUT, resp.chunk())
            .await
            .map_err(|_| {
                EngineError::new(ErrorCode::IoError)
                    .with_msg("image body read timed out")
                    .with_detail(format!("no data within {:?}", HTTP_IMAGE_TOTAL_TIMEOUT))
            })?
            .map_err(|e| {
                EngineError::new(ErrorCode::IoError)
                    .with_msg("image body read failed")
                    .with_detail(e.to_string())
            })?;
        match chunk {
            Some(bytes) => {
                if (buf.len() as u64).saturating_add(bytes.len() as u64) > MAX_HTTP_IMAGE_BYTES {
                    return Err(EngineError::new(ErrorCode::IoError)
                        .with_msg("image body exceeds limit")
                        .with_detail(format!("streamed > {} bytes", MAX_HTTP_IMAGE_BYTES)));
                }
                buf.extend_from_slice(&bytes);
            }
            None => break,
        }
    }
    // `HTTP_IMAGE_CONNECT_TIMEOUT` is baked into the shared reqwest
    // client built by `fetch::get_or_create_client_from_state`; refer
    // to that module if you want to adjust per-request connect time.
    let _ = HTTP_IMAGE_CONNECT_TIMEOUT;
    Ok(buf)
}
