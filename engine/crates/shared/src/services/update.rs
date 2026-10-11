//! The host's updates of the content it runs.
//!
//! A package update is the host's business from start to finish: it checks for
//! a new version every time the game starts, warm starts included, downloads
//! what it finds, and restarts the game on it when content asks. The engine
//! carries the outcome to content's `UpdateManager` -- the host reports each
//! step as an event, `{"event": "checkForUpdate", "hasUpdate"}`, then
//! `{"event": "updateReady"}` or `{"event": "updateFailed"}` -- and content's
//! `applyUpdate` back to the host.
//!
//! A host that offers this service is one that checks: content waits for its
//! report. A host that does not has no update to give, and content is told so
//! at once.

use crate::protocol::error::ServiceError;

/// Host-provided package updates.
pub trait UpdateService: Send + Sync {
    /// Restart the game on the version the host downloaded. Content asks only
    /// after the host reported it ready.
    fn apply_update(&self) -> Result<(), ServiceError>;
}
