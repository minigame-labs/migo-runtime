//! Regression tests for `createDeferredApi` (V07 + RT-01).
//!
//! Every test loads the *production* `02_async.js` source (ES-module
//! import/export syntax stripped) into a bare `JsRuntime` alongside a small
//! mock preamble.  That preamble replaces the two imports the file needs
//! (`op_alloc_host_callback_id`, `_perf`) and provides configurable stubs for
//! `setTimeout`/`clearTimeout`/`performance` so the rollback paths can be
//! exercised without a full runtime.
//!
//! The pattern follows `network/tcp_socket.rs`
//! (`fnet01_connect_generation_discards_stale_result_and_closes_orphan`): inject
//! mock globals, include the real source, assert observable state.
//!
//! ## Why these tests are RED before the V07 + RT-01 fix
//!
//! * Every assertion uses `api.pendingCount()`, which does not exist on the
//!   object returned by `createDeferredApi` before the fix.
//! * `v07_timer_full_rolls_back_and_calls_fail_complete` additionally requires
//!   that `fail` / `complete` are invoked when `setTimeout` throws — a code path
//!   that currently does not execute them.

#[cfg(test)]
mod deferred_api_tests {
    use deno_core::{FastString, JsRuntime, RuntimeOptions};

    // ---------------------------------------------------------------------------
    // Infrastructure
    // ---------------------------------------------------------------------------

    /// Mock globals injected before the module source.
    ///
    /// Replaces the two ES-module imports and provides configurable timer stubs.
    /// Each test gets a fresh `JsRuntime`, so these globals are reset per test.
    const PREAMBLE: &str = r#"
        // Mock: op_alloc_host_callback_id (from "ext:core/ops")
        var _idCounter = 0;
        function op_alloc_host_callback_id() { return ++_idCounter; }

        // Mock: _perf (from "ext:host_v8_base/05_perf.js")
        var _perf = { enabled: false, deferredMs: 1000, syncMs: 1000, asyncMs: 1000 };

        // Mock: performance (Web API, absent in bare JsRuntime)
        var performance = { now: function() { return 0.0; } };

        // Provide console if the bare runtime does not have it.
        if (typeof console === 'undefined') {
            var console = {
                warn: function() {},
                error: function() {},
                log: function() {}
            };
        }

        // Configurable setTimeout / clearTimeout stubs.
        //
        // _timerThrows: set to true to simulate the 1024-live-timer RangeError.
        // _timerFns:    id → { fn, ms } for manual timer triggering.
        var _timerThrows = false;
        var _timerFns = {};
        var _timerNextId = 1;
        var _timerCount = 0;
        var _clearCount = 0;

        function setTimeout(fn, ms) {
            if (_timerThrows) {
                throw new RangeError("Too many live timers (limit: 1024)");
            }
            var id = _timerNextId++;
            _timerFns[id] = { fn: fn, ms: ms };
            _timerCount++;
            return id;
        }

        function clearTimeout(id) {
            if (id && _timerFns[id]) {
                delete _timerFns[id];
                _clearCount++;
            }
        }
    "#;

    /// Strip ES-module import and export declarations so the source runs as a
    /// plain script in a bare `JsRuntime`.
    fn module_source() -> String {
        let raw = include_str!("../base/02_async.js");
        // Drop "import …" lines — the PREAMBLE provides the replacements.
        let no_imports: String = raw
            .lines()
            .filter(|l| !l.trim_start().starts_with("import "))
            .collect::<Vec<_>>()
            .join("\n");
        // Drop the trailing "export { … }" block.
        no_imports
            .rfind("\nexport {")
            .map(|i| no_imports[..i].to_owned())
            .unwrap_or(no_imports)
    }

    /// Execute a JS snippet in the runtime, panicking with the label on error.
    fn exec(rt: &mut JsRuntime, label: &'static str, src: &str) {
        rt.execute_script(label, FastString::from(src.to_owned()))
            .unwrap_or_else(|e| panic!("{label}: {e}"));
    }

    /// Drain all pending microtasks and async work.
    fn drain(rt: &mut JsRuntime) {
        let ex = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime for drain");
        ex.block_on(rt.run_event_loop(Default::default()))
            .expect("event loop must complete");
    }

    /// Create a bare runtime pre-loaded with the mock preamble and the
    /// production `02_async.js` source.
    fn boot() -> JsRuntime {
        let src = format!("{}\n{}", PREAMBLE, module_source());
        let mut rt = JsRuntime::new(RuntimeOptions::default());
        exec(&mut rt, "<deferred_api_boot>", &src);
        rt
    }

    // ---------------------------------------------------------------------------
    // Tests
    // ---------------------------------------------------------------------------

    /// V07 (regression): when `setTimeout` throws — e.g. because the 1024-live-
    /// timer cap is exhausted — the pending entry must be rolled back, and
    /// `fail` / `complete` must be called exactly once.
    ///
    /// Before the fix: `setTimeout` is called outside the executor try/catch, so
    /// the RangeError escapes the Promise constructor without touching
    /// `fail`/`complete` and the pending entry leaks.
    #[test]
    fn v07_timer_full_rolls_back_and_calls_fail_complete() {
        let mut rt = boot();
        exec(
            &mut rt,
            "<v07_setup>",
            r#"
            _timerThrows = true;
            var _failCount = 0, _completeCount = 0, _execCount = 0;
            var api = createDeferredApi('testApi', 30000);
            api.invoke({
                fail:     function() { _failCount++; },
                complete: function() { _completeCount++; }
            }, function(opts, id) {
                _execCount++;
            }).catch(function() {});
            "#,
        );
        drain(&mut rt);
        exec(
            &mut rt,
            "<v07_assert>",
            r#"
            if (_failCount !== 1)
                throw new Error("fail count: " + _failCount + " (expected 1)");
            if (_completeCount !== 1)
                throw new Error("complete count: " + _completeCount + " (expected 1)");
            if (_execCount !== 0)
                throw new Error("executor was called: " + _execCount + " (expected 0)");
            if (api.pendingCount() !== 0)
                throw new Error("pending leaked: " + api.pendingCount() + " (expected 0)");
            "#,
        );
    }

    /// Executor throwing must roll back the pending entry, clear the timer,
    /// and call `fail` / `complete` exactly once.
    ///
    /// Before the fix: executor rollback exists but `api.pendingCount()` does not.
    #[test]
    fn executor_throw_rolls_back_pending_entry() {
        let mut rt = boot();
        exec(
            &mut rt,
            "<exec_throw_setup>",
            r#"
            _timerThrows = false;
            var _failCount = 0, _completeCount = 0;
            var api = createDeferredApi('testApi', 30000);
            api.invoke({
                fail:     function() { _failCount++; },
                complete: function() { _completeCount++; }
            }, function(opts, id) {
                throw new Error("executor exploded");
            }).catch(function() {});
            "#,
        );
        drain(&mut rt);
        exec(
            &mut rt,
            "<exec_throw_assert>",
            r#"
            if (_failCount !== 1)
                throw new Error("fail count: " + _failCount + " (expected 1)");
            if (_completeCount !== 1)
                throw new Error("complete count: " + _completeCount + " (expected 1)");
            if (api.pendingCount() !== 0)
                throw new Error("pending leaked: " + api.pendingCount() + " (expected 0)");
            "#,
        );
    }

    /// A timeout that fires must settle the JS side exactly once.
    ///
    /// Before the fix: correct behaviour, but `api.pendingCount()` does not exist.
    #[test]
    fn timeout_settles_exactly_once_and_empties_pending() {
        let mut rt = boot();
        exec(
            &mut rt,
            "<timeout_setup>",
            r#"
            _timerThrows = false;
            var _failCount = 0, _completeCount = 0;
            var _lastTimerId = null;

            // Augment setTimeout to capture the last issued id so we can fire
            // the callback manually.
            function setTimeout(fn, ms) {
                var id = _timerNextId++;
                _timerFns[id] = { fn: fn, ms: ms };
                _timerCount++;
                _lastTimerId = id;
                return id;
            }

            var api = createDeferredApi('testApi', 0);
            api.invoke({
                _timeout: 5000,
                fail:     function() { _failCount++; },
                complete: function() { _completeCount++; }
            }, function(opts, id) { /* no-op */ }).catch(function() {});

            // Manually trigger the captured timer to simulate timeout expiry.
            if (_lastTimerId !== null) { _timerFns[_lastTimerId].fn(); }
            "#,
        );
        drain(&mut rt);
        exec(
            &mut rt,
            "<timeout_assert>",
            r#"
            if (_failCount !== 1)
                throw new Error("fail count: " + _failCount + " (expected 1)");
            if (_completeCount !== 1)
                throw new Error("complete count: " + _completeCount + " (expected 1)");
            if (api.pendingCount() !== 0)
                throw new Error("pending leaked: " + api.pendingCount() + " (expected 0)");
            "#,
        );
    }

    /// A normal platform result settling the promise must call `success` and
    /// `complete` exactly once, leave no pending entry, and ignore a second
    /// settle for the same `requestId`.
    ///
    /// Before the fix: correct behaviour, but `api.pendingCount()` does not exist.
    #[test]
    fn normal_completion_settles_once() {
        let mut rt = boot();
        exec(
            &mut rt,
            "<normal_setup>",
            r#"
            _timerThrows = false;
            var _successCount = 0, _completeCount = 0;
            var capturedId = null;
            var api = createDeferredApi('testApi', 0);

            // timeout = 0 so no timer is registered; platform settles manually.
            api.invoke({
                success:  function() { _successCount++; },
                complete: function() { _completeCount++; }
            }, function(opts, id) {
                capturedId = id;
            }).catch(function() {});
            "#,
        );
        exec(
            &mut rt,
            "<normal_settle>",
            r#"
            api.settle(JSON.stringify({ requestId: capturedId, value: 42 }));
            "#,
        );
        drain(&mut rt);
        exec(
            &mut rt,
            "<normal_assert>",
            r#"
            if (_successCount !== 1)
                throw new Error("success count: " + _successCount + " (expected 1)");
            if (_completeCount !== 1)
                throw new Error("complete count: " + _completeCount + " (expected 1)");
            if (api.pendingCount() !== 0)
                throw new Error("pending leaked: " + api.pendingCount() + " (expected 0)");
            "#,
        );
    }

    /// A platform result arriving after the JS timeout has already fired must
    /// be silently discarded: `success` must never be called, and the counts
    /// for `fail` and `complete` must remain exactly 1.
    ///
    /// Before the fix: correct discard behaviour, but `api.pendingCount()` absent.
    #[test]
    fn late_callback_after_timeout_is_discarded() {
        let mut rt = boot();
        exec(
            &mut rt,
            "<late_setup>",
            r#"
            _timerThrows = false;
            var _failCount = 0, _successCount = 0, _completeCount = 0;
            var capturedTimerId = null, capturedId = null;

            function setTimeout(fn, ms) {
                var id = _timerNextId++;
                _timerFns[id] = { fn: fn, ms: ms };
                _timerCount++;
                capturedTimerId = id;
                return id;
            }

            var api = createDeferredApi('testApi', 0);
            api.invoke({
                _timeout:  5000,
                fail:      function() { _failCount++; },
                success:   function() { _successCount++; },
                complete:  function() { _completeCount++; }
            }, function(opts, id) {
                capturedId = id;
            }).catch(function() {});

            // JS-side timeout fires first.
            if (capturedTimerId !== null) { _timerFns[capturedTimerId].fn(); }

            // Late platform result arrives for the same requestId — must be ignored.
            api.settle(JSON.stringify({ requestId: capturedId, value: 99 }));
            "#,
        );
        drain(&mut rt);
        exec(
            &mut rt,
            "<late_assert>",
            r#"
            if (_failCount !== 1)
                throw new Error("fail count: " + _failCount + " (expected 1)");
            if (_completeCount !== 1)
                throw new Error("complete count: " + _completeCount + " (expected 1)");
            if (_successCount !== 0)
                throw new Error("late success not discarded: " + _successCount + " (expected 0)");
            if (api.pendingCount() !== 0)
                throw new Error("pending leaked: " + api.pendingCount() + " (expected 0)");
            "#,
        );
    }

    /// Explicit cancellation uses the same once-only settlement path as
    /// timeout: it removes the entry, clears the timer, and ignores a later
    /// completion for that request id.
    #[test]
    fn cancel_settles_once_and_discards_late_callback() {
        let mut rt = boot();
        exec(
            &mut rt,
            "<cancel_setup>",
            r#"
            var _failCount = 0, _completeCount = 0, _successCount = 0;
            var capturedId = null;
            var api = createDeferredApi('testApi', 0);
            api.invoke({
                _timeout: 5000,
                fail: function() { _failCount++; },
                success: function() { _successCount++; },
                complete: function() { _completeCount++; }
            }, function(opts, id) {
                capturedId = id;
            }).catch(function() {});
            if (!api.cancel(capturedId, 'cancelled')) throw new Error('cancel did not find request');
            if (api.cancel(capturedId, 'cancelled again')) throw new Error('cancel settled twice');
            api.settle(JSON.stringify({ requestId: capturedId }));
            "#,
        );
        drain(&mut rt);
        exec(
            &mut rt,
            "<cancel_assert>",
            r#"
            if (_failCount !== 1) throw new Error("fail count: " + _failCount);
            if (_completeCount !== 1) throw new Error("complete count: " + _completeCount);
            if (_successCount !== 0) throw new Error("late success: " + _successCount);
            if (api.pendingCount() !== 0) throw new Error("pending leaked");
            "#,
        );
    }

    /// When `_pending` reaches `_MAX_PENDING` (256) the next `invoke` must reject
    /// immediately without allocating an id, without adding an entry to the map,
    /// and without calling the executor.
    ///
    /// Before the fix: `_MAX_PENDING` and the cap check do not exist.
    #[test]
    fn max_pending_cap_rejects_without_adding_to_pending() {
        let mut rt = boot();
        // Fill to 256 with timeout disabled so no timers are consumed.
        exec(
            &mut rt,
            "<cap_fill>",
            r#"
            _timerThrows = false;
            var api = createDeferredApi('testApi', 0);
            var otherApi = createDeferredApi('otherApi', 0);
            for (var i = 0; i < 256; i++) {
                api.invoke({}, function(opts, id) { /* no-op */ }).catch(function() {});
            }
            "#,
        );
        exec(
            &mut rt,
            "<cap_overflow>",
            r#"
            var _overflowFail = 0, _overflowComplete = 0, _overflowExec = 0;
            otherApi.invoke({
                fail:     function() { _overflowFail++; },
                complete: function() { _overflowComplete++; }
            }, function(opts, id) {
                _overflowExec++;
            }).catch(function() {});
            "#,
        );
        drain(&mut rt);
        exec(
            &mut rt,
            "<cap_assert>",
            r#"
            if (_overflowFail !== 1)
                throw new Error("overflow fail count: " + _overflowFail + " (expected 1)");
            if (_overflowComplete !== 1)
                throw new Error("overflow complete count: " + _overflowComplete + " (expected 1)");
            if (_overflowExec !== 0)
                throw new Error("overflow executor called: " + _overflowExec + " (expected 0)");
            // The map must have exactly 256 entries — the overflow must not add a 257th.
            if (api.pendingCount() !== 256)
                throw new Error("pending count: " + api.pendingCount() + " (expected 256)");
            if (otherApi.pendingCount() !== 0)
                throw new Error("other pending count: " + otherApi.pendingCount());
            "#,
        );
    }
}
