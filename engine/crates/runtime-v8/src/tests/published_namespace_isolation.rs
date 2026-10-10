//! Content must not be able to reach the native op table.
//!
//! Every JS-level API is a policy point: the network allowlist, the file
//! sandbox, the ad reward verdict. All of it is bypassed if content can call
//! ops directly, so "no reachable op table" is the assumption the rest of the
//! sandbox is built on rather than one hardening step among many.
//!
//! It was not holding. The bootstrap file (`97_migo_namespace.js`; before the
//! engine stopped building a second, platform-compat namespace of its own, it
//! built that namespace alongside `migo` during bootstrap) built its
//! namespaces during bootstrap by copying property descriptors off
//! globalThis; `harden_global_scope` deletes deno_core's internals
//! *afterwards*, because deleting them from JS breaks deno_core's snapshot
//! restore path. A mirror built first therefore captured `Deno`, and deleting
//! `globalThis.Deno` later did nothing to the copy -- leaving the mirrored
//! namespace's `.Deno.core.ops` with 616 invocable ops, including file and
//! network ops. `__bootstrap` escaped only because its name starts with an
//! underscore, which the mirror filter happens to skip.
//!
//! These tests search the published namespaces for an op table **by shape**,
//! not by name. A future internal that leaks the same way fails here even if
//! nobody thinks to add it to an exclusion list -- which is the failure mode
//! that produced this bug.

#[cfg(test)]
mod published_namespace_isolation_tests {
    use std::{
        path::PathBuf,
        sync::{Arc, atomic::AtomicBool},
    };

    use deno_core::{FastString, JsRuntime, RuntimeOptions};
    use shared::{
        channel::ThreadWakeup,
        device::gpu_caps::GpuCaps,
        op_state::{AudioSender, HostOpState, NetworkPolicy},
        render_command_sender::CommandSender,
    };

    fn host_state() -> HostOpState {
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

    /// Boot the way a real session does: extensions, then hardening. The order
    /// is the whole point -- hardening runs after the namespaces are built.
    fn boot() -> JsRuntime {
        let mut rt = JsRuntime::new(RuntimeOptions {
            extensions: crate::main_extensions(host_state()),
            ..Default::default()
        });
        crate::harden_global_scope(&mut rt);
        rt
    }

    fn assert_js(rt: &mut JsRuntime, src: &str) {
        let wrapped = format!(
            "(()=>{{ {src}; if (!__ok) throw new Error('assertion failed: ' + __msg); }})()"
        );
        rt.execute_script("<test:isolation>", FastString::from(wrapped))
            .expect("isolation assertion script");
    }

    /// Walks every published namespace looking for the op table's shape.
    ///
    /// Deliberately shape-based. Asserting `migo.Deno === undefined` would pass
    /// the day someone exposes the same object under a different name, and
    /// that is exactly how this bug survived: the exclusion list was correct
    /// for the names on it.
    const FIND_OP_TABLE: &str = r#"
        globalThis.__findOpTable = function () {
            const roots = [
                ['globalThis', globalThis],
                ['migo', globalThis.migo],
                ['GameGlobal', globalThis.GameGlobal],
            ];
            const looksLikeOpTable = (value) => {
                if (!value || typeof value !== 'object') return false;
                let keys;
                try { keys = Object.getOwnPropertyNames(value); } catch (_) { return false; }
                // The op table is a bag of `op_*` functions. One stray property
                // named `op_x` on a game object should not trip this, so require
                // several.
                let hits = 0;
                for (const k of keys) if (k.indexOf('op_') === 0) { hits += 1; if (hits >= 3) return true; }
                return false;
            };
            const found = [];
            for (const [rootName, root] of roots) {
                if (!root || typeof root !== 'object') continue;
                let names;
                try { names = Object.getOwnPropertyNames(root); } catch (_) { continue; }
                for (const name of names) {
                    let holder;
                    try { holder = root[name]; } catch (_) { continue; }
                    if (!holder || typeof holder !== 'object') continue;
                    if (looksLikeOpTable(holder)) { found.push(rootName + '.' + name); continue; }
                    let core;
                    try { core = holder.core; } catch (_) { continue; }
                    if (!core || typeof core !== 'object') continue;
                    let ops;
                    try { ops = core.ops; } catch (_) { continue; }
                    if (looksLikeOpTable(ops)) found.push(rootName + '.' + name + '.core.ops');
                }
            }
            return found;
        };
    "#;

    /// The invariant: no published namespace reaches the op table.
    #[test]
    fn content_cannot_reach_the_op_table_through_any_published_namespace() {
        let mut rt = boot();
        rt.execute_script("<test:helper>", FastString::from_static(FIND_OP_TABLE))
            .expect("helper");
        assert_js(
            &mut rt,
            "const found = globalThis.__findOpTable(); \
             let __ok = found.length === 0; \
             let __msg = 'op table reachable via: ' + found.join(', ')",
        );
    }

    /// Anti-vacuity: the search must actually be able to find an op table.
    ///
    /// Without this, a helper that silently matches nothing -- a typo in the
    /// shape check, an exception swallowed by one of the try/catch guards --
    /// would report a clean sandbox forever.
    ///
    /// Plants on `migo`: the only namespace the engine itself ever builds
    /// (see 97_migo_namespace.js). This check's job is proving the search
    /// mechanism itself works, independent of any platform-compat namespace
    /// an external adapter might add.
    #[test]
    fn the_op_table_search_finds_a_planted_one() {
        let mut rt = boot();
        rt.execute_script("<test:helper>", FastString::from_static(FIND_OP_TABLE))
            .expect("helper");
        assert_js(
            &mut rt,
            "globalThis.migo.__planted = { core: { ops: { op_a(){}, op_b(){}, op_c(){} } } }; \
             const found = globalThis.__findOpTable(); \
             let __ok = found.indexOf('migo.__planted.core.ops') !== -1; \
             let __msg = 'planted op table not found; search reported: ' + found.join(', ')",
        );
    }

    /// The specific escape that was live, pinned by name as well.
    ///
    /// The shape test above is the durable one; this one names `Deno` so a
    /// regression reads as what it is instead of as an anonymous shape hit.
    #[test]
    fn deno_is_absent_from_the_global_and_from_the_migo_mirror() {
        let mut rt = boot();
        assert_js(
            &mut rt,
            "const where = []; \
             if (typeof globalThis.Deno !== 'undefined') where.push('globalThis'); \
             if (globalThis.migo && typeof globalThis.migo.Deno !== 'undefined') where.push('migo'); \
             let __ok = where.length === 0; \
             let __msg = 'Deno still reachable on: ' + where.join(', ')",
        );
    }

    #[test]
    fn bootstrap_is_absent_from_the_global_and_from_the_migo_mirror() {
        let mut rt = boot();
        assert_js(
            &mut rt,
            "const where = []; \
             if (typeof globalThis.__bootstrap !== 'undefined') where.push('globalThis'); \
             if (globalThis.migo && typeof globalThis.migo.__bootstrap !== 'undefined') where.push('migo'); \
             let __ok = where.length === 0; \
             let __msg = '__bootstrap still reachable on: ' + where.join(', ')",
        );
    }

    /// The mirror must keep working: this fix removes internals, not APIs.
    #[test]
    fn the_migo_namespace_still_publishes_content_apis() {
        // Profile-dependent, and asserting the Full number unconditionally
        // would assert the product profile instead of the publication. Slim
        // cfg-deletes whole capability extensions -- it published 127
        // names against Full's 300-plus when this was measured -- and
        // `getSystemInfoSync` is one of the names it removes. The floor is
        // still well above what a collapsed namespace would report, and the
        // API probed in both profiles is one neither can drop.
        #[cfg(feature = "api-connectivity")]
        let (floor, probe) = (300, "globalThis.migo.getSystemInfoSync");
        #[cfg(not(feature = "api-connectivity"))]
        let (floor, probe) = (100, "globalThis.migo.getStorageSync");

        let mut rt = boot();
        assert_js(
            &mut rt,
            &format!(
                "const migoNames = Object.getOwnPropertyNames(globalThis.migo || {{}}); \
             let __ok = migoNames.length > {floor} \
                 && typeof globalThis.migo.createCanvas === 'function' \
                 && typeof {probe} === 'function'; \
             let __msg = 'migo=' + migoNames.length"
            ),
        );
    }

    /// The gamepad API is the common mini-game platform's (getGamepads and its
    /// connection events, which follow the browser standard), so migo
    /// publishes it like every other name.
    #[test]
    fn gamepad_capabilities_are_published_on_migo() {
        let mut rt = boot();
        assert_js(
            &mut rt,
            "let __ok = typeof globalThis.migo.getGamepads === 'function'; \
             let __msg = 'migo.getGamepads=' + typeof globalThis.migo.getGamepads",
        );
    }
}
