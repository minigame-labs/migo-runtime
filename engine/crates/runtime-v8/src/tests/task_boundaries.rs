//! Observable host task boundaries; no event-loop poll between host messages.
use super::support::{Sandbox, test_host_state};
use crate::HostJsRuntime;
use shared::protocol::host_cmd::{TouchPoint, TouchType};

fn boot() -> (Sandbox, HostJsRuntime) {
    let sandbox = Sandbox::new("task-boundaries");
    let runtime = HostJsRuntime::new(
        1,
        test_host_state(),
        &sandbox.paths.cache_dir(),
        #[cfg(feature = "v8-limits")]
        crate::V8LimitsConfig::default(),
        #[cfg(feature = "code-signing")]
        false,
        #[cfg(feature = "code-signing")]
        None,
    );
    (sandbox, runtime)
}

#[tokio::test]
async fn prelude_promise_jobs_finish_before_the_next_script() {
    let (_sandbox, mut runtime) = boot();
    runtime
        .exec_script_owned(
            "prelude.js".into(),
            r#"
        globalThis.ready = false;
        Promise.resolve().then(() => Promise.resolve()).then(() => ready = true);
    "#,
        )
        .unwrap();
    runtime
        .exec_script(
            "entry.js",
            "if (!ready) throw new Error('prelude checkpoint missing');",
        )
        .unwrap();
}

#[tokio::test]
async fn a_checkpoint_does_not_dispatch_a_ready_timer() {
    let (_sandbox, mut runtime) = boot();
    runtime
        .exec_script(
            "tasks.js",
            r#"
        globalThis.timerRan = false;
        globalThis.promiseRan = false;
        setTimeout(() => timerRan = true, 0);
        Promise.resolve().then(() => promiseRan = true);
    "#,
        )
        .unwrap();
    runtime
        .exec_script(
            "check.js",
            "if (!promiseRan || timerRan) throw new Error('task boundary');",
        )
        .unwrap();
    runtime.pump_event_loop().await.unwrap();
    runtime
        .exec_script(
            "after.js",
            "if (!timerRan) throw new Error('timer never ran');",
        )
        .unwrap();
}

#[tokio::test]
async fn touch_jobs_finish_before_the_next_input_message() {
    let (_sandbox, mut runtime) = boot();
    runtime
        .exec_script(
            "listen.js",
            r#"
        globalThis.order = [];
        migo.onTouchStart(() => {
            order.push('touch');
            Promise.resolve().then(() => order.push('promise'));
        });
        migo.onKeyDown(() => order.push('key'));
    "#,
        )
        .unwrap();
    runtime.dispatch_touch(
        TouchType::Start,
        &[TouchPoint {
            id: 1,
            x: 2.0,
            y: 3.0,
            pressure: 1.0,
            flags: 1,
        }],
        0,
    );
    runtime.dispatch_key_down("a", "KeyA", 0.0, 0, false);
    runtime
        .exec_script(
            "check.js",
            r#"
        if (order.join(',') !== 'touch,promise,key') throw new Error(order.join(','));
    "#,
        )
        .unwrap();
}

#[tokio::test]
async fn a_host_hook_finishes_its_jobs_before_the_next_hook() {
    let (_sandbox, mut runtime) = boot();
    runtime
        .exec_script(
            "listen.js",
            r#"
        globalThis.order = [];
        migo.onShow(() => {
            order.push('show');
            Promise.resolve().then(() => order.push('promise'));
        });
        migo.onHide(() => order.push('hide'));
    "#,
        )
        .unwrap();
    runtime.invoke_host_hook("_internalTriggerOnShow", "[{}]");
    runtime.invoke_host_hook("_internalTriggerOnHide", "[]");
    runtime
        .exec_script(
            "check.js",
            r#"
        if (order.join(',') !== 'show,promise,hide') throw new Error(order.join(','));
    "#,
        )
        .unwrap();
}

#[tokio::test]
async fn a_throwing_script_still_finishes_jobs_it_queued() {
    let (_sandbox, mut runtime) = boot();
    assert!(
        runtime
            .exec_script(
                "throw.js",
                r#"
        globalThis.finished = false;
        Promise.resolve().then(() => finished = true);
        throw new Error('script failed');
    "#
            )
            .is_err()
    );
    runtime
        .exec_script(
            "check.js",
            "if (!finished) throw new Error('jobs skipped after exception');",
        )
        .unwrap();
}

#[cfg(feature = "v8-limits")]
#[tokio::test]
async fn checkpoint_work_is_covered_by_the_script_watchdog() {
    use std::time::{Duration, Instant};
    let (_sandbox, mut runtime) = boot();
    runtime
        .install_watchdog(crate::watchdog::DeadlineWatchdogConfig::new(
            Duration::from_millis(200),
            "task-checkpoint",
        ))
        .unwrap();
    let start = Instant::now();
    let result = runtime.exec_script(
        "runaway-job.js",
        "Promise.resolve().then(() => { while (true) {} });",
    );
    assert!(
        result.is_err(),
        "a checkpoint terminated by the watchdog must fail"
    );
    assert!(runtime.watchdog_timed_out());
    assert!(start.elapsed() < Duration::from_secs(5));
}

/// An isolate made outside a tokio runtime has nowhere for V8's delayed tasks
/// to go, and deno_core aborts the process at the first one -- later, from
/// inside V8. The Host's runtime refuses to be made there instead, and says why.
#[test]
#[should_panic(expected = "HostJsRuntime::new must run inside its session's tokio runtime")]
fn the_host_s_runtime_is_made_inside_a_tokio_runtime_or_not_at_all() {
    let _ = boot();
}
