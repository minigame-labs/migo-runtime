use std::cell::RefCell;
use std::sync::{Arc, Mutex};

use deno_core::{Extension, extension, op2, v8};
use shared::console_log::ConsoleLogBuffer;
use tracing::{debug, error, info, warn};

thread_local! {
    /// This isolate's devtools console buffer, or `None` when the session runs
    /// without debug enabled.
    ///
    /// Resolved **once**, at bring-up, and never again. Finding it means reading
    /// a registry shared with every other Session, whose writers are their
    /// bring-up and teardown, so doing that per call would put a cross-session
    /// lock on a path the content drives -- the cross-session contention trap, and the
    /// reason the text texture cache and the image alias table are wired the same
    /// way.
    static CONSOLE_SINK: RefCell<Option<Arc<Mutex<ConsoleLogBuffer>>>> =
        const { RefCell::new(None) };
}

/// Resolve this thread's console buffer. Must be called from the host thread
/// before the JS event loop starts.
///
/// Safe to resolve this early, and it has to be checked rather than assumed:
/// registration happens in the host's pre-JS services, ahead of the isolate on
/// the first start, and a restart builds a new isolate without unregistering, so
/// the answer is already final both times this runs. Teardown unregisters, by
/// which point the isolate is gone.
pub fn bind_thread_console(id: i32) {
    set_thread_console_sink(shared::console_log::get_console_log(id));
}

/// Install an already-resolved sink on this thread.
///
/// Split out from the lookup because the two happen at different times and only
/// one of them may touch the registry: resolving is bring-up work, installing is
/// not. It is also what lets the contention gate resolve before the registry is
/// locked, the way an isolate does.
fn set_thread_console_sink(sink: Option<Arc<Mutex<ConsoleLogBuffer>>>) {
    CONSOLE_SINK.replace(sink);
}

/// Write one entry into the buffer this thread resolved at bring-up.
///
/// Separate from the op so the contention gate can reach it: an `op2` needs a V8
/// scope, and what the gate is about is which lock this path takes.
fn record_for_devtools(level: u8, message: String) {
    CONSOLE_SINK.with_borrow(|sink| {
        if let Some(buffer) = sink {
            if let Ok(mut buffer) = buffer.lock() {
                buffer.push(level, message);
            }
        }
    });
}

#[op2(fast)]
pub fn op_console<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    value: v8::Local<'s, v8::Value>,
    level: u8,
) {
    let msg = value
        .to_string(scope)
        .map(|s| s.to_rust_string_lossy(scope))
        .unwrap_or_else(|| "<invalid value>".to_string());

    match level {
        1 => info!("{}", msg),
        2 => warn!("{}", msg),
        3 => error!("{}", msg),
        _ => debug!("{}", msg),
    }

    // Write to the per-session ring buffer (no-op if the session has no buffer,
    // i.e. debug mode is disabled).
    record_for_devtools(level, msg);
}
extension!(host_v8_console,
ops = [op_console],
esm = [
    dir "src/console",
    "01_console.js",
    "01_alert.js"
]
);

pub fn console_extensions() -> Vec<Extension> {
    vec![host_v8_console::init()]
}

pub fn console_lazy_extensions() -> Vec<Extension> {
    vec![host_v8_console::lazy_init()]
}

// ── No cross-session lock on a per-event path ───────────────────────────────

#[cfg(test)]
mod cross_session_contention {
    use super::{bind_thread_console, record_for_devtools, set_thread_console_sink};
    use migo_contention_probe::{PATIENCE, PerEventPath, assert_completes_while_locked};

    /// Cross-session contention gate, on the path content drives every time it calls `console.log`.
    ///
    /// The registry that maps a Session to its console buffer is shared with every
    /// other Session, and its writers are their bring-up and teardown. Looking a
    /// session up there per call *works* — it returns the right buffer — which is
    /// exactly why only a gate catches it: the cost is a queue behind an unrelated
    /// game starting or stopping, on a path a game can reach every frame.
    ///
    /// The write guard is what makes this meaningful. An `RwLock` admits
    /// concurrent readers, so a held read guard would let a per-call `read()`
    /// straight through and the gate would pass while the defect was present.
    #[test]
    fn writing_a_console_entry_does_not_queue_behind_the_console_registry() {
        let host_id = 9_400;
        let buffer = shared::console_log::register_console_log(host_id);
        struct Unregister(i32);
        impl Drop for Unregister {
            fn drop(&mut self) {
                shared::console_log::unregister_console_log(self.0);
            }
        }
        let _guard = Unregister(host_id);

        // Resolved at bring-up, exactly as the isolate does -- before anything is
        // contended, and once.
        bind_thread_console(host_id);
        let resolved = buffer.clone();

        assert_completes_while_locked(
            PerEventPath {
                path: "console.log writing into its session's ring buffer",
                shared_lock: "console_log CONSOLE_LOGS (every live Session's buffer)",
                patience: PATIENCE,
            },
            shared::console_log::console_registry_lock_for_contention_probe(),
            // The probe runs the body on a thread of its own, so the already
            // resolved sink is installed there rather than looked up again --
            // which is exactly the production sequence, and the whole claim.
            move || {
                set_thread_console_sink(Some(resolved));
                record_for_devtools(3, "contended".to_owned());
            },
        );

        let entries = buffer.lock().expect("buffer lock").read_since(0).0.len();
        assert_eq!(
            entries, 1,
            "the fixture must actually write the entry, or a completion proves \
             nothing about the path"
        );
    }
}
