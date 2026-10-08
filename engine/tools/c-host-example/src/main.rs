//! Entry point comes from C.
//!
//! `main` is defined in `tests/c_host/linux/main.c`, which build.rs compiles and
//! links in. `#![no_main]` keeps Rust from emitting one of its own, so the
//! resulting binary is a C program that happens to be linked by cargo — which
//! is the point: the C side sees nothing but the public ABI, while cargo
//! resolves the engine's native dependencies correctly.
//!
//! The `capi` dependency is what pulls the `migo_*` symbols into the link.

#![no_main]

// Referencing the crate keeps the linker from dropping it: nothing in Rust
// calls into it, the C side does. The name here is the dependency alias from
// Cargo.toml (`capi = { package = "migo-capi" }`), not the library's own
// `migo_capi` — a `package =` rename is what the extern name follows.
extern crate capi as _;
