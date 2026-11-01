#![doc = include_str!("../README.md")]
#![cfg_attr(not(feature = "std"), no_std)]

/// Type conversion of samples values.
pub mod sample;

/// Extensions to the standard [std::io::Read] and [std::io::Write] traits.
#[cfg(feature = "std")]
pub mod readwrite;
