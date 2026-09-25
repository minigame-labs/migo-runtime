//! Why a service call failed, as content will see it.

use shared::error::EngineError;

/// A failure, carrying the class of error content will be thrown and its
/// message.
///
/// The class is part of the answer, not decoration. The embedded runtime's ops
/// throw classed errors -- storage's awaited ops `StorageError`, most sync ops a
/// plain `Error` -- and content that catches one class and not another has to
/// see the same class whichever execution answered it. Carried by name because
/// that is how it crosses to a producer in another process.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServiceError {
    pub class: &'static str,
    pub message: String,
}

/// The class a plain `Error` has.
pub const CLASS_ERROR: &str = "Error";

impl ServiceError {
    /// A plain `Error`.
    pub fn generic(message: impl Into<String>) -> Self {
        Self {
            class: CLASS_ERROR,
            message: message.into(),
        }
    }

    /// An error of a named class.
    pub fn classed(class: &'static str, message: impl Into<String>) -> Self {
        Self {
            class,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for ServiceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.class, self.message)
    }
}

impl std::error::Error for ServiceError {}

/// The message a sync op has always shown for an engine error: its detail when
/// there is one, otherwise its summary. Kept so the text content matches on is
/// the text it always saw.
pub(crate) fn detail_or_message(error: &EngineError) -> String {
    match &error.detail {
        Some(detail) => detail.clone(),
        None => error.msg.to_string(),
    }
}

/// The message an awaited op has always shown: summary, then detail.
pub(crate) fn message_and_detail(error: &EngineError) -> String {
    match &error.detail {
        Some(detail) => format!("{} ({})", error.msg, detail),
        None => error.msg.to_string(),
    }
}
