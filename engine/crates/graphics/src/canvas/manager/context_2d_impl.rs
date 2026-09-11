//! Per-canvas Skia Canvas2D initialisation + flush helpers.
//!
//! This module used to wire up femtovg; it now wraps the
//! `backend::gl::surface::Canvas2DContext` that owns `SkSurface` +
//! `GrDirectContext` for each live 2D canvas.
//!
//! GL-state hygiene helpers live here too (the `begin_canvas2d_gl_scope`
//! guard that Skia's text-atlas upload and WebGL share an EGL context
//! with mutually-visible state — see the struct doc for the full list).

extern crate khronos_egl as egl;

use glow::HasContext;
use shared::{
    error::{EngineResult, ErrorCode},
    protocol::render_cmd::CanvasId,
};

use super::CanvasManager;
use super::types::ee;
use crate::BoundContext;
use crate::backend::gl::readback::PixelPackState;
use crate::backend::gl::surface::{Canvas2DContext, FboKind};

/// Initialise a Skia-backed Canvas2D context for `canvas_id`.
///
/// Idempotent — returns immediately if the context already exists.
/// Caller is responsible for making the target EGL context current
/// before invocation (which this function then double-checks via
/// `make_current_needed`).
pub(super) fn init_skia_for_canvas(
    cm: &mut CanvasManager,
    canvas_id: CanvasId,
) -> EngineResult<()> {
    cm.make_current_needed(canvas_id)?;

    if cm.contexts_2d.contains_key(&canvas_id) {
        return Ok(());
    }

    // Pull the FBO + size from the canvas entry.  Onscreen canvases
    // (id=1) render into the DrawingBuffer FBO; offscreen pbuffers
    // render into the default framebuffer (FBO 0).
    let (fbo_id, width, height, kind) = {
        let entry = cm.canvases.get(&canvas_id).ok_or_else(|| {
            ee(
                ErrorCode::NotFound,
                format!("canvas not found: id={canvas_id}"),
            )
        })?;
        match &entry.drawing_buffer {
            Some(db) => (db.fbo.0.get(), db.width, db.height, FboKind::DrawingBuffer),
            None => (
                0,
                entry.physical_width,
                entry.physical_height,
                FboKind::DefaultFb,
            ),
        }
    };
    let entry_is_onscreen = {
        let entry = cm
            .canvases
            .get(&canvas_id)
            .expect("looked up on the line above");
        entry.info.is_onscreen
    };

    // An offscreen canvas can share one `GrDirectContext` with every other
    // offscreen canvas instead of owning one. That context is 96% of what an
    // offscreen canvas costs in `Graphics` -- 4.66 MB of 4.86 MB, measured; see
    // `docs/performance/android/multicanvas-fixed-cost.md`.
    //
    // Only offscreen. The onscreen canvas renders into the DrawingBuffer FBO
    // that the present blit reads, and it is one canvas, so it has nothing to
    // share with and everything to lose from being moved off its own context.
    let share_offscreen = !entry_is_onscreen
        && shared::feature_policy::is_enabled(
            shared::feature_policy::FeatureKey::CanvasSharedDirectContext,
        );

    let created = if share_offscreen {
        let (gr_ctx, interface, ctx_tag) = cm.bind_shared_2d_context(canvas_id)?;
        Canvas2DContext::new_shared_offscreen(&gr_ctx, interface, width, height, ctx_tag)
    } else {
        // Scoped so the immutable borrow for the loader ends before `cm` is used
        // mutably below. Skia resolves its GL entry points through the same EGL
        // implementation this manager was built with, rather than through whichever
        // one Skia itself linked.
        let load_gl = |symbol: &str| cm.gl_proc_address(symbol);
        Canvas2DContext::new(fbo_id, width, height, kind, &load_gl)
    };
    let ctx = created.ok_or_else(|| {
        ee(
            ErrorCode::RenderBackendError,
            format!("Skia Canvas2DContext::new failed for canvas_id={canvas_id} ({width}x{height} fbo={fbo_id})"),
        )
    })?;
    cm.contexts_2d.insert(canvas_id, ctx);
    // Rebalance this Session's contexts now that the process-wide denominator
    // changed. Other Sessions' contexts pick the new share up at their own next
    // create or destroy: a Skia context may only be touched from the render thread
    // that owns it.
    for ctx in cm.contexts_2d.values_mut() {
        ctx.rebalance_resource_cache();
    }
    // A freshly-created onscreen (id=1) 2D context invalidates DrawingBuffer
    // bypass: Skia renders into the DrawingBuffer FBO, which the bypass path
    // (single-canvas WebGL optimization) would skip blitting to the window,
    // stranding every 2D draw offscreen. Re-evaluate so bypass turns off and
    // the DrawingBuffer→window blit runs.
    if canvas_id == CanvasId::from(1u32) {
        cm.evaluate_bypass();
    }
    Ok(())
}

/// GL-state snapshot used to keep Skia's text-atlas upload from tripping
/// WebGL state (or vice versa).
///
/// Skia's glyph rasterisation and `glTexSubImage2D` uploads bind the
/// 0th texture unit and `GL_PIXEL_UNPACK_BUFFER = 0`, and assume the
/// default alignment of 4.  WebGL games mutate these freely, so we
/// snapshot on scope entry and restore on drop. PACK row length and skips
/// also belong to content and must not affect Skia's compact CPU reads.
pub(crate) struct Canvas2DGlState {
    active_texture: i32,
    unpack_pbo: Option<<glow::Context as glow::HasContext>::Buffer>,
    unpack_alignment: i32,
    pack: PixelPackState,
}

pub(crate) struct Canvas2DGlScopeGuard {
    gl: *const glow::Context,
    state: Option<Canvas2DGlState>,
    /// Optional shadow-state pointer.  When present, the guard
    /// invalidates the dedup shadow on drop so the next WebGL call
    /// re-issues rather than trusting stale values that Skia has
    /// since mutated outside our handlers.
    gl_shadow: *mut super::types::CanvasGLState,
}

impl Drop for Canvas2DGlScopeGuard {
    fn drop(&mut self) {
        if let Some(state) = self.state.take() {
            let gl = unsafe { &*self.gl };
            unsafe {
                gl.active_texture(state.active_texture as u32);
                gl.bind_buffer(glow::PIXEL_UNPACK_BUFFER, state.unpack_pbo);
                gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, state.unpack_alignment);
            }
            state.pack.restore(gl);
            // After Skia has drawn and we've restored the saved
            // raw-GL bindings, the WebGL dedup shadow still holds
            // whatever it thought before Skia ran.  Skia may have
            // bound different programs, VAOs, FBOs, scissor,
            // stencil, blend equations, textures on other units,
            // etc., none of which went through our handler, so
            // the shadow is stale for every slot except those we
            // explicitly restored.  Wipe everything defensively;
            // the next WebGL draw pays one rebind per state it
            // actually uses, which is cheap compared to a silent
            // wrong-buffer paint.
            if !self.gl_shadow.is_null() {
                let shadow = unsafe { &mut *self.gl_shadow };
                shadow.invalidate_after_external_gl_use();
                // `active_texture` is back to the snapshot value —
                // reflect that in the shadow so the *next*
                // `activeTexture` call for it doesn't trigger a
                // redundant raw GL call. `unpack_alignment` /
                // `pack_alignment` need no equivalent: they dedup
                // through the generic `pixel_store_i32` map, which
                // `invalidate_after_external_gl_use` above already
                // cleared, so the next `pixelStorei` re-issues
                // regardless of pname. The PIXEL_(UN)PACK_BUFFER
                // binding does NOT live in CanvasGLState at all —
                // WebGL 2 code that manipulates it goes through the
                // raw handler directly. We only re-synthesise the
                // state that the shadow actually tracks.
                shadow.active_texture_unit = Some(state.active_texture as u32);
            }
        }
    }
}

/// Save + reset GL state for a Skia text-atlas / image-upload sequence.
/// Returns a guard that restores the WebGL-visible state on drop.
///
/// `gl_shadow` is optional: when `Some`, the guard invalidates and
/// re-syncs the dedup shadow on drop so the next WebGL call can't
/// be fooled by Skia's under-the-hood state mutations.  Pass `None`
/// only from test helpers that don't own a shadow.
pub(super) fn begin_canvas2d_gl_scope(
    gl: &glow::Context,
    gl_shadow: Option<&mut super::types::CanvasGLState>,
) -> Canvas2DGlScopeGuard {
    unsafe {
        let active_texture = gl.get_parameter_i32(glow::ACTIVE_TEXTURE);
        let unpack_pbo = gl.get_parameter_buffer(glow::PIXEL_UNPACK_BUFFER_BINDING);
        let unpack_alignment = gl.get_parameter_i32(glow::UNPACK_ALIGNMENT);
        let pack = PixelPackState::capture(gl);

        gl.bind_buffer(glow::PIXEL_UNPACK_BUFFER, None);
        gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 4);
        pack.set_tight(gl, 4);
        gl.active_texture(glow::TEXTURE0);

        Canvas2DGlScopeGuard {
            gl: gl as *const glow::Context,
            state: Some(Canvas2DGlState {
                active_texture,
                unpack_pbo,
                unpack_alignment,
                pack,
            }),
            gl_shadow: gl_shadow
                .map(|s| s as *mut _)
                .unwrap_or(std::ptr::null_mut()),
        }
    }
}

/// Flush all Canvas2D contexts with pending Skia draws.
///
/// Called at frame-end before `eglSwapBuffers` and also at each
/// Canvas2D → WebGL boundary (`Materialize` op).  Returns the list of
/// canvas IDs that actually flushed, so the render-thread layer can
/// clear its per-layer dirty bit.
pub(super) fn flush_dirty_2d_contexts(cm: &mut CanvasManager) -> EngineResult<Vec<CanvasId>> {
    let saved = cm.bound;

    let dirty_ids: Vec<CanvasId> = cm.dirty_2d.drain().collect();
    let mut flushed_ids = Vec::with_capacity(dirty_ids.len());

    // Canvases that share one `GrDirectContext` are flushed as a group, once.
    //
    // Everything the per-canvas loop below does is per-*context* work wearing a
    // per-canvas name: `flush_and_submit` submits the whole `GrDirectContext`,
    // `reset_gl_state` discards that context's entire cached GL state, and the
    // scope guard reads back the raw GL bindings. Run once per canvas on a
    // shared context, 80 canvases means 80 full submits and 80 state
    // invalidations per frame -- so every canvas after the first redraws from a
    // cache another canvas just threw away. Measured on a Mate 30 Pro, that is
    // what turned an 80-canvas frame from 60 fps into 54.
    //
    // Grouping is not an optimisation of the sharing; it is what sharing means.
    let shared_ids: Vec<CanvasId> = dirty_ids
        .iter()
        .copied()
        .filter(|id| cm.canvas_uses_shared_2d(*id))
        .collect();
    if !shared_ids.is_empty() {
        let group_head = *shared_ids.first().expect("checked non-empty above");
        cm.bind_shared_2d_context(group_head)?;
        // No dedup shadow: the shadow exists so a WebGL batch cannot serve
        // state Skia changed, and nothing but offscreen 2D drawing ever binds
        // this context. See `CanvasManager::bind_shared_2d_context`.
        let gl_ref = &*cm.gl as *const glow::Context;
        // SAFETY: `cm.gl` is not mutated for the duration of this block, and
        // the guard drops before `cm` is used mutably again.
        let _gl_scope = unsafe { begin_canvas2d_gl_scope(&*gl_ref, None) };
        // Submit per canvas, invalidate once for the group.
        //
        // The two halves have opposite best shapes and the earlier versions of
        // this got each one wrong in turn. `reset_gl_state` discards the whole
        // context's cached GL state, so doing it per canvas made every canvas
        // after the first redraw from a cache its predecessor had just thrown
        // away. But collapsing the *submit* to one call per frame was worse in
        // a different way: all 240 render passes then reach the driver only
        // after the last canvas is recorded, so the GPU idles through the whole
        // recording phase. Measured at 240 canvases on a Mate 30 Pro: same CPU
        // (116% vs 118%), 22 fps against 25 -- a pipelining loss, not a CPU one.
        //
        // So: submit as each canvas finishes, which is what lets the GPU start
        // early, and invalidate once at the end because the only thing that
        // dirties this context behind Skia's back is the scope guard's restore
        // on drop.
        for id in &shared_ids {
            if let Some(ctx) = cm.contexts_2d.get_mut(id) {
                ctx.flush_and_submit();
            }
        }
        if let Some(ctx) = cm.contexts_2d.get_mut(&group_head) {
            ctx.reset_gl_state();
        }
        flushed_ids.extend_from_slice(&shared_ids);
    }

    for id in dirty_ids {
        if cm.canvas_uses_shared_2d(id) {
            continue; // flushed as part of the shared group above
        }
        if !cm.contexts_2d.contains_key(&id) {
            continue;
        }
        cm.make_current_needed(id)?;
        // Borrow gymnastics: `begin_canvas2d_gl_scope` needs both the
        // GL context (borrowed &cm.gl) and mutable access to the
        // per-canvas dedup shadow.  Take them through a single
        // `split_gl_and_shadow_for` helper that re-borrows disjoint
        // fields of `cm` so the borrow checker accepts the call.
        let (gl_ref, shadow_ref) = {
            let cm_mut: &mut CanvasManager = cm;
            (
                &*cm_mut.gl as *const glow::Context,
                cm_mut.gl_state.entry(id).or_default() as *mut _,
            )
        };
        // SAFETY: both pointers come from distinct fields of `cm`
        // that are not mutated for the duration of this block.  The
        // guard drops before `cm` is touched again.
        let _gl_scope = unsafe { begin_canvas2d_gl_scope(&*gl_ref, Some(&mut *shadow_ref)) };
        if let Some(ctx) = cm.contexts_2d.get_mut(&id) {
            ctx.flush_and_submit();
            // Drop Skia's internal GL-state tracking so subsequent WebGL
            // / DrawingBuffer-blit code doesn't see stale assumptions.
            ctx.reset_gl_state();
            flushed_ids.push(id);
        }
    }

    match saved {
        BoundContext::Resource => cm.bind_resource()?,
        // Restoring the shared 2D context is a rebind, not a recreate: the
        // context already exists if it was ever bound.
        BoundContext::Shared2D(id) => {
            cm.bind_shared_2d_context(id)?;
        }
        BoundContext::Canvas(id) => {
            if cm.canvases.contains_key(&id) {
                cm.make_current_needed(id)?;
            } else {
                cm.bind_resource()?;
            }
        }
    }
    Ok(flushed_ids)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::gl::readback_test_gl as test_gl;

    #[test]
    fn internal_readback_canvas2d_scope_isolates_and_restores_pack_layout() {
        let gl = test_gl::context();
        for original in [
            test_gl::Bindings {
                pack: [8, 9, 2, 3],
                pack_buffer: 17,
                active_texture: glow::TEXTURE0 + 3,
                unpack_buffer: 19,
                unpack_alignment: 8,
                ..Default::default()
            },
            test_gl::Bindings::default(),
        ] {
            test_gl::set_bindings(original);
            {
                let _scope = begin_canvas2d_gl_scope(&gl, None);
                let actual = test_gl::bindings();
                assert_eq!(actual.pack, [4, 0, 0, 0]);
                assert_eq!(actual.pack_buffer, 0);
                assert_eq!(actual.unpack_buffer, 0);
                // Skia can mutate state directly; restoration must include
                // slots which did not need resetting on entry (default case).
                test_gl::set_bindings(test_gl::Bindings {
                    pack: [2, 5, 7, 1],
                    pack_buffer: 29,
                    ..Default::default()
                });
            }
            assert_eq!(test_gl::bindings(), original);
        }
    }
}
