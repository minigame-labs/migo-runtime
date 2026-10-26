//! Self-contained V8 startup-snapshot create→restore round-trip on the HOST.
//!
//! This is a regression guard for the deno_core snapshot integration: creating a
//! snapshot from `lazy_extensions()` and restoring it must keep `Deno.core`
//! (ops + `__eventLoopTick`/`__processTimers`/rejection callbacks) intact in
//! the snapshotted heap, because deno_core's restore path re-runs
//! `store_js_callbacks` unconditionally and reads them back out.
//!
//! The historical bug: `99_main.js` did `delete globalThis.Deno` during
//! bootstrap, which is baked into the snapshot, so restore panicked with
//! `bindings.rs: "unable to convert"` ("Deno is not defined"). The fix moved
//! that hardening to runtime (`HostJsRuntime::new` -> `harden_global_scope`),
//! out of the snapshot. This test reproduces the create→restore round-trip and
//! asserts the builtins survive.
//!
//! Why a dedicated integration-test binary (not a `#[cfg(test)]` unit test):
//! `create_snapshot` puts V8 into "predictable"/snapshot mode, which is a
//! PROCESS-WIDE flag set on first init. Mixing it with the many normal-runtime
//! unit tests in the same binary (run multithreaded) causes a SIGILL. Its own
//! binary runs in its own process, so the mode is consistent.

/// Mirrors `migo-snapshot-gen` (creation) + `HostJsRuntime::new` up to and
/// including `JsRuntime::new` (restore), which is where `store_js_callbacks`
/// runs — the exact point the bug used to panic. `lazy_init_extensions` (which
/// needs per-host state) is intentionally not exercised here; it runs after the
/// failure point and is covered by the on-device flow and the webgl/font tests
/// that build a runtime via `HostJsRuntime::new`.
#[test]
fn snapshot_roundtrip_restores_deno_core() {
    // ---- CREATION SIDE (mirror snapshot-gen/src/main.rs) ----
    let output = deno_core::snapshot::create_snapshot(
        deno_core::snapshot::CreateSnapshotOptions {
            cargo_manifest_dir: env!("CARGO_MANIFEST_DIR"),
            startup_snapshot: None,
            skip_op_registration: false,
            extensions: runtime_v8::snapshot::lazy_extensions(),
            extension_transpiler: None,
            // Verify the deno_core builtins are present in the heap right before
            // serialization. If this fires, the snapshot is broken at CREATION
            // (e.g. `Deno` was deleted by a bootstrap module); if it passes but
            // restore fails below, the problem is on the RESTORE side.
            with_runtime_cb: Some(Box::new(|rt| {
                rt.execute_script(
                    "creation-check",
                    "if (typeof Deno.core.ops !== 'object') throw new Error('CREATION: Deno.core.ops is ' + typeof Deno.core.ops);\n\
                     for (const name of ['__eventLoopTick', '__processTimers', '__drainNextTickAndMacrotasks', '__handleRejections', 'runImmediateCallbacks']) { if (typeof Deno.core[name] !== 'function') throw new Error('CREATION: missing callback ' + name); }\n\
                     if (new Intl.NumberFormat('en-US').format(1234567.89) !== '1,234,567.89') throw new Error('CREATION: embedder ICU data missing');\n\
                     'ok'",
                )
                .expect("deno_core builtins must exist at snapshot creation");
            })),
        },
        None, // no warmup script
    )
    .expect("create snapshot");

    let snapshot_bytes: &'static [u8] = Box::leak(output.output);

    // create_snapshot owns its executor; the restored live isolate must bind
    // to an executor that outlives it as well.
    let executor = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let _guard = executor.enter();

    // ---- RESTORE SIDE (mirror HostJsRuntime::new) ----
    // store_js_callbacks runs inside JsRuntime::new; if the snapshot lacks
    // Deno.core.* this construction panics ("unable to convert").
    let mut rt = deno_core::JsRuntime::new(deno_core::RuntimeOptions {
        extensions: runtime_v8::snapshot::lazy_extensions(),
        startup_snapshot: Some(snapshot_bytes),
        skip_op_registration: true,
        ..Default::default()
    });

    // The snapshot must still expose `Deno.core` so deno_core could re-capture
    // its callbacks on restore.
    rt.execute_script(
        "restore-check",
        "if (typeof Deno === 'undefined') throw new Error('RESTORE: Deno missing from snapshot');\n\
         for (const name of ['__eventLoopTick', '__processTimers', '__drainNextTickAndMacrotasks', '__handleRejections', 'runImmediateCallbacks']) { if (typeof Deno.core[name] !== 'function') throw new Error('RESTORE: missing callback ' + name); }\n\
         if (new Intl.NumberFormat('de-DE').format(1234567.89) !== '1.234.567,89') throw new Error('RESTORE: embedder ICU data missing');\n\
         globalThis.__migo_ok = 1 + 2;",
    )
    .expect("execute_script after restore");

    // The game-visible-global hardening (delete Deno/__bootstrap) is applied at
    // RUNTIME, not baked into the snapshot. Verify it works post-restore — this
    // is exactly what `runtime_v8::harden_global_scope` does.
    runtime_v8::harden_global_scope(&mut rt);
    rt.execute_script(
        "harden-check",
        "if (typeof globalThis.Deno !== 'undefined') throw new Error('hardening failed');\n\
         for (const name of ['Temporal', 'Float16Array']) { if (Object.hasOwn(migo, name)) throw new Error('standard builtin leaked into restored migo: ' + name); }\n\
         if (new Float16Array([1.5])[0] !== 1.5) throw new Error('restored Float16Array support');\n\
         if (Temporal.PlainDate.from('2026-10-25').add({days:1}).toString() !== '2026-10-26') throw new Error('restored Temporal support');",
    )
    .expect("hardening after restore");

    #[cfg(feature = "profile-full")]
    let baseline = include_str!("../src/tests/published_surface_full_v0.txt");
    #[cfg(feature = "profile-slim")]
    let baseline = include_str!("../src/tests/published_surface_slim_v0.txt");
    let expected: Vec<&str> = baseline
        .split("[migo]\n")
        .nth(1)
        .unwrap()
        .lines()
        .filter(|name| !name.is_empty())
        .collect();
    let names = rt
        .execute_script(
            "restored-namespace-surface",
            "Object.getOwnPropertyNames(migo).sort()",
        )
        .unwrap();
    deno_core::scope!(scope, rt);
    let names = deno_core::v8::Local::new(scope, names);
    let actual: Vec<String> = deno_core::serde_v8::from_v8(scope, names).unwrap();
    assert_eq!(
        actual, expected,
        "source and restored snapshots must publish the same migo namespace"
    );
}
