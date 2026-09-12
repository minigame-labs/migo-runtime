use crate::device_caps::{DeviceCapabilities, DeviceTier};

/// Why a particular [`DeviceRenderProfile`] was selected.
///
/// Exposes the branching reason so operators can verify the profile matches
/// the device class without running on hardware.  The budget values for
/// [`NonAndroid`] and [`AndroidModernApi`] are identical today; callers
/// MUST NOT assume that will remain true — device measurement may diverge
/// them once real upload latency data exists.
///
/// [`NonAndroid`]: DeviceProfileSource::NonAndroid
/// [`AndroidModernApi`]: DeviceProfileSource::AndroidModernApi
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceProfileSource {
    /// ES 2.0 or a driver that lacks fence sync or PBO — upload thread is
    /// disabled; conservative sync-fallback-only profile.
    TierBDevice,
    /// Android API level ≤ 23 (pre-Marshmallow).  The GL tier is TierA, but
    /// the system graphics stack from that era has known upload instabilities
    /// on several OEM driver lines, so the conservative profile is preferred
    /// until device-validated data says otherwise.
    AndroidOldApi { level: u32 },
    /// Android API level > 23 (Marshmallow+).  Full async upload capabilities
    /// confirmed; 4-job / 4-MiB per-frame budget applies.
    AndroidModernApi { level: u32 },
    /// Non-Android host (Linux, Windows, macOS / ANGLE Metal / D3D).  The
    /// API-level gate that exists for old Android OEM drivers does not apply
    /// here.  Budget values are carried over from the Android-modern profile;
    /// they need device measurement before being published as a performance
    /// claim — do not widen this profile further without bench evidence.
    NonAndroid,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeviceRenderProfile {
    pub max_upload_jobs_per_frame: usize,
    pub max_upload_bytes_per_frame: usize,
    pub enable_partial_damage: bool,
    pub enable_layer_cache: bool,
    /// Why this profile was selected.  Observable without a device.
    pub source: DeviceProfileSource,
}

impl DeviceRenderProfile {
    /// Build from the detected device tier and a raw Android API level.
    ///
    /// Pass `android_api_level()` here on Android; on non-Android hosts use
    /// [`from_detected_device_platform`] with `None` so the API-level gate
    /// (designed for old OEM drivers) is not applied to a capable desktop
    /// or ANGLE Metal context.
    pub fn from_detected_device(caps: &DeviceCapabilities, api_level: u32) -> Self {
        let tier = caps.tier();
        Self::from_tier(caps, api_level, tier)
    }

    /// Platform-aware variant.  Pass `None` on non-Android hosts; the API
    /// gate is skipped and [`DeviceProfileSource::NonAndroid`] is recorded.
    pub fn from_detected_device_platform(
        caps: &DeviceCapabilities,
        android_api: Option<u32>,
    ) -> Self {
        let tier = caps.tier();
        Self::from_tier_with_api(caps, android_api, tier)
    }

    /// Build from an explicitly supplied tier (for testing and migration).
    ///
    /// In release mode the `tier` argument is ignored and the detected tier
    /// is used; in debug mode a tier mismatch panics.
    pub fn from_caps(caps: &DeviceCapabilities, api_level: u32, tier: DeviceTier) -> Self {
        let detected_tier = caps.tier();
        debug_assert_eq!(
            tier, detected_tier,
            "DeviceRenderProfile::from_caps tier must match detected device tier"
        );

        Self::from_tier(caps, api_level, detected_tier)
    }

    /// Internal: legacy `u32` bridge.  Wraps to the `Option<u32>` variant so
    /// callers that pre-date the platform split keep their existing semantics.
    /// `api_level = 0` is treated as "Android API unknown / ancient" → conservative,
    /// which is the correct interpretation for code that reads `android_api_level()`
    /// and does not need to distinguish "zero" from "non-Android".
    fn from_tier(caps: &DeviceCapabilities, api_level: u32, tier: DeviceTier) -> Self {
        Self::from_tier_with_api(caps, Some(api_level), tier)
    }

    /// Core profile selection.  `android_api = None` means the host is not
    /// Android; `Some(level)` is an Android API level (0 = unknown / ancient).
    fn from_tier_with_api(
        caps: &DeviceCapabilities,
        android_api: Option<u32>,
        tier: DeviceTier,
    ) -> Self {
        if tier == DeviceTier::TierB {
            return Self {
                max_upload_jobs_per_frame: 1,
                max_upload_bytes_per_frame: 512 * 1024,
                enable_partial_damage: false,
                enable_layer_cache: false,
                source: DeviceProfileSource::TierBDevice,
            };
        }

        if let Some(level) = android_api {
            if level <= 23 {
                return Self {
                    max_upload_jobs_per_frame: 1,
                    max_upload_bytes_per_frame: 512 * 1024,
                    enable_partial_damage: false,
                    enable_layer_cache: false,
                    source: DeviceProfileSource::AndroidOldApi { level },
                };
            }
        }
        // need device measurement on ANGLE/desktop GL before a performance
        // claim can be made for those paths.
        let source = match android_api {
            None => DeviceProfileSource::NonAndroid,
            Some(level) => DeviceProfileSource::AndroidModernApi { level },
        };
        Self {
            max_upload_jobs_per_frame: 4,
            max_upload_bytes_per_frame: 4 * 1024 * 1024,
            enable_partial_damage: caps.has_fence_sync,
            enable_layer_cache: true,
            source,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api23_tier_b_device_uses_conservative_profile() {
        let caps = DeviceCapabilities {
            has_parallel_shader_compile: false,
            gles_version: (2, 0),
            has_pbo: false,
            has_fence_sync: false,
            has_compute: false,
            ahb_available: false,
            has_buffer_age: false,
            has_ext_buffer_age: false,
            has_partial_update: false,
            compressed_format_support: crate::compressed_upload::CompressedFormatSupport {
                etc2: false,
                astc: false,
            },
        };

        let profile = DeviceRenderProfile::from_caps(&caps, 23, DeviceTier::TierB);
        assert_eq!(profile.max_upload_jobs_per_frame, 1);
        assert!(!profile.enable_partial_damage);
    }

    #[test]
    fn api24_tier_a_device_uses_aggressive_profile() {
        let caps = DeviceCapabilities {
            has_parallel_shader_compile: false,
            gles_version: (3, 0),
            has_pbo: true,
            has_fence_sync: true,
            has_compute: false,
            ahb_available: false,
            has_buffer_age: false,
            has_ext_buffer_age: false,
            has_partial_update: false,
            compressed_format_support: crate::compressed_upload::CompressedFormatSupport {
                etc2: true,
                astc: false,
            },
        };

        let profile = DeviceRenderProfile::from_caps(&caps, 24, DeviceTier::TierA);

        assert_eq!(profile.max_upload_jobs_per_frame, 4);
        assert_eq!(profile.max_upload_bytes_per_frame, 4 * 1024 * 1024);
        assert!(profile.enable_partial_damage);
        assert!(profile.enable_layer_cache);
    }

    #[test]
    fn from_caps_uses_detected_tier_when_caller_input_disagrees() {
        let caps = DeviceCapabilities {
            has_parallel_shader_compile: false,
            gles_version: (2, 0),
            has_pbo: false,
            has_fence_sync: false,
            has_compute: false,
            ahb_available: false,
            has_buffer_age: false,
            has_ext_buffer_age: false,
            has_partial_update: false,
            compressed_format_support: crate::compressed_upload::CompressedFormatSupport {
                etc2: false,
                astc: false,
            },
        };

        assert_eq!(caps.tier(), DeviceTier::TierB);

        let render_profile = caps.render_profile(24);
        let detected_profile = DeviceRenderProfile::from_detected_device(&caps, 24);

        assert_eq!(render_profile, detected_profile);
        assert_eq!(render_profile.max_upload_jobs_per_frame, 1);
        assert!(!render_profile.enable_partial_damage);

        let mismatched = std::panic::catch_unwind(|| {
            DeviceRenderProfile::from_caps(&caps, 24, DeviceTier::TierA)
        });

        if cfg!(debug_assertions) {
            assert!(mismatched.is_err());
        } else {
            let profile = mismatched.unwrap();
            assert_eq!(profile, render_profile);
        }
    }

    #[test]
    fn non_android_tier_a_profile_is_explicit_and_not_api_zero() {
        let caps = DeviceCapabilities {
            has_parallel_shader_compile: false,
            gles_version: (3, 0),
            has_pbo: true,
            has_fence_sync: true,
            has_compute: false,
            ahb_available: false,
            has_buffer_age: false,
            has_ext_buffer_age: false,
            has_partial_update: false,
            compressed_format_support: crate::compressed_upload::CompressedFormatSupport {
                etc2: true,
                astc: false,
            },
        };

        let profile = DeviceRenderProfile::from_detected_device_platform(&caps, None);
        assert_eq!(profile.source, DeviceProfileSource::NonAndroid);
        assert_eq!(profile.max_upload_jobs_per_frame, 4);
        assert_eq!(profile.max_upload_bytes_per_frame, 4 * 1024 * 1024);
    }
}
