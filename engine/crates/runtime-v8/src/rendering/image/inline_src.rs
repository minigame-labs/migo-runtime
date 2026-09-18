//! Fetching an `http(s)://` image source under this execution's network
//! policy. Decoding what comes back is `migo_services::image::inline`'s.

use std::rc::Rc;
use std::time::Duration;

use deno_core::OpState;
use deno_core::url::Url;
use migo_services::image::inline::MAX_HTTP_IMAGE_BYTES;
use shared::error::{EngineError, EngineResult, ErrorCode};

use crate::network::gate::{GateKind, enforce_from_state};

/// TCP connect timeout for HTTP image fetches.
const HTTP_IMAGE_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// Total request timeout for HTTP image fetches (connect + body).
const HTTP_IMAGE_TOTAL_TIMEOUT: Duration = Duration::from_secs(15);

/// Fetch an HTTP/HTTPS image and return its body bytes.
///
/// **Security**: this path now runs the same preflight as `fetch()`
/// via [`crate::network::gate::enforce_from_state`]. Previously the
/// op short-circuited the shared reqwest client pool, which made the
/// domain whitelist / HTTPS enforcement / IP-literal block
/// effectively optional for `Image.src = "http(s)://..."`.
pub async fn fetch_http_image(
    state: Rc<std::cell::RefCell<OpState>>,
    url: &str,
) -> EngineResult<Vec<u8>> {
    let parsed = Url::parse(url).map_err(|e| {
        EngineError::new(ErrorCode::InvalidArgument)
            .with_msg("invalid image URL")
            .with_detail(e.to_string())
    })?;

    let client = {
        let mut st = state.borrow_mut();
        // Enforce *before* we touch the shared client, because the
        // resolver-level SSRF guard doesn't cover IP-literal hosts
        // and does nothing for scheme/whitelist/HTTPS policy.
        enforce_from_state(&parsed, &st, GateKind::ImageInlineSrc).map_err(|e| {
            EngineError::new(ErrorCode::PermissionDenied)
                .with_msg("image fetch blocked by network policy")
                .with_detail(e.to_string())
        })?;
        crate::network::fetch::get_or_create_client_from_state(&mut st, false).map_err(|e| {
            EngineError::new(ErrorCode::IoError)
                .with_msg("http client not available")
                .with_detail(e.to_string())
        })?
    };
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
