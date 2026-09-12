use std::borrow::Cow;
use std::cell::RefCell;
use std::cmp::min;
use std::pin::Pin;
use std::rc::Rc;
use std::task::Context;
use std::task::Poll;
use std::time::Duration;

use bytes::Bytes;
use data_url::DataUrl;
use deno_core::AsyncRefCell;
use deno_core::AsyncResult;
use deno_core::BufView;
use deno_core::ByteString;
use deno_core::CancelFuture;
use deno_core::CancelHandle;
use deno_core::CancelTryFuture;
use deno_core::Canceled;
use deno_core::JsBuffer;
use deno_core::OpState;
use deno_core::RcRef;
use deno_core::Resource;
use deno_core::ResourceId;
use deno_core::error::AnyError;
use deno_core::futures::Future;
use deno_core::futures::FutureExt;
use deno_core::futures::Stream;
use deno_core::futures::StreamExt;
use deno_core::futures::stream::Peekable;
use deno_core::op2;
use deno_core::url::Url;
use deno_error::JsErrorBox;
use http::HeaderMap;
use http::HeaderName;
use http::HeaderValue;
use http::Uri;
use http::header::ACCEPT_ENCODING;
use http::header::CACHE_CONTROL;
use http::header::CONTENT_LENGTH;
use http::header::HOST;
use http::header::PRAGMA;
use http::header::RANGE;
use http::header::USER_AGENT;
use reqwest::Body;
use reqwest::Client;
use reqwest::Method;
use reqwest::Response;
use reqwest::redirect::Policy;
use serde::Deserialize;
use serde::Serialize;
use tracing::debug;

use crate::io_state::IoSchedulerState;
use crate::network::Options;
use migo_io::pools::{ByteTicket, IoPools};

/// Existing JS buffered-response ceiling (`04_request.js`). The native
/// resource charges this same bound when Content-Length is absent; it is an
/// admission ceiling, not a claim about device memory or throughput.
const MAX_BUFFERED_BODY_BYTES: u64 = 32 * 1024 * 1024;
// ---------------------------------------------------------------------------
// SSRF-preventing DNS resolver
// ---------------------------------------------------------------------------
//
// All per-URL policy enforcement lives in `super::gate`. This module
// owns only (a) the reqwest `Client` pool + its TLS / redirect
// plumbing, and (b) the `SsrfCheckingResolver` below which catches
// DNS-name hosts at connect time. IP-literal hosts bypass the
// resolver, so the gate MUST be called before any `client.request()`.

/// Headers that game JS must not inject — these can amplify SSRF,
/// bypass reverse proxies, or leak internal routing information.
/// Shared with the WebSocket op so raw `new WebSocket(url, ...)` headers
/// get the same filtering as `fetch`.
pub(crate) fn is_blocked_header(name: &HeaderName) -> bool {
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

/// Custom DNS resolver that checks ALL resolved addresses against the
/// blocked-address list before returning them to reqwest.  This is injected
/// into every `reqwest::Client` via `ClientBuilder::dns_resolver()`, so
/// reqwest connects **only** to addresses we have verified — no separate
/// pre-flight check needed, no double-resolution TOCTOU window.
///
/// Note: hyper-util bypasses the resolver for IP-literal hosts, so callers
/// must also call `reject_blocked_ip_literal()` before sending requests.
struct SsrfCheckingResolver {
    operation: &'static str,
}

impl reqwest::dns::Resolve for SsrfCheckingResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let operation = self.operation;
        Box::pin(async move {
            let host = name.as_str();
            let addr_str = format!("{}:0", host);
            let addrs: Vec<std::net::SocketAddr> = tokio::net::lookup_host(&addr_str)
                .await
                .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { Box::new(e) })?
                .collect();

            for addr in &addrs {
                if super::address_filter::is_blocked_address(addr) {
                    return Err(Box::new(std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied,
                        format!(
                            "{}: connection to {} is not allowed (private/loopback address)",
                            operation,
                            addr.ip()
                        ),
                    ))
                        as Box<dyn std::error::Error + Send + Sync>);
                }
            }

            let addrs: reqwest::dns::Addrs = Box::new(addrs.into_iter());
            Ok(addrs)
        })
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FetchReturn {
    pub request_rid: ResourceId,
    pub cancel_handle_rid: Option<ResourceId>,
}

pub struct HttpClientResource {
    pub client: Client,
    pub allow_host: bool,
}

impl Resource for HttpClientResource {
    fn name(&'_ self) -> Cow<'_, str> {
        "httpClient".into()
    }
}

impl HttpClientResource {}

type CancelableResponseResult = Result<Result<Response, AnyError>, Canceled>;

pub struct FetchRequestResource(pub Pin<Box<dyn Future<Output = CancelableResponseResult>>>);

impl Resource for FetchRequestResource {
    fn name(&'_ self) -> Cow<'_, str> {
        "fetchRequest".into()
    }
}

#[allow(clippy::type_complexity)]
pub struct ResourceToBodyAdapter(
    Rc<dyn Resource>,
    Option<Pin<Box<dyn Future<Output = Result<BufView, JsErrorBox>>>>>,
);

impl ResourceToBodyAdapter {
    pub fn new(resource: Rc<dyn Resource>) -> Self {
        let future = resource.clone().read(64 * 1024);
        Self(resource, Some(future))
    }
}

unsafe impl Send for ResourceToBodyAdapter {}
unsafe impl Sync for ResourceToBodyAdapter {}

impl Stream for ResourceToBodyAdapter {
    type Item = Result<Bytes, JsErrorBox>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        if let Some(mut fut) = this.1.take() {
            match fut.poll_unpin(cx) {
                Poll::Pending => {
                    this.1 = Some(fut);
                    Poll::Pending
                }
                Poll::Ready(res) => match res {
                    Ok(buf) if buf.is_empty() => Poll::Ready(None),
                    Ok(buf) => {
                        this.1 = Some(this.0.clone().read(64 * 1024));
                        // Use copy_from_slice for clearer intent (single allocation)
                        Poll::Ready(Some(Ok(Bytes::copy_from_slice(&buf))))
                    }
                    Err(e) => Poll::Ready(Some(Err(e))),
                },
            }
        } else {
            Poll::Ready(None)
        }
    }
}

impl Drop for ResourceToBodyAdapter {
    fn drop(&mut self) {
        self.0.clone().close()
    }
}

pub struct FetchCancelHandle(pub Rc<CancelHandle>);

impl Resource for FetchCancelHandle {
    fn name(&'_ self) -> Cow<'_, str> {
        "fetchCancelHandle".into()
    }

    fn close(self: Rc<Self>) {
        self.0.cancel()
    }
}

type BytesStream = Pin<Box<dyn Stream<Item = Result<bytes::Bytes, std::io::Error>> + Unpin>>;

pub enum FetchResponseReader {
    Start(Response),
    BodyReader(Peekable<BytesStream>),
}

impl Default for FetchResponseReader {
    fn default() -> Self {
        let stream: BytesStream = Box::pin(deno_core::futures::stream::empty());
        Self::BodyReader(stream.peekable())
    }
}

/// Cached HTTP/1.1-only client
struct Http1Client(Client);

/// Cached HTTP/2-capable client
struct Http2Client(Client);

pub fn get_or_create_client_from_state(
    state: &mut OpState,
    enable_http2: bool,
) -> Result<reqwest::Client, AnyError> {
    if enable_http2 {
        if let Some(client) = state.try_borrow::<Http2Client>() {
            Ok(client.0.clone())
        } else {
            let options = state.borrow::<Options>();
            let user_agent = options.user_agent.clone();
            let policy = state
                .borrow::<shared::op_state::HostOpState>()
                .network_policy
                .clone();
            let client = create_http_client(&user_agent, true, &policy)?;
            state.put::<Http2Client>(Http2Client(client.clone()));
            Ok(client)
        }
    } else {
        if let Some(client) = state.try_borrow::<Http1Client>() {
            Ok(client.0.clone())
        } else {
            let options = state.borrow::<Options>();
            let user_agent = options.user_agent.clone();
            let policy = state
                .borrow::<shared::op_state::HostOpState>()
                .network_policy
                .clone();
            let client = create_http_client(&user_agent, false, &policy)?;
            state.put::<Http1Client>(Http1Client(client.clone()));
            Ok(client)
        }
    }
}

#[op2]
#[serde]
#[allow(clippy::too_many_arguments)]
pub fn op_fetch(
    state: &mut OpState,
    #[serde] method: ByteString,
    #[string] url: String,
    #[serde] headers: Vec<(ByteString, ByteString)>,
    #[smi] client_rid: Option<u32>,
    has_body: bool,
    #[buffer] data: Option<JsBuffer>,
    #[smi] resource: Option<ResourceId>,
    #[smi] timeout: u32,
    enable_http2: bool,
    enable_cache: bool,
) -> Result<FetchReturn, JsErrorBox> {
    let (client, allow_host) = if let Some(rid) = client_rid {
        let r = state
            .resource_table
            .get::<HttpClientResource>(rid)
            .map_err(|_e| JsErrorBox::generic("Failed to get HTTP client"))?;
        (r.client.clone(), r.allow_host)
    } else {
        (
            get_or_create_client_from_state(state, enable_http2)
                .map_err(|_e| JsErrorBox::generic("Failed to create HTTP client"))?,
            false,
        )
    };

    let method =
        Method::from_bytes(&method).map_err(|_| JsErrorBox::type_error("Invalid HTTP method"))?;
    let url = Url::parse(&url).map_err(|_| JsErrorBox::type_error("Invalid URL"))?;

    debug!("Fetch request: {} {}", method, url);

    // Check scheme before asking for net permission
    let scheme = url.scheme();
    let (request_rid, cancel_handle_rid) = match scheme {
        "file" => {
            let _path = url.to_file_path().map_err(|_| {
                JsErrorBox::type_error("NetworkError when attempting to fetch resource.")
            })?;

            if method != Method::GET {
                return Err(JsErrorBox::not_supported());
            }
            return Err(JsErrorBox::not_supported());
        }
        "http" | "https" => {
            // Make sure that we have a valid URI early, as reqwest's `RequestBuilder::send`
            // internally uses `expect_uri`, which panics instead of returning a usable `Result`.
            if url.as_str().parse::<Uri>().is_err() {
                return Err(JsErrorBox::type_error("Invalid URL"));
            }

            // Security: SSRF + domain whitelist + HTTPS enforcement.
            // All URL-based ops go through the shared gate so a
            // single-line addition there (e.g. new `NetworkPolicy`
            // field) applies to fetch/image/websocket/prefetch at
            // once; individual ops can't accidentally skip a rule.
            super::gate::enforce_from_state(&url, state, super::gate::GateKind::Fetch)?;

            // Capture the URL for the [NetTrace] log line before the
            // `client.request(..., url)` call below consumes it.
            let url_for_trace = url.to_string();
            let mut request = client
                .request(method.clone(), url)
                .timeout(Duration::from_millis(timeout as u64));

            if has_body {
                match (data, resource) {
                    (Some(data), _) => {
                        // If a body is passed, we use it, and don't return a body for streaming.
                        request = request.body(data.to_vec());
                    }
                    (_, Some(resource)) => {
                        let resource = state
                            .resource_table
                            .take_any(resource)
                            .map_err(|_| JsErrorBox::generic("Failed to take resource"))?;
                        match resource.size_hint() {
                            (body_size, Some(n)) if body_size == n && body_size > 0 => {
                                request =
                                    request.header(CONTENT_LENGTH, HeaderValue::from(body_size));
                            }
                            _ => {}
                        }
                        request =
                            request.body(Body::wrap_stream(ResourceToBodyAdapter::new(resource)))
                    }
                    (None, None) => unreachable!(),
                }
            } else {
                if matches!(method, Method::POST | Method::PUT) {
                    request = request.header(CONTENT_LENGTH, HeaderValue::from(0));
                }
            };

            let mut header_map = HeaderMap::new();
            for (key, value) in headers {
                let name = HeaderName::from_bytes(&key)
                    .map_err(|_| JsErrorBox::type_error("Invalid Header"))?;
                let v = HeaderValue::from_bytes(&value)
                    .map_err(|_| JsErrorBox::type_error("Invalid Header Value"))?;

                if (name != HOST || allow_host)
                    && name != CONTENT_LENGTH
                    && !is_blocked_header(&name)
                {
                    header_map.append(name, v);
                }
            }

            if header_map.contains_key(RANGE) {
                header_map.insert(ACCEPT_ENCODING, HeaderValue::from_static("identity"));
            }

            if !enable_cache {
                if !header_map.contains_key(CACHE_CONTROL) {
                    header_map.insert(CACHE_CONTROL, HeaderValue::from_static("no-cache"));
                }
                if !header_map.contains_key(PRAGMA) {
                    header_map.insert(PRAGMA, HeaderValue::from_static("no-cache"));
                }
            }

            request = request.headers(header_map);

            let cancel_handle = CancelHandle::new_rc();
            let cancel_handle_ = cancel_handle.clone();

            let fut = async move {
                // DNS resolution and SSRF check happen inside
                // SsrfCheckingResolver when reqwest opens the connection.
                let net_started = std::time::Instant::now();
                let result = request
                    .send()
                    .or_cancel(cancel_handle_)
                    .await
                    .map(|res| res.map_err(|err| err.into()));
                // Tight network-only timer: covers TLS + TCP + request
                // write + response-head read.  Note this is the future's
                // own observed elapsed; if the V8 event loop is stalled
                // when the network IO completes, the wake-up of this
                // task is delayed by the stall, inflating the number.
                // It is therefore an upper bound on real network time,
                // not an exact measurement.
                let net_ms = net_started.elapsed().as_millis() as u64;
                if net_ms >= 50 {
                    tracing::warn!("[NetTrace] reqwest send {}ms url={}", net_ms, url_for_trace);
                }
                result
            };

            let request_rid = state
                .resource_table
                .add(FetchRequestResource(Box::pin(fut)));

            let cancel_handle_rid = state.resource_table.add(FetchCancelHandle(cancel_handle));

            (request_rid, Some(cancel_handle_rid))
        }
        "data" => {
            let data_url = DataUrl::process(url.as_str())
                .map_err(|_| JsErrorBox::type_error("Invalid Data URL"))?;

            let (body, _) = data_url
                .decode_to_vec()
                .map_err(|_| JsErrorBox::type_error("Invalid Base64"))?;

            let response = http::Response::builder()
                .status(http::StatusCode::OK)
                .header(http::header::CONTENT_TYPE, data_url.mime_type().to_string())
                .body(reqwest::Body::from(body))
                .map_err(|e| JsErrorBox::generic(e.to_string()))?;

            let fut = async move { Ok(Ok(Response::from(response))) };

            let request_rid = state
                .resource_table
                .add(FetchRequestResource(Box::pin(fut)));

            (request_rid, None)
        }
        "blob" => {
            return Err(JsErrorBox::type_error("BlobNotFound"));
        }
        _ => return Err(JsErrorBox::type_error("SchemeNotSupported")),
    };

    Ok(FetchReturn {
        request_rid,
        cancel_handle_rid,
    })
}

#[derive(Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FetchResponse {
    pub status: u16,
    pub status_text: String,
    pub headers: Vec<(ByteString, ByteString)>,
    pub url: String,
    pub response_rid: ResourceId,
    pub content_length: Option<u64>,
    pub remote_addr_ip: Option<String>,
    pub remote_addr_port: Option<u16>,
    pub error: Option<String>,
}
pub struct FetchResponseResource {
    pub response_reader: AsyncRefCell<FetchResponseReader>,
    pub cancel: CancelHandle,
    pub size: Option<u64>,
    byte_ticket: Option<ByteTicket>,
}

impl FetchResponseResource {
    pub fn new(response: Response, size: Option<u64>, byte_ticket: Option<ByteTicket>) -> Self {
        Self {
            response_reader: AsyncRefCell::new(FetchResponseReader::Start(response)),
            cancel: CancelHandle::default(),
            size,
            byte_ticket,
        }
    }
}

impl Resource for FetchResponseResource {
    fn name(&'_ self) -> Cow<'_, str> {
        "fetchResponse".into()
    }

    fn read(self: Rc<Self>, limit: usize) -> AsyncResult<BufView> {
        Box::pin(async move {
            let mut reader = RcRef::map(&self, |r| &r.response_reader).borrow_mut().await;

            let body = loop {
                match &mut *reader {
                    FetchResponseReader::BodyReader(reader) => break reader,
                    FetchResponseReader::Start(_) => {}
                }

                match std::mem::take(&mut *reader) {
                    FetchResponseReader::Start(resp) => {
                        let stream: BytesStream = Box::pin(resp.bytes_stream().map(|r| {
                            r.map_err(|err| std::io::Error::new(std::io::ErrorKind::Other, err))
                        }));
                        *reader = FetchResponseReader::BodyReader(stream.peekable());
                    }
                    FetchResponseReader::BodyReader(_) => unreachable!(),
                }
            };
            let fut = async move {
                let mut reader = Pin::new(body);
                loop {
                    match reader.as_mut().peek_mut().await {
                        Some(Ok(chunk)) if !chunk.is_empty() => {
                            let len = min(limit, chunk.len());
                            let chunk = chunk.split_to(len);
                            break Ok(chunk.into());
                        }
                        Some(_) => match reader.as_mut().next().await.unwrap() {
                            Ok(chunk) => assert!(chunk.is_empty()),
                            Err(err) => break Err(JsErrorBox::generic(err.to_string())),
                        },
                        None => break Ok(BufView::empty()),
                    }
                }
            };

            let cancel_handle = RcRef::map(self, |r| &r.cancel);
            fut.try_or_cancel(cancel_handle).await
        })
    }

    fn size_hint(&self) -> (u64, Option<u64>) {
        (self.size.unwrap_or(0), self.size)
    }

    fn close(self: Rc<Self>) {
        self.cancel.cancel()
    }
}

#[op2(async(lazy), fast)]
#[serde]
pub async fn op_fetch_send(
    state: Rc<RefCell<OpState>>,
    #[smi] rid: ResourceId,
) -> Result<FetchResponse, JsErrorBox> {
    let request = state
        .borrow_mut()
        .resource_table
        .take::<FetchRequestResource>(rid)
        .map_err(|_| JsErrorBox::generic("Failed to take fetch request resource"))?;

    let request = Rc::try_unwrap(request)
        .ok()
        .expect("multiple op_fetch_send ongoing");

    let started_at = std::time::Instant::now();

    let res = match request.0.await {
        Ok(Ok(res)) => res,
        Ok(Err(err)) => {
            let mut err_ref: &dyn std::error::Error = err.as_ref();
            while let Some(err) = std::error::Error::source(err_ref) {
                if let Some(err) = err.downcast_ref::<reqwest::Error>() {
                    if err.is_body() {
                        // Extracts the next error cause and uses that for the message
                        if let Some(err) = std::error::Error::source(err) {
                            return Ok(FetchResponse {
                                error: Some(err.to_string()),
                                ..Default::default()
                            });
                        }
                    }
                }
                err_ref = err;
            }

            return Err(JsErrorBox::type_error(err_ref.to_string()));
        }
        Err(_) => return Err(JsErrorBox::type_error("request was cancelled")),
    };

    let status = res.status();
    let url = res.url().to_string();
    let mut res_headers = Vec::new();
    for (key, val) in res.headers().iter() {
        res_headers.push((key.as_str().into(), val.as_bytes().into()));
    }

    let content_length = res.content_length();
    let remote_addr = res.remote_addr();

    // SSRF prevention: check the *actual* resolved address after reqwest
    // performed DNS resolution internally.  The pre-flight check in op_fetch
    // only catches IP-literal URLs; this covers the domain-name path.
    if let Some(addr) = remote_addr {
        if super::address_filter::is_blocked_address(&addr) {
            return Err(JsErrorBox::generic(format!(
                "fetch: connection to {} is not allowed (private/loopback address)",
                addr.ip()
            )));
        }
    }

    let (remote_addr_ip, remote_addr_port) = if let Some(addr) = remote_addr {
        (Some(addr.ip().to_string()), Some(addr.port()))
    } else {
        (None, None)
    };

    // Admit the declared response body before publishing its resource. The
    // ticket stays with that resource until it is closed or dropped, so a
    // rejected body does not allocate or expose a reader.
    let pools = state
        .borrow()
        .borrow::<IoSchedulerState>()
        .0
        .pools()
        .clone();
    let byte_ticket = reserve_fetch_response_bytes(&pools, content_length)?;

    let response_rid = state
        .borrow_mut()
        .resource_table
        .add(FetchResponseResource::new(
            res,
            content_length,
            Some(byte_ticket),
        ));

    let elapsed_ms = started_at.elapsed().as_millis() as u64;
    if elapsed_ms >= 100 {
        tracing::warn!(
            "[NetTrace] fetch slow {}ms status={} url={}",
            elapsed_ms,
            status.as_u16(),
            url
        );
    }

    Ok(FetchResponse {
        status: status.as_u16(),
        status_text: status.canonical_reason().unwrap_or("").to_string(),
        headers: res_headers,
        url,
        response_rid,
        content_length,
        remote_addr_ip,
        remote_addr_port,
        error: None,
    })
}

fn reserve_fetch_response_bytes(
    pools: &IoPools,
    content_length: Option<u64>,
) -> Result<ByteTicket, JsErrorBox> {
    let body_bytes = content_length.unwrap_or(MAX_BUFFERED_BODY_BYTES);
    pools
        .reserve_bytes(body_bytes)
        .map_err(|error| JsErrorBox::generic(format!("fetch:fail byte limit: {error}")))
}

pub fn create_http_client(
    user_agent: &str,
    enable_http2: bool,
    net_policy: &shared::op_state::NetworkPolicy,
) -> Result<Client, AnyError> {
    create_policy_http_client(
        user_agent,
        enable_http2,
        net_policy,
        super::gate::GateKind::FetchRedirect,
        "fetch",
    )
}

/// Build the per-host client used by streamed audio. Construction is lazy at
/// the audio-service boundary; this function only centralizes the same TLS,
/// resolver, redirect, and pool configuration used by fetch.
#[cfg(feature = "api-media")]
pub fn create_audio_http_client(
    net_policy: &shared::op_state::NetworkPolicy,
) -> Result<Client, AnyError> {
    create_policy_http_client(
        "migo",
        true,
        net_policy,
        super::gate::GateKind::AudioStreamRedirect,
        "audio",
    )
}

fn create_policy_http_client(
    user_agent: &str,
    enable_http2: bool,
    net_policy: &shared::op_state::NetworkPolicy,
    redirect_kind: super::gate::GateKind,
    operation: &'static str,
) -> Result<Client, AnyError> {
    let mut headers = HeaderMap::new();
    headers.insert(USER_AGENT, user_agent.parse().unwrap());

    // Capture network policy for the redirect closure.  `Policy`
    // builds a boxed `Fn` internally, so we must move a *clone* of
    // the policy into the closure rather than borrowing from
    // `net_policy`.
    let redirect_policy = net_policy.clone();

    // Custom redirect policy: runs the shared gate on every redirect
    // target so `allowed.com -> 302 -> blocked.com` and
    // `https -> 302 -> http` are both rejected.  Centralising here
    // means redirect enforcement never drifts from the initial-URL
    // enforcement.
    let ssrf_redirect_policy = Policy::custom(move |attempt| {
        if attempt.previous().len() >= 10 {
            return attempt.stop();
        }
        match super::gate::evaluate_policy(attempt.url(), &redirect_policy, redirect_kind) {
            Ok(()) => attempt.follow(),
            Err(reject) => attempt.error(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                redirect_reject_message(operation, &reject),
            )),
        }
    });

    let mut builder = Client::builder()
        .dns_resolver(std::sync::Arc::new(SsrfCheckingResolver { operation }))
        .redirect(ssrf_redirect_policy)
        .default_headers(headers)
        // Connect timeout applies to TCP + TLS handshake only; per-
        // request `RequestBuilder::timeout` still bounds the full
        // exchange. Mobile networks frequently stall at connect time
        // when going through captive portals, so cap that specifically
        // rather than waiting for the OS-level SYN retry window.
        .connect_timeout(Duration::from_secs(5));

    if enable_http2 {
        // HTTP/2 multiplexes many streams over a single TCP connection, so we
        // need fewer idle connections but want to keep them alive longer.
        builder = builder
            .pool_max_idle_per_host(3)
            .pool_idle_timeout(Duration::from_secs(120))
            .http2_adaptive_window(true)
            .http2_keep_alive_interval(Duration::from_secs(30))
            .http2_keep_alive_timeout(Duration::from_secs(10));
    } else {
        // HTTP/1.1 needs more idle connections since each handles one
        // request at a time. Shorter idle timeout to free resources.
        builder = builder
            .pool_max_idle_per_host(6)
            .pool_idle_timeout(Duration::from_secs(90))
            .http1_only();
    }

    builder.build().map_err(|e| e.into())
}

fn redirect_reject_message(operation: &str, reject: &super::gate::GateReject) -> String {
    let detail = match reject {
        super::gate::GateReject::BlockedAddress { display } => {
            format!("connection to {display} is not allowed (private/loopback address)")
        }
        super::gate::GateReject::NotWhitelisted { host } => {
            format!("'{host}' is not in the allowed domain list")
        }
        super::gate::GateReject::HttpsRequired => "HTTPS required (enforce_https=true)".to_string(),
        super::gate::GateReject::UnsupportedScheme { scheme } => {
            format!("scheme '{scheme}' is not allowed")
        }
        super::gate::GateReject::MissingHost => "URL has no host".to_string(),
    };
    format!("{operation}: redirect rejected: {detail}")
}

#[cfg(all(test, feature = "api-media"))]
mod q10_client_tests {
    use super::*;

    #[test]
    fn fetch_redirect_error_vocabulary_stays_backward_compatible() {
        let reject = super::super::gate::GateReject::NotWhitelisted {
            host: "blocked.example".to_string(),
        };
        assert_eq!(
            redirect_reject_message("fetch", &reject),
            "fetch: redirect rejected: 'blocked.example' is not in the allowed domain list"
        );
        assert_eq!(
            redirect_reject_message("audio", &reject),
            "audio: redirect rejected: 'blocked.example' is not in the allowed domain list"
        );
    }

    #[test]
    fn audio_policy_client_build_is_side_effect_free() {
        let policy = shared::op_state::NetworkPolicy {
            domain_whitelist: vec!["media.example".to_string()],
            enforce_https: true,
        };
        create_audio_http_client(&policy).expect("client construction must not require a runtime");
    }
}

#[cfg(test)]
mod byte_admission_tests {
    use super::*;

    #[test]
    fn io04_fetch_response_refuses_oversized_declared_body() {
        let pools = IoPools::new(9501);
        let result = reserve_fetch_response_bytes(&pools, Some(256 * 1024 * 1024 + 1));
        let error = match result {
            Ok(_) => panic!("oversized response must be refused before resource creation"),
            Err(error) => error,
        };
        let message = error.to_string();
        assert!(
            message.contains("byte limit") && message.contains("requested"),
            "structured response admission error: {message}"
        );
    }

    #[test]
    fn io04_fetch_response_ticket_releases_on_resource_drop() {
        let pools = IoPools::new(9502);
        let ticket = reserve_fetch_response_bytes(&pools, Some(4096)).unwrap();
        let response = Response::from(
            http::Response::builder()
                .status(http::StatusCode::OK)
                .body(Body::from("body"))
                .unwrap(),
        );
        let resource = FetchResponseResource::new(response, Some(4096), Some(ticket));
        drop(resource);
        reserve_fetch_response_bytes(&pools, Some(4096))
            .expect("dropping the response must return its byte credit");
    }
}

// ── Upload ──

/// Resolve a JS-visible virtual path into a real filesystem path for
/// upload. We deliberately reuse neither `op_read_file`'s private
/// resolver nor the VFS error variants — upload only cares whether
/// the path is readable, and the error prefix stays consistent with
/// other `uploadFile:*` failure messages.
fn resolve_upload_path(
    vfs: Option<&shared::vfs::VirtualFS>,
    mount_table: Option<&shared::vfs::MountTable>,
    path: &str,
) -> Result<std::path::PathBuf, JsErrorBox> {
    use shared::vfs::VfsError;

    let virtual_path = if path.starts_with('/') {
        path.to_string()
    } else {
        format!("/code/{}", path)
    };

    // `/code` is preferred via the mount table because it may be
    // package-backed; other prefixes go through the normal VFS.
    if virtual_path == "/code" || virtual_path.starts_with("/code/") {
        if let Some(mt) = mount_table {
            if let Some(res) = mt.resolve_code_path(&virtual_path) {
                if let Some(real) = res.real_path {
                    return Ok(real);
                }
                return Err(JsErrorBox::generic(format!(
                    "uploadFile:fail {} is inside a package and cannot be streamed",
                    path
                )));
            }
        }
    }

    let vfs = vfs.ok_or_else(|| JsErrorBox::generic("uploadFile:fail VFS not initialised"))?;
    vfs.resolve(&virtual_path, shared::vfs::FileOp::Read)
        .map_err(|e| {
            JsErrorBox::generic(match e {
                VfsError::PathNotAllowed => format!(
                    "uploadFile:fail path not allowed: {} (use /user, /cache, /code, /tmp)",
                    path
                ),
                VfsError::PermissionDenied => {
                    format!("uploadFile:fail permission denied: {}", path)
                }
                VfsError::PathTraversal => {
                    format!("uploadFile:fail path traversal: {}", path)
                }
                VfsError::SymlinkEscape => {
                    format!("uploadFile:fail symlink escape: {}", path)
                }
                VfsError::SymlinkNotAllowed => {
                    format!("uploadFile:fail symlinks not allowed: {}", path)
                }
                VfsError::InvalidPath => format!("uploadFile:fail invalid path: {}", path),
            })
        })
}

/// Convert a `tokio::fs::File` into a stream of `Bytes` chunks that
/// `reqwest::Body::wrap_stream` accepts. 64 KiB is large enough to
/// keep syscall overhead low but small enough that a paused upload
/// (backpressure) doesn't pin down megabytes of RAM per connection.
fn file_to_byte_stream(file: tokio::fs::File) -> impl Stream<Item = std::io::Result<Bytes>> {
    use tokio::io::AsyncReadExt;
    deno_core::futures::stream::unfold((file, false), |(mut f, done)| async move {
        if done {
            return None;
        }
        let mut buf = vec![0u8; 64 * 1024];
        match f.read(&mut buf).await {
            Ok(0) => None,
            Ok(n) => {
                buf.truncate(n);
                Some((Ok(Bytes::from(buf)), (f, false)))
            }
            Err(e) => Some((Err(e), (f, true))),
        }
    })
}

#[derive(Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FetchUploadResult {
    pub data: String,
    pub status_code: u16,
    pub headers: Vec<(ByteString, ByteString)>,
    pub total_bytes_sent: u64,
    pub error: Option<String>,
}

/// Create a cancel handle for an in-flight `uploadFile`.
///
/// The JS layer gets the rid back immediately (before it awaits
/// `op_fetch_upload`), stores it on the upload task, and `abort()` closes
/// it — closing the resource cancels the upload future via
/// [`FetchCancelHandle::close`]. Mirrors the cancel handle `op_fetch`
/// hands back for `downloadFile`, which previously had no analogue for
/// uploads (so `UploadTask.abort()` could not stop an in-flight request).
#[op2(fast)]
#[smi]
pub fn op_fetch_upload_cancel_handle(state: &mut OpState) -> ResourceId {
    state
        .resource_table
        .add(FetchCancelHandle(CancelHandle::new_rc()))
}

/// Streaming multipart upload.
///
/// The JS caller passes the **virtual path** of the file to upload
/// (e.g. `/user/foo.png`). The op resolves it through the host's VFS
/// and feeds a `tokio::fs::File` straight into `reqwest::Body::wrap_stream`,
/// so the process never materialises a second copy of the file in
/// user-space. For a 50 MiB upload this removes ~100 MiB of peak
/// allocation compared with the old `buffer -> Vec<u8> -> clone`
/// pipeline.
/// Scalar options for `op_fetch_upload`, bundled into one `#[serde]` arg.
///
/// deno_core's `setUpAsyncStub` only codegens async-op arities up to 10
/// (`arg_count` = js args + 2). With every scalar passed separately this op
/// hit `arg_count = 11`, tripping the "Too many arguments for async op codegen"
/// throw and aborting V8 snapshot creation. Folding the trailing scalars into
/// one serde struct keeps the op under the limit.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct FetchUploadOpts {
    timeout: u32,
    enable_http2: bool,
}

#[op2(async(lazy))]
#[serde]
pub async fn op_fetch_upload(
    state: Rc<RefCell<OpState>>,
    #[smi] cancel_rid: ResourceId,
    #[string] url: String,
    #[string] file_path: String,
    #[string] name: String,
    #[string] filename: String,
    #[serde] headers: Vec<(ByteString, ByteString)>,
    #[serde] form_data: Vec<(String, String)>,
    #[serde] opts: FetchUploadOpts,
) -> Result<FetchUploadResult, JsErrorBox> {
    let FetchUploadOpts {
        timeout,
        enable_http2,
    } = opts;
    let client = {
        let mut st = state.borrow_mut();
        get_or_create_client_from_state(&mut st, enable_http2)
            .map_err(|e| JsErrorBox::generic(e.to_string()))?
    };

    // Resolve the JS-visible virtual path before entering asynchronous file
    // operations, using the same VFS boundary as the file API.
    let real_path = {
        let st = state.borrow();
        let host = st.borrow::<shared::op_state::HostOpState>();
        let vfs = host.vfs.as_ref().map(|arc| arc.as_ref());
        let mount_table = host.mount_table.as_ref().map(|arc| arc.as_ref());
        resolve_upload_path(vfs, mount_table, &file_path)?
    };
    // Capture the strong cancel owner before the first file-system await.
    // JS abort() closes/removes this rid synchronously; a supplied rid that
    // is already absent therefore means "cancelled", never "uncancellable".
    let cancel_handle = if cancel_rid == 0 {
        None
    } else {
        let st = state.borrow();
        st.resource_table
            .get::<FetchCancelHandle>(cancel_rid)
            .ok()
            .map(|h| h.0.clone())
    };
    if cancel_rid != 0 && cancel_handle.is_none() {
        return Ok(FetchUploadResult {
            error: Some("uploadFile:fail aborted".to_string()),
            ..Default::default()
        });
    }

    let file_open = tokio::fs::File::open(&real_path);
    let file = match cancel_handle.clone() {
        Some(cancel) => match file_open.or_cancel(cancel).await {
            Ok(result) => result.map_err(|e| {
                JsErrorBox::generic(format!("uploadFile:fail open {}: {}", file_path, e))
            })?,
            Err(_) => {
                return Ok(FetchUploadResult {
                    error: Some("uploadFile:fail aborted".to_string()),
                    ..Default::default()
                });
            }
        },
        None => file_open.await.map_err(|e| {
            JsErrorBox::generic(format!("uploadFile:fail open {}: {}", file_path, e))
        })?,
    };
    let file_size = match cancel_handle.clone() {
        Some(cancel) => match file.metadata().or_cancel(cancel).await {
            Ok(metadata) => metadata.map(|m| m.len()).unwrap_or(0),
            Err(_) => {
                return Ok(FetchUploadResult {
                    error: Some("uploadFile:fail aborted".to_string()),
                    ..Default::default()
                });
            }
        },
        None => file.metadata().await.map(|m| m.len()).unwrap_or(0),
    };

    // Guess MIME type from filename extension
    let mime = match filename.rsplit('.').next().map(|e| e.to_lowercase()) {
        Some(ext) => match ext.as_str() {
            "jpg" | "jpeg" => "image/jpeg",
            "png" => "image/png",
            "gif" => "image/gif",
            "webp" => "image/webp",
            "mp3" => "audio/mpeg",
            "mp4" => "video/mp4",
            "wav" => "audio/wav",
            "ogg" => "audio/ogg",
            "json" => "application/json",
            "xml" => "application/xml",
            "txt" => "text/plain",
            "zip" => "application/zip",
            _ => "application/octet-stream",
        },
        None => "application/octet-stream",
    };

    // Streaming body: pull 64 KiB at a time from the file and emit
    // `Bytes` chunks to the multipart encoder. `reqwest::Body::wrap_stream`
    // owns the stream and drives it as the HTTP layer asks for data,
    // so peak user-space memory per upload is one chunk, not the
    // whole file.
    //
    // One `file_name` call for both bodies, because two of them is how the
    // branches came to disagree: the zero-length branch named the part
    // `file_path` -- the engine's internal VFS path -- instead of the display
    // name the caller gave, so every file whose metadata reported no length
    // leaked the internal path layout to the server.
    let file_part = if file_size == 0 {
        // No length from metadata, so fall back to chunked encoding; reqwest
        // picks that for a part with no declared length.
        let reopened = tokio::fs::File::open(&real_path);
        let reopened = match cancel_handle.clone() {
            Some(cancel) => match reopened.or_cancel(cancel).await {
                Ok(result) => result
                    .map_err(|e| JsErrorBox::generic(format!("uploadFile:fail reopen: {e}")))?,
                Err(_) => {
                    return Ok(FetchUploadResult {
                        error: Some("uploadFile:fail aborted".to_string()),
                        ..Default::default()
                    });
                }
            },
            None => reopened
                .await
                .map_err(|e| JsErrorBox::generic(format!("uploadFile:fail reopen: {e}")))?,
        };
        reqwest::multipart::Part::stream(Body::wrap_stream(file_to_byte_stream(reopened)))
    } else {
        reqwest::multipart::Part::stream_with_length(
            Body::wrap_stream(file_to_byte_stream(file)),
            file_size,
        )
    }
    .file_name(filename)
    .mime_str(mime)
    .map_err(|e| JsErrorBox::generic(e.to_string()))?;

    let mut form = reqwest::multipart::Form::new().part(name, file_part);

    for (key, value) in form_data {
        form = form.text(key, value);
    }

    // Parse URL
    let parsed_url = Url::parse(&url).map_err(|_| JsErrorBox::type_error("Invalid URL"))?;

    // Security: SSRF + domain whitelist + HTTPS enforcement via the
    // shared gate. See `op_fetch` for rationale.
    {
        let st = state.borrow();
        super::gate::enforce_from_state(&parsed_url, &*st, super::gate::GateKind::FetchUpload)?;
    }

    debug!("Upload request: POST {}", parsed_url);

    // Build request
    let mut request = client
        .post(parsed_url)
        .timeout(Duration::from_millis(timeout as u64))
        .multipart(form);

    // Apply custom headers — same security filtering as op_fetch.
    let mut header_map = HeaderMap::new();
    for (key, value) in headers {
        let hname =
            HeaderName::from_bytes(&key).map_err(|_| JsErrorBox::type_error("Invalid Header"))?;
        let hval = HeaderValue::from_bytes(&value)
            .map_err(|_| JsErrorBox::type_error("Invalid Header Value"))?;
        // Skip Content-Type and Content-Length (reqwest manages these for multipart),
        // HOST (prevent host-header attacks), and proxy-related headers (SSRF hardening).
        if hname != http::header::CONTENT_TYPE
            && hname != CONTENT_LENGTH
            && hname != HOST
            && !is_blocked_header(&hname)
        {
            header_map.append(hname, hval);
        }
    }
    request = request.headers(header_map);

    // Upload responses are buffered for UploadResponse.data, so bound the
    // response independently of the streamed request body. Content-Length is
    // not sufficient: chunked responses are checked while bytes arrive.
    const MAX_BUFFERED_RESPONSE_BYTES: usize = 32 * 1024 * 1024;

    // Drive send + response read as one cancellable unit.
    let exchange = async move {
        let res = request.send().await.map_err(|e| e.to_string())?;
        let status = res.status().as_u16();
        let mut res_headers = Vec::new();
        for (key, val) in res.headers().iter() {
            res_headers.push((key.as_str().into(), val.as_bytes().into()));
        }
        let mut stream = res.bytes_stream();
        let mut body = Vec::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| e.to_string())?;
            let next_len = body.len().saturating_add(chunk.len());
            if next_len > MAX_BUFFERED_RESPONSE_BYTES {
                return Ok::<FetchUploadResult, String>(FetchUploadResult {
                    status_code: status,
                    headers: res_headers,
                    total_bytes_sent: file_size,
                    error: Some("uploadFile:fail response body exceeds limit".to_string()),
                    ..Default::default()
                });
            }
            body.extend_from_slice(&chunk);
        }
        Ok::<FetchUploadResult, String>(FetchUploadResult {
            data: String::from_utf8_lossy(&body).into_owned(),
            status_code: status,
            headers: res_headers,
            total_bytes_sent: file_size,
            error: None,
        })
    };

    let outcome = match cancel_handle {
        Some(c) => match exchange.or_cancel(c).await {
            Ok(inner) => inner,
            Err(_canceled) => {
                return Ok(FetchUploadResult {
                    error: Some("uploadFile:fail aborted".to_string()),
                    ..Default::default()
                });
            }
        },
        None => exchange.await,
    };

    // Network / decode errors surface through the result's `error` field
    // (same contract as the send-error path) rather than throwing.
    match outcome {
        Ok(result) => Ok(result),
        Err(err) => Ok(FetchUploadResult {
            error: Some(err.to_string()),
            ..Default::default()
        }),
    }
}

#[cfg(test)]
mod js_regression_tests {
    //! JS-mock regression tests for NET-01, NET-02, NET-04.
    //!
    //! Seam: the production `.js` source is executed in a bare `JsRuntime` with
    //! all imported symbols replaced by in-script stubs (FNET-01 pattern from
    //! `tcp_socket.rs`).  No gate modification; no loopback TCP peer required.
    //! Each test documents what the seam does *not* cover (real HTTP teardown /
    //! real filesystem conflict / real HTTP streaming).
    use deno_core::{JsRuntime, RuntimeOptions};

    /// Strip ES `import` and `export` statements so the source can be inlined
    fn strip_module_declarations(src: &str) -> String {
        let mut output = Vec::new();
        let mut skipping = false;
        for line in src.lines() {
            if !skipping && (line.starts_with("import ") || line.starts_with("export ")) {
                skipping = !line.contains(';');
                continue;
            }
            if skipping {
                if line.contains(';') {
                    skipping = false;
                }
                continue;
            }
            output.push(line);
        }
        output.join("\n")
    }

    fn new_executor() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test executor")
    }

    // ── NET-01 ─────────────────────────────────────────────────────────────────

    /// NET-01 regression (OPEN-ITEMS-host-vs-device.md):
    /// `downloadFile` abort() used to close only the send cancel handle; the
    /// response body resource (`responseRid`) was not touched until `core.read`
    /// unblocked — non-deterministic, and the resource leaked if the read never
    /// finished.  The fix adds `core.tryClose(this.responseRid)` to
    /// `cancellation.abort()` in `05_download.js`.
    ///
    /// Seam: production `05_download.js` with mocked ops and a `core` stub.
    /// Does not cover real HTTP stream teardown; covers resource-table
    /// close-accounting via the JS cancellation object.
    ///
    /// Scenario A — abort while `core.read` is pending: responseRid must appear
    ///   in `core.closedRids` immediately after `abort()`.
    /// Scenario B — late abort after success: responseRid must appear exactly
    ///   once (closed in the success path, not re-closed by the late abort).
    #[test]
    fn net01_abort_closes_response_rid_exactly_once() {
        let dl = strip_module_declarations(include_str!("05_download.js"));

        // ── Scenario A: abort while core.read is pending ──────────────────────
        {
            let mut rt = JsRuntime::new(RuntimeOptions::default());
            // `_resolveFS` is captured by the mock `op_fetch_send` closure.
            // The IIFE runs synchronously up to its first `await`, calling
            // `op_fetch_send`, which stores the resolver. We call it immediately
            // after `downloadFile()` returns, so the resolve is queued before the
            // event loop starts — deterministic ordering without timers.
            let setup = format!(
                r#"
                const core = {{
                    closedRids: [],
                    read(_rid, _buf) {{ return new Promise(() => {{}}); }},
                    tryClose(rid) {{ core.closedRids.push(rid); }},
                }};
                const primordials = {{ TypeError: globalThis.TypeError }};
                function createListenerGroup(_lbl) {{
                    return {{ on() {{}}, off() {{}}, trigger() {{}} }};
                }}
                class Header {{ constructor(_h, _s) {{}} }}
                class NetworkTask {{
                    constructor(t) {{
                        this._aborted = false;
                        this._terminator = t;
                        this._headersReceivedListeners = createListenerGroup('h');
                    }}
                    abort() {{
                        if (this._aborted) return;
                        this._aborted = true;
                        this._terminator?.abort();
                        this._headersReceivedListeners.off();
                        this._onCleanup();
                    }}
                    _onCleanup() {{}}
                    _triggerHeadersReceived(h) {{
                        if (this._aborted) return;
                        this._headersReceivedListeners.trigger(h);
                    }}
                }}
                class DownloadResponse {{ constructor() {{}} }}
                class DownloadErrorResponse {{ constructor(_e, _x) {{}} }}
                class Exception {{ constructor(_c, m, _n) {{ this.message = m; }} }}
                function abortedNetworkError() {{ return {{ errMsg: "aborted" }}; }}
                let _resolveFS = null;
                function op_fetch(_m, _u, _h, ..._r) {{
                    return {{ requestRid: 1, cancelHandleRid: 2 }};
                }}
                function op_fetch_send(_rid) {{
                    return new Promise(r => {{ _resolveFS = r; }});
                }}
                function op_open_file(_p, _m) {{ return Promise.resolve(10); }}
                function op_write_file(..._a) {{ return Promise.resolve(); }}
                function op_close_file(..._a) {{ return Promise.resolve(); }}
                function op_rename(..._a) {{ return Promise.resolve(); }}
                function op_unlink(..._a) {{ return Promise.resolve(); }}
                {dl}
                const task = downloadFile({{
                    url: "http://ex.com/f",
                    filePath: "/tmp/net01a.dat",
                }});
                globalThis.net01ATask = task;
                // IIFE has run to its first `await`, so _resolveFS is now set.
                // Resolving here queues the fetch_send continuation for the event loop.
                _resolveFS({{
                    status: 200, headers: [],
                    responseRid: 42, contentLength: 0, error: null,
                }});
                "#,
                dl = dl,
            );
            rt.execute_script("net01a-setup", setup)
                .expect("NET-01A setup must execute");
            let exec = new_executor();
            // Event loop drains: fetch_send → sets responseRid=42, open_file → fd,
            // then parks at core.read (never-resolving stub).
            exec.block_on(rt.run_event_loop(Default::default()))
                .expect("NET-01A event loop");
            // responseRid=42 is now registered. Abort must close it immediately.
            rt.execute_script(
                "net01a-abort",
                "net01ATask.abort(); globalThis.net01AClosedRids = core.closedRids.slice();",
            )
            .expect("NET-01A abort");
            exec.block_on(rt.run_event_loop(Default::default()))
                .expect("NET-01A post-abort loop");
            rt.execute_script(
                "net01a-check",
                r#"
                if (!net01AClosedRids.includes(42)) {
                    throw new Error(
                        "NET-01: responseRid 42 not closed after abort while "
                        + "core.read is pending. closedRids="
                        + JSON.stringify(net01AClosedRids)
                    );
                }
                "#,
            )
            .expect("NET-01A: abort while pending must close responseRid immediately");
        }

        // ── Scenario B: late abort after success must not double-close ─────────
        {
            let mut rt = JsRuntime::new(RuntimeOptions::default());
            let setup = format!(
                r#"
                const core = {{
                    closedRids: [],
                    // EOF immediately: bytesRead = 0 → loop exits, success fires.
                    read(_rid, _buf) {{ return Promise.resolve(0); }},
                    tryClose(rid) {{ core.closedRids.push(rid); }},
                    close(rid) {{ core.closedRids.push(rid); }},
                }};
                const primordials = {{ TypeError: globalThis.TypeError }};
                function createListenerGroup(_lbl) {{
                    return {{ on() {{}}, off() {{}}, trigger() {{}} }};
                }}
                class Header {{ constructor(_h, _s) {{}} }}
                class NetworkTask {{
                    constructor(t) {{
                        this._aborted = false;
                        this._terminator = t;
                        this._headersReceivedListeners = createListenerGroup('h');
                    }}
                    abort() {{
                        if (this._aborted) return;
                        this._aborted = true;
                        this._terminator?.abort();
                        this._headersReceivedListeners.off();
                        this._onCleanup();
                    }}
                    _onCleanup() {{}}
                    _triggerHeadersReceived(h) {{
                        if (this._aborted) return;
                        this._headersReceivedListeners.trigger(h);
                    }}
                }}
                class DownloadResponse {{ constructor() {{}} }}
                class DownloadErrorResponse {{ constructor(_e, _x) {{}} }}
                class Exception {{ constructor(_c, m, _n) {{ this.message = m; }} }}
                function abortedNetworkError() {{ return {{ errMsg: "aborted" }}; }}
                let _resolveFS = null;
                let completeFired = false;
                function op_fetch(_m, _u, _h, ..._r) {{
                    return {{ requestRid: 1, cancelHandleRid: 2 }};
                }}
                function op_fetch_send(_rid) {{
                    return new Promise(r => {{ _resolveFS = r; }});
                }}
                function op_open_file(_p, _m) {{ return Promise.resolve(10); }}
                function op_write_file(..._a) {{ return Promise.resolve(); }}
                function op_close_file(..._a) {{ return Promise.resolve(); }}
                function op_rename(..._a) {{ return Promise.resolve(); }}
                function op_unlink(..._a) {{ return Promise.resolve(); }}
                {dl}
                const task = downloadFile({{
                    url: "http://ex.com/f2",
                    filePath: "/tmp/net01b.dat",
                    success() {{ completeFired = true; }},
                    complete() {{}},
                }});
                globalThis.net01BTask = task;
                _resolveFS({{
                    status: 200, headers: [],
                    responseRid: 99, contentLength: 0, error: null,
                }});
                "#,
                dl = dl,
            );
            rt.execute_script("net01b-setup", setup)
                .expect("NET-01B setup");
            let exec = new_executor();
            exec.block_on(rt.run_event_loop(Default::default()))
                .expect("NET-01B event loop — download must complete");
            // After the loop: success fired, responseRid=99 closed once (line 169
            // of 05_download.js), cancellation.responseRid set to null.
            rt.execute_script(
                "net01b-late-abort",
                r#"
                if (!completeFired) {
                    throw new Error("NET-01B: success must fire before late abort");
                }
                net01BTask.abort();
                globalThis.net01BRid99Count =
                    core.closedRids.filter(r => r === 99).length;
                "#,
            )
            .expect("NET-01B late abort");
            exec.block_on(rt.run_event_loop(Default::default()))
                .expect("NET-01B post-late-abort loop");
            rt.execute_script(
                "net01b-check",
                r#"
                if (net01BRid99Count !== 1) {
                    throw new Error(
                        "NET-01: late abort must not double-close responseRid; "
                        + "got " + net01BRid99Count + " closes. "
                        + "all closedRids=" + JSON.stringify(core.closedRids)
                    );
                }
                "#,
            )
            .expect("NET-01B: late abort must not re-close responseRid");
        }
    }

    // ── NET-02 ─────────────────────────────────────────────────────────────────

    /// NET-02 regression (OPEN-ITEMS-host-vs-device.md):
    /// Concurrent `downloadFile` calls to the same destination shared a fixed
    /// `.part` path — the second task would truncate the first.  The fix adds a
    /// module-level monotonic counter to `generateTempFilePath` in `05_download.js`.
    ///
    /// Seam: same JS-mock pattern.  Captures the path argument passed to
    /// `op_open_file`.  Does not cover real filesystem exclusive-open collision;
    /// covers the path-uniqueness contract.
    #[test]
    fn net02_concurrent_downloads_use_distinct_temp_paths() {
        let dl = strip_module_declarations(include_str!("05_download.js"));

        let mut rt = JsRuntime::new(RuntimeOptions::default());
        // op_fetch_send resolves immediately so both IIFEs advance to op_open_file
        // in the same event-loop drain, each recording their temp path before
        // parking at the hanging op_open_file stub.
        let setup = format!(
            r#"
            const core = {{
                closedRids: [],
                read(_rid, _buf) {{ return new Promise(() => {{}}); }},
                tryClose(rid) {{ core.closedRids.push(rid); }},
            }};
            const primordials = {{ TypeError: globalThis.TypeError }};
            function createListenerGroup(_lbl) {{
                return {{ on() {{}}, off() {{}}, trigger() {{}} }};
            }}
            class Header {{ constructor(_h, _s) {{}} }}
            class NetworkTask {{
                constructor(t) {{
                    this._aborted = false;
                    this._terminator = t;
                    this._headersReceivedListeners = createListenerGroup('h');
                }}
                abort() {{
                    if (this._aborted) return;
                    this._aborted = true;
                    this._terminator?.abort();
                    this._headersReceivedListeners.off();
                    this._onCleanup();
                }}
                _onCleanup() {{}}
                _triggerHeadersReceived(_h) {{}}
            }}
            class DownloadResponse {{ constructor() {{}} }}
            class DownloadErrorResponse {{ constructor(_e, _x) {{}} }}
            class Exception {{ constructor(_c, m, _n) {{ this.message = m; }} }}
            function abortedNetworkError() {{ return {{ errMsg: "aborted" }}; }}
            const openedPaths = [];
            function op_fetch(_m, _u, _h, ..._r) {{
                return {{ requestRid: 1, cancelHandleRid: 2 }};
            }}
            function op_fetch_send(_rid) {{
                // Immediately resolved: both IIFEs advance in the same drain.
                return Promise.resolve({{
                    status: 200, headers: [],
                    responseRid: 1, contentLength: 0, error: null,
                }});
            }}
            function op_open_file(path, _mode) {{
                openedPaths.push(path);
                return new Promise(() => {{}});  // park; path already recorded
            }}
            function op_write_file(..._a) {{ return Promise.resolve(); }}
            function op_close_file(..._a) {{ return Promise.resolve(); }}
            function op_rename(..._a) {{ return Promise.resolve(); }}
            function op_unlink(..._a) {{ return Promise.resolve(); }}
            {dl}
            // Two concurrent downloads to the same filePath.
            downloadFile({{ url: "http://ex.com/f", filePath: "/tmp/shared.dat" }});
            downloadFile({{ url: "http://ex.com/f", filePath: "/tmp/shared.dat" }});
            globalThis.openedPaths = openedPaths;
            "#,
            dl = dl,
        );
        rt.execute_script("net02-setup", setup)
            .expect("NET-02 setup");
        let exec = new_executor();
        exec.block_on(rt.run_event_loop(Default::default()))
            .expect("NET-02 event loop");
        rt.execute_script(
            "net02-check",
            r#"
            if (openedPaths.length !== 2) {
                throw new Error(
                    "NET-02: expected 2 op_open_file calls, got "
                    + openedPaths.length + ": " + JSON.stringify(openedPaths)
                );
            }
            if (openedPaths[0] === openedPaths[1]) {
                throw new Error(
                    "NET-02: concurrent downloads must use distinct temp paths; "
                    + "both got: " + openedPaths[0]
                );
            }
            "#,
        )
        .expect("NET-02: concurrent downloads must produce distinct temp paths");
    }

    // ── NET-04 ────────────────────────────────────────────────────────────────

    /// NET-04 regression (OPEN-ITEMS-host-vs-device.md):
    /// The buffered-body pull in `request()` had no byte ceiling, allowing an
    /// unbounded allocation.  Separately, `responseRid` leaked until GC when
    /// a successful `readAll` completed without error.  Two fixes in
    /// `04_request.js`: (1) the `MAX_BUFFERED_BODY_BYTES` (32 MiB) guard inside
    /// the pull callback; (2) the `finally` block that calls
    /// `core.tryClose(cancellation.responseRid)`.
    ///
    /// Seam: production `04_request.js` with mocked ops and a `ReadableStream`
    /// stub whose `pull` feeds the callback a single 33 MiB synthetic chunk
    /// (only `.byteLength` is read, so no real allocation).
    /// Does not cover actual HTTP streaming; covers the JS accumulation guard
    /// and the `finally`-block release path.
    #[test]
    fn net04_buffered_pull_ceiling_and_prompt_rid_release() {
        let req = strip_module_declarations(include_str!("04_request.js"));

        let mut rt = JsRuntime::new(RuntimeOptions::default());
        let setup = format!(
            r#"
            const core = {{
                closedRids: [],
                tryClose(rid) {{ core.closedRids.push(rid); }},
                close(rid) {{ core.closedRids.push(rid); }},
                encode(s) {{ return new TextEncoder().encode(s); }},
                decode(b) {{ return new TextDecoder().decode(b); }},
            }};
            const primordials = {{
                TypeError: globalThis.TypeError,
                JSONParse: JSON.parse,
                TypedArrayPrototypeGetBuffer: (ta) => ta.buffer,
                TypedArrayPrototypeGetByteLength: (ta) => ta.byteLength,
            }};
            function createListenerGroup(_lbl) {{
                return {{ on() {{}}, off() {{}}, trigger() {{}} }};
            }}
            class Header {{ constructor(_h, _s) {{}} }}
            class NetworkTask {{
                constructor(t) {{
                    this._aborted = false;
                    this._terminator = t;
                    this._headersReceivedListeners = createListenerGroup('h');
                }}
                abort() {{
                    if (this._aborted) return;
                    this._aborted = true;
                    this._terminator?.abort();
                    this._headersReceivedListeners.off();
                    this._onCleanup();
                }}
                _onCleanup() {{}}
                _triggerHeadersReceived(_h) {{}}
            }}
            class Response {{ constructor(h) {{ this.header = h; }} }}
            class ErrorResponse {{ constructor(c, e) {{ this.code = c; this.err = e; }} }}
            class Exception {{ constructor(_c, m, _n) {{ this.message = m; }} }}
            function abortedNetworkError(r) {{
                return new ErrorResponse(-1, new Exception(-1, "aborted", r));
            }}
            function nullBodyStatus(s) {{
                return s === 101 || s === 204 || s === 205 || s === 304;
            }}
            // ReadableStream stub: the first request gets one synthetic chunk
            // over the 32 MiB ceiling; the second gets a non-empty 1-byte body.
            // No real 33 MiB allocation: the callback reads only .byteLength.
            class ReadableStream {{
                constructor(rid) {{ this._rid = rid; }}
                pull(_buffer, cb) {{
                    return new Promise((resolve, reject) => {{
                        try {{
                            cb({{
                                byteLength: globalThis.net04Oversized
                                    ? 33 * 1024 * 1024
                                    : 1,
                            }});
                            resolve();
                        }} catch (e) {{
                            reject(e);
                        }}
                    }});
                }}
                cancel() {{}}
            }}
            let _resolveFS = null;
            const failResults = [];
            const successResults = [];
            function op_fetch(_m, _u, _h, ..._r) {{
                return {{ requestRid: 1, cancelHandleRid: 2 }};
            }}
            function op_fetch_send(_rid) {{
                return new Promise(r => {{ _resolveFS = r; }});
            }}
            {req}
            globalThis.net04Oversized = true;
            const _task = request({{
                url: "http://ex.com/api",
                method: "GET",
                responseType: "arraybuffer",
                success() {{ successResults.push("oversized"); }},
                fail(e) {{ failResults.push(e); }},
                complete() {{}},
            }});
            globalThis.net04FailResults = failResults;
            globalThis.net04SuccessResults = successResults;
            // IIFE has run to its first await (op_fetch_send), so _resolveFS is set.
            _resolveFS({{
                status: 200, statusText: "OK", headers: [],
                url: "http://ex.com/api",
                responseRid: 77, contentLength: null,
                remoteAddrIp: null, remoteAddrPort: null, error: null,
            }});
            "#,
            req = req,
        );
        rt.execute_script("net04-setup", setup)
            .expect("NET-04 setup");
        let exec = new_executor();
        exec.block_on(rt.run_event_loop(Default::default()))
            .expect("NET-04 event loop");
        rt.execute_script(
            "net04-check",
            r#"
            if (net04FailResults.length === 0) {
                throw new Error(
                    "NET-04: oversized body must trigger fail callback, "
                    + "but success fired instead"
                );
            }
            if (!core.closedRids.includes(77)) {
                throw new Error(
                    "NET-04: responseRid 77 must be closed promptly in the "
                    + "finally block, not deferred to GC. "
                    + "closedRids=" + JSON.stringify(core.closedRids)
                );
            }
            "#,
        )
        .expect("NET-04: ceiling enforced and responseRid released promptly");
    }
}
