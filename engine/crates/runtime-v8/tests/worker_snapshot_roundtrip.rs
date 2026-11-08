//! Dedicated-process create/restore guard for the Worker startup snapshot.
//!
//! V8 snapshot mode is process-wide, so this must not share the normal unit
//! test binary. The generated heap must contain deno_core restore callbacks and
//! the deferred Worker pump hook, but no pending receive op.

#![cfg(feature = "api-system")]

#[test]
fn worker_snapshot_roundtrip_keeps_an_unstarted_bootstrap() {
    runtime_v8::install_icu_data();
    runtime_v8::snapshot::prepare_snapshot_creation();
    let output = deno_core::snapshot::create_snapshot(
        deno_core::snapshot::CreateSnapshotOptions {
            cargo_manifest_dir: env!("CARGO_MANIFEST_DIR"),
            startup_snapshot: None,
            skip_op_registration: false,
            extensions: runtime_v8::snapshot::worker_lazy_extensions(),
            extension_transpiler: None,
            with_runtime_cb: Some(Box::new(|rt| {
                rt.execute_script(
                    "worker-snapshot-creation-check",
                    "for (const name of ['__eventLoopTick', '__processTimers', '__drainNextTickAndMacrotasks', '__handleRejections', 'runImmediateCallbacks']) { if (typeof Deno.core[name] !== 'function') throw new Error('Deno.core callback missing: ' + name); }\n\
                     if (typeof globalThis.__migoStartWorkerMessagePump !== 'function') throw new Error('deferred Worker pump hook missing');\n\
                     if (new Intl.NumberFormat('en-US').format(1234567.89) !== '1,234,567.89') throw new Error('Worker embedder ICU data missing');",
                )
                .expect("Worker bootstrap must be serializable before runtime state exists");
                let mut context =
                    std::task::Context::from_waker(std::task::Waker::noop());
                assert!(
                    matches!(
                        rt.poll_event_loop(
                            &mut context,
                            deno_core::PollEventLoopOptions::default()
                        ),
                        std::task::Poll::Ready(Ok(()))
                    ),
                    "snapshot generation must not capture a pending Worker receive"
                );
            })),
        },
        None,
    )
    .expect("create Worker snapshot");

    let snapshot_bytes: &'static [u8] = Box::leak(output.output);
    let executor = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let _guard = executor.enter();
    let mut rt = deno_core::JsRuntime::new(deno_core::RuntimeOptions {
        extensions: runtime_v8::snapshot::worker_lazy_extensions(),
        startup_snapshot: Some(snapshot_bytes),
        skip_op_registration: true,
        ..Default::default()
    });

    rt.execute_script(
        "worker-snapshot-restore-check",
        "for (const name of ['__eventLoopTick', '__processTimers', '__drainNextTickAndMacrotasks', '__handleRejections', 'runImmediateCallbacks']) { if (typeof Deno.core[name] !== 'function') throw new Error('restored Deno.core callback missing: ' + name); }\n\
         if (typeof globalThis.__migoStartWorkerMessagePump !== 'function') throw new Error('restored Worker pump hook missing');\n\
         if (new Intl.NumberFormat('de-DE').format(1234567.89) !== '1.234.567,89') throw new Error('RESTORE: Worker embedder ICU data missing');\n\
         delete globalThis.__migoStartWorkerMessagePump;\n\
         delete globalThis.Deno;\n\
         delete globalThis.__bootstrap;\n\
         if ('__migoStartWorkerMessagePump' in globalThis || 'Deno' in globalThis || '__bootstrap' in globalThis) throw new Error('Worker hardening failed');",
    )
    .expect("restored Worker heap can be hardened before game code");
    let mut context = std::task::Context::from_waker(std::task::Waker::noop());
    assert!(
        matches!(
            rt.poll_event_loop(&mut context, deno_core::PollEventLoopOptions::default()),
            std::task::Poll::Ready(Ok(()))
        ),
        "restoring an unstarted Worker must not materialize a pending receive"
    );
}

#[test]
fn independent_host_and_worker_snapshots_coexist() {
    const NAME: &str = "independent_host_and_worker_snapshots_coexist";
    const ROLE: &str = "MIGO_SNAPSHOT_COEXIST_ROLE";
    const DIRECTORY: &str = "MIGO_SNAPSHOT_COEXIST_DIRECTORY";
    if let Ok(role) = std::env::var(ROLE) {
        let directory = std::path::PathBuf::from(std::env::var_os(DIRECTORY).unwrap());
        runtime_v8::install_icu_data();
        if role == "host" || role == "worker" {
            // Mirror the generator; each image is made in its own process.
            runtime_v8::snapshot::prepare_snapshot_creation();
            let extensions = if role == "host" {
                runtime_v8::snapshot::lazy_extensions()
            } else {
                runtime_v8::snapshot::worker_lazy_extensions()
            };
            let output = deno_core::snapshot::create_snapshot(
                deno_core::snapshot::CreateSnapshotOptions {
                    cargo_manifest_dir: env!("CARGO_MANIFEST_DIR"),
                    startup_snapshot: None,
                    skip_op_registration: false,
                    extensions,
                    extension_transpiler: None,
                    with_runtime_cb: None,
                },
                None,
            )
            .unwrap();
            std::fs::write(directory.join(format!("{role}.bin")), output.output).unwrap();
            return;
        }
        assert_eq!(role, "consume");
        let host_bytes: &'static [u8] = Box::leak(
            std::fs::read(directory.join("host.bin"))
                .unwrap()
                .into_boxed_slice(),
        );
        let worker_bytes: &'static [u8] = Box::leak(
            std::fs::read(directory.join("worker.bin"))
                .unwrap()
                .into_boxed_slice(),
        );
        let executor = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let _guard = executor.enter();
        let mut host = deno_core::JsRuntime::new(deno_core::RuntimeOptions {
            extensions: runtime_v8::snapshot::lazy_extensions(),
            startup_snapshot: Some(host_bytes),
            skip_op_registration: true,
            ..Default::default()
        });
        for snapshot in [None, Some(worker_bytes)] {
            eprintln!(
                "coexist: source isolate={} worker snapshot={}",
                snapshot.is_none(),
                snapshot.is_some()
            );
            // The Host remains alive on its own thread throughout both forms.
            std::thread::spawn(move || {
                let executor = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
                let _guard = executor.enter();
                let mut worker = deno_core::JsRuntime::new(deno_core::RuntimeOptions {
                    extensions: if snapshot.is_some() {
                        runtime_v8::snapshot::worker_lazy_extensions()
                    } else {
                        Vec::new()
                    },
                    startup_snapshot: snapshot,
                    skip_op_registration: snapshot.is_some(),
                    ..Default::default()
                });
                worker.execute_script("coexist-worker", "if (new Intl.NumberFormat('en-US').format(1234567.89) !== '1,234,567.89') throw new Error('worker ICU'); if (typeof Deno.core.__eventLoopTick !== 'function') throw new Error('worker callbacks');").unwrap();
                if snapshot.is_some() {
                    worker.execute_script("coexist-worker-bootstrap", "if (typeof __migoStartWorkerMessagePump !== 'function') throw new Error('worker pump hook');").unwrap();
                }
                let mut context = std::task::Context::from_waker(std::task::Waker::noop());
                assert!(matches!(worker.poll_event_loop(&mut context, deno_core::PollEventLoopOptions::default()), std::task::Poll::Ready(Ok(()))));
            }).join().unwrap();
            host.execute_script("coexist-host", "if (new Intl.NumberFormat('de-DE').format(1234567.89) !== '1.234.567,89') throw new Error('host ICU'); if (typeof Deno.core.__eventLoopTick !== 'function') throw new Error('host callbacks');").unwrap();
        }
        return;
    }
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let directory = std::env::temp_dir().join(format!(
        "migo-snapshot-coexist-{}-{unique}",
        std::process::id()
    ));
    std::fs::create_dir(&directory).unwrap();
    eprintln!("coexist fixtures: {}", directory.display());
    for role in ["host", "worker", "consume"] {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", NAME, "--nocapture"])
            .env(ROLE, role)
            .env(DIRECTORY, &directory)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{role} failed: {:?}\n{}\n{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    std::fs::remove_dir_all(directory).unwrap();
}
