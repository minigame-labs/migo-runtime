//! Share service traits for share-related operations.

use crate::protocol::error::ServiceError;

/// Share service for app sharing operations.
///
/// Mode C (async): `share_app_message` fires the platform share flow;
/// the result arrives via `_internalOnShareAppMessageResult` EvalScript callback.
pub trait ShareService: Send + Sync {
    /// Trigger the native share flow.
    ///
    /// JSON fields (input), each absent when content did not set it: `requestId`,
    /// `title`, `imageUrl` (a real path resolved through the sandbox, or an
    /// http(s) URL), `query`, `imageUrlId`, `toCurrentGroup`, `path`.
    ///
    /// Result delivered via `_internalOnShareAppMessageResult`.
    fn share_app_message(&self, _options_json: &str) -> Result<(), ServiceError> {
        Err(ServiceError::not_supported(
            "shareAppMessage:fail not supported",
        ))
    }

    /// Share to one friend from the relationship chain (Mode C, async).
    ///
    /// JSON fields (input): `requestId`, `openId`, and when set `title`,
    /// `imageUrl` (a real path resolved through the sandbox), `imageUrlId`, and
    /// the `query` / `shareMessageToFriendScene` setMessageToFriendQuery set.
    /// Result delivered via `_internalOnShareMessageToFriendResult`.
    fn share_message_to_friend(&self, _options_json: &str) -> Result<(), ServiceError> {
        Err(ServiceError::not_supported(
            "shareMessageToFriend:fail not supported",
        ))
    }

    /// Open the host's share sheet for an image (Mode C, async).
    ///
    /// JSON fields (input): `requestId`, `path` (a real path resolved through
    /// the sandbox), `needShowEntrance`, and `entrancePath` when set. Result
    /// delivered via `_internalOnShowShareImageMenuResult`.
    fn show_share_image_menu(&self, _options_json: &str) -> Result<(), ServiceError> {
        Err(ServiceError::not_supported(
            "showShareImageMenu:fail not supported",
        ))
    }
}
