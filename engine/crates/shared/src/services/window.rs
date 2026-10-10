//! The desktop window the game runs in: its size, the mouse cursor over it, and
//! pointer lock.

use crate::protocol::error::ServiceError;

/// What a desktop host does with its window for content.
///
/// The common mini-game platform offers these on its desktop clients only, so a
/// mobile host supplies none: `setCursor` then answers `false`, pointer lock does
/// nothing and `setWindowSize` fails as not supported. What the host observes --
/// the window minimised, maximised or restored, pointer lock taken or lost --
/// arrives as events.
pub trait WindowService: Send + Sync {
    /// `setWindowSize` `{"requestId", "width", "height"}` (pixels): answers `{}`
    /// through `_internalOnSetWindowSizeResult`.
    fn set_window_size(&self, _request_json: &str) -> Result<(), ServiceError> {
        Err(ServiceError::not_supported(
            "setWindowSize:fail not supported",
        ))
    }

    /// `setCursor`: `{"keyword"}` -- a CSS cursor keyword, `default` restoring the
    /// system's -- or `{"path", "x", "y"}`, a real path to an image (ico, cur or
    /// any format the host's platform reads) with its hotspot. A command.
    fn set_cursor(&self, _json: &str) -> Result<(), ServiceError> {
        Err(ServiceError::not_supported("setCursor:fail not supported"))
    }

    /// Lock and hide the pointer. A command: the host reports whether it took the
    /// lock -- it may refuse without a user gesture -- as an event, and while
    /// locked keeps reporting mouse positions that accumulate the motion, so
    /// movement stays derivable while the cursor stands still.
    fn request_pointer_lock(&self) -> Result<(), ServiceError> {
        Err(ServiceError::not_supported(
            "requestPointerLock:fail not supported",
        ))
    }

    /// Release the pointer. A command; the release arrives as an event.
    fn exit_pointer_lock(&self) -> Result<(), ServiceError> {
        Err(ServiceError::not_supported(
            "exitPointerLock:fail not supported",
        ))
    }
}
