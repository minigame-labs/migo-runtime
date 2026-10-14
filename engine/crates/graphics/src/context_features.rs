//! What the engine asks of every GLES context it creates, decided once per display from the extensions it advertises.
//!
//! Every context is in one share group -- the canvases', the resource context, the upload thread's -- so all of them
//! are created with the same attributes, from here: EGL refuses a share context whose robustness attributes differ, and
//! ANGLE refuses one whose resource initialization differs.

use khronos_egl as egl;

// EGL_EXT_create_context_robustness: paired with GL_KHR_robustness it turns a slow "invisible corruption, caught only
// at the next eglSwapBuffers" failure into "every GL call returns GL_CONTEXT_LOST and glGetGraphicsResetStatus says
// why", giving the render thread a chance to tear down and rebuild before the user sees a black frame (the pattern of
// wgpu-hal's GLES backend).
const EGL_CONTEXT_OPENGL_RESET_NOTIFICATION_STRATEGY_EXT: egl::Int = 0x3138;
const EGL_LOSE_CONTEXT_ON_RESET_EXT: egl::Int = 0x31BF;
const EGL_ROBUST_RESOURCE_INITIALIZATION_ANGLE: egl::Int = 0x3453;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ContextFeatures {
    /// `EGL_EXT_create_context_robustness`: the context asks to be lost on a GPU reset, so
    /// `glGetGraphicsResetStatus` reports the reset to the render loop instead of a swap failing later.
    pub(crate) robust_context: bool,
    /// `EGL_ANGLE_robust_resource_initialization`: whatever a context defines without data -- a texture image, a
    /// renderbuffer, a buffer -- reads as zeros, which ANGLE keeps by clearing it on the GPU the first time it is
    /// used. WebGL requires exactly that; where ANGLE does it, the renderer passes no data through, and elsewhere
    /// it supplies the zeros itself.
    pub(crate) robust_resource_initialization: bool,
}

impl ContextFeatures {
    /// The features a display advertising `extensions` (its `EGL_EXTENSIONS` string) gives its contexts.
    pub(crate) fn from_display_extensions(extensions: &str) -> Self {
        let advertised = |name: &str| {
            extensions
                .split_ascii_whitespace()
                .any(|extension| extension == name)
        };
        Self {
            robust_context: advertised("EGL_EXT_create_context_robustness"),
            robust_resource_initialization: advertised("EGL_ANGLE_robust_resource_initialization"),
        }
    }

    /// The `eglCreateContext` attribute list for a GLES `gles_major` context with these features.
    pub(crate) fn context_attribs(self, gles_major: u32) -> Vec<egl::Int> {
        let mut attribs = vec![
            egl::CONTEXT_CLIENT_VERSION as egl::Int,
            gles_major as egl::Int,
        ];
        if self.robust_context {
            // A driver that advertises robustness but does not honour the strategy ignores the attribute, which is
            // the wanted fallback; one that does not advertise it would refuse the attribute, so it is not asked.
            attribs.extend([
                EGL_CONTEXT_OPENGL_RESET_NOTIFICATION_STRATEGY_EXT,
                EGL_LOSE_CONTEXT_ON_RESET_EXT,
            ]);
        }
        if self.robust_resource_initialization {
            attribs.extend([
                EGL_ROBUST_RESOURCE_INITIALIZATION_ANGLE,
                egl::TRUE as egl::Int,
            ]);
        }
        attribs.push(egl::NONE as egl::Int);
        attribs
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn features_are_what_the_display_advertises_by_whole_name() {
        let both = ContextFeatures::from_display_extensions(
            "EGL_KHR_image EGL_EXT_create_context_robustness EGL_ANGLE_robust_resource_initialization",
        );
        assert!(both.robust_context && both.robust_resource_initialization);
        // A name that only contains another's is not it.
        let neither =
            ContextFeatures::from_display_extensions("EGL_EXT_create_context_robustness_other");
        assert_eq!(neither, ContextFeatures::default());
    }

    #[test]
    fn the_attribute_list_asks_for_each_advertised_feature_and_nothing_else() {
        let none = ContextFeatures::default().context_attribs(3);
        assert_eq!(
            none,
            vec![
                egl::CONTEXT_CLIENT_VERSION as egl::Int,
                3,
                egl::NONE as egl::Int
            ]
        );
        let both = ContextFeatures {
            robust_context: true,
            robust_resource_initialization: true,
        }
        .context_attribs(2);
        assert_eq!(
            both,
            vec![
                egl::CONTEXT_CLIENT_VERSION as egl::Int,
                2,
                EGL_CONTEXT_OPENGL_RESET_NOTIFICATION_STRATEGY_EXT,
                EGL_LOSE_CONTEXT_ON_RESET_EXT,
                EGL_ROBUST_RESOURCE_INITIALIZATION_ANGLE,
                egl::TRUE as egl::Int,
                egl::NONE as egl::Int,
            ]
        );
    }
}
