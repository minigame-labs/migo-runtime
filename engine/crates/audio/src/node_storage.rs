//! Shared error for size arithmetic in node and graph storage.
use shared::error::{EngineError, ErrorCode};

pub(crate) fn size_overflow() -> EngineError {
    EngineError::from_detail(ErrorCode::InputSaturated, "node storage size overflow")
}
