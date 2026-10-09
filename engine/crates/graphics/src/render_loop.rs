//! Render-loop state container.
//!
//! Historically the render-loop body in `render_thread::RenderThread::
//! spawn` was one 700-line closure with three nested closures
//! (`handle_one_cmd`, `drain_cmds`, `present_frame_and_signal_raf`)
//! that threaded **eleven** arguments through their signatures.
//! Every new command handler added a twelfth parameter.  This module
//! bundles that mutable state into a single struct so handlers can
//! be refactored into methods with a single `&mut self` receiver,
//! with the migration happening incrementally — adding a new command
//! variant no longer needs to touch every closure signature.
//!
//! Only the bundling is landed in this revision; migrating the
//! closure bodies to methods is a mechanical, commit-by-commit
//! follow-up.  The struct is already threaded into the
//! `RenderThread::spawn` body so future migrations can land one
//! handler at a time without having to re-bundle the state.
//!
//! See `render_thread.rs` top-of-file doc comment (`P2-1`) for the
//! tracked migration plan.

use std::time::Instant;

use crate::canvas2d_dispatcher::Renderer2d;
use crate::frame_scheduler::{FrameScheduler, SoftwareFrameClock};
use crate::render_server::RenderServer;
use crate::surface_system::SurfaceSystem;
use crate::{CanvasHandler, CanvasManager, RendererGL};

/// Owns every mutable per-loop value the render thread juggles.
///
/// Fields are `pub(crate)` so handler methods (living here or in
/// `render_thread.rs` during the gradual migration) can reach the
/// individual sub-components without going through accessors that
/// would force their own lifetime/borrow patterns.
///
/// One instance exists per spawned render thread.  Dropping the
/// struct tears down every child (CanvasManager destroys EGL
/// contexts; Renderer2d releases the `Arc<Mutex<TextContext>>`;
/// etc.), which is why `RenderLoopState` is deliberately not
/// `Clone` or `Default` — constructing it is a coordinated
/// startup sequence in `RenderThread::spawn`.
// Unconstructed today: the incremental migration this module was built
// for (see the module doc above) hasn't landed its first handler yet, so
// `render_thread.rs` still threads its own local state and its own
// private `LoopCtl` rather than this one.
#[allow(dead_code)]
pub(crate) struct RenderLoopState {
    pub(crate) cm: CanvasManager,
    pub(crate) canvas_handler: CanvasHandler,
    pub(crate) renderer_2d: Renderer2d,
    pub(crate) renderer_gl: RendererGL,
    pub(crate) render_server: RenderServer,
    pub(crate) frame_scheduler: FrameScheduler,
    pub(crate) surface_system: SurfaceSystem,

    /// Engine-paced frame clock for hosts with no external vsync
    /// source.  Never armed when one is present.
    pub(crate) frame_clock: SoftwareFrameClock,
    pub(crate) fps: u32,
    pub(crate) dirty: bool,
    pub(crate) paused: bool,
    /// Set when `swap_buffers` reports `EGL_CONTEXT_LOST` or the
    /// `GL_KHR_robustness` poll reports a reset.  Checked
    /// at the top of the next iteration; recovery is deferred
    /// there so the swap path stays lean.
    pub(crate) needs_context_recovery: bool,

    // ---- FPS + latency stats ----
    pub(crate) frame_count: u32,
    pub(crate) fps_timer: Instant,
    pub(crate) last_frame_time: Instant,
    pub(crate) first_frame_recorded: bool,
}

/// Control-flow hint returned by each command handler.  The
/// render-loop driver translates `Shutdown` into a clean
/// teardown.  Equivalent to the private `LoopCtl` previously
/// declared inside the `RenderThread::spawn` body; extracted so
/// the struct and handlers can share a single vocabulary.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LoopCtl {
    Continue,
    Shutdown,
}

// The dispatch body lives in `render_thread::dispatch_one_cmd` so
// the closure body can stay where it has always been (diff
// minimisation for reviewers); only its signature changes from
// 11 threaded arguments to a single `&mut RenderLoopState` plus
// the externally-owned `gl` / `events` / `debug_stats` handles.
