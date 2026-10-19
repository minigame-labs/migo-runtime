//! Engine-owned GLES3 texture copies preserve the actual READ binding.
//! Client framebuffer IDs never enter this native-handle scope.

use glow::HasContext;

use super::readback::CompactPixelUnpackGuard;

struct ReadFramebufferScope<'a> {
    gl: &'a glow::Context,
    previous: Option<glow::NativeFramebuffer>,
    changed: bool,
}

impl<'a> ReadFramebufferScope<'a> {
    /// The caller keeps this GLES3 context current until the scope is dropped.
    fn bind(gl: &'a glow::Context, framebuffer: glow::NativeFramebuffer) -> Self {
        let previous = unsafe { gl.get_parameter_framebuffer(glow::READ_FRAMEBUFFER_BINDING) };
        let changed = previous != Some(framebuffer);
        if changed {
            unsafe { gl.bind_framebuffer(glow::READ_FRAMEBUFFER, Some(framebuffer)) };
            crate::render_diagnostics::bump_state_change();
        }
        Self {
            gl,
            previous,
            changed,
        }
    }
}

impl Drop for ReadFramebufferScope<'_> {
    fn drop(&mut self) {
        if self.changed {
            unsafe {
                self.gl
                    .bind_framebuffer(glow::READ_FRAMEBUFFER, self.previous)
            };
            crate::render_diagnostics::bump_state_change();
        }
    }
}

pub(crate) enum TextureCopy {
    /// `texImage2D(source)`: the level is specified as the call specifies it -- `internal_format`, `format`, `type_` --
    /// and the source copied into the whole of it.
    ///
    /// Two calls, not `glCopyTexImage2D`. That one specifies and copies at once, but for an unsized internal format it
    /// takes the level's format from the framebuffer it reads, not from the call, so the texture could come out other
    /// than the content named -- and a later `texSubImage2D` be judged against a format the content never gave. And on
    /// the Android emulator's GLES a sized one (`RGBA8`, which Pixi asks for under WebGL 2) left the level empty, with
    /// no error: every Pixi sprite sampled transparent black.
    Image {
        target: u32,
        level: i32,
        internal_format: u32,
        format: u32,
        type_: u32,
        source_x: i32,
        source_y: i32,
        width: i32,
        height: i32,
    },
    SubImage {
        target: u32,
        level: i32,
        xoffset: i32,
        yoffset: i32,
        source_x: i32,
        source_y: i32,
        width: i32,
        height: i32,
    },
}

/// Attach a source to the private copy FBO, copy when complete, then detach it
/// and restore READ. DRAW and the client binding shadow are never changed.
/// Returns the FBO status so callers retain their existing failure reporting.
pub(crate) fn copy_texture(
    gl: &glow::Context,
    framebuffer: glow::NativeFramebuffer,
    source: glow::NativeTexture,
    copy: TextureCopy,
) -> u32 {
    let _binding = ReadFramebufferScope::bind(gl, framebuffer);
    unsafe {
        gl.framebuffer_texture_2d(
            glow::READ_FRAMEBUFFER,
            glow::COLOR_ATTACHMENT0,
            glow::TEXTURE_2D,
            Some(source),
            0,
        );
        let status = gl.check_framebuffer_status(glow::READ_FRAMEBUFFER);
        if status == glow::FRAMEBUFFER_COMPLETE {
            match copy {
                TextureCopy::Image {
                    target,
                    level,
                    internal_format,
                    format,
                    type_,
                    source_x,
                    source_y,
                    width,
                    height,
                } => {
                    {
                        // No PIXEL_UNPACK_BUFFER: the empty data below is no data, not an offset into the content's.
                        let _unpack = CompactPixelUnpackGuard::new(gl, 4);
                        gl.tex_image_2d(
                            target,
                            level,
                            internal_format as i32,
                            width,
                            height,
                            0,
                            format,
                            type_,
                            glow::PixelUnpackData::Slice(None),
                        );
                    }
                    gl.copy_tex_sub_image_2d(
                        target, level, 0, 0, source_x, source_y, width, height,
                    );
                }
                TextureCopy::SubImage {
                    target,
                    level,
                    xoffset,
                    yoffset,
                    source_x,
                    source_y,
                    width,
                    height,
                } => {
                    gl.copy_tex_sub_image_2d(
                        target, level, xoffset, yoffset, source_x, source_y, width, height,
                    );
                }
            }
        }
        gl.framebuffer_texture_2d(
            glow::READ_FRAMEBUFFER,
            glow::COLOR_ATTACHMENT0,
            glow::TEXTURE_2D,
            None,
            0,
        );
        status
    }
}

/// Read a region of `source` as tightly packed RGBA8 through the private copy FBO.
///
/// The counterpart of [`copy_texture`] for a caller that has to change the pixels on the
/// way: it attaches, reads and detaches the same way and leaves DRAW and the client
/// bindings alone. `None` when the framebuffer is not complete, so the caller can fall
/// back to the plain GPU copy.
pub(crate) fn read_texture_rgba8(
    gl: &glow::Context,
    framebuffer: glow::NativeFramebuffer,
    source: glow::NativeTexture,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
) -> Option<Vec<u8>> {
    let len = (width.max(0) as usize)
        .checked_mul(height.max(0) as usize)?
        .checked_mul(4)?;
    let mut pixels = vec![0u8; len];
    let _binding = ReadFramebufferScope::bind(gl, framebuffer);
    unsafe {
        gl.framebuffer_texture_2d(
            glow::READ_FRAMEBUFFER,
            glow::COLOR_ATTACHMENT0,
            glow::TEXTURE_2D,
            Some(source),
            0,
        );
        let complete =
            gl.check_framebuffer_status(glow::READ_FRAMEBUFFER) == glow::FRAMEBUFFER_COMPLETE;
        if complete {
            // Rows packed tightly whatever the content set: PACK_ALIGNMENT, ROW_LENGTH and the skips
            // are its state in both directions.
            let _pack = crate::backend::gl::readback::CompactPixelPackGuard::new(gl, 1);
            gl.read_pixels(
                x,
                y,
                width,
                height,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                glow::PixelPackData::Slice(Some(&mut pixels)),
            );
        }
        gl.framebuffer_texture_2d(
            glow::READ_FRAMEBUFFER,
            glow::COLOR_ATTACHMENT0,
            glow::TEXTURE_2D,
            None,
            0,
        );
        complete.then_some(pixels)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::gl::readback_test_gl as fixture;

    fn fbo(name: u32) -> glow::NativeFramebuffer {
        glow::NativeFramebuffer(std::num::NonZeroU32::new(name).unwrap())
    }

    #[test]
    fn native_read_scope_restores_high_bit_names_and_preserves_draw() {
        let gl = fixture::context();
        let original = fixture::Bindings {
            read_framebuffer: 0x8000_0001,
            draw_framebuffer: 0x8000_0002,
            ..Default::default()
        };
        fixture::set_bindings(original);
        {
            let _scope = ReadFramebufferScope::bind(&gl, fbo(9));
            assert_eq!(fixture::bindings().read_framebuffer, 9);
            assert_eq!(
                fixture::bindings().draw_framebuffer,
                original.draw_framebuffer
            );
            {
                let _inner = ReadFramebufferScope::bind(&gl, fbo(11));
                assert_eq!(fixture::bindings().read_framebuffer, 11);
            }
            assert_eq!(fixture::bindings().read_framebuffer, 9);
        }
        assert_eq!(fixture::bindings(), original);
    }

    #[test]
    fn native_read_scope_restores_on_unwind_and_skips_redundant_binds() {
        let gl = fixture::context();
        let original = fixture::bindings();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _scope = ReadFramebufferScope::bind(&gl, fbo(9));
            let changes = fixture::mutations();
            {
                let _same = ReadFramebufferScope::bind(&gl, fbo(9));
            }
            assert_eq!(fixture::mutations(), changes);
            panic!("test unwind");
        }));
        assert!(result.is_err());
        assert_eq!(fixture::bindings(), original);
    }
}
