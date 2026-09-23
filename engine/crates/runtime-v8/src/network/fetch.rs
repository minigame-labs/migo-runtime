use std::borrow::Cow;
use std::cell::RefCell;
use std::rc::Rc;

use deno_core::AsyncResult;
use deno_core::BufView;
use deno_core::ByteString;
use deno_core::JsBuffer;
use deno_core::OpState;
use deno_core::Resource;
use deno_core::ResourceId;
use deno_core::error::AnyError;
use deno_core::op2;
use deno_error::JsErrorBox;
use migo_services::network as service_net;
use migo_services::network::resources::ResourceTable;
use reqwest::Client;
use serde::Deserialize;
use serde::Serialize;

use crate::io_state::IoSchedulerState;
use crate::network::Options;

/// What `op_fetch` answers, in the shape `04_request.js` reads.
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

/// This session's network resources: the requests, cancel handles and
/// response bodies `migo_services::network` owns for it.
///
/// The service owns them so both executions hold the same ones; what lives in
/// deno's table is a handle that names one, because `core.read` and
/// `core.close` look content's rid up there and would otherwise find nothing.
pub(crate) struct NetworkResources(pub(crate) std::sync::Arc<ResourceTable>);

/// A service resource, as deno's table holds it.
pub(crate) struct ServiceHandle {
    pub(crate) resources: std::sync::Arc<ResourceTable>,
    pub(crate) id: service_net::resources::ResourceId,
    kind: &'static str,
}

impl ServiceHandle {
    pub(crate) fn new(
        resources: std::sync::Arc<ResourceTable>,
        id: service_net::resources::ResourceId,
        kind: &'static str,
    ) -> Self {
        Self {
            resources,
            id,
            kind,
        }
    }
}

impl Resource for ServiceHandle {
    fn name(&'_ self) -> Cow<'_, str> {
        self.kind.into()
    }

    /// A body read, in the chunk size content asked for. Only a response has
    /// one; a read of anything else is the service's refusal.
    fn read(self: Rc<Self>, limit: usize) -> AsyncResult<BufView> {
        Box::pin(async move {
            let chunk = service_net::fetch::read(&self.resources, self.id, limit)
                .await
                .map_err(|error| JsErrorBox::generic(error.message))?;
            Ok(BufView::from(chunk.to_vec()))
        })
    }

    fn size_hint(&self) -> (u64, Option<u64>) {
        let size = self
            .resources
            .response(self.id)
            .ok()
            .and_then(|body| body.size());
        (size.unwrap_or(0), size)
    }

    /// Closing is the service's: a cancel handle aborts the send, a response
    /// stops its read, and the id names nothing afterwards.
    fn close(self: Rc<Self>) {
        self.resources.close(self.id);
    }
}

/// What a fetch is made with, from this runtime's state.
fn fetch_env<'a>(
    state: &'a OpState,
    client: &'a Client,
    resources: &'a ResourceTable,
    allow_host: bool,
) -> service_net::fetch::FetchEnv<'a> {
    service_net::fetch::FetchEnv {
        policy: &state
            .borrow::<shared::op_state::HostOpState>()
            .network_policy,
        client,
        resources,
        pools: state.borrow::<IoSchedulerState>().0.pools(),
        allow_host,
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
        let client = state
            .resource_table
            .get::<HttpClientResource>(rid)
            .map_err(|_| JsErrorBox::generic("Failed to get HTTP client"))?;
        (client.client.clone(), client.allow_host)
    } else {
        (
            get_or_create_client_from_state(state, enable_http2)
                .map_err(|_| JsErrorBox::generic("Failed to create HTTP client"))?,
            false,
        )
    };
    // The engine's own JavaScript never passes one: a request body is bytes it
    // has in hand (`04_request.js`), and `uploadFile` streams its file through
    // `op_fetch_upload` instead. Refused rather than ignored, so a caller that
    // starts passing one is told rather than silently sending no body.
    if resource.is_some() {
        return Err(JsErrorBox::not_supported());
    }
    let resources = std::sync::Arc::clone(&state.borrow::<NetworkResources>().0);

    let headers: Vec<(Vec<u8>, Vec<u8>)> = headers
        .into_iter()
        .map(|(name, value)| (name.to_vec(), value.to_vec()))
        .collect();
    let body = match (has_body, data.as_deref()) {
        (true, Some(bytes)) => service_net::fetch::RequestBody::Bytes(bytes),
        _ => service_net::fetch::RequestBody::None,
    };
    let handles = service_net::fetch::fetch(
        fetch_env(state, &client, &resources, allow_host),
        &method,
        &url,
        &headers,
        body,
        timeout,
        enable_cache,
    )
    .map_err(service_error)?;

    // The ids content holds are this runtime's, naming the service's.
    let request_rid = state.resource_table.add(ServiceHandle::new(
        std::sync::Arc::clone(&resources),
        handles.request_rid,
        "fetchRequest",
    ));
    let cancel_handle_rid = handles.cancel_handle_rid.map(|id| {
        state.resource_table.add(ServiceHandle::new(
            std::sync::Arc::clone(&resources),
            id,
            "fetchCancelHandle",
        ))
    });
    Ok(FetchReturn {
        request_rid,
        cancel_handle_rid,
    })
}

/// A service failure, as the class this op's callers already catch.
fn service_error(error: migo_services::ServiceError) -> JsErrorBox {
    if error.class == "TypeError" {
        JsErrorBox::type_error(error.message)
    } else {
        JsErrorBox::generic(error.message)
    }
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

#[op2(async(lazy), fast)]
#[serde]
pub async fn op_fetch_send(
    state: Rc<RefCell<OpState>>,
    #[smi] rid: ResourceId,
) -> Result<FetchResponse, JsErrorBox> {
    let (resources, request_id, pools) = {
        let state = state.borrow();
        let handle = state
            .resource_table
            .get::<ServiceHandle>(rid)
            .map_err(|_| JsErrorBox::generic("Failed to take fetch request resource"))?;
        let pools = state.borrow::<IoSchedulerState>().0.pools().clone();
        (std::sync::Arc::clone(&handle.resources), handle.id, pools)
    };
    // The request is consumed by the send, so its handle is too: a second send
    // then finds nothing, as it did when the resource itself was taken.
    let _ = state.borrow_mut().resource_table.take::<ServiceHandle>(rid);

    let answer = service_net::fetch::fetch_send(&resources, &pools, request_id)
        .await
        .map_err(service_error)?;

    let response_rid = state.borrow_mut().resource_table.add(ServiceHandle::new(
        resources,
        answer.response_rid,
        "fetchResponse",
    ));
    Ok(FetchResponse {
        status: answer.status,
        status_text: answer.status_text,
        headers: answer
            .headers
            .into_iter()
            .map(|(name, value)| (name.into(), value.into()))
            .collect(),
        url: answer.url,
        response_rid,
        content_length: answer.content_length,
        remote_addr_ip: answer.remote_addr_ip,
        remote_addr_port: answer.remote_addr_port,
        error: answer.error,
    })
}

pub fn create_http_client(
    user_agent: &str,
    enable_http2: bool,
    net_policy: &shared::op_state::NetworkPolicy,
) -> Result<Client, AnyError> {
    migo_services::network::client::create_policy_http_client(
        user_agent,
        enable_http2,
        net_policy,
        super::gate::GateKind::FetchRedirect,
        "fetch",
    )
    .map_err(|error| AnyError::from(std::io::Error::other(error.to_string())))
}

/// Build the per-host client used by streamed audio. Construction is lazy at
/// the audio-service boundary; this function only centralizes the same TLS,
/// resolver, redirect, and pool configuration used by fetch.
#[cfg(feature = "api-media")]
pub fn create_audio_http_client(
    net_policy: &shared::op_state::NetworkPolicy,
) -> Result<Client, AnyError> {
    migo_services::network::client::create_policy_http_client(
        "migo",
        true,
        net_policy,
        super::gate::GateKind::AudioStreamRedirect,
        "audio",
    )
    .map_err(|error| AnyError::from(std::io::Error::other(error.to_string())))
}

// -- Upload --
//
// The upload itself is the service's (`migo_services::network::upload`): the
// VFS resolution, the streamed multipart body, the policy and the bound on the
// answer. What is here is the two things only an op can do -- take the
// arguments out of V8 and name the service's cancel handle through deno's
// table.

#[derive(Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FetchUploadResult {
    pub data: String,
    pub status_code: u16,
    pub headers: Vec<(ByteString, ByteString)>,
    pub total_bytes_sent: u64,
    pub error: Option<String>,
}

impl From<service_net::upload::UploadAnswer> for FetchUploadResult {
    fn from(answer: service_net::upload::UploadAnswer) -> Self {
        Self {
            data: answer.data,
            status_code: answer.status_code,
            headers: answer
                .headers
                .into_iter()
                .map(|(name, value)| (name.into(), value.into()))
                .collect(),
            total_bytes_sent: answer.total_bytes_sent,
            error: answer.error,
        }
    }
}

/// The handle an in-flight `uploadFile` is aborted with.
///
/// Content is given the rid before it awaits the upload, keeps it on the task,
/// and `abort()` closes it -- which cancels the upload. The mirror of the
/// cancel handle `op_fetch` hands back for a download.
#[op2(fast)]
#[smi]
pub fn op_fetch_upload_cancel_handle(state: &mut OpState) -> ResourceId {
    let resources = std::sync::Arc::clone(&state.borrow::<NetworkResources>().0);
    let id = service_net::upload::cancel_handle(&resources);
    state
        .resource_table
        .add(ServiceHandle::new(resources, id, "fetchCancelHandle"))
}

/// Scalar options, bundled into one `#[serde]` argument.
///
/// deno_core's `setUpAsyncStub` codegens async ops up to ten arguments
/// (`arg_count` = the JavaScript arguments plus two). With every scalar passed
/// separately this op reached eleven, which throws "Too many arguments for
/// async op codegen" and aborts snapshot creation.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct FetchUploadOpts {
    timeout: u32,
    enable_http2: bool,
}

#[op2(async(lazy))]
#[serde]
#[allow(clippy::too_many_arguments)]
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
    let (resources, client, policy, content) = {
        let mut st = state.borrow_mut();
        let client = get_or_create_client_from_state(&mut st, enable_http2)
            .map_err(|error| JsErrorBox::generic(error.to_string()))?;
        let resources = std::sync::Arc::clone(&st.borrow::<NetworkResources>().0);
        let host = st.borrow::<shared::op_state::HostOpState>();
        (
            resources,
            client,
            host.network_policy.clone(),
            (host.vfs.clone(), host.mount_table.clone()),
        )
    };
    // Zero is "no handle"; anything else names the service resource this
    // runtime's rid stands for. A rid content already closed is gone from the
    // table, which the service reads as aborted -- the same answer it gave
    // when the handle itself lived here.
    let cancel_rid = if cancel_rid == 0 {
        0
    } else {
        match state
            .borrow()
            .resource_table
            .get::<ServiceHandle>(cancel_rid)
        {
            Ok(handle) => handle.id,
            Err(_) => return Ok(service_net::upload::UploadAnswer::aborted_answer().into()),
        }
    };

    let (vfs, mount_table) = content;
    service_net::upload::upload(
        service_net::upload::UploadEnv {
            policy: &policy,
            client: &client,
            resources: &resources,
            vfs: vfs.as_deref(),
            mount_table: mount_table.as_deref(),
        },
        service_net::upload::UploadRequest {
            cancel_rid,
            url,
            file_path,
            name,
            filename,
            headers: headers
                .into_iter()
                .map(|(name, value)| (name.to_vec(), value.to_vec()))
                .collect(),
            form_data,
            timeout_ms: timeout,
        },
    )
    .await
    .map(FetchUploadResult::from)
    .map_err(service_error)
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
