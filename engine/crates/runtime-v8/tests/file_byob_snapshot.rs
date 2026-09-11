//! Explicit official-snapshot integration test. Kept in its own binary so
//! snapshot and source startup modes never mix in one V8 test process.
use deno_core::{JsRuntime, RuntimeOptions};
use std::{path::PathBuf, sync::Arc};

#[test]
#[ignore = "requires the freshly generated official host snapshot; run explicitly"]
fn official_snapshot_preserves_file_buffer_ownership_and_completion() {
    let snapshot =
        runtime_v8::snapshot::SNAPSHOT_BYTES.expect("official host snapshot must be embedded");
    let root = std::env::temp_dir().join(format!(
        "migo-byob-snapshot-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    for name in ["code", "user", "cache", "tmp"] {
        std::fs::create_dir_all(root.join(name)).unwrap();
    }
    let mut host = test_host_state();
    host.vfs = Some(Arc::new(shared::vfs::VirtualFS::new(
        root.join("code"),
        root.join("user"),
        root.join("cache"),
        root.join("tmp"),
    )));
    let executor = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let _guard = executor.enter();
    let mut runtime = JsRuntime::new(RuntimeOptions {
        extensions: runtime_v8::snapshot::lazy_extensions(),
        startup_snapshot: Some(snapshot),
        skip_op_registration: true,
        ..Default::default()
    });
    runtime
        .lazy_init_extensions(runtime_v8::snapshot::extension_args(host))
        .unwrap();
    runtime.execute_script("snapshot-file-scenarios", r#"
        globalThis.done = false;
        const fs = migo.getFileSystemManager();
        (async () => {
            const input = new Uint8Array([99,1,2,3,99]);
            const pending = fs.writeFile({filePath: '/user/file.bin', data: input.subarray(1,4)});
            input.fill(9);
            await pending;
            const fd = fs.openSync({filePath: '/user/file.bin', flag: 'r+'});
            try {
                const bytes = new Uint8Array(7).fill(165);
                const events = [];
                const result = await fs.read({fd, arrayBuffer: bytes.buffer, offset: 2, length: 20,
                    success(value) {
                        if (value.arrayBuffer === bytes.buffer && value.bytesRead === 3 &&
                            bytes.join(',') === '165,165,1,2,3,165,165') events.push('success');
                    }, complete() { events.push('complete'); }});
                if (result.arrayBuffer !== bytes.buffer || events.join(',') !== 'success,complete')
                    throw new Error('snapshot read commit/identity');
                const rawTarget = new Uint8Array([165,165,165,165]);
                const raw = await Deno.core.ops.op_read_fd_into(Number(fd), rawTarget, 0n);
                if (rawTarget.join(',') !== '165,165,165,165' || raw.join(',') !== '1,2,3')
                    throw new Error('snapshot raw read retained JS target');
                if (raw.byteLength !== 3 || raw.buffer.byteLength < 3 ||
                    new Uint8Array(raw.buffer).subarray(3).some(x => x !== 0))
                    throw new Error('staging capacity exposed uninitialized bytes');
                const eof = await fs.read({fd, arrayBuffer: bytes.buffer});
                if (eof.bytesRead !== 0) throw new Error('EOF');
                for (const length of [0, 4]) {
                    const target = new ArrayBuffer(length);
                    let failed = 0, completed = 0, rejected = false;
                    const read = fs.read({fd, arrayBuffer: target, position: 0,
                        fail() { ++failed; }, complete() { ++completed; }});
                    target.transfer();
                    try { await read; } catch (_) { rejected = true; }
                    if (!rejected || failed !== 1 || completed !== 1) throw new Error('detached completion');
                }
                fs.readSync({fd, arrayBuffer: new ArrayBuffer(0), position: 0});
                const source = new Uint8Array([4,5,6]);
                const write = fs.write({fd, data: source});
                source.buffer.transfer();
                await write;
                const final = new Uint8Array(3);
                await fs.read({fd, arrayBuffer: final.buffer, position: 0});
                if (final.join(',') !== '4,5,6') throw new Error('snapshot fd write input ownership');
                for (const store of [new SharedArrayBuffer(4), new ArrayBuffer(4, {maxByteLength:8})]) {
                    let rejected = false;
                    try { await Deno.core.ops.op_write_file(Number(fd), new Uint8Array(store), null, null, 0n); }
                    catch (_) { rejected = true; }
                    if (!rejected) throw new Error('unsafe backing accepted');
                }
            } finally { fs.closeSync({fd}); }
            done = true;
        })();
    "#).unwrap();
    executor
        .block_on(runtime.run_event_loop(Default::default()))
        .unwrap();
    runtime
        .execute_script(
            "snapshot-file-result",
            "if (!done) throw new Error('snapshot scenarios did not finish');",
        )
        .unwrap();
    drop(runtime);
    assert_eq!(
        std::fs::read(root.join("user/file.bin")).unwrap(),
        [4, 5, 6]
    );
    std::fs::remove_dir_all(root).unwrap();
}

fn test_host_state() -> shared::op_state::HostOpState {
    use shared::channel::ThreadWakeup;
    use shared::device::gpu_caps::GpuCaps;
    use shared::op_state::{AudioSender, HostOpState, NetworkPolicy};
    use shared::render_command_sender::CommandSender;
    use std::sync::atomic::AtomicBool;

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
        audio_tx: AudioSender::new(shared::audio_channel::disconnected(), ThreadWakeup::new()),
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
