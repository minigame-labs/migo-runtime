//! Full-runtime tests for the shared network binary helper `00_binary.js`
//! (`toExactArrayBuffer`), plus source/extension guards proving the three
//! receive loops route through it and no receive-side redundant Uint8Array
//! constructor remains.

#[cfg(test)]
mod binary_helper_tests {
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;

    use deno_core::{FastString, JsRuntime, RuntimeOptions};
    use shared::channel::ThreadWakeup;
    use shared::device::gpu_caps::GpuCaps;
    use shared::render_command_sender::CommandSender;
    use shared::{
        network_policy::NetworkPolicy,
        op_state::{AudioSender, HostOpState},
    };

    // Test-only bridge: expose the real `toExactArrayBuffer` export on
    // globalThis so synchronous `execute_script` assertions can exercise it in
    // a live runtime with the real network extension loaded.
    deno_core::extension!(
        binary_test_bridge,
        deps = [host_v8_network],
        esm_entry_point = "ext:binary_test_bridge/bridge.js",
        esm = ["ext:binary_test_bridge/bridge.js" = {
            source = r#"
                import { toExactArrayBuffer } from "ext:host_v8_network/00_binary.js";
                globalThis.__toExactArrayBuffer = toExactArrayBuffer;
            "#
        },],
    );

    fn test_host_state() -> HostOpState {
        let (render_tx, _render_rx) = CommandSender::new();
        let (host_tx, _critical_host_tx, _host_rx) = shared::host_channel::channel(1);

        HostOpState {
            callback_ids: std::sync::Arc::new(shared::callback_id::CallbackIdAllocator::default()),
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
            audio_tx: AudioSender::new(shared::audio_channel::disconnected(), ThreadWakeup::new()),
            host_tx,
            device_services: None,
            raf_rx: None,
            raf_demand: std::sync::Arc::new(shared::raf_signal::RafDemand::new()),
            request_vsync: None,
            sub_packages: Vec::new(),
            workers_path: None,
            network_policy: NetworkPolicy::default(),
            backgrounded: Arc::new(AtomicBool::new(false)),
            timer_backgrounded: Arc::new(AtomicBool::new(false)),
            webgl_context_created: Arc::new(AtomicBool::new(false)),
            context_lost: Arc::new(shared::op_state::ContextLostState::default()),
            code_signing_enabled: false,
            gpu_caps: GpuCaps::new(),
        }
    }

    fn boot_runtime_with_binary_helper() -> JsRuntime {
        let mut extensions = crate::main_extensions(test_host_state());
        extensions.push(binary_test_bridge::init());
        let mut rt = JsRuntime::new(RuntimeOptions {
            extensions,
            ..Default::default()
        });
        crate::harden_global_scope(&mut rt);
        rt
    }

    fn exec(rt: &mut JsRuntime, source: impl Into<String>) {
        rt.execute_script("<test:binary>", FastString::from(source.into()))
            .expect("binary helper script");
    }

    fn assert_js(rt: &mut JsRuntime, expression: &str) {
        exec(
            rt,
            format!(
                "if (!({expression})) throw new Error('binary helper assertion failed: ' + `{expression}`);"
            ),
        );
    }

    #[test]
    fn full_window_typed_view_returns_backing_buffer_by_identity() {
        let mut rt = boot_runtime_with_binary_helper();
        // Every ToJsBuffer arrives as a full-window Uint8Array (offset 0,
        // length == backing length) → the backing ArrayBuffer is handed back
        // by identity, with no copy.
        assert_js(
            &mut rt,
            r#"(() => {
                const u8 = new Uint8Array([1, 2, 3, 4]);
                const out = globalThis.__toExactArrayBuffer(u8);
                return out === u8.buffer && out.byteLength === 4;
            })()"#,
        );
    }

    #[test]
    fn zero_length_full_view_returns_backing_by_identity() {
        let mut rt = boot_runtime_with_binary_helper();
        assert_js(
            &mut rt,
            r#"(() => {
                const u8 = new Uint8Array(0);
                const out = globalThis.__toExactArrayBuffer(u8);
                return out === u8.buffer && out.byteLength === 0;
            })()"#,
        );
    }

    #[test]
    fn partial_window_typed_view_returns_exact_slice_copy() {
        let mut rt = boot_runtime_with_binary_helper();
        // A sub-window view (offset 1, length 3 of a 6-byte buffer) is NOT the
        // full backing, so the helper returns an exact 3-byte slice — a new
        // buffer, not the shared 6-byte one.
        assert_js(
            &mut rt,
            r#"(() => {
                const backing = new Uint8Array([10, 11, 12, 13, 14, 15]).buffer;
                const view = new Uint8Array(backing, 1, 3);
                const out = globalThis.__toExactArrayBuffer(view);
                const bytes = new Uint8Array(out);
                return out !== backing
                    && out.byteLength === 3
                    && bytes[0] === 11 && bytes[1] === 12 && bytes[2] === 13;
            })()"#,
        );
    }

    #[test]
    fn shadowed_typed_array_metadata_cannot_redirect_result() {
        let mut rt = boot_runtime_with_binary_helper();
        // Own-property shadows on .buffer/.byteOffset/.byteLength must be
        // ignored: the helper reads V8 internal slots via primordials, so the
        // result is the real full-window 3-byte backing with exact bytes.
        assert_js(
            &mut rt,
            r#"(() => {
                const src = new Uint8Array([7, 8, 9]);
                Object.defineProperty(src, "buffer", { value: new ArrayBuffer(999) });
                Object.defineProperty(src, "byteOffset", { value: 5 });
                Object.defineProperty(src, "byteLength", { value: 1 });
                const out = globalThis.__toExactArrayBuffer(src);
                const bytes = new Uint8Array(out);
                return out.byteLength === 3
                    && bytes[0] === 7 && bytes[1] === 8 && bytes[2] === 9;
            })()"#,
        );
    }

    #[test]
    fn legacy_numeric_array_is_converted_to_exact_backing() {
        let mut rt = boot_runtime_with_binary_helper();
        // Backward compatibility with an un-regenerated snapshot whose event
        // shape is still a numeric Array: convert through primordial Uint8Array.
        assert_js(
            &mut rt,
            r#"(() => {
                const out = globalThis.__toExactArrayBuffer([21, 22, 23]);
                const bytes = new Uint8Array(out);
                return (out instanceof ArrayBuffer)
                    && out.byteLength === 3
                    && bytes[0] === 21 && bytes[1] === 22 && bytes[2] === 23;
            })()"#,
        );
    }

    // ── Source / extension guards ──

    const BINARY_JS: &str = include_str!("../network/00_binary.js");
    const WS_JS: &str = include_str!("../network/07_websocket.js");
    const TCP_JS: &str = include_str!("../network/08_tcp_socket.js");
    const UDP_JS: &str = include_str!("../network/09_udp_socket.js");
    const NETWORK_MOD_RS: &str = include_str!("../network/mod.rs");

    const HELPER_IMPORT: &str =
        r#"import { toExactArrayBuffer } from "ext:host_v8_network/00_binary.js";"#;

    #[test]
    fn helper_module_exports_the_function() {
        assert!(
            BINARY_JS.contains("export function toExactArrayBuffer"),
            "00_binary.js must export toExactArrayBuffer"
        );
    }

    #[test]
    fn mod_registers_binary_helper_first() {
        let binary = NETWORK_MOD_RS
            .find("\"00_binary.js\"")
            .expect("mod.rs must register 00_binary.js");
        let header = NETWORK_MOD_RS
            .find("\"01_header.js\"")
            .expect("mod.rs must still register 01_header.js");
        assert!(
            binary < header,
            "00_binary.js must be registered before 01_header.js"
        );
    }

    #[test]
    fn all_three_receive_loops_import_and_call_the_helper() {
        for (name, src, call) in [
            (
                "07_websocket.js",
                WS_JS,
                "toExactArrayBuffer(event.dataBin)",
            ),
            ("08_tcp_socket.js", TCP_JS, "toExactArrayBuffer(event.data)"),
            ("09_udp_socket.js", UDP_JS, "toExactArrayBuffer(event.data)"),
        ] {
            assert!(
                src.contains(HELPER_IMPORT),
                "{name} must import toExactArrayBuffer from the shared helper"
            );
            assert!(
                src.contains(call),
                "{name} must call {call} in its receive branch"
            );
        }
    }

    #[test]
    fn no_receive_side_redundant_uint8array_constructor_remains() {
        // The receive-side redundant copy (`new Uint8Array(event.*).buffer`)
        // must be gone from all three modules; send-side constructors (built
        // from user input, never from `event.data`) are untouched.
        assert!(
            !WS_JS.contains("new Uint8Array(event.dataBin).buffer"),
            "07_websocket.js still builds a redundant receive-side Uint8Array"
        );
        assert!(
            !TCP_JS.contains("new Uint8Array(event.data).buffer"),
            "08_tcp_socket.js still builds a redundant receive-side Uint8Array"
        );
        assert!(
            !UDP_JS.contains("new Uint8Array(event.data).buffer"),
            "09_udp_socket.js still builds a redundant receive-side Uint8Array"
        );
    }
}
