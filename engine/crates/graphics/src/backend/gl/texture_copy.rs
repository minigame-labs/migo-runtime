//! Engine-owned GLES3 texture copies preserve the actual READ binding.
//! Client framebuffer IDs never enter this native-handle scope.

use glow::HasContext;

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
    Image {
        target: u32,
        level: i32,
        internal_format: u32,
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
                    source_x,
                    source_y,
                    width,
                    height,
                } => {
                    gl.copy_tex_image_2d(
                        target,
                        level,
                        internal_format,
                        source_x,
                        source_y,
                        width,
                        height,
                        0,
                    );
                }
                TextureCopy::SubImage {
                    target,
                    level,
                    xoffset,
                    yoffset,
                    width,
                    height,
                } => {
                    gl.copy_tex_sub_image_2d(target, level, xoffset, yoffset, 0, 0, width, height);
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
