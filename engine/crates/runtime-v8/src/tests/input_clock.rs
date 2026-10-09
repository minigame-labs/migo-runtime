//! The time on an input event is on the clock content reads.
//!
//! A host stamps input in its own clock -- the system uptime on Apple and Android -- and the C ABI names none. Content
//! compares `event.timeStamp` with `performance.now()` and with the requestAnimationFrame timestamp, so a host's epoch
//! reaching it makes every such comparison wrong on exactly the platforms whose clock disagrees. Found by
//! the conformance suite's `input-touch-spec` on macOS: a touch arrived stamped 62 067 560 ms ahead of `performance.now()`.
//!
//! These run the real input modules through the host bridge, as a host's events do.

#[cfg(test)]
mod input_clock_tests {
    use std::time::Duration;

    use deno_core::{FastString, JsRuntime, PollEventLoopOptions, RuntimeOptions};

    use super::super::support::test_host_state;

    fn boot() -> (tokio::runtime::Runtime, JsRuntime) {
        let executor = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("current-thread executor");
        let runtime = {
            let _guard = executor.enter();
            JsRuntime::new(RuntimeOptions {
                extensions: crate::main_extensions(test_host_state()),
                ..Default::default()
            })
        };
        (executor, runtime)
    }

    fn run(executor: &tokio::runtime::Runtime, runtime: &mut JsRuntime, source: &str) {
        let _guard = executor.enter();
        runtime
            .execute_script("<test:input-clock>", FastString::from(source.to_owned()))
            .unwrap_or_else(|error| panic!("{error}"));
        executor
            .block_on(runtime.run_event_loop(PollEventLoopOptions::default()))
            .expect("the event loop settles");
    }

    /// A one-point touch of `kind` (0 start, 1 move, 2 end), stamped `host` by the host.
    fn touch(host: &str) -> String {
        format!(
            "{{ const buf = new ArrayBuffer(20); const dv = new DataView(buf); \
               dv.setFloat32(4, 5, true); dv.setFloat32(8, 6, true); dv.setUint32(16, 1, true); \
               globalThis[Symbol.for('Migo.hostBridge')]._internalEnqueueRawTouchEvent(1, buf, 1, {host}); \
               globalThis.__nowAtDelivery.push(performance.now()); }}"
        )
    }

    const SETUP: &str = "globalThis.__stamps = []; globalThis.__nowAtDelivery = []; \
        migo.onTouchMove((e) => { globalThis.__stamps.push(e.timeStamp); });";

    fn check(executor: &tokio::runtime::Runtime, runtime: &mut JsRuntime, condition: &str) {
        run(
            executor,
            runtime,
            &format!(
                "if (!({condition})) throw new Error('stamps=' + JSON.stringify(globalThis.__stamps) \
                 + ' now=' + JSON.stringify(globalThis.__nowAtDelivery));"
            ),
        );
    }

    /// The host's epoch does not reach content: a host stamping in system uptime (here 17 hours) hands content a time
    /// on its own clock, never in the future and never negative.
    #[test]
    fn a_host_epoch_does_not_reach_content() {
        let (executor, mut runtime) = boot();
        run(&executor, &mut runtime, SETUP);
        run(&executor, &mut runtime, &touch("62067560000"));
        check(
            &executor,
            &mut runtime,
            "__stamps.length === 1 && __stamps[0] >= 0 && __stamps[0] <= performance.now() \
             && performance.now() - __stamps[0] < 1000",
        );
    }

    /// Inside a burst the host's spacing is what content sees, not the spacing of when the game read them.
    #[test]
    fn the_spacing_inside_a_burst_is_the_hosts() {
        let (executor, mut runtime) = boot();
        run(&executor, &mut runtime, SETUP);
        run(&executor, &mut runtime, &touch("62067560000"));
        std::thread::sleep(Duration::from_millis(120));
        run(&executor, &mut runtime, &touch("62067560100"));
        check(
            &executor,
            &mut runtime,
            "__stamps.length === 2 && Math.abs((__stamps[1] - __stamps[0]) - 100) < 0.001",
        );
    }

    /// A pause of a quarter second starts a new burst: the two clocks are anchored again, so a host clock that did not
    /// tick through a sleep (or one that was never the page's rate) cannot walk away from the page's.
    #[test]
    fn a_pause_anchors_the_clocks_again() {
        let (executor, mut runtime) = boot();
        run(&executor, &mut runtime, SETUP);
        run(&executor, &mut runtime, &touch("62067560000"));
        std::thread::sleep(Duration::from_millis(400));
        // The host's clock says 10 ms passed; 400 ms did.
        run(&executor, &mut runtime, &touch("62067560010"));
        check(
            &executor,
            &mut runtime,
            "__stamps.length === 2 && __stamps[1] - __stamps[0] > 350 \
             && __stamps[1] <= performance.now() && performance.now() - __stamps[1] < 1000",
        );
    }

    /// A stamp that is not a number is the moment it was read, not NaN in content's hands.
    #[test]
    fn a_stamp_that_is_not_a_number_is_now() {
        let (executor, mut runtime) = boot();
        run(&executor, &mut runtime, SETUP);
        run(&executor, &mut runtime, &touch("NaN"));
        run(&executor, &mut runtime, &touch("Infinity"));
        check(
            &executor,
            &mut runtime,
            "__stamps.length === 2 && __stamps.every((t) => Number.isFinite(t) && t <= performance.now())",
        );
    }

    /// Keys, the mouse and the wheel are on the same clock as touches.
    #[test]
    fn keys_mouse_and_wheel_share_the_clock() {
        let (executor, mut runtime) = boot();
        run(
            &executor,
            &mut runtime,
            "globalThis.__all = []; \
             const keep = (kind) => (e) => globalThis.__all.push([kind, e.timeStamp]); \
             migo.onKeyDown(keep('key')); migo.onMouseDown(keep('mouse')); migo.onWheel(keep('wheel')); \
             const b = globalThis[Symbol.for('Migo.hostBridge')]; \
             b._internalTriggerKeyDown('a', 'KeyA', 62067560000, 0, false); \
             b._internalTriggerMouseDown(1, 2, 0, 62067560005); \
             b._internalTriggerWheel(0, 1, 0, 62067560010, 0);",
        );
        run(
            &executor,
            &mut runtime,
            "if (__all.length !== 3 || !__all.every(([k, t]) => t >= 0 && t <= performance.now())) \
             throw new Error(JSON.stringify(__all));",
        );
    }
}
