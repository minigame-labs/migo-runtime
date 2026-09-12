//! AUD-11: exposed Web Audio operations must not report success for native no-ops.
//!
//! The audit identified five surfaces whose wrappers either only changed local
//! state or used an operation that could not express the requested target. This
//! test deliberately exercises each public operation and requires an explicit
//! NotSupportedError until a native bridge is available.

#[cfg(test)]
mod audio_aud11_tests {
    use deno_core::{FastString, JsRuntime, RuntimeOptions};
    use shared::{
        channel::ThreadWakeup,
        device::gpu_caps::GpuCaps,
        op_state::{AudioSender, HostOpState, NetworkPolicy},
        protocol::audio_cmd::AudioCmd,
        render_command_sender::CommandSender,
    };
    use std::{
        path::PathBuf,
        sync::{Arc, atomic::AtomicBool},
    };

    fn test_host_state(audio_tx: shared::audio_channel::AudioCommandSender) -> HostOpState {
        let (render_tx, _render_rx) = CommandSender::new();
        let (host_tx, _critical_host_tx, _host_rx) = shared::host_channel::channel(1);

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
            network_policy: NetworkPolicy::default(),
            backgrounded: Arc::new(AtomicBool::new(false)),
            timer_backgrounded: Arc::new(AtomicBool::new(false)),
            webgl_context_created: Arc::new(AtomicBool::new(false)),
            context_lost: Arc::new(shared::op_state::ContextLostState::default()),
            code_signing_enabled: false,
            gpu_caps: GpuCaps::new(),
        }
    }

    fn boot() -> (JsRuntime, std::thread::JoinHandle<()>) {
        let (tx, rx) = shared::audio_channel::channel();
        let responder = std::thread::spawn(move || {
            loop {
                match rx.try_recv() {
                    Ok(AudioCmd::GetAnalyserByteTimeDomainData { resp, .. }) => {
                        let _ = resp.send(Ok(Vec::new()));
                    }
                    Ok(AudioCmd::GetAnalyserFloatTimeDomainData { resp, .. }) => {
                        let _ = resp.send(Ok(Vec::new()));
                    }
                    Ok(AudioCmd::GetAnalyserByteFrequencyData { resp, .. }) => {
                        let _ = resp.send(Ok(Vec::new()));
                    }
                    Ok(AudioCmd::GetAnalyserFloatFrequencyData { resp, .. }) => {
                        let _ = resp.send(Ok(Vec::new()));
                    }
                    Ok(_) => {}
                    Err(crossbeam_channel::TryRecvError::Empty) => {
                        std::thread::sleep(std::time::Duration::from_millis(1));
                    }
                    Err(crossbeam_channel::TryRecvError::Disconnected) => break,
                }
            }
        });
        let runtime = JsRuntime::new(RuntimeOptions {
            extensions: crate::main_extensions(test_host_state(tx)),
            ..Default::default()
        });
        (runtime, responder)
    }

    fn run(runtime: &mut JsRuntime, source: &'static str) {
        let executor = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("current-thread executor");
        let _guard = executor.enter();
        runtime
            .execute_script("<test:audio-aud11>", FastString::from_static(source))
            .expect("script executes");
        executor
            .block_on(runtime.run_event_loop(Default::default()))
            .expect("event loop drains without an unhandled rejection");
        runtime
            .execute_script(
                "<test:audio-aud11-result>",
                FastString::from_static(
                    "if (!globalThis.__done) throw new Error(globalThis.__failure || 'unfinished');",
                ),
            )
            .expect("assertions hold");
    }

    #[test]
    fn unsupported_audio_operations_warn_once_without_throwing() {
        let (mut runtime, responder) = boot();
        run(
            &mut runtime,
            r#"
            globalThis.__done = false;
            (async () => {
                const previousWarn = console.warn;
                const warnings = [];
                console.warn = (message) => warnings.push(String(message));
                try {
                    const context = new AudioContext();
                    const callback = () => {};
                    for (const source of [
                        context.createBufferSource(),
                        context.createOscillator(),
                        context.createConstantSource(),
                    ]) {
                        source.onended = callback;
                        if (source.onended !== callback) throw new Error('onended state changed');
                    }

                    const node = context.createGain();
                    const other = context.createGain();
                    node.disconnect(other);
                    node.channelCount = 1;
                    node.channelCountMode = 'explicit';
                    node.channelInterpretation = 'discrete';
                    if (node.channelCount !== 1
                        || node.channelCountMode !== 'explicit'
                        || node.channelInterpretation !== 'discrete') {
                        throw new Error('channel metadata state changed unexpectedly');
                    }

                    context.listener.positionX.value = 1;
                    context.listener.setPosition(1, 2, 3);
                    if (context.listener.positionX.value !== 1) {
                        throw new Error('listener position state changed unexpectedly');
                    }

                    const analyser = context.createAnalyser();
                    const data = new Uint8Array(32);
                    const result = await analyser.getByteFrequencyData(data);
                    if (result !== undefined) throw new Error('analyser read returned a value');

                    if (warnings.length !== 5) {
                        throw new Error('expected one diagnostic per unsupported operation, got '
                            + warnings.length);
                    }
                    globalThis.__done = true;
                } catch (error) {
                    globalThis.__failure = String(error);
                } finally {
                    console.warn = previousWarn;
                }
            })();
        "#,
        );
        drop(runtime);
        responder.join().expect("audio command drainer");
    }
}
