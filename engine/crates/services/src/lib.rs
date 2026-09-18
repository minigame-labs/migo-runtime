//! The engine's host services, callable without a script runtime.
//!
//! See the manifest for why this crate exists. In one line: the work an op does
//! is here, and the ops -- the embedded runtime's, and the external session's
//! service dispatcher -- are adapters over it.

pub mod content;
pub mod error;
pub mod image;
pub mod storage;

pub use error::ServiceError;
