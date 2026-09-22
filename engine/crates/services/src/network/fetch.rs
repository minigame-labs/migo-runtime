//! `fetch`, as the host makes it.
//!
//! Content's request is built here, sent by the host's policy-checked client
//! (`super::client`), and answered as a status, its headers and a body content
//! reads in chunks. The work is the same for both executions: the embedded
//! runtime's ops take the arguments out of V8 and wrap the handles in deno
//! resources so `core.read` finds them in process, and the external session's
//! dispatcher takes the same arguments off the service stream and answers the
//! same ids.
//!
//! # Why a request is two steps
//!
//! `op_fetch` builds the request and answers its handles without sending it,
//! and `op_fetch_send` sends it. Content needs the cancel handle before the
//! send it may abort -- `AbortController` is allowed to fire in the same task
//! that started the request -- so the handles cannot wait for the response.
//!
//! # What crosses
//!
//! The body crosses in the reads content asks for, never as one answer: a
//! 200 MB download is 64 KiB of message at a time, which is what keeps a
//! streamed response streamed and a large one from being held twice.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use futures::StreamExt;
use futures::stream::{Peekable, Stream};
use http::HeaderMap;
use http::HeaderName;
use http::HeaderValue;
use http::Uri;
use http::header::{ACCEPT_ENCODING, CACHE_CONTROL, CONTENT_LENGTH, HOST, PRAGMA, RANGE};
use migo_io::pools::{ByteTicket, IoPools};
use reqwest::{Client, Method, Response};
use shared::op_state::NetworkPolicy;
use tokio::sync::Mutex;
use url::Url;

use super::gate::{GateKind, enforce};
use super::resources::{CancelFlag, ResourceId, ResourceTable};
use crate::ServiceError;

/// The buffered-response ceiling the engine's JavaScript enforces
/// (`04_request.js`). The response resource charges this same bound when
/// `Content-Length` is absent; it is an admission ceiling, not a claim about
/// device memory or throughput.
pub const MAX_BUFFERED_BODY_BYTES: u64 = 32 * 1024 * 1024;

/// Headers that game JavaScript must not inject -- these can amplify SSRF,
/// bypass reverse proxies, or leak internal routing information. Shared with
/// the WebSocket op so raw `new WebSocket(url, ...)` headers get the same
/// filtering as `fetch`.
pub fn is_blocked_header(name: &HeaderName) -> bool {
    matches!(
        name.as_str(),
        "x-forwarded-for"
            | "x-forwarded-host"
            | "x-forwarded-proto"
            | "x-original-url"
            | "x-rewrite-url"
            | "x-real-ip"
            | "forwarded"
            | "proxy-authorization"
    )
}

fn type_error(message: impl Into<String>) -> ServiceError {
    ServiceError::classed("TypeError", message)
}

/// What `op_fetch` answers: the request to send, and the handle that aborts it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FetchHandles {
    pub request_rid: ResourceId,
    /// Absent for a `data:` URL, which is answered without a connection.
    pub cancel_handle_rid: Option<ResourceId>,
}

/// What `op_fetch_send` answers, in the field order serde_v8 gives it.
#[derive(Debug, Clone)]
pub struct FetchAnswer {
    pub status: u16,
    pub status_text: String,
    pub headers: Vec<(Vec<u8>, Vec<u8>)>,
    pub url: String,
    pub response_rid: ResourceId,
    pub content_length: Option<u64>,
    pub remote_addr_ip: Option<String>,
    pub remote_addr_port: Option<u16>,
    pub error: Option<String>,
}

/// A request that has been built and not yet sent.
pub struct PendingRequest {
    send: RequestSend,
    cancel: Arc<CancelFlag>,
}

enum RequestSend {
    /// An HTTP(S) request, ready for the client that will carry it.
    Http(reqwest::RequestBuilder),
    /// A `data:` URL, already decoded: answered without a connection.
    Data(http::Response<Vec<u8>>),
}

/// A response's body, read in chunks.
///
/// One reader at a time -- content's `ReadableStream` pulls sequentially --
/// and the byte ticket the response was admitted with stays here until it is
/// closed or dropped.
pub struct ResponseBody {
    reader: Mutex<BodyReader>,
    cancel: CancelFlag,
    size: Option<u64>,
    _byte_ticket: Option<ByteTicket>,
}

type BytesStream = std::pin::Pin<Box<dyn Stream<Item = Result<Bytes, std::io::Error>> + Send>>;

enum BodyReader {
    /// Before the first read: the response head, whose body has not started.
    Start(Box<Response>),
    Reading(Peekable<BytesStream>),
    Done,
}

impl ResponseBody {
    fn new(response: Response, size: Option<u64>, byte_ticket: Option<ByteTicket>) -> Self {
        Self {
            reader: Mutex::new(BodyReader::Start(Box::new(response))),
            cancel: CancelFlag::default(),
            size,
            _byte_ticket: byte_ticket,
        }
    }

    /// What the body says its length is, where it said.
    pub fn size(&self) -> Option<u64> {
        self.size
    }

    pub(crate) fn cancel(&self) {
        self.cancel.cancel();
    }

    /// The next chunk, at most `limit` bytes. An empty answer is the end of
    /// the body -- which is what `core.read` means by zero.
    pub async fn read(&self, limit: usize) -> Result<Bytes, ServiceError> {
        let read = async {
            let mut reader = self.reader.lock().await;
            loop {
                match &mut *reader {
                    BodyReader::Done => return Ok(Bytes::new()),
                    BodyReader::Reading(stream) => {
                        let mut stream = std::pin::Pin::new(stream);
                        loop {
                            match stream.as_mut().peek_mut().await {
                                Some(Ok(chunk)) if !chunk.is_empty() => {
                                    let take = limit.min(chunk.len());
                                    return Ok(chunk.split_to(take));
                                }
                                // An empty chunk is not the end: take it and
                                // keep going, as the embedded reader does.
                                Some(Ok(_)) => {
                                    let _ = stream.as_mut().next().await;
                                }
                                Some(Err(_)) => {
                                    let error = match stream.as_mut().next().await {
                                        Some(Err(error)) => error.to_string(),
                                        _ => "the response body ended in an error".to_string(),
                                    };
                                    return Err(ServiceError::generic(error));
                                }
                                None => return Ok(Bytes::new()),
                            }
                        }
                    }
                    BodyReader::Start(_) => {
                        let BodyReader::Start(response) =
                            std::mem::replace(&mut *reader, BodyReader::Done)
                        else {
                            unreachable!("the arm above matched Start");
                        };
                        let stream: BytesStream = Box::pin(
                            response
                                .bytes_stream()
                                .map(|chunk| chunk.map_err(std::io::Error::other)),
                        );
                        *reader = BodyReader::Reading(stream.peekable());
                    }
                }
            }
        };
        match self.cancel.until_cancelled(read).await {
            Some(result) => result,
            None => Err(ServiceError::generic("request was cancelled")),
        }
    }
}

/// What a fetch is made with: the session's policy, its clients and its
/// resources.
pub struct FetchEnv<'a> {
    pub policy: &'a NetworkPolicy,
    /// The client for this request's protocol version. The caller keeps them
    /// (one per version), because building one is TLS setup.
    pub client: &'a Client,
    pub resources: &'a ResourceTable,
    pub pools: &'a IoPools,
    /// Whether a `Host` header content set is honoured, which only a client
    /// the host built for that purpose allows.
    pub allow_host: bool,
}

/// A request's body, as the caller holds it.
pub enum RequestBody<'a> {
    None,
    Bytes(&'a [u8]),
}

/// Build a request and answer its handles. Nothing is sent yet.
#[allow(clippy::too_many_arguments)]
pub fn fetch(
    env: FetchEnv<'_>,
    method: &[u8],
    url: &str,
    headers: &[(Vec<u8>, Vec<u8>)],
    body: RequestBody<'_>,
    timeout_ms: u32,
    enable_cache: bool,
) -> Result<FetchHandles, ServiceError> {
    let method = Method::from_bytes(method).map_err(|_| type_error("Invalid HTTP method"))?;
    let url = Url::parse(url).map_err(|_| type_error("Invalid URL"))?;

    match url.scheme() {
        "http" | "https" => {
            // reqwest's `RequestBuilder::send` uses `expect_uri` internally,
            // which panics rather than answering, so the URI is checked here.
            if url.as_str().parse::<Uri>().is_err() {
                return Err(type_error("Invalid URL"));
            }
            // SSRF, allow list and HTTPS enforcement, before anything is
            // built: every URL-based op goes through the one gate.
            enforce(&url, env.policy, GateKind::Fetch).map_err(ServiceError::generic)?;

            let mut request = env
                .client
                .request(method.clone(), url)
                .timeout(Duration::from_millis(u64::from(timeout_ms)));
            match body {
                RequestBody::Bytes(bytes) => request = request.body(bytes.to_vec()),
                RequestBody::None => {
                    if matches!(method, Method::POST | Method::PUT) {
                        request = request.header(CONTENT_LENGTH, HeaderValue::from(0));
                    }
                }
            }
            request = request.headers(header_map(headers, env.allow_host, enable_cache)?);

            let cancel = Arc::new(CancelFlag::default());
            let request_rid = env.resources.add_request(PendingRequest {
                send: RequestSend::Http(request),
                cancel: Arc::clone(&cancel),
            });
            let cancel_handle_rid = env.resources.add_cancel(cancel);
            Ok(FetchHandles {
                request_rid,
                cancel_handle_rid: Some(cancel_handle_rid),
            })
        }
        "data" => {
            let data_url = data_url::DataUrl::process(url.as_str())
                .map_err(|_| type_error("Invalid Data URL"))?;
            let (body, _) = data_url
                .decode_to_vec()
                .map_err(|_| type_error("Invalid Base64"))?;
            let response = http::Response::builder()
                .status(http::StatusCode::OK)
                .header(http::header::CONTENT_TYPE, data_url.mime_type().to_string())
                .body(body)
                .map_err(|error| ServiceError::generic(error.to_string()))?;
            let request_rid = env.resources.add_request(PendingRequest {
                send: RequestSend::Data(response),
                cancel: Arc::new(CancelFlag::default()),
            });
            Ok(FetchHandles {
                request_rid,
                cancel_handle_rid: None,
            })
        }
        "file" => Err(type_error(
            "NetworkError when attempting to fetch resource.",
        )),
        "blob" => Err(type_error("BlobNotFound")),
        _ => Err(type_error("SchemeNotSupported")),
    }
}

/// The headers a request carries: content's, minus the ones it must not set,
/// plus the cache directives it asked for.
fn header_map(
    headers: &[(Vec<u8>, Vec<u8>)],
    allow_host: bool,
    enable_cache: bool,
) -> Result<HeaderMap, ServiceError> {
    let mut map = HeaderMap::new();
    for (key, value) in headers {
        let name = HeaderName::from_bytes(key).map_err(|_| type_error("Invalid Header"))?;
        let value =
            HeaderValue::from_bytes(value).map_err(|_| type_error("Invalid Header Value"))?;
        if (name != HOST || allow_host) && name != CONTENT_LENGTH && !is_blocked_header(&name) {
            map.append(name, value);
        }
    }
    // A range request must not be transformed by an encoding the server
    // applies to the whole body.
    if map.contains_key(RANGE) {
        map.insert(ACCEPT_ENCODING, HeaderValue::from_static("identity"));
    }
    if !enable_cache {
        if !map.contains_key(CACHE_CONTROL) {
            map.insert(CACHE_CONTROL, HeaderValue::from_static("no-cache"));
        }
        if !map.contains_key(PRAGMA) {
            map.insert(PRAGMA, HeaderValue::from_static("no-cache"));
        }
    }
    Ok(map)
}

/// Send the request `request_rid` names and answer its head.
///
/// The request is taken out of the table: a second send on the same id is an
/// error rather than a second request.
pub async fn fetch_send(
    resources: &ResourceTable,
    pools: &IoPools,
    request_rid: ResourceId,
) -> Result<FetchAnswer, ServiceError> {
    let PendingRequest { send, cancel } = resources.take_request(request_rid)?;
    let started_at = std::time::Instant::now();
    let response = match send {
        RequestSend::Data(response) => {
            let (parts, body) = response.into_parts();
            Response::from(http::Response::from_parts(parts, reqwest::Body::from(body)))
        }
        RequestSend::Http(request) => {
            let sent = cancel.until_cancelled(request.send()).await;
            match sent {
                None => return Err(ServiceError::generic("request was cancelled")),
                Some(Err(error)) => return Err(send_failure(&error)),
                Some(Ok(response)) => response,
            }
        }
    };

    let status = response.status();
    let url = response.url().to_string();
    let headers = response
        .headers()
        .iter()
        .map(|(name, value)| (name.as_str().as_bytes().to_vec(), value.as_bytes().to_vec()))
        .collect();
    let content_length = response.content_length();
    let remote_addr = response.remote_addr();

    // SSRF: the address reqwest actually connected to. The gate before the
    // send catches IP-literal URLs; this covers the domain-name path, where
    // the name resolved after the check.
    if let Some(addr) = remote_addr {
        if super::address_filter::is_blocked_address(&addr) {
            return Err(ServiceError::generic(format!(
                "fetch: connection to {} is not allowed (private/loopback address)",
                addr.ip()
            )));
        }
    }

    // Admit the declared body before the resource exists, so a refused body
    // allocates nothing and exposes no reader.
    let byte_ticket = reserve_response_bytes(pools, content_length)?;
    let response_rid = resources.add_response(Arc::new(ResponseBody::new(
        response,
        content_length,
        Some(byte_ticket),
    )));

    let elapsed_ms = started_at.elapsed().as_millis() as u64;
    if elapsed_ms >= 100 {
        tracing::warn!(
            "[NetTrace] fetch slow {}ms status={} url={}",
            elapsed_ms,
            status.as_u16(),
            url
        );
    }

    Ok(FetchAnswer {
        status: status.as_u16(),
        status_text: status.canonical_reason().unwrap_or("").to_string(),
        headers,
        url,
        response_rid,
        content_length,
        remote_addr_ip: remote_addr.map(|addr| addr.ip().to_string()),
        remote_addr_port: remote_addr.map(|addr| addr.port()),
        error: None,
    })
}

/// The message a failed send shows: the innermost cause a body error carries,
/// as the embedded op has always reported it, and the error itself otherwise.
fn send_failure(error: &reqwest::Error) -> ServiceError {
    let mut source: &dyn std::error::Error = error;
    while let Some(next) = std::error::Error::source(source) {
        if let Some(request_error) = next.downcast_ref::<reqwest::Error>() {
            if request_error.is_body() {
                if let Some(cause) = std::error::Error::source(request_error) {
                    return type_error(cause.to_string());
                }
            }
        }
        source = next;
    }
    type_error(error.to_string())
}

/// Read from a response body, at most `limit` bytes; empty means the end.
pub async fn read(
    resources: &ResourceTable,
    rid: ResourceId,
    limit: usize,
) -> Result<Bytes, ServiceError> {
    resources.response(rid)?.read(limit).await
}

fn reserve_response_bytes(
    pools: &IoPools,
    content_length: Option<u64>,
) -> Result<ByteTicket, ServiceError> {
    pools
        .reserve_bytes(content_length.unwrap_or(MAX_BUFFERED_BODY_BYTES))
        .map_err(|error| ServiceError::generic(format!("fetch:fail byte limit: {error}")))
}

#[cfg(test)]
#[path = "fetch_tests.rs"]
mod tests;
