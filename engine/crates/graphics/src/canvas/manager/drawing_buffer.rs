//! Chromium-style DrawingBuffer: an intermediate FBO that WebGL renders to
//! instead of the native window surface directly.
//!
//! On every frame present the color attachment is blitted to the real window
//! surface via `glBlitFramebuffer` (ES 3.0) before `eglSwapBuffers`.

use glow::HasContext;
use shared::error::{EngineResult, ErrorCode};

use super::types::ee;
use crate::backend::gl::readback::CompactPixelUnpackGuard;

/// What a WebGL context asked its drawing buffer to have (WebGL 1.0 5.2, `WebGLContextAttributes`): a colour buffer
/// with alpha or without, and a depth buffer and a stencil buffer or not. A drawing buffer has exactly these, so what
/// the content observes -- `getParameter(ALPHA_BITS / DEPTH_BITS / STENCIL_BITS)`, whether a depth or stencil test can
/// fail, the alpha `readPixels` answers -- is what it asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct DrawingBufferFormat {
    pub alpha: bool,
    pub depth: bool,
    pub stencil: bool,
}

impl DrawingBufferFormat {
    /// The buffers a drawing buffer of this format has, as a `glClear` mask.
    pub(crate) fn buffers(self) -> u32 {
        glow::COLOR_BUFFER_BIT
            | if self.depth {
                glow::DEPTH_BUFFER_BIT
            } else {
                0
            }
            | if self.stencil {
                glow::STENCIL_BUFFER_BIT
            } else {
                0
            }
    }

    /// Everything: what a canvas's drawing buffer is before a WebGL context declares otherwise (the screen canvas, which
    /// a 2D context may also draw into).
    pub(crate) const FULL: Self = Self {
        alpha: true,
        depth: true,
        stencil: true,
    };

    /// The colour texture's internal format and the format it is specified with.
    fn colour(self) -> (u32, u32) {
        if self.alpha {
            (glow::RGBA8, glow::RGBA)
        } else {
            (glow::RGB8, glow::RGB)
        }
    }

    /// The depth/stencil renderbuffer's internal format and the attachment point it takes, or `None` for neither.
    fn depth_stencil(self) -> Option<(u32, u32)> {
        match (self.depth, self.stencil) {
            (true, true) => Some((glow::DEPTH24_STENCIL8, glow::DEPTH_STENCIL_ATTACHMENT)),
            (true, false) => Some((glow::DEPTH_COMPONENT24, glow::DEPTH_ATTACHMENT)),
            (false, true) => Some((glow::STENCIL_INDEX8, glow::STENCIL_ATTACHMENT)),
            (false, false) => None,
        }
    }
}

/// What a WebGL context declared of its drawing buffer when it was created (`GLCmd::WebglContext`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct WebglBufferSpec {
    pub format: DrawingBufferFormat,
    /// `preserveDrawingBuffer`: the buffer keeps its contents across a present. When false it is cleared once it has
    /// been presented (`CanvasManager::settle_owed_clear`).
    pub preserve: bool,
}

/// An engine-owned framebuffer standing in for a WebGL canvas's default framebuffer: the screen canvas's (presented by
/// a blit to the window surface) and, once a WebGL context declares its attributes, an offscreen canvas's.
pub(crate) struct DrawingBuffer {
    /// FBO that WebGL commands target when `bindFramebuffer(null)` is called.
    pub fbo: glow::NativeFramebuffer,
    /// Colour attachment: RGBA8, or RGB8 for a context without alpha.
    pub color_tex: glow::NativeTexture,
    /// Depth and/or stencil attachment, as `format` asks; `None` when it asks for neither.
    pub depth_stencil_rb: Option<glow::NativeRenderbuffer>,
    pub format: DrawingBufferFormat,
    /// Current buffer width in physical pixels.
    pub width: u32,
    /// Current buffer height in physical pixels.
    pub height: u32,
}

/// Create a new DrawingBuffer of `format` at the given dimensions.
///
/// The caller must ensure an EGL context is current. An Android resume reuses a
/// preserved context, so the content's pixel-store state and its texture and
/// renderbuffer bindings can all still be live here; the allocation owns the
/// former and restores the latter on every path.
pub(crate) fn create(
    gl: &glow::Context,
    width: u32,
    height: u32,
    format: DrawingBufferFormat,
) -> EngineResult<DrawingBuffer> {
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
        let depth_stencil_rb = match format.depth_stencil() {
            None => None,
            Some(_) => Some(gl.create_renderbuffer().map_err(|e| {
                gl.delete_framebuffer(fbo);
                gl.delete_texture(color_tex);
                ee(
                    ErrorCode::RenderBackendError,
                    format!("DrawingBuffer: create_renderbuffer failed: {e}"),
                )
            })?),
        };
        let db = DrawingBuffer {
            fbo,
            color_tex,
            depth_stencil_rb,
            format,
            width,
            height,
        };

        gl.bind_texture(glow::TEXTURE_2D, Some(color_tex));
        for (pname, value) in [
            (glow::TEXTURE_MIN_FILTER, glow::NEAREST),
            (glow::TEXTURE_MAG_FILTER, glow::NEAREST),
            (glow::TEXTURE_WRAP_S, glow::CLAMP_TO_EDGE),
            (glow::TEXTURE_WRAP_T, glow::CLAMP_TO_EDGE),
        ] {
            gl.tex_parameter_i32(glow::TEXTURE_2D, pname, value as i32);
        }
        gl.bind_framebuffer(glow::FRAMEBUFFER, Some(fbo));
        if let Err(e) = allocate(gl, &db, width, height) {
            gl.bind_framebuffer(glow::FRAMEBUFFER, None);
            destroy(gl, db);
            return Err(e);
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
            destroy(gl, db);
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
        clear_to_initial_state(gl, Some(fbo), EVERY_BUFFER);

        Ok(db)
    }
}

/// Give the buffer's attachments storage of its format at `width` x `height` and attach them to its framebuffer, which
/// the caller has bound to DRAW_FRAMEBUFFER (FRAMEBUFFER binds both): the colour texture, and the depth/stencil
/// renderbuffer at the attachment point its format takes, with the others left empty. The framebuffer must then be
/// complete. `gl` is current, and `db`'s objects are its share group's.
fn allocate(gl: &glow::Context, db: &DrawingBuffer, width: u32, height: u32) -> EngineResult<()> {
    let (internal, format) = db.format.colour();
    unsafe {
        gl.bind_texture(glow::TEXTURE_2D, Some(db.color_tex));
        gl.tex_image_2d(
            glow::TEXTURE_2D,
            0,
            internal as i32,
            width as i32,
            height as i32,
            0,
            format,
            glow::UNSIGNED_BYTE,
            glow::PixelUnpackData::Slice(None),
        );
        if let (Some(rb), Some((internal, _))) = (db.depth_stencil_rb, db.format.depth_stencil()) {
            gl.bind_renderbuffer(glow::RENDERBUFFER, Some(rb));
            gl.renderbuffer_storage(glow::RENDERBUFFER, internal, width as i32, height as i32);
        }
    }
    attach(gl, glow::DRAW_FRAMEBUFFER, db);
    let status = unsafe { gl.check_framebuffer_status(glow::DRAW_FRAMEBUFFER) };
    if status != glow::FRAMEBUFFER_COMPLETE {
        return Err(ee(
            ErrorCode::RenderBackendError,
            format!(
                "DrawingBuffer {:?}: framebuffer incomplete (status=0x{status:X})",
                db.format
            ),
        ));
    }
    Ok(())
}

/// Attach the buffer's own storage to its framebuffer, bound to `target`: the colour texture, nothing at the
/// depth-stencil point, and the renderbuffer at the point its format takes. Re-attaching after a reallocation is
/// required on some drivers, and it heals a framebuffer the content's WebGL calls changed the attachments of.
pub(crate) fn attach(gl: &glow::Context, target: u32, db: &DrawingBuffer) {
    unsafe {
        gl.framebuffer_texture_2d(
            target,
            glow::COLOR_ATTACHMENT0,
            glow::TEXTURE_2D,
            Some(db.color_tex),
            0,
        );
        // DEPTH_STENCIL detaches both points; the format's renderbuffer then takes the one it uses.
        gl.framebuffer_renderbuffer(
            target,
            glow::DEPTH_STENCIL_ATTACHMENT,
            glow::RENDERBUFFER,
            None,
        );
        if let (Some(rb), Some((_, point))) = (db.depth_stencil_rb, db.format.depth_stencil()) {
            gl.framebuffer_renderbuffer(target, point, glow::RENDERBUFFER, Some(rb));
        }
    }
}

/// Clear `buffers` (a `glClear` mask) of `target` (the framebuffer bound for
/// drawing; `None` is the default framebuffer) to the initial state of a WebGL
/// drawing buffer: transparent black, depth 1, stencil 0.
///
/// The specification has a drawing buffer start that way, and again after it is
/// resized. The storage under it is whatever the driver returned: ANGLE's Metal
/// backend recycles the storage of destroyed surfaces without clearing it, so
/// after ~90 offscreen WebGL canvases had been created and collected each new one
/// began with an earlier one's pixels (measured on macOS: 93 of 360).
///
/// The content owns the state a clear reads -- clear values, write masks, the
/// scissor box, rasterizer discard -- so what the cleared buffers read of it is set
/// aside and put back; a `colorMask(false, ...)` or a scissor must not make the
/// initial clear a partial one, and the clear must not change what the content sees
/// afterwards.
/// A target that is not complete (an offscreen context with no surface) is left
/// alone rather than raising `INVALID_FRAMEBUFFER_OPERATION` into the content's
/// first `getError`.
pub(crate) fn clear_to_initial_state(
    gl: &glow::Context,
    target: Option<glow::NativeFramebuffer>,
    buffers: u32,
) {
    let _scope = ClearStateScope::enter(gl, target, buffers);
}

/// Every buffer a drawing buffer can have, as a `glClear` mask.
pub(crate) const EVERY_BUFFER: u32 =
    glow::COLOR_BUFFER_BIT | glow::DEPTH_BUFFER_BIT | glow::STENCIL_BUFFER_BIT;

struct ClearStateScope<'a> {
    gl: &'a glow::Context,
    draw_framebuffer: Option<glow::NativeFramebuffer>,
    scissor: bool,
    rasterizer_discard: bool,
    /// What the clear of each buffer reads -- its write mask and clear value -- for the buffers cleared.
    color: Option<([bool; 4], [f32; 4])>,
    depth: Option<(bool, f32)>,
    stencil: Option<(i32, i32, i32)>,
}

impl<'a> ClearStateScope<'a> {
    fn enter(
        gl: &'a glow::Context,
        target: Option<glow::NativeFramebuffer>,
        buffers: u32,
    ) -> Option<Self> {
        unsafe {
            let es3 = gl.version().major >= 3;
            let scope = Self {
                gl,
                draw_framebuffer: gl.get_parameter_framebuffer(glow::DRAW_FRAMEBUFFER_BINDING),
                scissor: gl.is_enabled(glow::SCISSOR_TEST),
                rasterizer_discard: es3 && gl.is_enabled(glow::RASTERIZER_DISCARD),
                color: (buffers & glow::COLOR_BUFFER_BIT != 0).then(|| {
                    let mut value = [0.0; 4];
                    gl.get_parameter_f32_slice(glow::COLOR_CLEAR_VALUE, &mut value);
                    (
                        gl.get_parameter_bool_array::<4>(glow::COLOR_WRITEMASK),
                        value,
                    )
                }),
                depth: (buffers & glow::DEPTH_BUFFER_BIT != 0).then(|| {
                    (
                        gl.get_parameter_bool(glow::DEPTH_WRITEMASK),
                        gl.get_parameter_f32(glow::DEPTH_CLEAR_VALUE),
                    )
                }),
                stencil: (buffers & glow::STENCIL_BUFFER_BIT != 0).then(|| {
                    (
                        gl.get_parameter_i32(glow::STENCIL_WRITEMASK),
                        gl.get_parameter_i32(glow::STENCIL_BACK_WRITEMASK),
                        gl.get_parameter_i32(glow::STENCIL_CLEAR_VALUE),
                    )
                }),
            };
            gl.bind_framebuffer(glow::DRAW_FRAMEBUFFER, target);
            if gl.check_framebuffer_status(glow::DRAW_FRAMEBUFFER) != glow::FRAMEBUFFER_COMPLETE {
                // Dropping `scope` restores the binding.
                return None;
            }
            gl.disable(glow::SCISSOR_TEST);
            if es3 {
                gl.disable(glow::RASTERIZER_DISCARD);
            }
            if scope.color.is_some() {
                gl.color_mask(true, true, true, true);
                gl.clear_color(0.0, 0.0, 0.0, 0.0);
            }
            if scope.depth.is_some() {
                gl.depth_mask(true);
                gl.clear_depth_f32(1.0);
            }
            if scope.stencil.is_some() {
                gl.stencil_mask(0xFFFF_FFFF);
                gl.clear_stencil(0);
            }
            gl.clear(buffers & EVERY_BUFFER);
            Some(scope)
        }
    }
}

impl Drop for ClearStateScope<'_> {
    fn drop(&mut self) {
        unsafe {
            let gl = self.gl;
            if let Some((mask, value)) = self.color {
                gl.clear_color(value[0], value[1], value[2], value[3]);
                gl.color_mask(mask[0], mask[1], mask[2], mask[3]);
            }
            if let Some((mask, value)) = self.depth {
                gl.clear_depth_f32(value);
                gl.depth_mask(mask);
            }
            if let Some((front, back, value)) = self.stencil {
                gl.clear_stencil(value);
                gl.stencil_mask_separate(glow::FRONT, front as u32);
                gl.stencil_mask_separate(glow::BACK, back as u32);
            }
            if self.scissor {
                gl.enable(glow::SCISSOR_TEST);
            }
            if self.rasterizer_discard {
                gl.enable(glow::RASTERIZER_DISCARD);
            }
            gl.bind_framebuffer(glow::DRAW_FRAMEBUFFER, self.draw_framebuffer);
        }
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
        gl.bind_framebuffer(glow::DRAW_FRAMEBUFFER, Some(db.fbo));
        allocate(gl, db, new_w, new_h)?;
    }

    // The reallocated storage is whatever the driver returned.
    clear_to_initial_state(gl, Some(db.fbo), EVERY_BUFFER);

    db.width = new_w;
    db.height = new_h;
    Ok(())
}

/// A canvas's size was assigned (`canvas.width = ...`): its drawing buffer starts again at that size, cleared to its
/// initial state, even when the size is the one it already has -- the specification resets the buffer on every
/// assignment, and `canvas.width = canvas.width` is how content clears one. Only a new size reallocates.
pub(crate) fn reset(
    gl: &glow::Context,
    db: &mut DrawingBuffer,
    width: u32,
    height: u32,
) -> EngineResult<()> {
    if (db.width, db.height) == (width, height) {
        clear_to_initial_state(gl, Some(db.fbo), EVERY_BUFFER);
        return Ok(());
    }
    resize(gl, db, width, height)
}

/// Give the buffer another format at its size: a WebGL context declared attributes the buffer it already has (the
/// screen canvas's, made before any context) does not match. The framebuffer and colour texture keep their names, so
/// a default-framebuffer mapping that names them stays right; the depth/stencil renderbuffer is made or deleted as the
/// format needs one. The storage is new, and cleared to a drawing buffer's initial state.
pub(crate) fn reformat(
    gl: &glow::Context,
    db: &mut DrawingBuffer,
    format: DrawingBufferFormat,
) -> EngineResult<()> {
    if db.format == format {
        return Ok(());
    }
    let _unpack = CompactPixelUnpackGuard::new(gl, 4);
    let _bindings = ReallocationScope::new(gl);
    unsafe {
        match (db.depth_stencil_rb, format.depth_stencil()) {
            (None, Some(_)) => {
                db.depth_stencil_rb = Some(gl.create_renderbuffer().map_err(|e| {
                    ee(
                        ErrorCode::RenderBackendError,
                        format!("DrawingBuffer: create_renderbuffer failed: {e}"),
                    )
                })?);
            }
            (Some(rb), None) => {
                gl.bind_framebuffer(glow::DRAW_FRAMEBUFFER, Some(db.fbo));
                gl.framebuffer_renderbuffer(
                    glow::DRAW_FRAMEBUFFER,
                    glow::DEPTH_STENCIL_ATTACHMENT,
                    glow::RENDERBUFFER,
                    None,
                );
                gl.delete_renderbuffer(rb);
                db.depth_stencil_rb = None;
            }
            _ => {}
        }
        db.format = format;
        gl.bind_framebuffer(glow::DRAW_FRAMEBUFFER, Some(db.fbo));
        allocate(gl, db, db.width, db.height)?;
    }
    clear_to_initial_state(gl, Some(db.fbo), EVERY_BUFFER);
    Ok(())
}

/// Destroy the DrawingBuffer and release all GL resources.
pub(crate) fn destroy(gl: &glow::Context, db: DrawingBuffer) {
    unsafe {
        gl.delete_framebuffer(db.fbo);
        gl.delete_texture(db.color_tex);
        if let Some(rb) = db.depth_stencil_rb {
            gl.delete_renderbuffer(rb);
        }
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
                // Try to heal: re-attach its own colour and depth/stencil storage.
                gl.bind_framebuffer(glow::FRAMEBUFFER, Some(db.fbo));
                attach(gl, glow::FRAMEBUFFER, db);
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
    use crate::backend::gl::readback_test_gl::native_gles3_context as gles3_context;

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

    /// A clear of some of the buffers leaves the others, and the content's state as it found it: the clear a present
    /// leaves owed is of what the frame's own clear will not write over, made with the content's state in place.
    #[test]
    #[ignore = "requires Mesa surfaceless EGL and GLES3"]
    fn clearing_some_buffers_leaves_the_others_and_the_contents_state() {
        let (_scope, gl) = gles3_context();
        let db = create(&gl, 3, 2, DrawingBufferFormat::FULL).expect("drawing buffer");
        let outside_the_scissor = |gl: &glow::Context| unsafe {
            let mut pixel = [0; 4];
            gl.read_pixels(
                2,
                1,
                1,
                1,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                glow::PixelPackData::Slice(Some(&mut pixel)),
            );
            pixel
        };
        unsafe {
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(db.fbo));
            gl.clear_color(1.0, 0.0, 0.0, 1.0);
            gl.clear(glow::COLOR_BUFFER_BIT);
            gl.clear_color(0.0, 0.0, 1.0, 1.0);
            gl.color_mask(true, false, true, true);
            gl.depth_mask(false);
            gl.clear_depth_f32(0.25);
            gl.stencil_mask(0x0f);
            gl.enable(glow::SCISSOR_TEST);
            gl.scissor(0, 0, 1, 1);
        }
        clear_to_initial_state(
            &gl,
            Some(db.fbo),
            glow::DEPTH_BUFFER_BIT | glow::STENCIL_BUFFER_BIT,
        );
        assert_eq!(
            outside_the_scissor(&gl),
            [255, 0, 0, 255],
            "colour was not named"
        );
        clear_to_initial_state(&gl, Some(db.fbo), glow::COLOR_BUFFER_BIT);
        assert_eq!(
            outside_the_scissor(&gl),
            [0, 0, 0, 0],
            "colour is cleared whole, through the content's write mask and scissor"
        );
        unsafe {
            assert_eq!(
                gl.get_parameter_bool_array::<4>(glow::COLOR_WRITEMASK),
                [true, false, true, true]
            );
            let mut colour = [0.0; 4];
            gl.get_parameter_f32_slice(glow::COLOR_CLEAR_VALUE, &mut colour);
            assert_eq!(colour, [0.0, 0.0, 1.0, 1.0]);
            assert!(!gl.get_parameter_bool(glow::DEPTH_WRITEMASK));
            assert_eq!(gl.get_parameter_f32(glow::DEPTH_CLEAR_VALUE), 0.25);
            assert_eq!(gl.get_parameter_i32(glow::STENCIL_WRITEMASK), 0x0f);
            assert_eq!(gl.get_parameter_i32(glow::STENCIL_BACK_WRITEMASK), 0x0f);
            assert!(gl.is_enabled(glow::SCISSOR_TEST));
            assert_eq!(
                gl.get_parameter_framebuffer(glow::DRAW_FRAMEBUFFER_BINDING),
                Some(db.fbo)
            );
            assert_eq!(gl.get_error(), glow::NO_ERROR);
        }
        destroy(&gl, db);
    }

    #[test]
    #[ignore = "requires Mesa surfaceless EGL and GLES3"]
    fn forward_colour_migration_native_preserves_pixels() {
        let (_scope, gl) = gles3_context();
        let db = create(&gl, 3, 2, DrawingBufferFormat::FULL).expect("drawing buffer");
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
}
