//! # Core Runtime Module
//!
//! The central orchestration module of the Migo engine, responsible for:
//!
//! - Spawning and managing the host thread (JS runtime)
//! - Coordinating services (render, audio, I/O)
//! - Platform abstraction via [`PlatformServices`]
//!
//! ## Architecture Overview
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────────────┐
//! │                           Platform Layer                             │
//! │  ┌─────────────────────────────────────────────────────────────────┐│
//! │  │                        MiniGameSDK (Java)                       ││
//! │  │  - Surface management                                           ││
//! │  │  - Touch event handling                                         ││
//! │  │  - Lifecycle events (show/hide)                                 ││
//! │  └────────────────────────────┬────────────────────────────────────┘│
//! │                               │ JNI                                  │
//! │  ┌────────────────────────────▼────────────────────────────────────┐│
//! │  │                    PlatformServices (Rust)                      ││
//! │  │  - Platform-specific extensions                                 ││
//! │  │  - System settings                                              ││
//! │  └────────────────────────────┬────────────────────────────────────┘│
//! └───────────────────────────────│─────────────────────────────────────┘
//!                                 │
//! ┌───────────────────────────────▼─────────────────────────────────────┐
//! │                           Core Runtime                               │
//! │  ┌─────────────────────────────────────────────────────────────────┐│
//! │  │                         Host Thread                             ││
//! │  │  ┌───────────────┐  ┌───────────────┐  ┌───────────────┐      ││
//! │  │  │  JS Runtime   │  │ HostOpState   │  │  JsBindings   │      ││
//! │  │  │  (Deno Core)  │  │  (channels)   │  │ (V8 globals)  │      ││
//! │  │  └───────────────┘  └───────────────┘  └───────────────┘      ││
//! │  └────────────────────────────┬────────────────────────────────────┘│
//! │                               │ Commands                             │
//! │  ┌────────────────────────────┴────────────────────────────────────┐│
//! │  │                          Services                               ││
//! │  │  ┌─────────────┐  ┌─────────────┐  ┌─────────────┐             ││
//! │  │  │   Render    │  │    Audio    │  │     I/O     │             ││
//! │  │  │   Service   │  │   Service   │  │   Service   │             ││
//! │  │  └──────┬──────┘  └──────┬──────┘  └──────┬──────┘             ││
//! │  │         │                │                │                     ││
//! │  │  ┌──────▼──────┐  ┌──────▼──────┐  ┌──────▼──────┐             ││
//! │  │  │   Render    │  │    Audio    │  │  I/O Task   │             ││
//! │  │  │   Thread    │  │    Thread   │  │(Host tokio) │             ││
//! │  │  └─────────────┘  └─────────────┘  └─────────────┘             ││
//! │  └─────────────────────────────────────────────────────────────────┘│
//! └─────────────────────────────────────────────────────────────────────┘
//! ```
//!
//! ## Usage
//!
//! The core module is the main entry point for native code:
//!
//! ```rust,ignore
//! use core::{spawn_host_thread, send_command_to_host, PlatformServices};
//! use shared::protocol::host_cmd::HostCommand;
//!
//! // 1. Spawn the host thread
//! let mut host = spawn_host_thread(Some(surface), graphics_platform, platform, init_options)?;
//! let host_id = host.id();
//!
//! // 2. Send commands to run a game
//! send_command_to_host(host_id, HostCommand::EvaluateModule {
//!     dir: "/data/game".into(),
//!     entry: "main.js".into(),
//! })?;
//!
//! // 3. Forward platform events
//! send_command_to_host(host_id, HostCommand::OnTouch(Box::new(...)))?;
//!
//! // 4. Shutdown when done
//! host.shutdown_and_join()?;
//! ```
//!
//! ## Thread Model
//!
//! The engine uses a multi-threaded architecture:
//!
//! | Thread / Task | Responsibility |
//! |--------------|---------------|
//! | **Host** | JS runtime, event loop, command dispatch, I/O dispatch (tokio task) |
//! | **Render** | OpenGL/Canvas2D rendering, vsync |
//! | **Audio** | Audio decoding, mixing, output |
//! | **Process I/O executor** | Bounded file ops, image decode, archive work (`Migo-IO-*`) |
//! | **Tokio blocking fallback** | Lazy `tokio::fs` and resolver compatibility work |
//!
//! The host event loop shares one Tokio epoll fd and timer wheel. Heavy engine
//! I/O is routed through the process-wide bounded I/O executor; the host's
//! small Tokio blocking pool is created only if a remaining `tokio::fs` or
//! resolver compatibility path needs it.
//!
//! All threads/tasks communicate via typed channels, ensuring thread safety
//! without shared mutable state.
//!
//! ## Module Structure
//!
//! - [`runtime`]: Host thread lifecycle and registry
//! - [`services`]: Service abstractions and platform interface

// Section 7.3's steady-state allocation gate reads this. `#[cfg(test)]` scopes it
// to this crate's own test binary: a `#[global_allocator]` is unique per binary, so
// one declared unconditionally here would follow the library into every shipped
// cdylib. Deleting it does not make the gates pass silently -- each burst proves the
// allocator is installed before it trusts a zero count.
#[cfg(test)]
#[global_allocator]
static COUNTING_ALLOCATOR: migo_alloc_probe::CountingAllocator =
    migo_alloc_probe::CountingAllocator::system();

mod runtime;
pub mod services;

pub use runtime::{
    // Exported because it is already part of the public signature of
    // `host_ingress`, `send_command_to_host` and `shutdown_host`. Without it a
    // caller cannot name the type those functions take, which pushes hosts into
    // guessing a width and writing a cast -- and a cast that guesses wrong
    // truncates silently instead of failing to compile.
    HostId,
    HostIngress,
    HostIngressSendError,
    host_ingress,
    lease_surface,
    lease_surface_tracked,
    lease_surface_with_resource,
    retire_surface,
    send_command_to_host,
    send_critical_command_to_host,
    send_reliable_command_to_host,
    shutdown_host,
};
// The host thread and its spawn entry points belong to the embedded execution:
// they own a JavaScript runtime. A build without one does not get a narrower
// version of them, it gets a different execution mode.
/// The synchronous barrier's vocabulary, re-exported for the same reason: the C
/// boundary maps these onto stable numbers the producer reads, and it should
/// not have to reach into the wire crate to name them.
#[cfg(feature = "external-frames")]
pub use frame_wire::sync::{SyncError, SyncRequest, SyncState};
/// Re-exported so the C boundary can translate an outcome without depending on
/// the wire crate directly: the boundary's job is to copy numbers across, not
/// to know how a packet is parsed.
#[cfg(feature = "external-frames")]
pub use frame_wire::{IngressDecision, IngressOutcome};
/// The external-frame execution. A session with no script runtime in this
/// process, for the Apple Performance+ product.
#[cfg(feature = "external-frames")]
pub use runtime::external::{
    ExternalFrameClock, ExternalFrameSession, SpawnedExternalSession, SyncSnapshot,
    spawn_external_frame_session,
};
pub use runtime::{HostThread, SpawnedSurfaceHost};
#[cfg(feature = "embedded-v8")]
pub use runtime::{spawn_host_thread, spawn_host_thread_tracked};
pub use services::{
    DeviceServiceProvider, FrameClock, HostNotifier, PlatformServices, RuntimeGenerationNotifier,
};
#[cfg(all(feature = "profile-full", feature = "profile-slim"))]
compile_error!("profile-full and profile-slim are mutually exclusive");
// Rejected at compile time rather than resolved by precedence. The external
// lane's product claim is that the archive contains no JavaScript engine; a
// build carrying both modes has an engine, whichever mode it happens to run,
// and a dependency-closure gate would rightly call it a violation. Failing here
// says which flag to drop; failing there says only that V8 is present.
#[cfg(all(feature = "embedded-v8", feature = "external-frames"))]
compile_error!(
    "embedded-v8 and external-frames are mutually exclusive: the external lane exists to      prove no JavaScript engine is linked, and a build with both links one"
);
#[cfg(all(feature = "worker-snapshot", not(feature = "profile-full")))]
compile_error!("worker-snapshot requires profile-full");
#[cfg(all(
    feature = "profile-slim",
    any(
        feature = "api-sensors",
        feature = "api-media",
        feature = "api-connectivity",
        feature = "api-commerce",
        feature = "api-system"
    )
))]
compile_error!("profile-slim is exact and cannot be combined with optional API groups");
