//! Chromium-style DrawingBuffer: an intermediate FBO that WebGL renders to
//! instead of the native window surface directly.
//!
//! On every frame present the color attachment is blitted to the real window
//! surface via `glBlitFramebuffer` (ES 3.0) before `eglSwapBuffers`.

use glow::HasContext;
use shared::error::{EngineResult, ErrorCode};

use super::types::ee;

/// Intermediate render target for the onscreen canvas.
pub(crate) struct DrawingBuffer {
    /// FBO that WebGL commands target when `bindFramebuffer(null)` is called.
    pub fbo: glow::NativeFramebuffer,
    /// Color attachment (RGBA8 texture).
    pub color_tex: glow::NativeTexture,
    /// Depth + stencil attachment (renderbuffer).
    pub depth_stencil_rb: glow::NativeRenderbuffer,
    /// Current buffer width in physical pixels.
    pub width: u32,
    /// Current buffer height in physical pixels.
    pub height: u32,
}

/// Create a new DrawingBuffer at the given dimensions.
///
/// The caller must ensure an EGL context is current.
pub(crate) fn create(gl: &glow::Context, width: u32, height: u32) -> EngineResult<DrawingBuffer> {
    unsafe {
        // Save current bindings to restore later.
        let prev_tex = gl.get_parameter_i32(glow::TEXTURE_BINDING_2D) as u32;
        let prev_rb = gl.get_parameter_i32(glow::RENDERBUFFER_BINDING) as u32;
        let prev_fbo = gl.get_parameter_i32(glow::FRAMEBUFFER_BINDING) as u32;

        let fbo = gl.create_framebuffer().map_err(|e| {
            ee(
                ErrorCode::RenderBackendError,
                format!("DrawingBuffer: create_framebuffer failed: {e}"),
            )
        })?;
        let color_tex = gl.create_texture().map_err(|e| {
            gl.delete_framebuffer(fbo);
            ee(
                ErrorCode::RenderBackendError,
                format!("DrawingBuffer: create_texture failed: {e}"),
            )
        })?;
        let depth_stencil_rb = gl.create_renderbuffer().map_err(|e| {
            gl.delete_framebuffer(fbo);
            gl.delete_texture(color_tex);
            ee(
                ErrorCode::RenderBackendError,
                format!("DrawingBuffer: create_renderbuffer failed: {e}"),
            )
        })?;

        // Allocate color texture.
        gl.bind_texture(glow::TEXTURE_2D, Some(color_tex));
        gl.tex_parameter_i32(
            glow::TEXTURE_2D,
            glow::TEXTURE_MIN_FILTER,
            glow::NEAREST as i32,
        );
        gl.tex_parameter_i32(
            glow::TEXTURE_2D,
            glow::TEXTURE_MAG_FILTER,
            glow::NEAREST as i32,
        );
        gl.tex_parameter_i32(
            glow::TEXTURE_2D,
            glow::TEXTURE_WRAP_S,
            glow::CLAMP_TO_EDGE as i32,
        );
        gl.tex_parameter_i32(
            glow::TEXTURE_2D,
            glow::TEXTURE_WRAP_T,
            glow::CLAMP_TO_EDGE as i32,
        );
        gl.tex_image_2d(
            glow::TEXTURE_2D,
            0,
            glow::RGBA as i32,
            width as i32,
            height as i32,
            0,
            glow::RGBA,
            glow::UNSIGNED_BYTE,
            glow::PixelUnpackData::Slice(None),
        );

        // Allocate depth+stencil renderbuffer.
        gl.bind_renderbuffer(glow::RENDERBUFFER, Some(depth_stencil_rb));
        gl.renderbuffer_storage(
            glow::RENDERBUFFER,
            glow::DEPTH24_STENCIL8,
            width as i32,
            height as i32,
        );

        // Assemble FBO.
        gl.bind_framebuffer(glow::FRAMEBUFFER, Some(fbo));
        gl.framebuffer_texture_2d(
            glow::FRAMEBUFFER,
            glow::COLOR_ATTACHMENT0,
            glow::TEXTURE_2D,
            Some(color_tex),
            0,
        );
        gl.framebuffer_renderbuffer(
            glow::FRAMEBUFFER,
            glow::DEPTH_STENCIL_ATTACHMENT,
            glow::RENDERBUFFER,
            Some(depth_stencil_rb),
        );

        let status = gl.check_framebuffer_status(glow::FRAMEBUFFER);
        if status != glow::FRAMEBUFFER_COMPLETE {
            gl.bind_framebuffer(glow::FRAMEBUFFER, None);
            gl.delete_framebuffer(fbo);
            gl.delete_texture(color_tex);
            gl.delete_renderbuffer(depth_stencil_rb);
            return Err(ee(
                ErrorCode::RenderBackendError,
                format!("DrawingBuffer: framebuffer incomplete (status=0x{status:X})"),
            ));
        }

        // Ensure framebuffer blit path is usable on this context/driver.
        // `glow::blit_framebuffer` panics if the symbol is not loaded; on
        // some GLES2 stacks this can happen. We probe once here and fall back
        // to direct-to-surface rendering if unavailable.
        clear_gl_errors(gl);
        let blit_probe = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            gl.bind_framebuffer(glow::READ_FRAMEBUFFER, Some(fbo));
            gl.bind_framebuffer(glow::DRAW_FRAMEBUFFER, None);
            gl.blit_framebuffer(
                0,
                0,
                1,
                1,
                0,
                0,
                1,
                1,
                glow::COLOR_BUFFER_BIT,
                glow::NEAREST,
            );
        }));
        let blit_err = gl.get_error();
        if blit_probe.is_err() || blit_err != glow::NO_ERROR {
            gl.bind_framebuffer(glow::FRAMEBUFFER, None);
            gl.delete_framebuffer(fbo);
            gl.delete_texture(color_tex);
            gl.delete_renderbuffer(depth_stencil_rb);
            return Err(ee(
                ErrorCode::RenderBackendError,
                if blit_probe.is_err() {
                    "DrawingBuffer: glBlitFramebuffer not available in this GL context".to_string()
                } else {
                    format!(
                        "DrawingBuffer: glBlitFramebuffer probe failed (gl_error=0x{blit_err:X})"
                    )
                },
            ));
        }
        gl.bind_framebuffer(glow::FRAMEBUFFER, Some(fbo));

        // Restore previous bindings.
        restore_texture_binding(gl, prev_tex);
        restore_renderbuffer_binding(gl, prev_rb);
        // Leave the DrawingBuffer FBO bound — caller expects it for onscreen canvas.
        if prev_fbo != 0 {
            // Only restore if there was a non-default FBO (shouldn't happen at init).
            // We intentionally keep our FBO bound as the "default" for onscreen.
        }

        Ok(DrawingBuffer {
            fbo,
            color_tex,
            depth_stencil_rb,
            width,
            height,
        })
    }
}

/// Resize the DrawingBuffer storage without recreating GL objects.
pub(crate) fn resize(
    gl: &glow::Context,
    db: &mut DrawingBuffer,
    new_w: u32,
    new_h: u32,
) -> EngineResult<()> {
    if db.width == new_w && db.height == new_h {
        return Ok(());
    }

    unsafe {
        let prev_tex = gl.get_parameter_i32(glow::TEXTURE_BINDING_2D) as u32;
        let prev_rb = gl.get_parameter_i32(glow::RENDERBUFFER_BINDING) as u32;

        // Re-allocate color texture.
        gl.bind_texture(glow::TEXTURE_2D, Some(db.color_tex));
        gl.tex_image_2d(
            glow::TEXTURE_2D,
            0,
            glow::RGBA as i32,
            new_w as i32,
            new_h as i32,
            0,
            glow::RGBA,
            glow::UNSIGNED_BYTE,
            glow::PixelUnpackData::Slice(None),
        );

        // Re-allocate depth+stencil renderbuffer.
        gl.bind_renderbuffer(glow::RENDERBUFFER, Some(db.depth_stencil_rb));
        gl.renderbuffer_storage(
            glow::RENDERBUFFER,
            glow::DEPTH24_STENCIL8,
            new_w as i32,
            new_h as i32,
        );

        // Re-attach (required on some drivers after storage reallocation).
        gl.bind_framebuffer(glow::FRAMEBUFFER, Some(db.fbo));
        gl.framebuffer_texture_2d(
            glow::FRAMEBUFFER,
            glow::COLOR_ATTACHMENT0,
            glow::TEXTURE_2D,
            Some(db.color_tex),
            0,
        );
        gl.framebuffer_renderbuffer(
            glow::FRAMEBUFFER,
            glow::DEPTH_STENCIL_ATTACHMENT,
            glow::RENDERBUFFER,
            Some(db.depth_stencil_rb),
        );

        let status = gl.check_framebuffer_status(glow::FRAMEBUFFER);
        if status != glow::FRAMEBUFFER_COMPLETE {
            return Err(ee(
                ErrorCode::RenderBackendError,
                format!("DrawingBuffer resize: framebuffer incomplete (status=0x{status:X})"),
            ));
        }

        // Restore texture/renderbuffer bindings.
        restore_texture_binding(gl, prev_tex);
        restore_renderbuffer_binding(gl, prev_rb);
        // Leave DrawingBuffer FBO bound.
    }

    db.width = new_w;
    db.height = new_h;
    Ok(())
}

/// Destroy the DrawingBuffer and release all GL resources.
pub(crate) fn destroy(gl: &glow::Context, db: DrawingBuffer) {
    unsafe {
        gl.delete_framebuffer(db.fbo);
        gl.delete_texture(db.color_tex);
        gl.delete_renderbuffer(db.depth_stencil_rb);
    }
}

/// Blit the DrawingBuffer color attachment to the real default framebuffer (FBO 0).
///
/// Uses `glBlitFramebuffer` (ES 3.0). `surface_w`/`surface_h` are the actual
/// EGL window surface dimensions (the blit destination).
///
/// If the DrawingBuffer FBO has become incomplete (e.g. the game's WebGL code
/// modified its attachments), this function re-attaches the original textures
/// before retrying the blit.
#[inline]
/// Copy the window surface into the DrawingBuffer.
///
/// The reverse of [`blit_to_surface`], and it exists for one moment: the frame
/// in which the engine stops bypassing the DrawingBuffer because the game asked
/// to read the default framebuffer. Until then WebGL has been drawing straight
/// to the surface, so the DrawingBuffer holds nothing; binding it and answering
/// the read from it returns an empty buffer for pixels the game just drew.
/// `signal_default_fbo_readback` documents this snapshot; this is it.
///
/// Always a full-surface copy. There is no damage history that spans the mode
/// change -- that is exactly why the mode change discards it -- so there is no
/// smaller correct rectangle.
pub(crate) fn blit_from_surface(
    gl: &glow::Context,
    db: &DrawingBuffer,
    surface_w: u32,
    surface_h: u32,
) -> bool {
    if surface_w == 0 || surface_h == 0 || db.width == 0 || db.height == 0 {
        return false;
    }
    let mut succeeded = false;
    unsafe {
        let saved_read = gl.get_parameter_framebuffer(glow::READ_FRAMEBUFFER_BINDING);
        let saved_draw = gl.get_parameter_framebuffer(glow::DRAW_FRAMEBUFFER_BINDING);
        clear_gl_errors(gl);

        // Same reason as the forward blit: `glBlitFramebuffer` writes through
        // the scissor test, and a game routinely leaves one enabled over a
        // sub-window box. Restored on every exit path below.
        let scissor_was_enabled = gl.is_enabled(glow::SCISSOR_TEST);
        if scissor_was_enabled {
            gl.disable(glow::SCISSOR_TEST);
        }

        'blit: {
            // READ from the window surface (FBO 0), DRAW to the DrawingBuffer.
            gl.bind_framebuffer(glow::READ_FRAMEBUFFER, None);
            gl.bind_framebuffer(glow::DRAW_FRAMEBUFFER, Some(db.fbo));

            let err = gl.get_error();
            if err != glow::NO_ERROR {
                tracing::warn!(
                    "DrawingBuffer reverse blit: bind failed (gl_error=0x{err:X}), db={}x{} surface={}x{}",
                    db.width,
                    db.height,
                    surface_w,
                    surface_h
                );
                break 'blit;
            }

            let status = gl.check_framebuffer_status(glow::DRAW_FRAMEBUFFER);
            if status != glow::FRAMEBUFFER_COMPLETE {
                tracing::warn!(
                    "DrawingBuffer reverse blit: destination FBO incomplete (0x{status:X})"
                );
                break 'blit;
            }

            // NEAREST when the rectangles match, which is the ordinary case;
            // `glBlitFramebuffer` rejects a scaling blit asking for NEAREST on
            // some drivers, and a scaled snapshot is better than none.
            let filter = if db.width == surface_w && db.height == surface_h {
                glow::NEAREST
            } else {
                glow::LINEAR
            };
            gl.blit_framebuffer(
                0,
                0,
                surface_w as i32,
                surface_h as i32,
                0,
                0,
                db.width as i32,
                db.height as i32,
                glow::COLOR_BUFFER_BIT,
                filter,
            );
            let err = gl.get_error();
            if err != glow::NO_ERROR {
                tracing::warn!("DrawingBuffer reverse blit: blit failed (gl_error=0x{err:X})");
                break 'blit;
            }
            succeeded = true;
        }

        // Restore native READ and DRAW independently, on success and failure.
        // Logical client bindings have not changed and must not be reset.
        if saved_read == saved_draw {
            gl.bind_framebuffer(glow::FRAMEBUFFER, saved_read);
        } else {
            gl.bind_framebuffer(glow::READ_FRAMEBUFFER, saved_read);
            gl.bind_framebuffer(glow::DRAW_FRAMEBUFFER, saved_draw);
        }
        if scissor_was_enabled {
            gl.enable(glow::SCISSOR_TEST);
        }
    }
    succeeded
}

pub(crate) fn blit_to_surface(
    gl: &glow::Context,
    db: &DrawingBuffer,
    surface_w: u32,
    surface_h: u32,
    plan: &crate::present_damage::BlitPlan,
) -> bool {
    use crate::present_damage::BlitPlan;
    let mut succeeded = false;
    unsafe {
        // Clear any pending GL error.
        clear_gl_errors(gl);

        // `glBlitFramebuffer` writes to the DRAW framebuffer through the
        // scissor test. Games routinely leave GL_SCISSOR_TEST enabled with a
        // sub-window box (e.g. Phaser scissors to its 960x640 render size); if
        // we blit with that still active, the present is clipped to that box —
        // the game lands in a corner of the window with the rest black. The
        // blit is a system-level present, so disable scissor for the blit and
        // restore the game's enable state from the single cleanup epilogue
        // below — which runs on every exit path, including early failures.
        let scissor_was_enabled = gl.is_enabled(glow::SCISSOR_TEST);
        if scissor_was_enabled {
            gl.disable(glow::SCISSOR_TEST);
        }

        'blit: {
            // READ from DrawingBuffer, DRAW to window surface (FBO 0). Bound
            // once for every rect in the plan.
            gl.bind_framebuffer(glow::READ_FRAMEBUFFER, Some(db.fbo));
            gl.bind_framebuffer(glow::DRAW_FRAMEBUFFER, None);

            let err = gl.get_error();
            if err != glow::NO_ERROR {
                tracing::warn!(
                    "DrawingBuffer blit: bind failed (gl_error=0x{err:X}), db={}x{} surface={}x{}",
                    db.width,
                    db.height,
                    surface_w,
                    surface_h
                );
                gl.bind_framebuffer(glow::FRAMEBUFFER, None);
                break 'blit;
            }

            // Check READ framebuffer completeness before the first blit.  Game
            // WebGL code can accidentally modify the DrawingBuffer FBO
            // attachments (e.g. framebufferTexture2D on "null" framebuffer),
            // making it incomplete.
            let status = gl.check_framebuffer_status(glow::READ_FRAMEBUFFER);
            if status != glow::FRAMEBUFFER_COMPLETE {
                // Try to heal: re-attach original color + depth/stencil.
                gl.bind_framebuffer(glow::FRAMEBUFFER, Some(db.fbo));
                gl.framebuffer_texture_2d(
                    glow::FRAMEBUFFER,
                    glow::COLOR_ATTACHMENT0,
                    glow::TEXTURE_2D,
                    Some(db.color_tex),
                    0,
                );
                gl.framebuffer_renderbuffer(
                    glow::FRAMEBUFFER,
                    glow::DEPTH_STENCIL_ATTACHMENT,
                    glow::RENDERBUFFER,
                    Some(db.depth_stencil_rb),
                );
                let healed = gl.check_framebuffer_status(glow::FRAMEBUFFER);
                if healed != glow::FRAMEBUFFER_COMPLETE {
                    tracing::warn!(
                        "DrawingBuffer blit: FBO incomplete (0x{status:X}), re-attach failed (0x{healed:X})"
                    );
                    gl.bind_framebuffer(glow::FRAMEBUFFER, None);
                    break 'blit;
                }
                tracing::debug!("DrawingBuffer blit: FBO healed after re-attach");
                // Re-bind for blit.
                gl.bind_framebuffer(glow::READ_FRAMEBUFFER, Some(db.fbo));
                gl.bind_framebuffer(glow::DRAW_FRAMEBUFFER, None);
            }

            match plan {
                BlitPlan::Full { linear } => {
                    // Legacy / scaled path: one blit over the whole surface,
                    // preserving the existing filter (LINEAR for scaling).
                    let filter = if *linear { glow::LINEAR } else { glow::NEAREST };
                    gl.blit_framebuffer(
                        0,
                        0,
                        db.width as i32,
                        db.height as i32,
                        0,
                        0,
                        surface_w as i32,
                        surface_h as i32,
                        glow::COLOR_BUFFER_BIT,
                        filter,
                    );
                }
                BlitPlan::Rects(rects) => {
                    // Same-size partial repair: identical lower-left source and
                    // destination coordinates (no scaling, no Y flip) with
                    // NEAREST. Up to four bounded rect ops; no full retry after
                    // a partial declaration succeeded.
                    for r in rects.rects() {
                        let (rx, ry, rw, rh) = r.xywh();
                        let x0 = rx;
                        let y0 = ry;
                        let x1 = rx + rw;
                        let y1 = ry + rh;
                        gl.blit_framebuffer(
                            x0,
                            y0,
                            x1,
                            y1,
                            x0,
                            y0,
                            x1,
                            y1,
                            glow::COLOR_BUFFER_BIT,
                            glow::NEAREST,
                        );
                    }
                }
            }

            let err = gl.get_error();
            if err != glow::NO_ERROR {
                tracing::warn!(
                    "DrawingBuffer blit: glBlitFramebuffer failed (gl_error=0x{err:X}), db={}x{} surface={}x{}",
                    db.width,
                    db.height,
                    surface_w,
                    surface_h
                );
                break 'blit;
            }
            succeeded = true;
        }

        // Single cleanup epilogue: restore the game's scissor-test enable state
        // on every exit path — normal completion, bind failure, and FBO-heal
        // failure alike. We only touched the enable flag; the game reprograms
        // the scissor box itself. No glInvalidate*: clean destination and
        // persistent DrawingBuffer pixels are required for future buffer-age
        // repair, so we must not discard either framebuffer's contents.
        if scissor_was_enabled {
            gl.enable(glow::SCISSOR_TEST);
        }
    }
    succeeded
}

/// Restore a texture binding from a raw GL integer (0 = unbind).
unsafe fn restore_texture_binding(gl: &glow::Context, prev: u32) {
    unsafe {
        let handle = if prev == 0 {
            None
        } else {
            Some(glow::NativeTexture(std::num::NonZeroU32::new_unchecked(
                prev,
            )))
        };
        gl.bind_texture(glow::TEXTURE_2D, handle);
    }
}

/// Restore a renderbuffer binding from a raw GL integer (0 = unbind).
unsafe fn restore_renderbuffer_binding(gl: &glow::Context, prev: u32) {
    unsafe {
        let handle = if prev == 0 {
            None
        } else {
            Some(glow::NativeRenderbuffer(
                std::num::NonZeroU32::new_unchecked(prev),
            ))
        };
        gl.bind_renderbuffer(glow::RENDERBUFFER, handle);
    }
}

/// Drain pending GL errors without risking an infinite loop on broken drivers.
#[inline]
unsafe fn clear_gl_errors(gl: &glow::Context) {
    for _ in 0..16 {
        if unsafe { gl.get_error() } == glow::NO_ERROR {
            break;
        }
    }
}
