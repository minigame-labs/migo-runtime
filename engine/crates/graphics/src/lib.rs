//! # Graphics Rendering Module
//!
//! Canvas2D + WebGL rendering for the Migo engine.
//!
//! ## Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────────────┐
//! │                           JS Thread                                  │
//! │                                                                      │
//! │   RAF → ctx.fillRect() → ctx.drawImage() → ... → RAF ends           │
//! │         UnifiedFrameCollector batches all commands per frame        │
//! └────────────────────────────────────────────┬─────────────────────────┘
//!                                              │
//!                          FramePacket (single IPC per frame)
//!                                              │
//!                                              ▼
//! ┌─────────────────────────────────────────────────────────────────────┐
//! │                         Render Thread                                │
//! │                                                                      │
//! │   RenderThread receives Canvas2DBatch / GL / Canvas commands        │
//! │   → backend::gl (Skia Canvas2D via Ganesh GL)                       │
//! │   → renderergl (WebGL via glow)                                     │
//! │   → EGL context management via canvas module                        │
//! └─────────────────────────────────────────────────────────────────────┘
//! ```
//!
//! ## Module Structure
//!
//! - [`render_thread`]: Render thread loop and command dispatch.
//! - [`canvas`]: `CanvasManager` (EGL contexts, DrawingBuffer FBO, image
//!   registry).
//! - [`backend`]: Backend-specific rendering glue.  Only `backend::gl`
//!   (Skia Ganesh on GL ES 3.0) is implemented today; the module is
//!   organised as if it were pluggable because we may add a Vulkan /
//!   wgpu backend later, but there is **no** `RenderBackend` trait
//!   abstraction and no runtime choice between backends yet — the
//!   name is aspirational, not plug-and-play.  See
//!   `AUDIT.md` P2-2 for the full-trait roadmap.
//! - [`renderergl`]: WebGL 1.0 / 2.0 command handler (glow-backed).

#[cfg(all(feature = "profile-full", feature = "profile-slim"))]
compile_error!("profile-full and profile-slim are mutually exclusive");

#[cfg(all(feature = "embed_icudtl", feature = "external_icudtl"))]
compile_error!("embed_icudtl and external_icudtl are mutually exclusive");

/// Whether this concrete graphics artifact contains ICU data.
///
/// Feature tests are crate-local in Cargo: downstream crates must query this
/// exported capability instead of repeating `cfg(feature = "embed_icudtl")`,
/// which would inspect their own unrelated feature namespace.
pub const EMBEDS_ICU_DATA: bool = cfg!(feature = "embed_icudtl");

// Section 7.3's steady-state allocation gate reads this. `#[cfg(test)]` scopes it
// to this crate's own test binary: a `#[global_allocator]` is unique per binary, so
// one declared unconditionally here would follow the library into every shipped
// cdylib. Deleting it does not make the gates pass silently -- each burst proves the
// allocator is installed before it trusts a zero count.
#[cfg(test)]
#[global_allocator]
static COUNTING_ALLOCATOR: migo_alloc_probe::CountingAllocator =
    migo_alloc_probe::CountingAllocator::system();

pub mod atlas;
pub mod atrace;
#[doc(hidden)]
pub mod backend;
mod canvas;
mod canvas2d_dispatcher;
pub(crate) mod canvas_keyed;
pub mod compressed_upload;
pub(crate) mod damage_effect;
pub mod device_caps;
pub mod device_profile;
pub mod dirty_region;
pub mod draw_atlas;
pub mod egl_platform;
pub mod frame_capture;
pub mod frame_scheduler;
pub mod image_decode_ahb;
mod legacy_frame_bridge;
pub mod render_diagnostics;
mod render_frame_state;
pub(crate) mod render_loop;
mod render_server;
mod render_thread;
mod render_wait;
mod renderergl;
pub(crate) mod shader_cache;
mod surface_binding;
// Crate-private, and that is the point rather than tidiness.
//
// `SurfaceSystem` is the Surface lifecycle state machine, and it belongs to the thread
// that presents. `RenderService` used to keep a second one on the session thread: seven
// write sites, zero reads, a full state machine nobody consulted. Two copies of one state
// machine is how two paths come to disagree about it, and this branch spent its whole
// length on exactly that class of defect.
//
// Unreachable from `core` is stronger than a test asserting the session does not hold one:
// re-adding the field does not compile.
pub(crate) mod surface_system;
pub mod text_measurer_impl;
pub mod texture_import;
// `upload_policy` was removed rather than wired up, and the reason belongs here
// so it is not rebuilt.
//
// It offered one `select()` returning an `UploadStrategy` for all three upload
// paths — AHB zero-copy, compressed GPU, RGBA PBO/direct — and its module doc
// described that centralisation in the past tense, as work already done. It had
// no callers at all: the three decisions stayed where they were, in
// `canvas/manager/image.rs`, `pbo_upload::TextureUploadPath::select`, and
// `compressed_upload::CompressedFormatSupport::is_supported`. A reader who
// believed the doc would have thought changing one of those changed all three.
//
// Wiring it up would also have been wrong, which is the part worth keeping. Its
// premise was that the choice is a pure function of static device capabilities,
// and it is not:
//
// * `load_ahb_image` additionally consults `gpu_caps.snapshot().ahb`, a
//   *runtime* flag that a rejected import turns off for the rest of the session.
//   `UploadInputs` had no field for it, so a wired-up `select` would have kept
//   returning `AndroidHardwareBuffer` after the driver refused one.
// * That same path falls back *into* the RGBA path when the import fails, so its
//   decision is entangled with its own error handling rather than made up front.
// * `pbo_upload` tiers PBO against direct by image size internally, after the
//   strategy would already have been chosen.
//
// A single up-front selector cannot express a decision that depends on runtime
// state and on the outcome of attempting it. Its `Fallback` variant — never
// constructed, documented as "returned when no specialised path applies" — was
// the shape of that gap showing through.
pub(crate) mod upload_server;
pub mod upload_thread;
mod webgl_gpu_budget;

pub(crate) mod present_damage;
pub(crate) use canvas::*;
pub(crate) use legacy_frame_bridge::LegacyFrameBridge;
pub use render_server::RenderServer;
pub use render_thread::*;

pub(crate) use renderergl::*;

/// Tracks which EGL context is currently bound.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub(crate) enum BoundContext {
    /// Shared resource context (texture uploads, shader compilation).
    Resource,
    /// A specific canvas's context.
    Canvas(shared::protocol::render_cmd::CanvasId),
    /// The context every offscreen Canvas2D surface shares, together with the
    /// canvas currently being drawn on it.
    ///
    /// The id is not decoration. Sharing makes the *EGL context* common, but
    /// "which canvas is current" stays meaningful: the scissor bookkeeping and
    /// the per-canvas GL dedup shadow are keyed by it, and a variant without it
    /// made `current_canvas_id()` answer `None` while a canvas was very much
    /// being drawn. That tripped `restore_scissor`'s `debug_assert` -- which is
    /// compiled out of release, so on a device it would have restored scissor
    /// state against the wrong record instead of saying so.
    Shared2D(shared::protocol::render_cmd::CanvasId),
}

#[cfg(test)]
#[test]
fn defers_jobs_when_budget_is_exhausted() {
    upload_server::assert_defers_jobs_when_budget_is_exhausted();
}

#[cfg(test)]
#[test]
fn bridge_flushes_pending_ops_without_packet_level_present_boundary() {
    legacy_frame_bridge::assert_bridge_flushes_pending_ops_without_packet_level_present_boundary();
}

#[cfg(test)]
#[test]
fn hits_60fps_on_90hz_without_jittering_to_45fps() {
    frame_scheduler::assert_hits_60fps_on_90hz_without_jittering_to_45fps();
}

#[cfg(test)]
#[test]
fn session_relative_time_starts_from_first_vsync() {
    frame_scheduler::assert_session_relative_time_starts_from_first_vsync();
}

#[cfg(test)]
#[test]
fn does_not_present_when_surface_is_not_ready() {
    frame_scheduler::assert_does_not_present_when_surface_is_not_ready();
}

#[cfg(test)]
#[test]
fn gl_only_packet_triggers_present_when_batch_hits_onscreen() {
    use shared::FramePacket;

    let packet = FramePacket::for_gl_batch(0, 0.0, Vec::new().into());

    let mut hit_count = 0u32;
    let should_present = render_thread::execute_frame_packet_with_present_tracking(
        packet,
        &mut hit_count,
        |_state, _payload| -> bool {
            panic!("should not receive canvas batch in GL-only packet");
        },
        |state, _payload| -> bool {
            *state += 1;
            true
        },
    );

    assert!(should_present);
    assert_eq!(hit_count, 1);
}

#[cfg(test)]
#[test]
fn gl_only_packet_no_present_when_batch_is_offscreen() {
    use shared::FramePacket;

    let packet = FramePacket::for_gl_batch(0, 0.0, Vec::new().into());

    let should_present = render_thread::execute_frame_packet_with_present_tracking(
        packet,
        &mut (),
        |_, _| -> bool { panic!("no canvas batch expected") },
        |_, _| -> bool { false },
    );

    assert!(!should_present);
}

#[cfg(test)]
#[test]
fn mixed_canvas2d_and_gl_packet_unions_present_signals() {
    use shared::protocol::render_cmd::{Canvas2DCmd, CanvasBatchPayload};
    use shared::{FrameOp, FramePacketBuilder};

    let packet = FramePacketBuilder::new(0, 0.0)
        .push(FrameOp::BeginFrame)
        .push(FrameOp::CanvasBatch(CanvasBatchPayload {
            canvas_id: 1,
            commands: vec![Canvas2DCmd::Save].into(),
            present: true,
            dirty_rect: None,
        }))
        .push(FrameOp::GlBatch(
            shared::protocol::render_cmd::GlBatchPayload {
                commands: Vec::new().into(),
            },
        ))
        .finish();

    let should_present = render_thread::execute_frame_packet_with_present_tracking(
        packet,
        &mut (0u32, 0u32),
        |state, _| -> bool {
            state.0 += 1;
            true
        },
        |state, _| -> bool {
            state.1 += 1;
            true
        },
    );

    assert!(should_present);
}
