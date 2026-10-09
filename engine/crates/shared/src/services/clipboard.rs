//! Clipboard service trait.

use crate::protocol::error::ServiceError;

/// The system clipboard, as content's `setClipboardData` / `getClipboardData`
/// reach it.
///
/// Both are requests the host answers (Mode C), as they are asynchronous APIs
/// in the first place: a host that owns the clipboard on another thread -- every
/// C ABI host does -- cannot answer a synchronous read, and a platform that can
/// answers on the same path a moment later.
pub trait ClipboardService: Send + Sync {
    /// Write the clipboard.
    ///
    /// JSON fields (input): `requestId`, `data`. Result delivered via
    /// `_internalOnSetClipboardDataResult`: `{requestId}` or
    /// `{requestId, error}`.
    fn set_data(&self, _request_json: &str) -> Result<(), ServiceError> {
        Err(ServiceError::not_supported(
            "setClipboardData:fail not supported",
        ))
    }

    /// Read the clipboard.
    ///
    /// JSON fields (input): `requestId`. Result delivered via
    /// `_internalOnGetClipboardDataResult`: `{requestId, data}` or
    /// `{requestId, error}`.
    fn get_data(&self, _request_json: &str) -> Result<(), ServiceError> {
        Err(ServiceError::not_supported(
            "getClipboardData:fail not supported",
        ))
    }
}
