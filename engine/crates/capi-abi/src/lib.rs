//! Host-runnable definitions and validation for Migo's public C ABI.

pub mod callbacks;
pub mod config;
pub mod external_frames;
mod header;
pub mod host_services;
pub mod input;
pub mod surface;
pub mod validate;

pub use header::{
    AbiStruct, OutputVersionPolicy, VersionedHeader, copy_utf8, copy_utf8_with_length,
    copy_versioned, validate_header, write_versioned_output,
};

/// Mirrors `MigoResult` in `include/migo/types.h`.
pub type MigoResult = i32;

pub const MIGO_OK: MigoResult = 0;
pub const MIGO_ERROR_INVALID_ARGUMENT: MigoResult = -1;
pub const MIGO_ERROR_UNSUPPORTED_ABI: MigoResult = -2;
pub const MIGO_ERROR_UNSUPPORTED_PLATFORM: MigoResult = -3;
pub const MIGO_ERROR_UNSUPPORTED_CAPABILITY: MigoResult = -4;
pub const MIGO_ERROR_INVALID_STATE: MigoResult = -5;
pub const MIGO_ERROR_WRONG_THREAD: MigoResult = -6;
pub const MIGO_ERROR_STALE_SURFACE: MigoResult = -7;
pub const MIGO_ERROR_CANCELLED: MigoResult = -8;
pub const MIGO_ERROR_DISPATCH_REJECTED: MigoResult = -9;
pub const MIGO_ERROR_OUT_OF_MEMORY: MigoResult = -10;
pub const MIGO_ERROR_INTERNAL: MigoResult = -11;
pub const MIGO_ERROR_WOULD_BLOCK: MigoResult = -12;

pub const MIGO_ABI_VERSION_CURRENT: u32 = 1;
