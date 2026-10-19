use std::sync::Arc;

use super::DeviceServices;
use shared::surface::{PublicSurfaceGeneration, SurfaceLossReason};

/// Device-service factory capability supplied by a platform Host Kit.
pub trait DeviceServiceProvider: Send + Sync {
    /// Create device services for a specific host session.
    ///
    /// # Arguments
    /// * `host_id` - The session/host identifier for this runtime instance
    ///
    /// Returns None if device services are not available on this platform.
    fn create_device_services(&self, _host_id: i32) -> Option<Arc<dyn DeviceServices>> {
        None
    }
}

/// Display frame-clock capability supplied by a platform Host Kit.
pub trait FrameClock: Send + Sync {
    /// Whether this platform supplies frame timestamps through the external
    /// vsync channel. Platforms returning `false` are paced by the engine's own
    /// demand-driven frame clock instead.
    fn uses_external_vsync(&self) -> bool {
        false
    }

    /// R1: request exactly one display frame callback for `host_id`.
    ///
    /// Called by the render thread (and `op_await_next_frame`) when there is
    /// demand for a frame — an actual RAF waiter, dirty content, outstanding
    /// upload work, or a resume/recreate task. Must be cheap and safe to call
    /// from any thread; the implementation is responsible for hopping to the UI
    /// thread and rechecking the live session before touching the Choreographer.
    ///
    /// Default is a no-op. Platforms without an external display clock are
    /// routed to a nudge that wakes the render thread so it can arm its own
    /// clock, which the host installs in place of this call.
    fn request_vsync(&self, _host_id: i32) {
        // Default: no-op.
    }
}

/// Host-application notification callbacks supplied by a platform Host Kit.
pub trait HostNotifier: Send + Sync {
    /// Notify the host application that the game module has been loaded and
    /// is ready to run.
    ///
    /// Called after `EvaluateModule` completes successfully.
    fn notify_game_ready(&self, _host_id: i32) {
        // Default: no-op.
    }

    /// Notify the host application that the game module could not be started: the launch that
    /// ends in [`notify_game_ready`](Self::notify_game_ready) when it succeeds failed instead.
    ///
    /// The other half of the launch's outcome. A host waiting on a launch could only hear it
    /// as one more [`notify_error`](Self::notify_error), which also carries what goes wrong
    /// while content runs -- a render command failing, a lost context that did not come back --
    /// so it could not tell "the game never started" from "the game hit an error". The default
    /// reports it as `notify_error`, which is how every host heard it before.
    fn notify_game_failed(&self, host_id: i32, error_code: u16, message: &str, detail: &str) {
        self.notify_error(host_id, error_code, message, detail);
    }

    /// Notify the host application that the mini program is exiting.
    ///
    /// This is called when the JS side calls `exitMiniProgram`.
    fn notify_exit(&self, _host_id: i32) {
        // Default: no-op.
    }

    /// Notify the host application about an error the engine hit.
    ///
    /// Not every one ends the game, and the code says which: a host tells them apart by code
    /// alone (Android's `ErrorCode.isRecoverable`). A render command the content got wrong
    /// arrives as `RenderCommandFailed` and refused input as `InputSaturated`; the session goes
    /// on after both. The host thread panicking, the ANR watchdog terminating the isolate and
    /// the V8 heap limit end it. A launch that failed is [`Self::notify_game_failed`].
    ///
    /// On Android, this calls `NativeExports.onError(hostId, code, msg, detail)`
    /// via JNI.  The watchdog thread also uses this to report ANR events.
    ///
    /// Default implementation is a no-op (for platforms that haven't implemented
    /// error notification yet).
    ///
    /// # Arguments
    /// * `host_id` - The host session ID
    /// * `error_code` - The `ErrorCode` as u16
    /// * `message` - Human-readable error message
    /// * `detail` - Additional debugging details
    fn notify_error(&self, _host_id: i32, _error_code: u16, _message: &str, _detail: &str) {
        // Default: no-op. Override on platforms with JNI / native callbacks.
    }

    /// Notify an embedding host that a live native Surface was retired after
    /// an unexpected presentation failure. Ordinary host-requested detach must
    /// never call this hook.
    fn notify_surface_lost(
        &self,
        _host_id: i32,
        _public_generation: PublicSurfaceGeneration,
        _reason: SurfaceLossReason,
    ) {
        // Default: platform lifecycle owns Surface replacement.
    }

    /// Deliver a JS-to-host message to the host application.
    ///
    /// Called when game JS calls `migo.sendToHost(type, payload)`.
    /// `json` is a `{"type":"...","payload":"..."}` envelope.
    ///
    /// On Android, this calls `NativeExports.onHostMessage(hostId, json)` via JNI,
    /// which dispatches to the `GameSession.MessageHandler` on the main thread.
    ///
    /// Default implementation is a no-op.
    fn notify_host_message(&self, _host_id: i32, _json: &str) {
        // Default: no-op.
    }
}

/// Runtime-replacement notifications for a platform that keeps per-session
/// objects outside the JavaScript isolate.
///
/// A restart replaces the isolate but not the platform objects around it: on
/// Android a manager's listeners stay registered and keep firing. Those objects
/// need to know which runtime they belong to so their events can be told apart
/// from the replacement's, and only the engine knows when the numbering moves.
///
/// The engine remains the sole authority; these are notifications, not requests.
/// A platform that keeps no per-session objects — Linux, Windows, the C ABI host
/// kit — takes the defaults and needs to know nothing about generations.
pub trait RuntimeGenerationNotifier: Send + Sync {
    /// The runtime at `retired` is going away and `next` will replace it.
    ///
    /// Between this and `complete_runtime_restart` there is no live runtime, so
    /// a platform that hands out per-session resources should refuse rather than
    /// issue them against the generation that is leaving.
    fn begin_runtime_restart(&self, _host_id: i32, _retired: i64, _next: i64) {
        // Default: this platform keeps nothing that outlives the isolate.
    }

    /// `next` is live, and every object created from now belongs to it.
    fn complete_runtime_restart(&self, _host_id: i32, _next: i64) {
        // Default: as above.
    }
}

/// Backend-neutral services supplied by a platform Host Kit, composed of the
/// device, frame-clock, notification, and runtime-generation capability
/// interfaces.
///
/// A platform (Android, iOS, etc.) implements the capability traits; the
/// blanket impl below provides this marker automatically, so `core` can keep a
/// single `Arc<dyn PlatformServices>` handle while platforms implement focused
/// interfaces rather than one growing trait.
pub trait PlatformServices:
    DeviceServiceProvider + FrameClock + HostNotifier + RuntimeGenerationNotifier
{
}

impl<T> PlatformServices for T where
    T: DeviceServiceProvider + FrameClock + HostNotifier + RuntimeGenerationNotifier
{
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::HostNotifier;

    #[derive(Default)]
    struct ErrorsOnly(Mutex<Vec<(i32, u16, String, String)>>);

    impl HostNotifier for ErrorsOnly {
        fn notify_error(&self, host_id: i32, error_code: u16, message: &str, detail: &str) {
            self.0
                .lock()
                .unwrap()
                .push((host_id, error_code, message.into(), detail.into()));
        }
    }

    #[test]
    fn a_host_that_hears_only_errors_still_hears_a_launch_that_failed() {
        let host = ErrorsOnly::default();
        host.notify_game_failed(7, 42, "module evaluation failed", "ReferenceError: x");
        assert_eq!(
            *host.0.lock().unwrap(),
            vec![(
                7,
                42,
                "module evaluation failed".to_string(),
                "ReferenceError: x".to_string()
            )]
        );
    }
}
