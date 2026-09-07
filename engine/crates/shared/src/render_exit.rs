//! Why the render worker stopped, as a level rather than an event.

use std::{fmt, sync::OnceLock};

use crate::error::EngineError;

/// The render worker's terminal reason, written once by the worker itself.
///
/// A level and not an event, and that is the whole point. The session observes the
/// *consequence* of the worker stopping -- its frame clock closing -- on a
/// different channel from anything the worker could send, so a `select!` that saw
/// the closure first would break the loop and never drain a queued message. That
/// is how a renderer failing to initialise came to be reported by nothing at all
/// on the external-frame product: the worker logged, dropped its frame-clock
/// sender, and the session logged "frame clock closed" and exited, while the host
/// -- whose `on_error` exists for exactly this -- was told nothing. Attach used to
/// report it by accident, by racing the session's own teardown for a registry
/// entry, which is not a contract a host can program against.
///
/// Absence is meaningful: nothing recorded means the worker was asked to stop.
/// That distinction is structural rather than remembered -- the worker's body
/// returns a `Result`, so every exit has to name itself, and only the tail
/// publishes.
#[derive(Debug, Default)]
pub struct RenderExit {
    failure: OnceLock<EngineError>,
}

impl RenderExit {
    /// Record why the worker stopped.
    ///
    /// First write wins. A shutdown that arrives while a failure is already being
    /// reported must not overwrite the reason the host needs.
    pub fn publish_failure(&self, reason: EngineError) {
        let _ = self.failure.set(reason);
    }

    /// The failure the worker stopped for, or `None` if it was asked to stop.
    pub fn failure(&self) -> Option<&EngineError> {
        self.failure.get()
    }
}

impl fmt::Display for RenderExit {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.failure() {
            Some(failure) => write!(formatter, "{failure}"),
            None => formatter.write_str("the render worker was asked to stop"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::RenderExit;
    use crate::error::{EngineError, ErrorCode};

    #[test]
    fn nothing_recorded_means_the_worker_was_asked_to_stop() {
        let exit = RenderExit::default();

        assert!(exit.failure().is_none());
    }

    #[test]
    fn the_first_reason_is_the_one_the_host_is_told() {
        // A shutdown racing a failure is the ordinary case on a teardown that was
        // caused by the failure. Reporting the shutdown instead would replace the
        // only account of what went wrong with a description of the consequence.
        let exit = RenderExit::default();

        exit.publish_failure(
            EngineError::new(ErrorCode::Render2DInitError).with_msg("CanvasManager init failed"),
        );
        exit.publish_failure(EngineError::new(ErrorCode::Internal).with_msg("render thread panic"));

        let failure = exit.failure().expect("a failure was recorded");
        assert_eq!(failure.msg, "CanvasManager init failed");
        assert_eq!(failure.code, ErrorCode::Render2DInitError);
    }
}
