//! Chromium-style DrawingBuffer: an intermediate FBO that WebGL renders to
//! instead of the native window surface directly.
//!
//! On every frame present the color attachment is blitted to the real window
//! surface via `glBlitFramebuffer` (ES 3.0) before `eglSwapBuffers`.

use glow::HasContext;
use shared::error::{EngineResult, ErrorCode};

use super::types::ee;
use crate::backend::gl::readback::CompactPixelUnpackGuard;

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
/// The caller must ensure an EGL context is current. An Android resume reuses a
/// preserved context, so the content's pixel-store state and its texture and
/// renderbuffer bindings can all still be live here; the allocation owns the
/// former and restores the latter on every path.
pub(crate) fn create(gl: &glow::Context, width: u32, height: u32) -> EngineResult<DrawingBuffer> {
    let _unpack = CompactPixelUnpackGuard::new(gl, 4);
    let _bindings = ReallocationScope::without_framebuffer(gl);
    unsafe {
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
        // The DrawingBuffer FBO stays bound: the onscreen caller's contract is
        // that a fresh buffer is the default framebuffer's new meaning.

        Ok(DrawingBuffer {
            fbo,
            color_tex,
            depth_stencil_rb,
            width,
            height,
        })
    }
}

/// Engine-owned scope for the bindings a reallocation has to move. Restores on
/// every path, including an incomplete framebuffer and an unwind, because the
/// content owns these bindings and a failed resize is still a returning call.
///
/// Only DRAW is touched: attachment and completeness both work through it, so
/// READ never moves. Split targets are safe here because a DrawingBuffer only
/// exists after [`create`] probed them on this driver.
struct ReallocationScope<'a> {
    gl: &'a glow::Context,
    texture: u32,
    renderbuffer: u32,
    /// `None` when the caller's contract is to leave its own framebuffer bound,
    /// as a freshly created DrawingBuffer does.
    draw_framebuffer: Option<Option<glow::NativeFramebuffer>>,
}

impl<'a> ReallocationScope<'a> {
    /// The caller keeps this context current until the scope is dropped.
    fn new(gl: &'a glow::Context) -> Self {
        let mut scope = Self::without_framebuffer(gl);
        scope.draw_framebuffer =
            Some(unsafe { gl.get_parameter_framebuffer(glow::DRAW_FRAMEBUFFER_BINDING) });
        scope
    }

    fn without_framebuffer(gl: &'a glow::Context) -> Self {
        unsafe {
            Self {
                gl,
                texture: gl.get_parameter_i32(glow::TEXTURE_BINDING_2D) as u32,
                renderbuffer: gl.get_parameter_i32(glow::RENDERBUFFER_BINDING) as u32,
                draw_framebuffer: None,
            }
        }
    }
}

impl Drop for ReallocationScope<'_> {
    fn drop(&mut self) {
        unsafe {
            restore_texture_binding(self.gl, self.texture);
            restore_renderbuffer_binding(self.gl, self.renderbuffer);
            if let Some(previous) = self.draw_framebuffer {
                self.gl.bind_framebuffer(glow::DRAW_FRAMEBUFFER, previous);
            }
        }
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

    // A NULL `tex_image_2d` is an *offset* into whatever the content left bound
    // to PIXEL_UNPACK_BUFFER, and GL rejects the call outright when the new
    // extent would read past that buffer -- leaving the colour attachment at its
    // old size while everything downstream believes the resize happened. The
    // allocation is engine-owned, so it takes the pixel-store state with it.
    let _unpack = CompactPixelUnpackGuard::new(gl, 4);
    let _bindings = ReallocationScope::new(gl);

    unsafe {
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
        gl.bind_framebuffer(glow::DRAW_FRAMEBUFFER, Some(db.fbo));
        gl.framebuffer_texture_2d(
            glow::DRAW_FRAMEBUFFER,
            glow::COLOR_ATTACHMENT0,
            glow::TEXTURE_2D,
            Some(db.color_tex),
            0,
        );
        gl.framebuffer_renderbuffer(
            glow::DRAW_FRAMEBUFFER,
            glow::DEPTH_STENCIL_ATTACHMENT,
            glow::RENDERBUFFER,
            Some(db.depth_stencil_rb),
        );

        let status = gl.check_framebuffer_status(glow::DRAW_FRAMEBUFFER);
        if status != glow::FRAMEBUFFER_COMPLETE {
            return Err(ee(
                ErrorCode::RenderBackendError,
                format!("DrawingBuffer resize: framebuffer incomplete (status=0x{status:X})"),
            ));
        }
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
/// Always a full-surface colour-only copy. Depth/stencil is deliberately not
/// migrated: `glBlitFramebuffer` with `DEPTH_BUFFER_BIT` is
/// `INVALID_OPERATION` when the source and destination formats differ, while
/// the window surface's depth format is selected by its EGL config and is not
/// probed here. A mode transition therefore establishes a colour-preserving
/// boundary and explicitly discards depth/stencil rather than issuing an
/// unprobed blit that could fail or leave ambiguous state.
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

#[cfg(test)]
mod tests {
    use super::*;
    // One shared lock for the one Mesa surfaceless display; a private mutex
    // here serialised only this module and raced the readback fixtures.

    struct EglScope {
        api: khronos_egl::DynamicInstance<khronos_egl::EGL1_5>,
        display: khronos_egl::Display,
        surface: Option<khronos_egl::Surface>,
        context: Option<khronos_egl::Context>,
        _display_lifetime: std::sync::MutexGuard<'static, ()>,
    }

    impl Drop for EglScope {
        fn drop(&mut self) {
            let _ = self.api.make_current(self.display, None, None, None);
            if let Some(surface) = self.surface {
                let _ = self.api.destroy_surface(self.display, surface);
            }
            if let Some(context) = self.context {
                let _ = self.api.destroy_context(self.display, context);
            }
            let _ = self.api.terminate(self.display);
        }
    }

    fn gles3_context() -> (EglScope, glow::Context) {
        let display_lifetime = crate::backend::gl::readback_test_gl::lock_egl_display();
        let api = unsafe {
            khronos_egl::DynamicInstance::<khronos_egl::EGL1_5>::load_required_from_filename(
                "libEGL.so.1",
            )
        }
        .expect("load EGL 1.5");
        let display = unsafe {
            api.get_platform_display(0x31DD, std::ptr::null_mut(), &[khronos_egl::ATTRIB_NONE])
        }
        .expect("Mesa surfaceless display");
        api.initialize(display).expect("initialize EGL");
        let mut scope = EglScope {
            api,
            display,
            surface: None,
            context: None,
            _display_lifetime: display_lifetime,
        };
        scope.api.bind_api(khronos_egl::OPENGL_ES_API).unwrap();
        let config = scope
            .api
            .choose_first_config(
                display,
                &[
                    khronos_egl::SURFACE_TYPE,
                    khronos_egl::PBUFFER_BIT,
                    khronos_egl::RENDERABLE_TYPE,
                    0x40,
                    khronos_egl::RED_SIZE,
                    8,
                    khronos_egl::GREEN_SIZE,
                    8,
                    khronos_egl::BLUE_SIZE,
                    8,
                    khronos_egl::ALPHA_SIZE,
                    8,
                    khronos_egl::NONE,
                ],
            )
            .unwrap()
            .expect("RGBA8 GLES3 pbuffer config");
        scope.context = Some(
            scope
                .api
                .create_context(
                    display,
                    config,
                    None,
                    &[khronos_egl::CONTEXT_CLIENT_VERSION, 3, khronos_egl::NONE],
                )
                .unwrap(),
        );
        scope.surface = Some(
            scope
                .api
                .create_pbuffer_surface(
                    display,
                    config,
                    &[
                        khronos_egl::WIDTH,
                        3,
                        khronos_egl::HEIGHT,
                        2,
                        khronos_egl::NONE,
                    ],
                )
                .unwrap(),
        );
        scope
            .api
            .make_current(display, scope.surface, scope.surface, scope.context)
            .unwrap();
        let gl = unsafe {
            glow::Context::from_loader_function(|name| {
                scope
                    .api
                    .get_proc_address(name)
                    .map_or(std::ptr::null(), |f| f as *const std::ffi::c_void)
            })
        };
        (scope, gl)
    }

    unsafe fn read_pixel(gl: &glow::Context) -> [u8; 4] {
        let mut pixel = [0; 4];
        gl.read_pixels(
            0,
            0,
            1,
            1,
            glow::RGBA,
            glow::UNSIGNED_BYTE,
            glow::PixelPackData::Slice(Some(&mut pixel)),
        );
        pixel
    }

    #[test]
    #[ignore = "requires Mesa surfaceless EGL and GLES3"]
    fn reverse_colour_migration_native_preserves_pixels() {
        let (_scope, gl) = gles3_context();
        let db = create(&gl, 3, 2).expect("drawing buffer");
        unsafe {
            gl.bind_framebuffer(glow::FRAMEBUFFER, None);
            gl.clear_color(1.0, 0.0, 0.0, 1.0);
            gl.clear(glow::COLOR_BUFFER_BIT);
        }
        assert!(blit_from_surface(&gl, &db, 3, 2));
        unsafe {
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(db.fbo));
            assert_eq!(read_pixel(&gl), [255, 0, 0, 255]);
        }
        destroy(&gl, db);
    }

    #[test]
    #[ignore = "requires Mesa surfaceless EGL and GLES3"]
    fn forward_colour_migration_native_preserves_pixels() {
        let (_scope, gl) = gles3_context();
        let db = create(&gl, 3, 2).expect("drawing buffer");
        unsafe {
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(db.fbo));
            gl.clear_color(0.0, 0.0, 1.0, 1.0);
            gl.clear(glow::COLOR_BUFFER_BIT);
        }
        assert!(blit_to_surface(
            &gl,
            &db,
            3,
            2,
            &crate::present_damage::BlitPlan::Full { linear: false }
        ));
        unsafe {
            gl.bind_framebuffer(glow::FRAMEBUFFER, None);
            assert_eq!(read_pixel(&gl), [0, 0, 255, 255]);
        }
        destroy(&gl, db);
    }

    #[test]
    fn mode_transition_explicitly_discards_depth_stencil() {
        let source = include_str!("drawing_buffer.rs");
        let body = &source[source.find("pub(crate) fn blit_from_surface").unwrap()..];
        let body = &body[..body.find("pub(crate) fn blit_to_surface").unwrap()];
        assert!(body.contains("COLOR_BUFFER_BIT"));
        assert!(!body.contains("DEPTH_BUFFER_BIT"));
        assert!(source.contains("depth/stencil is deliberately not"));
    }
}
