//! A `fetch` reaches the host's network service and its body comes back
//! through `core.read`.
//!
//! The request, the handle that aborts it and the response body are the
//! service's resources now (`migo_services::network`), and what lives in
//! deno's table is a handle that names one. That indirection is invisible to
//! content and impossible to check by reading either side: the ops answer the
//! same shapes as before, and the engine's `ReadableStream` still pulls the
//! body with `core.read` on the rid `op_fetch_send` gave it.
//!
//! So this runs the whole path in a real runtime, over a `data:` URL -- the one
//! scheme that needs no network and still goes through every step: build,
//! send, read, close.

#[cfg(test)]
mod fetch_service_tests {
    use deno_core::{FastString, JsRuntime, RuntimeOptions};
    use shared::{
        channel::ThreadWakeup,
        device::gpu_caps::GpuCaps,
        network_policy::NetworkPolicy,
        op_state::{AudioSender, HostOpState},
        render_command_sender::CommandSender,
    };
    use std::{
        path::PathBuf,
        sync::{Arc, atomic::AtomicBool},
    };

    fn test_host_state() -> HostOpState {
        let (render_tx, _render_rx) = CommandSender::new();
        let (host_tx, _critical_host_tx, _host_rx) = shared::host_channel::channel(1);
        let (audio_tx, _audio_rx) = shared::audio_channel::channel();

        HostOpState {
            callback_ids: Arc::new(shared::callback_id::CallbackIdAllocator::default()),
            runtime_generation: 1,
            id: 1,
            app_cache_dir: PathBuf::from("/tmp/cache"),
            app_files_dir: PathBuf::from("/tmp/files"),
            code_dir: None,
            game_paths: None,
            vfs: None,
            mount_table: None,
            render_tx,
            text_measurer: None,
            audio_tx: AudioSender::new(audio_tx, ThreadWakeup::new()),
            host_tx,
            device_services: None,
            raf_rx: None,
            raf_demand: Arc::new(shared::raf_signal::RafDemand::new()),
            request_vsync: None,
            sub_packages: Vec::new(),
            workers_path: None,
            // One host, on the request channel only: the refusals below are the policy's.
            network_policy: NetworkPolicy::from_json(r#"{"request":["allowed.example"]}"#).unwrap(),
            backgrounded: Arc::new(AtomicBool::new(false)),
            timer_backgrounded: Arc::new(AtomicBool::new(false)),
            webgl_context_created: Arc::new(AtomicBool::new(false)),
            context_lost: Arc::new(shared::op_state::ContextLostState::default()),
            code_signing_enabled: false,
            gpu_caps: GpuCaps::new(),
        }
    }

    /// Run `source`, settle its promises, then run `check` -- which throws
    /// when the answer is not what it should be.
    ///
    /// Assertions are JavaScript throws because this deno_core exposes no
    /// handle scope on `JsRuntime` to read a value back with (the same reason
    /// `prelude.rs` gives), and the answer this test is about is built by
    /// asynchronous JavaScript.
    fn run(source: &'static str, check: &'static str) {
        run_under(test_host_state(), source, check);
    }

    fn run_under(state: HostOpState, source: &'static str, check: &'static str) {
        let executor = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("current-thread executor");
        let _guard = executor.enter();
        let mut runtime = JsRuntime::new(RuntimeOptions {
            extensions: crate::main_extensions(state),
            ..Default::default()
        });
        runtime
            .execute_script("<test:fetch>", FastString::from_static(source))
            .expect("script executes");
        executor
            .block_on(runtime.run_event_loop(Default::default()))
            .expect("the event loop settles");
        let check = format!(
            "(() => {{ const r = globalThis.result; const fail = (why) => {{ \
             throw new Error(why + ': ' + JSON.stringify(r)); }}; {check} }})()"
        );
        runtime
            .execute_script("<test:fetch-check>", FastString::from(check))
            .expect("the answer is what content should have seen");
    }

    /// The whole path, in the order content takes it: `op_fetch` answers a
    /// request and the handle that aborts it, `op_fetch_send` answers the head
    /// and a body rid, `core.read` pulls the body in the chunks content asked
    /// for, and a read past the end answers zero.
    #[test]
    fn a_data_url_is_fetched_and_its_body_read_through_core() {
        let v8_executor = crate::tests::support::v8_test_executor();
        let _v8_enter = v8_executor.as_ref().map(|executor| executor.enter());
        run(
            r#"
            (async () => {
                const { op_fetch, op_fetch_send } = Deno.core.ops;
                const handles = op_fetch(
                    "request", "GET", "data:text/plain;base64,aGVsbG8gd29ybGQ=", [], null,
                    false, null, null, 30000, false, false);
                const response = await op_fetch_send(handles.requestRid);
                const chunks = [];
                const buffer = new Uint8Array(4);
                for (;;) {
                    const read = await Deno.core.read(response.responseRid, buffer);
                    if (read === 0) break;
                    chunks.push(String.fromCharCode(...buffer.subarray(0, read)));
                }
                Deno.core.close(response.responseRid);
                return {
                    status: response.status,
                    statusText: response.statusText,
                    hasCancelHandle: handles.cancelHandleRid !== null
                        && handles.cancelHandleRid !== undefined,
                    body: chunks.join(""),
                    chunkSizes: chunks.map((chunk) => chunk.length).join(","),
                };
            })().then(
                (value) => { globalThis.result = value; },
                (error) => { globalThis.result = { error: `${error.name}: ${error.message}` }; },
            );
        "#,
            r#"
            if (r === undefined) fail("the fetch never settled");
            if (r.error) fail("the fetch failed");
            if (r.status !== 200) fail("status");
            if (r.statusText !== "OK") fail("statusText");
            if (r.body !== "hello world") fail("body");
            // A `data:` URL has no connection to abort, and one read answers at
            // most what the buffer content passed holds.
            if (r.hasCancelHandle !== false) fail("a data: URL has nothing to abort");
            if (r.chunkSizes !== "4,4,3") fail("reads are bounded by the buffer");
        "#,
        );
    }

    /// The policy is the service's, and it refuses before anything is built:
    /// content sees the throw its `fetch` facade turns into a failure.
    #[test]
    fn a_host_the_policy_does_not_allow_is_refused_at_the_op() {
        let v8_executor = crate::tests::support::v8_test_executor();
        let _v8_enter = v8_executor.as_ref().map(|executor| executor.enter());
        run(
            r#"
            globalThis.result = (() => {
                try {
                    Deno.core.ops.op_fetch(
                        "request", "GET", "https://blocked.example/a", [], null,
                        false, null, null, 30000, false, false);
                    return { thrown: null };
                } catch (error) {
                    return { thrown: error.message };
                }
            })();
        "#,
            r#"
            if (typeof r.thrown !== "string") fail("the policy allowed it");
            if (!r.thrown.includes("blocked.example")
                || !r.thrown.includes("is not in the request allowlist")) {
                fail("the refusal names the host and why");
            }
        "#,
        );
    }

    /// Through the facades content calls: `request` is made on the request
    /// channel and `downloadFile` on the download channel. Each is sent to the
    /// host only the *other* channel grants, so both must be refused -- a
    /// facade that passed the wrong kind would be let through instead.
    #[test]
    fn request_and_download_file_are_each_held_to_their_own_channel() {
        let v8_executor = crate::tests::support::v8_test_executor();
        let _v8_enter = v8_executor.as_ref().map(|executor| executor.enter());
        let mut state = test_host_state();
        state.network_policy = NetworkPolicy::from_json(
            r#"{"request":["api.example.com"],"download":["dl.example.com"]}"#,
        )
        .unwrap();
        run_under(
            state,
            r#"
            globalThis.result = {};
            migo.request({
                url: "https://dl.example.com/a",
                success: () => { result.request = "sent"; },
                fail: (error) => { result.request = error.errMsg + " | " + error.exception.reasons[0].errMsg; },
            });
            migo.downloadFile({
                url: "https://api.example.com/a",
                success: () => { result.download = "sent"; },
                fail: (error) => { result.download = error.errMsg + " | " + error.exception.reasons[0].errMsg; },
            });
        "#,
            r#"
            if (!String(r.request).includes("domain 'dl.example.com' is not in the request allowlist")) {
                fail("request is held to the request channel");
            }
            if (!String(r.download).includes("domain 'api.example.com' is not in the download allowlist")) {
                fail("downloadFile is held to the download channel");
            }
        "#,
        );
    }

    /// The kind the op is given is the channel the gate holds it to: the host
    /// the request channel grants is refused to a download, and a kind that is
    /// neither is a TypeError rather than a default.
    #[test]
    fn the_kind_the_op_is_given_is_the_channel_it_is_held_to() {
        let v8_executor = crate::tests::support::v8_test_executor();
        let _v8_enter = v8_executor.as_ref().map(|executor| executor.enter());
        run(
            r#"
            globalThis.result = (() => {
                const attempt = (kind) => {
                    try {
                        Deno.core.ops.op_fetch(
                            kind, "GET", "https://allowed.example/a", [], null,
                            false, null, null, 30000, false, false);
                        return null;
                    } catch (error) {
                        return error.name + ": " + error.message;
                    }
                };
                return {
                    request: attempt("request"),
                    download: attempt("download"),
                    upload: attempt("upload"),
                };
            })();
        "#,
            r#"
            if (r.request !== null) fail("the request channel grants it: " + r.request);
            if (r.download !== "Error: downloadFile: domain 'allowed.example' is not in the download allowlist") {
                fail("a download is held to the download channel: " + r.download);
            }
            if (!String(r.upload).startsWith("TypeError: fetch kind must be")) {
                fail("an unknown kind is refused: " + r.upload);
            }
        "#,
        );
    }

    /// Aborting is a close of the cancel handle, and the send that follows it
    /// answers that it was cancelled rather than connecting.
    #[test]
    fn closing_the_cancel_handle_stops_the_send() {
        let v8_executor = crate::tests::support::v8_test_executor();
        let _v8_enter = v8_executor.as_ref().map(|executor| executor.enter());
        run(
            r#"
            (async () => {
                const { op_fetch, op_fetch_send } = Deno.core.ops;
                const handles = op_fetch(
                    "request", "GET", "https://allowed.example/slow", [], null,
                    false, null, null, 30000, false, false);
                Deno.core.close(handles.cancelHandleRid);
                try {
                    await op_fetch_send(handles.requestRid);
                    return { thrown: null };
                } catch (error) {
                    return { thrown: error.message };
                }
            })().then((value) => { globalThis.result = value; });
        "#,
            r#"
            if (r === undefined) fail("the send never settled");
            if (r.thrown !== "request was cancelled") fail("the send after an abort");
        "#,
        );
    }
}
