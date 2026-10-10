//! The host's own ecosystem: the social, commerce and platform features a host
//! app offers on top of the engine -- friends and groups, live channels, voice
//! chat, handoff, subscriptions, privacy agreements.
//!
//! Their semantics are the host's, not the engine's, so they cross as one
//! request shape keyed by the content API's name rather than as an interface per
//! API: the engine routes and settles, the host decides. Which names may arrive
//! is a closed list (`contracts/runtime/host-services.json`, service
//! `ecosystem`), so a host knows the whole set it may be asked for.

use crate::protocol::error::ServiceError;

/// What a host does with its ecosystem features.
pub trait EcosystemService: Send + Sync {
    /// Whether anyone answers: a service whose host installed nothing to answer
    /// with (an SDK host with no handler) is no ecosystem at all, and the few
    /// APIs with a true answer for that give it.
    fn available(&self) -> bool {
        true
    }

    /// `{"requestId", "api", "options"}`: answers through
    /// `_internalOnEcosystemResult` with the API's result, or `{"error",
    /// "errCode"}` -- a failure with the code the API defines, if any.
    fn call(&self, _request_json: &str) -> Result<(), ServiceError> {
        Err(ServiceError::not_supported("ecosystem:fail not supported"))
    }

    /// `{"replyId", "data", "done"}`: content's answer to an event the host
    /// posted with a `replyId` -- what an `onCopyUrl` or `onHandoff` listener
    /// returned, or what an `onNeedPrivacyAuthorization` listener resolved. The
    /// last reply for a `replyId` has `done` true. A command.
    fn reply(&self, _json: &str) -> Result<(), ServiceError> {
        Err(ServiceError::not_supported("ecosystem:fail not supported"))
    }

    /// The value the host last reported for the synchronous getter `name`
    /// (`getExtConfigSync`, `isChatTool`, ...), as JSON; `None` when it reported
    /// none.
    fn value(&self, _name: &str) -> Option<String> {
        None
    }
}
