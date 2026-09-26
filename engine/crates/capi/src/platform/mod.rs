//! Where the ABI meets a platform.
//!
//! Every platform detail the entry points would otherwise name lives behind
//! this module: which surface type wraps the host's window, which graphics
//! platform drives it, and which `PlatformServices` implementation supplies
//! device services and the frame clock. The entry points stay free of `cfg`,
//! so adding a platform means adding a file here rather than editing the
//! middle of `surface.rs`.

#[cfg(target_os = "android")]
mod android;
// `target_vendor` and not two `target_os` arms: macOS and iOS share one module,
// and a third Apple platform arriving should reach the same code rather than
// silently fall through to `unsupported`.
//
// `test` compiles it on Linux too, where it is never wired up -- the re-export
// below stays Apple-only. That is deliberate: this module decides eight match
// arms over `ValidatedPlatformSurface`, and without it the only machine that
// ever type-checked them would be a macOS runner. `platform/test-support`, which
// this crate already enables from `[dev-dependencies]`, is what makes the Apple
// presenter visible here across the crate boundary.
#[cfg(any(target_vendor = "apple", test))]
mod apple;
#[cfg(all(target_os = "linux", not(target_env = "ohos")))]
mod linux;
#[cfg(all(target_os = "linux", target_env = "ohos"))]
mod ohos;
#[cfg(not(any(
    target_os = "android",
    target_os = "windows",
    target_vendor = "apple",
    all(target_os = "linux", not(target_env = "ohos")),
    all(target_os = "linux", target_env = "ohos")
)))]
mod unsupported;
#[cfg(target_os = "windows")]
mod windows;

#[cfg(target_os = "android")]
pub(crate) use android::{
    PlatformContext, PlatformTarget, build_target, rebuild_surface, supported_platform_kinds,
};
#[cfg(target_vendor = "apple")]
pub(crate) use apple::{
    PlatformContext, PlatformTarget, build_target, rebuild_surface, supported_platform_kinds,
};
#[cfg(all(target_os = "linux", not(target_env = "ohos")))]
pub(crate) use linux::{
    PlatformContext, PlatformTarget, build_target, rebuild_surface, supported_platform_kinds,
};
#[cfg(all(target_os = "linux", target_env = "ohos"))]
pub(crate) use ohos::{
    PlatformContext, PlatformTarget, build_target, rebuild_surface, supported_platform_kinds,
};
#[cfg(not(any(
    target_os = "android",
    target_os = "windows",
    target_vendor = "apple",
    all(target_os = "linux", not(target_env = "ohos")),
    all(target_os = "linux", target_env = "ohos")
)))]
pub(crate) use unsupported::{
    PlatformContext, PlatformTarget, build_target, rebuild_surface, supported_platform_kinds,
};
#[cfg(target_os = "windows")]
pub(crate) use windows::{
    PlatformContext, PlatformTarget, build_target, rebuild_surface, supported_platform_kinds,
};

#[cfg(all(test, target_os = "android"))]
pub(crate) use android::{test_platform_context, test_platform_target};
#[cfg(all(test, target_vendor = "apple"))]
pub(crate) use apple::{test_platform_context, test_platform_target};
#[cfg(all(test, target_os = "linux", not(target_env = "ohos")))]
pub(crate) use linux::{test_platform_context, test_platform_target};
#[cfg(all(test, target_os = "linux", target_env = "ohos"))]
pub(crate) use ohos::{test_platform_context, test_platform_target};
#[cfg(all(
    test,
    not(any(
        target_os = "android",
        target_os = "windows",
        target_vendor = "apple",
        all(target_os = "linux", not(target_env = "ohos")),
        all(target_os = "linux", target_env = "ohos")
    ))
))]
pub(crate) use unsupported::{test_platform_context, test_platform_target};
#[cfg(all(test, target_os = "windows"))]
pub(crate) use windows::{test_platform_context, test_platform_target};

#[cfg(test)]
mod contract_tests {
    use super::supported_platform_kinds;
    use migo_capi_abi::surface::MIGO_CAPI_IMPLEMENTED_PLATFORM_KINDS;

    /// Attaching a kind requires both halves: a platform module that can build
    /// the native objects, and a parser that will decode its payload.
    ///
    /// A kind present in only one half fails in a way that misdirects. Missing
    /// from the parser, attach returns UNSUPPORTED_PLATFORM even though the
    /// platform layer is right there, reading like the host passed the wrong
    /// kind. Missing from the platform module, the library loads, exports
    /// everything and advertises nothing -- which is how a Windows package
    /// shipped that could not attach a window.
    ///
    /// The constant is also a ledger, not only a parser capability: its own
    /// comment admits a kind "only after an attach succeeded on a device". So
    /// there is a second way to reach this assertion, and an Apple build is in it
    /// right now -- the parser has the arms, the platform module builds the
    /// objects, and the ledger has not admitted the kinds because the evidence
    /// lives on `apple-sdk.yml`, which does not run on pull requests. The message
    /// names both causes so whoever hits it does not go looking for a missing
    /// parser arm that is not missing.
    #[test]
    fn every_attachable_kind_is_also_parseable() {
        assert_eq!(
            supported_platform_kinds() & !MIGO_CAPI_IMPLEMENTED_PLATFORM_KINDS,
            0,
            "this build advertises a platform kind MIGO_CAPI_IMPLEMENTED_PLATFORM_KINDS does \
             not carry. Either the ABI parser has no arm for it, or it has one and the kind is \
             still waiting on the attach evidence the ledger admits kinds on"
        );
    }
}

/// Whether this build can attach the given `MIGO_PLATFORM_*` kind.
///
/// The single test both the attach path and the capability query go through. A
/// kind outside the bitmask's width is unsupported by definition rather than by
/// arithmetic accident.
#[cfg(test)]
pub(crate) fn kind_is_supported(platform_kind: u32) -> bool {
    if platform_kind >= u64::BITS {
        return false;
    }
    supported_platform_kinds() & (1u64 << platform_kind) != 0
}

/// Send the opt-in developer log to wherever this platform's diagnostics go.
///
/// A subscriber that writes to standard output is only a diagnostic channel on
/// a platform that shows standard output. Android discards it, so a C host
/// there saw nothing at all from the engine -- the one channel the header
/// promises was silently empty on the platform where a host has fewest
/// alternatives.
#[cfg(target_os = "android")]
pub(crate) fn install_dev_logging(level: shared::config::LogLevel) {
    platform::android::install_logcat_diagnostics(level);
}

/// Make sure the engine can decode images on this platform.
///
/// Every platform but Android compiles the Rust decoders in. Android leaves them
/// out and expects the Java SDK's JNI setup to register decoders, which a C host
/// never runs.
#[cfg(target_os = "android")]
pub(crate) fn install_image_decoders() {
    platform::android::install_skia_image_decoders();
}

#[cfg(not(target_os = "android"))]
pub(crate) fn install_image_decoders() {}

#[cfg(not(target_os = "android"))]
pub(crate) fn install_dev_logging(level: shared::config::LogLevel) {
    use shared::config::LogLevel;
    let level = match level {
        LogLevel::Trace => tracing::Level::TRACE,
        LogLevel::Debug => tracing::Level::DEBUG,
        LogLevel::Info => tracing::Level::INFO,
        LogLevel::Warn => tracing::Level::WARN,
        LogLevel::Error | LogLevel::Off => tracing::Level::ERROR,
    };
    // stderr, not the default stdout. Diagnostics are not program output, and
    // on Apple the difference is the whole channel: an XCTest host's stderr is
    // what `xcodebuild` folds into its log -- every "Test Case ... started" line
    // arrives that way -- and its stdout does not appear at all. So a session
    // that failed to start on the iOS simulator wrote its one explanatory line
    // to a stream nobody was reading, and the failure reached the lane as a bare
    // MIGO_ERROR_INTERNAL. Measured 2026-09-06: `MIGO_CAPI_LOG=info` produced
    // zero engine lines in a run whose XCTest lines were all present.
    let _ = tracing_subscriber::fmt()
        .with_max_level(level)
        .with_target(true)
        .with_writer(std::io::stderr)
        .try_init();
}
