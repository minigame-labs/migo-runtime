//! The engine's HTTP client: every outbound HTTP request is made with it.
//!
//! hyper carries the protocol (HTTP/1.1, HTTP/2 and the connection pool, through `hyper_util`'s legacy client); the
//! engine makes the connections ([`connector`]) and follows the redirects, because both are where its rules and its
//! measurements live: the address policy on every name looked up, the gate on every redirect hop, and the phases a
//! request's `profile` reports ([`trace`]).

mod body;
mod connector;
mod decode;
pub mod multipart;
pub mod range;
mod tcp_info;
#[cfg(test)]
mod tests;
pub mod trace;

use std::fmt;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use crate::network::url::Url;
use bytes::Bytes;
use futures::Stream;
use http::header::{
    ACCEPT_ENCODING, AUTHORIZATION, CONTENT_ENCODING, CONTENT_LENGTH, CONTENT_TYPE, COOKIE, HOST,
    LOCATION, PROXY_AUTHORIZATION, RANGE, REFERER, TRANSFER_ENCODING, USER_AGENT, WWW_AUTHENTICATE,
};
use http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, Version};
use http_body_util::BodyExt;
use hyper_util::client::legacy::Client;
use hyper_util::client::proxy::matcher::Matcher;
use hyper_util::rt::{TokioExecutor, TokioTimer};
use migo_io::pools::IoPools;
use shared::network_policy::NetworkPolicy;

pub use body::Body;
use body::{SendProgress, SentBody};
pub(super) use connector::connect_vetted_tcp;
use connector::{AddressRule, BoxError, Connector, ConnectorConfig};
pub use multipart::{Form, Part};
pub use trace::{ConnectionTrace, RequestTiming, Span};

use super::gate::{GateKind, evaluate_policy};

/// Most redirects one request follows. Past it, the last redirect response is the answer.
const MAX_REDIRECTS: usize = 10;

/// What the engine's requests are sent with: how one client is configured.
pub struct HttpClientBuilder {
    user_agent: String,
    enable_http2: bool,
    policy: NetworkPolicy,
    operation: &'static str,
    proxies: Option<Matcher>,
    rule: AddressRule,
    extra_roots: Vec<rustls::pki_types::CertificateDer<'static>>,
    pools: IoPools,
}

impl HttpClientBuilder {
    /// A client whose resolver refuses blocked addresses (naming `operation` in the refusal), whose redirects are
    /// gated under `policy` as the request that followed them, and whose proxies are the machine's.
    pub fn new(
        user_agent: &str,
        enable_http2: bool,
        policy: &NetworkPolicy,
        operation: &'static str,
        pools: IoPools,
    ) -> Self {
        Self {
            user_agent: user_agent.to_string(),
            enable_http2,
            policy: policy.clone(),
            operation,
            proxies: None,
            rule: AddressRule::Public { operation },
            extra_roots: Vec::new(),
            pools,
        }
    }

    /// Proxies from `matcher` instead of the machine's environment.
    pub fn proxies(mut self, matcher: Matcher) -> Self {
        self.proxies = Some(matcher);
        self
    }

    /// No proxy, whatever the environment says.
    pub fn no_proxy(self) -> Self {
        self.proxies(Matcher::builder().build())
    }

    /// Let the resolver answer any address: for tests that serve on the loopback.
    #[cfg(test)]
    pub(crate) fn any_address(mut self) -> Self {
        self.rule = AddressRule::Any;
        self
    }

    /// Add `root` to the selected trust profile, for local TLS fixtures.
    #[cfg(test)]
    pub(crate) fn add_root(mut self, root: rustls::pki_types::CertificateDer<'static>) -> Self {
        self.extra_roots.push(root);
        self
    }

    pub fn build(self) -> Result<HttpClient, HttpError> {
        let tls = super::tls::ClientTls::new(
            self.policy.tls_trust,
            self.enable_http2,
            self.extra_roots,
            self.pools.clone(),
        );
        // The machine's proxies: the environment's (and the system's, where the HTTP stack is built to read them).
        // A proxy is the machine owner's egress, not something content names.
        let proxies = Arc::new(self.proxies.unwrap_or_else(Matcher::from_system));
        let connector = Connector::new(ConnectorConfig {
            rule: self.rule,
            tls,
            proxies: Arc::clone(&proxies),
        });

        let mut builder = Client::builder(TokioExecutor::new());
        builder
            .pool_timer(TokioTimer::new())
            .timer(TokioTimer::new());
        if self.enable_http2 {
            // HTTP/2 multiplexes many streams over one connection: fewer idle connections, kept longer.
            builder
                .pool_max_idle_per_host(3)
                .pool_idle_timeout(Duration::from_secs(120))
                .http2_adaptive_window(true)
                .http2_keep_alive_interval(Duration::from_secs(30))
                .http2_keep_alive_timeout(Duration::from_secs(10));
        } else {
            // HTTP/1.1 carries one request at a time per connection: more idle connections, freed sooner. ALPN
            // offers only http/1.1, so no connection negotiates HTTP/2.
            builder
                .pool_max_idle_per_host(6)
                .pool_idle_timeout(Duration::from_secs(90));
        }
        let user_agent = HeaderValue::from_str(&self.user_agent).map_err(HttpError::builder)?;
        Ok(HttpClient {
            inner: Arc::new(ClientInner {
                hyper: builder.build(connector),
                proxies,
                user_agent,
                policy: self.policy,
                operation: self.operation,
                pools: self.pools,
            }),
        })
    }
}

/// The client. Cloning it shares its connection pool.
#[derive(Clone)]
pub struct HttpClient {
    inner: Arc<ClientInner>,
}

struct ClientInner {
    hyper: Client<Connector, SentBody>,
    proxies: Arc<Matcher>,
    user_agent: HeaderValue,
    policy: NetworkPolicy,
    operation: &'static str,
    pools: IoPools,
}

impl fmt::Debug for HttpClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HttpClient")
            .field("operation", &self.inner.operation)
            .finish_non_exhaustive()
    }
}

impl HttpClient {
    /// A request made as `kind`: every redirect it follows is gated as `kind` too, so it stays on the channel it was
    /// made on.
    pub fn request(&self, kind: GateKind, method: Method, url: Url) -> RequestBuilder {
        RequestBuilder {
            client: self.clone(),
            kind,
            method,
            url,
            headers: HeaderMap::new(),
            body: Body::empty(),
            timeout: None,
            error: None,
        }
    }

    pub fn get(&self, kind: GateKind, url: Url) -> RequestBuilder {
        self.request(kind, Method::GET, url)
    }

    pub fn post(&self, kind: GateKind, url: Url) -> RequestBuilder {
        self.request(kind, Method::POST, url)
    }
}

/// A request being put together.
pub struct RequestBuilder {
    client: HttpClient,
    kind: GateKind,
    method: Method,
    url: Url,
    headers: HeaderMap,
    body: Body,
    timeout: Option<Duration>,
    error: Option<HttpError>,
}

impl RequestBuilder {
    pub fn header(mut self, name: HeaderName, value: HeaderValue) -> Self {
        self.headers.insert(name, value);
        self
    }

    /// Adds every header of `headers`, keeping repeated names repeated.
    pub fn headers(mut self, headers: HeaderMap) -> Self {
        let mut last = None;
        for (name, value) in headers {
            if let Some(name) = name {
                last = Some(name);
            }
            if let Some(name) = &last {
                self.headers.append(name.clone(), value);
            }
        }
        self
    }

    pub fn body(mut self, body: impl Into<Body>) -> Self {
        self.body = body.into();
        self
    }

    pub fn multipart(mut self, form: Form) -> Self {
        match HeaderValue::from_str(&form.content_type()) {
            Ok(content_type) => {
                self.headers.insert(CONTENT_TYPE, content_type);
            }
            Err(error) => self.error = Some(HttpError::builder(error)),
        }
        self.body = form.into_body();
        self
    }

    /// Bounds the whole exchange: connecting, the redirects, the head and the body.
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    pub async fn send(self) -> Result<Response, HttpError> {
        if let Some(error) = self.error {
            return Err(error);
        }
        let deadline = self
            .timeout
            .map(|timeout| tokio::time::Instant::now() + timeout);
        let url = self.url.clone();
        let exchange = self.exchange(deadline);
        match deadline {
            Some(deadline) => match tokio::time::timeout_at(deadline, exchange).await {
                Ok(answered) => answered,
                Err(_) => Err(HttpError::timeout(url)),
            },
            None => exchange.await,
        }
    }

    async fn exchange(self, deadline: Option<tokio::time::Instant>) -> Result<Response, HttpError> {
        let Self {
            client,
            kind,
            mut method,
            mut url,
            mut headers,
            mut body,
            ..
        } = self;
        let inner = &client.inner;
        let fetch_start = Instant::now();
        let original_host = url.host_str().map(str::to_owned);
        let mut redirects = 0usize;
        // From the first redirect response to the last, while every hop stays on the request's host.
        let mut redirect: Option<Span> = None;
        let mut same_host = true;

        if !headers.contains_key(USER_AGENT) {
            headers.insert(USER_AGENT, inner.user_agent.clone());
        }
        let range_request = headers.contains_key(RANGE);
        if !headers.contains_key(ACCEPT_ENCODING) {
            headers.insert(
                ACCEPT_ENCODING,
                HeaderValue::from_static(if range_request {
                    "identity"
                } else {
                    "gzip, br"
                }),
            );
        }

        loop {
            let replay = body.try_clone();
            let progress = Arc::new(SendProgress::default());
            let head_bytes = request_head_bytes(&method, &url, &headers, body.length());
            let request = build_request(
                inner,
                &method,
                &url,
                &headers,
                body.sending(Arc::clone(&progress)),
            )?;
            let sent_at = Instant::now();
            let response = inner
                .hyper
                .request(request)
                .await
                .map_err(|error| HttpError::request(url.clone(), error))?;
            let response_start = Instant::now();

            let status = response.status();
            let next = response
                .headers()
                .get(LOCATION)
                .and_then(|location| redirect_target(&url, location));
            if let Some(next) = next.filter(|_| redirects < MAX_REDIRECTS) {
                // 301/302 turn a POST into a GET, 303 turns anything but a HEAD into one; 307/308 resend the request
                // as it was -- which a body read from a stream cannot be, so for one of those the redirect is the
                // answer.
                let to_get = match status {
                    StatusCode::SEE_OTHER => method != Method::HEAD,
                    StatusCode::MOVED_PERMANENTLY | StatusCode::FOUND => method == Method::POST,
                    _ => false,
                };
                let follows = match status {
                    StatusCode::MOVED_PERMANENTLY | StatusCode::FOUND | StatusCode::SEE_OTHER => {
                        true
                    }
                    StatusCode::TEMPORARY_REDIRECT | StatusCode::PERMANENT_REDIRECT => {
                        replay.is_some()
                    }
                    _ => false,
                };
                if follows {
                    evaluate_policy(&next, &inner.policy, kind).map_err(|reject| {
                        HttpError::redirect(
                            url.clone(),
                            super::client::redirect_reject_message(kind, &reject),
                        )
                    })?;
                    redirects += 1;
                    same_host &= next.host_str().map(str::to_owned) == original_host;
                    redirect = Some(Span {
                        start: redirect.map_or(response_start, |span| span.start),
                        end: response_start,
                    });
                    if to_get {
                        method = Method::GET;
                        body = Body::empty();
                        for name in [
                            CONTENT_TYPE,
                            CONTENT_LENGTH,
                            CONTENT_ENCODING,
                            TRANSFER_ENCODING,
                        ] {
                            headers.remove(name);
                        }
                    } else {
                        body = replay.unwrap_or_else(Body::empty);
                    }
                    if !same_origin(&url, &next) {
                        for name in [AUTHORIZATION, COOKIE, PROXY_AUTHORIZATION, WWW_AUTHENTICATE] {
                            headers.remove(name);
                        }
                    }
                    match referer(&url, &next) {
                        Some(value) => {
                            headers.insert(REFERER, value);
                        }
                        None => {
                            headers.remove(REFERER);
                        }
                    }
                    url = next;
                    continue;
                }
            }

            let (mut parts, incoming) = response.into_parts();
            let connection = parts.extensions.get::<Arc<ConnectionTrace>>().cloned();
            let socket_reused = connection.as_ref().is_some_and(|trace| !trace.claim());
            let request_start = match &connection {
                Some(trace) if !socket_reused => trace.established().max(sent_at),
                _ => sent_at,
            };
            let request_end = progress.ended().unwrap_or(request_start).max(request_start);
            let received_head_bytes = response_head_bytes(parts.status, &parts.headers);
            let timing = RequestTiming {
                fetch_start,
                redirect: redirect.filter(|_| same_host),
                connection,
                socket_reused,
                request_start,
                request_end,
                response_start,
                response_end: None,
                bytes_sent: head_bytes + progress.bytes(),
                bytes_received: received_head_bytes,
                version: parts.version,
            };
            let plan = decode::codings(&method, range_request, parts.status, &parts.headers)
                .map_err(|error| HttpError::body(url.clone(), error))?;
            let (body, wire_counter) = if let Some(plan) = plan {
                let body = decode::DecodedBody::new(incoming, plan, inner.pools.clone())
                    .map_err(|error| HttpError::body(url.clone(), error))?;
                let wire_counter = Some(body.wire_bytes.clone());
                parts.headers.remove(CONTENT_ENCODING);
                parts.headers.remove(CONTENT_LENGTH);
                (ResponseBody::Decoded(body), wire_counter)
            } else {
                (ResponseBody::Incoming(incoming), None)
            };
            return Ok(Response {
                status: parts.status,
                version: parts.version,
                headers: parts.headers,
                url,
                body,
                wire_counter,
                head_bytes: received_head_bytes,
                wire_bytes: 0,
                decoded_bytes: 0,
                timing,
                deadline,
            });
        }
    }
}

fn build_request(
    inner: &ClientInner,
    method: &Method,
    url: &Url,
    headers: &HeaderMap,
    body: SentBody,
) -> Result<http::Request<SentBody>, HttpError> {
    let uri: http::Uri = url.as_str().parse().map_err(HttpError::builder)?;
    let mut request = http::Request::builder()
        .method(method.clone())
        .uri(uri.clone())
        .body(body)
        .map_err(HttpError::builder)?;
    *request.headers_mut() = headers.clone();
    // Plain HTTP through a proxy is written to the proxy, which takes its credentials on every request. (HTTPS
    // presents them once, on the tunnel's CONNECT.)
    if url.scheme() == "http"
        && !headers.contains_key(PROXY_AUTHORIZATION)
        && let Some(auth) = inner
            .proxies
            .intercept(&uri)
            .and_then(|proxy| proxy.basic_auth().cloned())
    {
        request.headers_mut().insert(PROXY_AUTHORIZATION, auth);
    }
    Ok(request)
}

/// Where `location` sends a request that was for `from`, if it names an HTTP(S) URL.
fn redirect_target(from: &Url, location: &HeaderValue) -> Option<Url> {
    let location = std::str::from_utf8(location.as_bytes()).ok()?;
    let next = from.join(location).ok()?;
    matches!(next.scheme(), "http" | "https").then_some(next)
}

fn same_origin(a: &Url, b: &Url) -> bool {
    a.scheme() == b.scheme()
        && a.host_str() == b.host_str()
        && a.port_or_known_default() == b.port_or_known_default()
}

/// The `Referer` a redirect to `next` carries: the URL that redirected, without credentials or fragment -- and none
/// from HTTPS to HTTP, which would hand a secure page's address to an insecure one.
fn referer(from: &Url, next: &Url) -> Option<HeaderValue> {
    if from.scheme() == "https" && next.scheme() == "http" {
        return None;
    }
    let mut referer = from.clone();
    let _ = referer.set_username("");
    let _ = referer.set_password(None);
    referer.set_fragment(None);
    HeaderValue::from_str(referer.as_str()).ok()
}

/// The bytes of a request's head as HTTP/1.1 writes it: the request line, the headers (with the `Host` and
/// `Content-Length` hyper adds), the blank line.
fn request_head_bytes(method: &Method, url: &Url, headers: &HeaderMap, length: Option<u64>) -> u64 {
    let target = match url.query() {
        Some(query) => url.path().len() + 1 + query.len(),
        None => url.path().len(),
    };
    let mut bytes = method.as_str().len() + 1 + target + " HTTP/1.1\r\n".len();
    for (name, value) in headers {
        bytes += name.as_str().len() + 2 + value.len() + 2;
    }
    if !headers.contains_key(HOST) {
        let authority = match (url.host_str(), url.port()) {
            (Some(host), Some(port)) => host.len() + 1 + port.to_string().len(),
            (Some(host), None) => host.len(),
            _ => 0,
        };
        bytes += "host: \r\n".len() + authority;
    }
    if let Some(length) =
        length.filter(|length| *length > 0 && !headers.contains_key(CONTENT_LENGTH))
    {
        bytes += "content-length: \r\n".len() + length.to_string().len();
    }
    (bytes + 2) as u64
}

/// The bytes of a response's head as HTTP/1.1 writes it.
fn response_head_bytes(status: StatusCode, headers: &HeaderMap) -> u64 {
    let mut bytes = "HTTP/1.1 000 ".len() + status.canonical_reason().unwrap_or("").len() + 2;
    for (name, value) in headers {
        bytes += name.as_str().len() + 2 + value.len() + 2;
    }
    (bytes + 2) as u64
}

/// A response: its head at once, its body as it is read.
pub struct Response {
    status: StatusCode,
    version: Version,
    headers: HeaderMap,
    url: Url,
    body: ResponseBody,
    timing: RequestTiming,
    deadline: Option<tokio::time::Instant>,
    wire_counter: Option<Arc<AtomicU64>>,
    head_bytes: u64,
    wire_bytes: u64,
    decoded_bytes: u64,
}

enum ResponseBody {
    Incoming(hyper::body::Incoming),
    Decoded(decode::DecodedBody),
    Local(Option<Bytes>),
    Ended,
}

impl ResponseBody {
    async fn next(&mut self) -> std::io::Result<Option<Bytes>> {
        match self {
            Self::Ended => Ok(None),
            Self::Local(bytes) => Ok(bytes.take().filter(|bytes| !bytes.is_empty())),
            Self::Decoded(body) => body.chunk().await,
            Self::Incoming(incoming) => loop {
                match incoming.frame().await {
                    None => return Ok(None),
                    Some(Err(error)) => return Err(std::io::Error::other(error)),
                    Some(Ok(frame)) => {
                        if let Ok(bytes) = frame.into_data()
                            && !bytes.is_empty()
                        {
                            return Ok(Some(bytes));
                        }
                    }
                }
            },
        }
    }
}

impl fmt::Debug for Response {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Response")
            .field("status", &self.status)
            .field("url", &self.url.as_str())
            .finish_non_exhaustive()
    }
}

impl Response {
    /// A response the engine answers itself, without a connection (a `data:` URL).
    pub fn local(url: Url, response: http::Response<Vec<u8>>) -> Self {
        let (parts, body) = response.into_parts();
        let now = Instant::now();
        let head = response_head_bytes(parts.status, &parts.headers);
        Self {
            status: parts.status,
            version: parts.version,
            headers: parts.headers,
            url,
            timing: RequestTiming::local(now, head),
            body: ResponseBody::Local(Some(Bytes::from(body))),
            deadline: None,
            wire_counter: None,
            head_bytes: head,
            wire_bytes: 0,
            decoded_bytes: 0,
        }
    }

    pub fn status(&self) -> StatusCode {
        self.status
    }

    pub fn version(&self) -> Version {
        self.version
    }

    pub fn headers(&self) -> &HeaderMap {
        &self.headers
    }

    // Decoding removes Content-Encoding/Length from the public headers. Keep
    // the distinction available after body EOF too, for byte-range identity.
    pub(super) fn has_decoded_representation(&self) -> bool {
        self.wire_counter.is_some()
    }

    /// The URL the response came from: the request's, or the last redirect's.
    pub fn url(&self) -> &Url {
        &self.url
    }

    /// The body's length, where the response declares it.
    pub fn content_length(&self) -> Option<u64> {
        match &self.body {
            ResponseBody::Incoming(incoming) => http_body::Body::size_hint(incoming).exact(),
            ResponseBody::Local(bytes) => {
                Some(bytes.as_ref().map_or(0, |bytes| bytes.len() as u64))
            }
            ResponseBody::Decoded(_) => None,
            ResponseBody::Ended => Some(0),
        }
    }

    /// Who the connection the response came on is connected to: the server, or the machine's proxy.
    pub fn remote_addr(&self) -> Option<SocketAddr> {
        self.timing.connection.as_ref().map(|trace| trace.peer)
    }

    pub fn timing(&self) -> &RequestTiming {
        &self.timing
    }

    /// HTTP body bytes received, before Content-Encoding decoding (no headers).
    pub fn wire_body_bytes(&self) -> u64 {
        self.wire_bytes
    }

    /// Bytes delivered to the caller after Content-Encoding decoding.
    pub fn decoded_body_bytes(&self) -> u64 {
        self.decoded_bytes
    }

    /// The body's next chunk, or `None` at its end -- within the request's timeout.
    pub async fn chunk(&mut self) -> Result<Option<Bytes>, HttpError> {
        if matches!(self.body, ResponseBody::Ended) {
            return Ok(None);
        }
        let read = self.body.next();
        let next = match self.deadline {
            Some(deadline) => match tokio::time::timeout_at(deadline, read).await {
                Ok(next) => next.map_err(|error| HttpError::body(self.url.clone(), error)),
                Err(_) => Err(HttpError::timeout(self.url.clone())),
            },
            None => read
                .await
                .map_err(|error| HttpError::body(self.url.clone(), error)),
        };
        if let Some(counter) = &self.wire_counter {
            self.wire_bytes = counter.load(Ordering::Relaxed);
        } else if let Ok(Some(bytes)) = &next {
            self.wire_bytes += bytes.len() as u64;
        }
        self.timing.bytes_received = self.head_bytes + self.wire_bytes;
        match next {
            Ok(Some(bytes)) => {
                self.decoded_bytes += bytes.len() as u64;
                Ok(Some(bytes))
            }
            Ok(None) => {
                self.body = ResponseBody::Ended;
                if self.timing.response_end.is_none() {
                    self.timing.response_end = Some(Instant::now());
                    super::quality::estimator().lock().record(&self.timing);
                }
                Ok(None)
            }
            Err(error) => {
                self.body = ResponseBody::Ended;
                Err(error)
            }
        }
    }

    /// The body as a stream of chunks.
    pub fn bytes_stream(self) -> Pin<Box<dyn Stream<Item = Result<Bytes, HttpError>> + Send>> {
        Box::pin(futures::stream::unfold(Some(self), |response| async move {
            let mut response = response?;
            match response.chunk().await {
                Ok(Some(bytes)) => Some((Ok(bytes), Some(response))),
                Ok(None) => None,
                Err(error) => Some((Err(error), None)),
            }
        }))
    }
}

/// Why a request did not get its answer.
#[derive(Debug)]
pub struct HttpError {
    kind: ErrorKind,
    url: Option<Url>,
    source: Option<BoxError>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    /// The request could not be built.
    Builder,
    /// Connecting, or sending, or receiving the head failed.
    Request,
    /// A redirect the request's rules refuse.
    Redirect,
    /// Reading the body failed.
    Body,
    /// The request's timeout passed.
    Timeout,
}

impl HttpError {
    fn builder(error: impl Into<BoxError>) -> Self {
        Self {
            kind: ErrorKind::Builder,
            url: None,
            source: Some(error.into()),
        }
    }

    fn request(url: Url, error: impl Into<BoxError>) -> Self {
        Self {
            kind: ErrorKind::Request,
            url: Some(url),
            source: Some(error.into()),
        }
    }

    fn redirect(url: Url, message: String) -> Self {
        Self {
            kind: ErrorKind::Redirect,
            url: Some(url),
            source: Some(message.into()),
        }
    }

    fn body(url: Url, error: impl Into<BoxError>) -> Self {
        Self {
            kind: ErrorKind::Body,
            url: Some(url),
            source: Some(error.into()),
        }
    }

    fn timeout(url: Url) -> Self {
        Self {
            kind: ErrorKind::Timeout,
            url: Some(url),
            source: None,
        }
    }

    pub fn kind(&self) -> ErrorKind {
        self.kind
    }

    pub fn is_timeout(&self) -> bool {
        self.kind == ErrorKind::Timeout
    }
}

impl fmt::Display for HttpError {
    /// What failed, for which URL, and every cause beneath it -- the resolver's refusal of a private address
    /// included, which is what a game's `fail` reports.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self.kind {
            ErrorKind::Builder => "error building the request",
            ErrorKind::Request => "error sending request",
            ErrorKind::Redirect => "error following a redirect",
            ErrorKind::Body => "error reading the response body",
            ErrorKind::Timeout => "operation timed out",
        })?;
        if let Some(url) = &self.url {
            write!(f, " for url ({url})")?;
        }
        let mut cause: Option<&(dyn std::error::Error + 'static)> =
            self.source.as_deref().map(|e| e as _);
        while let Some(error) = cause {
            write!(f, ": {error}")?;
            cause = error.source();
        }
        Ok(())
    }
}

impl std::error::Error for HttpError {}
