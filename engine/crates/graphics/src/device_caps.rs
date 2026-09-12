//! Runtime device capability detection and tier classification.
//!
//! Probed once at EGL context creation time.  The resulting `DeviceTier`
//! controls which optimisation paths are enabled for the session lifetime.

use glow::HasContext;

use crate::device_profile::DeviceRenderProfile;

#[inline]
fn has_extension(extensions: &str, expected: &str) -> bool {
    extensions
        .split_ascii_whitespace()
        .any(|extension| extension == expected)
}

#[inline]
fn ahb_api_supported(android_api: Option<u32>) -> bool {
    android_api.is_some_and(|level| level >= 26)
}

/// Runtime-detected device capabilities.
#[derive(Debug, Clone)]
pub struct DeviceCapabilities {
    pub gles_version: (u32, u32),
    /// `GL_NV_pixel_buffer_object` or ES 3.0+.
    pub has_pbo: bool,
    /// `glFenceSync` available (ES 3.0+).
    pub has_fence_sync: bool,
    /// Compute shaders available (ES 3.1+).
    pub has_compute: bool,
    /// `AHardwareBuffer` available (Android API 26+).
    pub ahb_available: bool,
    /// Buffer-age query (`EGL_BUFFER_AGE_KHR`) is available — advertised by
    /// either `EGL_EXT_buffer_age` or `EGL_KHR_partial_update`.
    pub has_buffer_age: bool,
    /// `EGL_EXT_buffer_age` is *independently* advertised. Unlike
    /// [`Self::has_buffer_age`] this is never inferred from
    /// `EGL_KHR_partial_update`: EXT guarantees the aged back-buffer contents
    /// regardless of any `eglSetDamageRegionKHR` declaration, so a rejected/absent
    /// KHR declaration may keep a partial repair only when this is true.
    pub has_ext_buffer_age: bool,
    /// `EGL_KHR_partial_update` — can call `eglSetDamageRegionKHR` to tell
    /// the compositor which region changed before swap.
    pub has_partial_update: bool,
    /// Runtime-detected support for compressed texture formats (ETC2/ASTC).
    pub compressed_format_support: crate::compressed_upload::CompressedFormatSupport,
    /// `GL_KHR_parallel_shader_compile` — `GL_COMPLETION_STATUS_KHR` can be
    /// queried on a program without stalling on the compile. Without it there
    /// is no way to ask "is this link done?" that does not block, so the link
    /// queue drains at the end of the batch instead of across frames.
    pub has_parallel_shader_compile: bool,
}

/// Coarse device classification that gates optimisation paths.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceTier {
    /// ES 3.0+, PBO, fence sync — eligible for upload thread, AHB, ring buffer.
    TierA,
    /// ES 2.0 or broken drivers — falls back to current paths (no regression).
    TierB,
}

impl DeviceCapabilities {
    /// Detect capabilities from the current EGL/GL context.
    ///
    /// `egl_extensions` should be the result of `eglQueryString(display, EGL_EXTENSIONS)`.
    /// Pass an empty string if unavailable — AHB detection will fall back to
    /// GL extension check only.
    ///
    /// `negotiated_gles_major` is the GLES version actually requested in the
    /// EGL context attributes (from `EglInitResult::gles_major`). Features are
    /// clamped to this level: even if `GL_VERSION` reports a higher version,
    /// a context created with ES 2.0 may not expose ES 3.0 entry points on
    /// all drivers.
    ///
    /// Must be called with a valid GL context current on the calling thread.
    pub fn detect(gl: &glow::Context, egl_extensions: &str, negotiated_gles_major: u32) -> Self {
        let version_str = unsafe { gl.get_parameter_string(glow::VERSION) };
        let detected = parse_gles_version(&version_str);
        // Use the minimum of detected and negotiated — belt-and-suspenders.
        let gles_version = if negotiated_gles_major >= 3 {
            detected
        } else {
            (detected.0.min(negotiated_gles_major), detected.1)
        };

        let gl_extensions = unsafe { gl.get_parameter_string(glow::EXTENSIONS) };
        let has_pbo =
            gles_version >= (3, 0) || has_extension(&gl_extensions, "GL_NV_pixel_buffer_object");
        let has_fence_sync = gles_version >= (3, 0);
        let has_compute = gles_version >= (3, 1);

        // AHB texture import requires:
        // - Android API 26+
        // - GL_OES_EGL_image (GL side can consume EGLImage)
        // - EGL_ANDROID_image_native_buffer (EGL side can wrap AHB)
        let ahb_available = cfg!(target_os = "android")
            && ahb_api_supported(android_api_level())
            && has_extension(&gl_extensions, "GL_OES_EGL_image")
            && has_extension(egl_extensions, "EGL_ANDROID_image_native_buffer");

        // EGL_EXT_buffer_age: query surface for back buffer age, and guarantees
        // aged back-buffer contents independently of any damage declaration.
        // EGL_KHR_partial_update: set damage region before swap (and includes
        // the buffer age query). These are independent capabilities —
        // partial_update implies the age query per the KHR spec, but
        // EGL_EXT_buffer_age can exist without partial_update. Track the EXT
        // advertisement separately so the partial-blit path can tell "aged
        // contents guaranteed" (EXT) apart from "age query only via KHR".
        let has_ext_buffer_age = has_extension(egl_extensions, "EGL_EXT_buffer_age");
        let has_partial_update = has_extension(egl_extensions, "EGL_KHR_partial_update");
        let has_buffer_age = has_ext_buffer_age || has_partial_update;

        let compressed_format_support =
            crate::compressed_upload::CompressedFormatSupport::detect(gl);

        let has_parallel_shader_compile =
            has_extension(&gl_extensions, "GL_KHR_parallel_shader_compile");

        Self {
            gles_version,
            has_pbo,
            has_fence_sync,
            has_compute,
            ahb_available,
            has_buffer_age,
            has_ext_buffer_age,
            has_partial_update,
            compressed_format_support,
            has_parallel_shader_compile,
        }
    }

    pub fn tier(&self) -> DeviceTier {
        if self.gles_version >= (3, 0) && self.has_fence_sync && self.has_pbo {
            DeviceTier::TierA
        } else {
            DeviceTier::TierB
        }
    }

    pub fn render_profile(&self, api_level: u32) -> DeviceRenderProfile {
        DeviceRenderProfile::from_detected_device(self, api_level)
    }

    /// Select a profile with platform identity preserved.  `None` is the
    /// explicit non-Android case; it must not be interpreted as Android API 0.
    pub fn render_profile_for_platform(&self, android_api: Option<u32>) -> DeviceRenderProfile {
        DeviceRenderProfile::from_detected_device_platform(self, android_api)
    }
}

/// Parse "OpenGL ES X.Y ..." into (X, Y).  Returns (2, 0) on failure.
fn parse_gles_version(version: &str) -> (u32, u32) {
    // Typical: "OpenGL ES 3.1 v1.r28p0-01rel0.xxx"
    // Desktop: "4.6.0 NVIDIA 535.129.03"
    let digits = version
        .split(|c: char| !c.is_ascii_digit() && c != '.')
        .find(|s| s.contains('.'));
    if let Some(d) = digits {
        let mut parts = d.split('.');
        let major = parts.next().and_then(|s| s.parse().ok()).unwrap_or(2);
        let minor = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
        (major, minor)
    } else {
        (2, 0)
    }
}

/// Returns the Android API level at runtime, or `None` on non-Android.
///
/// `None` is intentionally distinct from `Some(0)`: the former means the
/// Android-only API gate does not apply, while the latter means Android was
/// detected but its SDK property could not be read.
#[cfg(target_os = "android")]
pub(crate) fn android_api_level() -> Option<u32> {
    // Read `ro.build.version.sdk` directly rather than calling
    // `android_get_device_api_level()`. That function is only an exported
    // library symbol from API 29 on; at API 21..=28 it is a `static inline` in
    // <android/api-level.h> that reads this very property. A bare `extern "C"`
    // declaration bypasses the inline and pins the API-29 dynamic symbol, so
    // the whole `libmigo.so` fails to `dlopen` on an API-26 device -- the floor
    // Migo claims to support -- with "cannot locate symbol
    // android_get_device_api_level". `__system_property_get` has been stable
    // since API 1, so reading the property ourselves works on every level.
    unsafe extern "C" {
        fn __system_property_get(name: *const u8, value: *mut u8) -> i32;
    }
    // PROP_VALUE_MAX is 92; the buffer must hold that plus the NUL.
    let mut buf = [0u8; 93];
    let len = unsafe {
        __system_property_get(
            c"ro.build.version.sdk".as_ptr() as *const u8,
            buf.as_mut_ptr(),
        )
    };
    if len <= 0 {
        return Some(0);
    }
    Some(
        core::str::from_utf8(&buf[..len as usize])
            .ok()
            .and_then(|s| s.trim().parse::<u32>().ok())
            .unwrap_or(0),
    )
}

#[cfg(not(target_os = "android"))]
pub(crate) fn android_api_level() -> Option<u32> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_gles_versions() {
        assert_eq!(parse_gles_version("OpenGL ES 3.1 v1.r28p0"), (3, 1));
        assert_eq!(parse_gles_version("OpenGL ES 3.0"), (3, 0));
        assert_eq!(parse_gles_version("OpenGL ES 2.0"), (2, 0));
        assert_eq!(parse_gles_version("4.6.0 NVIDIA 535"), (4, 6));
        assert_eq!(parse_gles_version("garbage"), (2, 0));
    }

    #[test]
    fn extension_matching_is_token_exact() {
        assert!(has_extension(
            "GL_EXT_debug_marker GL_OES_EGL_image GL_EXT_texture",
            "GL_OES_EGL_image"
        ));
        assert!(!has_extension(
            "GL_OES_EGL_image_external GL_OES_EGL_image_external_essl3",
            "GL_OES_EGL_image"
        ));
    }

    #[test]
    fn ahb_api_gate_preserves_non_android_and_api_26_boundary() {
        assert!(!ahb_api_supported(None));
        assert!(!ahb_api_supported(Some(25)));
        assert!(ahb_api_supported(Some(26)));
        assert!(ahb_api_supported(Some(35)));
    }
}
