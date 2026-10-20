//! Entry point comes from C++.
//!
//! `main` is defined in `tests/qt_host/linux/main.cpp`, which build.rs compiles
//! and links in with the Linux Host Kit. `#![no_main]` keeps Rust from emitting
//! one of its own: the binary is a Qt program that happens to be linked by
//! cargo, so the C++ side sees nothing but the public ABI and the Host Kit while
//! cargo resolves the engine's native dependencies.
//!
//! The `capi` dependency is what pulls the `migo_*` symbols into the link.

#![no_main]

// Referencing the crate keeps the linker from dropping it: nothing in Rust calls
// into it, the C++ side does.
extern crate capi as _;
