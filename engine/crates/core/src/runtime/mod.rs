pub type HostId = i32;

// The embedded execution: a JavaScript engine in this process, its event loop,
// and the host thread that owns both. Everything below it in this module --
// the registry, the generation boundary, the vsync clock -- is engine-neutral
// and compiled either way, because both execution modes need exactly those.
#[cfg(feature = "embedded-v8")]
mod host;
#[cfg(feature = "embedded-v8")]
mod input_state;
#[cfg(feature = "embedded-v8")]
mod session_temp;
#[cfg(feature = "embedded-v8")]
pub mod thread;

pub mod registry;
mod restart_boundary;
// The engine-neutral half of a session's bring-up. Compiled in both modes,
// because both need a render thread, a frame clock and a surface, and neither
// of those has an opinion about where JavaScript runs.
mod shell;
// Starting a session thread, with no opinion about what runs on it: shared by
// the embedded and external-frame executions.
mod session_thread;
// The external-frame execution: a session that renders work produced by a
// JavaScript agent in another process, and links no engine of its own.
#[cfg(feature = "external-frames")]
pub mod external;

#[cfg(all(test, feature = "embedded-v8"))]
mod tests;

pub use registry::{
    HostIngress, HostIngressSendError, host_ingress, lease_surface, lease_surface_tracked,
    lease_surface_with_resource, retire_surface, send_command_to_host,
    send_critical_command_to_host, send_reliable_command_to_host, shutdown_host,
};
pub use session_thread::{HostThread, SpawnedSurfaceHost};
#[cfg(feature = "embedded-v8")]
pub use thread::{spawn_host_thread, spawn_host_thread_tracked};
