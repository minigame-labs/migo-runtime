//! The engine's HTTP client: every outbound HTTP request is made with it.
//!
//! hyper carries the protocol (HTTP/1.1, HTTP/2 and the connection pool, through `hyper_util`'s legacy client); the
//! engine makes the connections ([`connector`]) and follows the redirects, because both are where its rules and its
//! measurements live: the address policy on every name looked up, the gate on every redirect hop, and the phases a
//! request's `profile` reports ([`trace`]).

mod body;
mod connector;
pub mod multipart;
mod tcp_info;
#[cfg(test)]
mod tests;
pub mod trace;

use std::fmt;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use futures::Stream;
use http::header::{
    AUTHORIZATION, CONTENT_ENCODING, CONTENT_LENGTH, CONTENT_TYPE, COOKIE, HOST, LOCATION,
    PROXY_AUTHORIZATION, REFERER, TRANSFER_ENCODING, USER_AGENT, WWW_AUTHENTICATE,
};
use http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, Version};
use http_body_util::BodyExt;
use hyper_util::client::legacy::Client;
use hyper_util::client::proxy::matcher::Matcher;
use hyper_util::rt::{TokioExecutor, TokioTimer};
use shared::op_state::NetworkPolicy;
use url::Url;

pub use body::Body;
use body::{SendProgress, SentBody};
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
    redirect_kind: GateKind,
    operation: &'static str,
    proxies: Option<Matcher>,
    rule: AddressRule,
    extra_roots: Vec<rustls::pki_types::CertificateDer<'static>>,
}

impl HttpClientBuilder {
    /// A client whose resolver refuses blocked addresses (naming `operation` in the refusal), whose redirects are
    /// gated as `redirect_kind` under `policy`, and whose proxies are the machine's.
    pub fn new(
        user_agent: &str,
        enable_http2: bool,
        policy: &NetworkPolicy,
        redirect_kind: GateKind,
        operation: &'static str,
    ) -> Self {
        Self {
            user_agent: user_agent.to_string(),
            enable_http2,
            policy: policy.clone(),
            redirect_kind,
            operation,
            proxies: None,
            rule: AddressRule::Public { operation },
            extra_roots: Vec::new(),
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

    /// Trust `root` as well as the Mozilla set: for tests that serve TLS with their own authority.
    #[cfg(test)]
    pub(crate) fn add_root(mut self, root: rustls::pki_types::CertificateDer<'static>) -> Self {
        self.extra_roots.push(root);
        self
    }

    pub fn build(self) -> Result<HttpClient, HttpError> {
        let tls = connector::tls_config(self.enable_http2, &self.extra_roots)
            .map_err(HttpError::builder)?;
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
                redirect_kind: self.redirect_kind,
                operation: self.operation,
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
    redirect_kind: GateKind,
    operation: &'static str,
}

impl fmt::Debug for HttpClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HttpClient")
            .field("operation", &self.inner.operation)
            .finish_non_exhaustive()
    }
}

impl HttpClient {
    pub fn request(&self, method: Method, url: Url) -> RequestBuilder {
        RequestBuilder {
            client: self.clone(),
            method,
            url,
            headers: HeaderMap::new(),
            body: Body::empty(),
            timeout: None,
            error: None,
        }
    }

    pub fn get(&self, url: Url) -> RequestBuilder {
        self.request(Method::GET, url)
    }

    pub fn post(&self, url: Url) -> RequestBuilder {
        self.request(Method::POST, url)
    }
}

/// A request being put together.
pub struct RequestBuilder {
    client: HttpClient,
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
                    evaluate_policy(&next, &inner.policy, inner.redirect_kind).map_err(
                        |reject| {
                            HttpError::redirect(
                                url.clone(),
                                super::client::redirect_reject_message(inner.operation, &reject),
                            )
                        },
                    )?;
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

            let (parts, incoming) = response.into_parts();
            let connection = parts.extensions.get::<Arc<ConnectionTrace>>().cloned();
            let socket_reused = connection.as_ref().is_some_and(|trace| !trace.claim());
            let request_start = match &connection {
                Some(trace) if !socket_reused => trace.established().max(sent_at),
                _ => sent_at,
            };
            let request_end = progress.ended().unwrap_or(request_start).max(request_start);
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
                bytes_received: response_head_bytes(parts.status, &parts.headers),
                version: parts.version,
            };
            return Ok(Response {
                status: parts.status,
                version: parts.version,
                headers: parts.headers,
                url,
                body: ResponseBody::Incoming(incoming),
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
}

enum ResponseBody {
    Incoming(hyper::body::Incoming),
    Local(Option<Bytes>),
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
            timing: RequestTiming::local(now, head + body.len() as u64),
            body: ResponseBody::Local(Some(Bytes::from(body))),
            deadline: None,
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
        }
    }

    /// Who the connection the response came on is connected to: the server, or the machine's proxy.
    pub fn remote_addr(&self) -> Option<SocketAddr> {
        self.timing.connection.as_ref().map(|trace| trace.peer)
    }

    pub fn timing(&self) -> &RequestTiming {
        &self.timing
    }

    /// The body's next chunk, or `None` at its end -- within the request's timeout.
    pub async fn chunk(&mut self) -> Result<Option<Bytes>, HttpError> {
        loop {
            let next = match &mut self.body {
                ResponseBody::Local(bytes) => bytes.take().filter(|bytes| !bytes.is_empty()),
                ResponseBody::Incoming(incoming) => {
                    let frame = match self.deadline {
                        Some(deadline) => tokio::time::timeout_at(deadline, incoming.frame())
                            .await
                            .map_err(|_| HttpError::timeout(self.url.clone()))?,
                        None => incoming.frame().await,
                    };
                    match frame {
                        None => None,
                        Some(Err(error)) => return Err(HttpError::body(self.url.clone(), error)),
                        Some(Ok(frame)) => match frame.into_data() {
                            Ok(data) if !data.is_empty() => Some(data),
                            // Trailers, or an empty frame: nothing for the reader.
                            _ => continue,
                        },
                    }
                }
            };
            match next {
                Some(bytes) => {
                    self.timing.bytes_received += bytes.len() as u64;
                    return Ok(Some(bytes));
                }
                None => {
                    if self.timing.response_end.is_none() {
                        self.timing.response_end = Some(Instant::now());
                    }
                    return Ok(None);
                }
            }
        }
    }

    /// The body as a stream of chunks.
    pub fn bytes_stream(self) -> Pin<Box<dyn Stream<Item = Result<Bytes, HttpError>> + Send>> {
        Box::pin(futures::stream::unfold(self, |mut response| async move {
            match response.chunk().await {
                Ok(Some(bytes)) => Some((Ok(bytes), response)),
                Ok(None) => None,
                Err(error) => Some((Err(error), response)),
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
