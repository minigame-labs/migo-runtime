//! `CAMetalLayer` surface construction for macOS and iOS hosts.
//!
//! One module for both, because at this boundary they differ in exactly one
//! thing: which `MIGO_PLATFORM_*` bit a host is expected to send. The layer, the
//! presenter, the graphics platform and the resize path are the same object on
//! both.

use std::{ffi::c_void, ptr::NonNull, sync::Arc};

use graphics::egl_platform::GraphicsPlatform;
use migo_capi_abi::surface::{
    MIGO_PLATFORM_IOS_CA_METAL_LAYER, MIGO_PLATFORM_MACOS_CA_METAL_LAYER, SurfaceDescriptorRef,
    ValidatedPlatformSurface,
};
use platform::apple::presenter::{AppleMetalLayerSurface, RetainedMetalLayer};
use shared::surface::SurfaceRef;

use migo_capi_abi::{MIGO_ERROR_INTERNAL, MIGO_ERROR_UNSUPPORTED_PLATFORM, MigoResult};

#[derive(Clone, Debug)]
pub(crate) enum PlatformContext {
    Graphics(GraphicsPlatform),
    #[cfg(test)]
    TestOnly,
}

/// Native identity retained for a later resize.
///
/// Shared native ownership, never a pointer into caller-owned descriptor
/// storage. The host controls the layer's geometry and display link.
#[derive(Clone)]
pub(crate) enum PlatformTarget {
    MetalLayer {
        layer: RetainedMetalLayer,
    },
    #[cfg(test)]
    TestOnly,
}

/// The layer kind THIS build can attach.
///
/// Per platform, not both: a macOS binary that advertised
/// `MIGO_PLATFORM_IOS_CA_METAL_LAYER` would be claiming an ability no host on
/// that machine can exercise, and a capability query is the one place a host
/// finds out what it may send.
///
/// A `cfg`-selected CONSTANT with the mask derived from it, rather than two
/// `cfg`-selected functions. Only one arm of a `cfg` pair is ever compiled, so
/// anything inside those arms is checked only on the platform it is for -- and
/// every gate in this repository runs on Linux, where the macOS arm would be
/// invisible. Mutation testing found that directly: a `supported_platform_kinds`
/// whose macOS arm advertised BOTH Apple layer kinds passed every test, because
/// no machine that runs them compiles that arm. With the difference reduced to
/// this one integer, the rule built on it is compiled once and checked
/// everywhere.
#[cfg(target_os = "macos")]
const OWN_LAYER_KIND: u32 = MIGO_PLATFORM_MACOS_CA_METAL_LAYER;
#[cfg(not(target_os = "macos"))]
const OWN_LAYER_KIND: u32 = MIGO_PLATFORM_IOS_CA_METAL_LAYER;

pub(crate) const fn supported_platform_kinds() -> u64 {
    1u64 << OWN_LAYER_KIND
}

/// Accept a layer payload only if its kind is the one this build advertises.
///
/// A macOS binary handed an iOS layer descriptor has been handed a pointer that
/// is a `CAMetalLayer` either way, so nothing downstream would fail -- it would
/// attach, and a host would have learned from a capability query that it could
/// send a kind this build never meant to serve. The refusal is what keeps the
/// mask and the attach path answering the same question.
fn own_layer_only(kind: u32, layer: NonNull<c_void>) -> Result<NonNull<c_void>, MigoResult> {
    if kind == OWN_LAYER_KIND {
        Ok(layer)
    } else {
        Err(MIGO_ERROR_UNSUPPORTED_PLATFORM)
    }
}

pub(crate) fn rebuild_surface(
    target: PlatformTarget,
    width: u32,
    height: u32,
) -> Result<SurfaceRef, MigoResult> {
    match target {
        PlatformTarget::MetalLayer { layer } => Ok(Arc::new(
            AppleMetalLayerSurface::from_retained_layer(layer, width, height),
        )),
        #[cfg(test)]
        PlatformTarget::TestOnly => Err(MIGO_ERROR_UNSUPPORTED_PLATFORM),
    }
}

/// Turn a fully copied and validated ABI value into Apple engine objects.
pub(crate) fn build_target(
    descriptor: SurfaceDescriptorRef,
    existing: Option<&PlatformContext>,
) -> Result<
    (
        SurfaceRef,
        GraphicsPlatform,
        PlatformTarget,
        PlatformContext,
    ),
    MigoResult,
> {
    build_target_with_retain(descriptor, existing, RetainedMetalLayer::retain)
}

fn build_target_with_retain(
    descriptor: SurfaceDescriptorRef,
    existing: Option<&PlatformContext>,
    retain: unsafe fn(NonNull<c_void>) -> RetainedMetalLayer,
) -> Result<
    (
        SurfaceRef,
        GraphicsPlatform,
        PlatformTarget,
        PlatformContext,
    ),
    MigoResult,
> {
    let configuration = descriptor.configuration();
    let layer = match descriptor.platform() {
        // Both Apple layer payloads, on both Apple platforms, with the decision
        // deferred to `own_layer_only`. No `cfg` inside this match, deliberately:
        // one arm of a `cfg` pair never reaches a compiler on the other platform,
        // and every gate in this repository runs on Linux -- so a `cfg`-split
        // match here would ship a macOS arm no machine that runs the tests had
        // ever type-checked, let alone executed. This shape also states the rule
        // once instead of twice: which kind belongs to this build is
        // `OWN_LAYER_KIND` and nothing else, the same source the capability mask
        // is derived from, so the two cannot disagree.
        ValidatedPlatformSurface::MacosMetalLayer { ca_metal_layer } => {
            own_layer_only(MIGO_PLATFORM_MACOS_CA_METAL_LAYER, ca_metal_layer)?
        }
        ValidatedPlatformSurface::IosMetalLayer { ca_metal_layer } => {
            own_layer_only(MIGO_PLATFORM_IOS_CA_METAL_LAYER, ca_metal_layer)?
        }

        // The view descriptors, and this is a DECISION rather than an omission.
        //
        // ANGLE's Metal backend takes `EGLNativeWindowType` as a plain
        // `CALayer *`, and when the layer it is handed is not already a
        // `CAMetalLayer` it allocates one of its own and adds it as a sublayer
        // (`WindowSurfaceMtl::initialize`). So passing `view.layer` would not
        // fail -- it would succeed and leave ANGLE owning the drawable layer,
        // after which the host can no longer set `maximumDrawableCount`,
        // `contentsScale` or `presentsWithTransaction`. Those are exactly the
        // surface properties the Apple design says must be measured rather than
        // fixed at a constant.
        //
        // `include/migo/platform/ios.h` already states the same division: the
        // Host Kit creates and owns the `CAMetalLayer` backing a view, and the
        // layer path "stays the authoritative one for the renderer".
        ValidatedPlatformSurface::MacosNsView { .. }
        | ValidatedPlatformSurface::IosUiView { .. } => {
            return Err(MIGO_ERROR_UNSUPPORTED_PLATFORM);
        }

        // Every non-Apple payload. Listed rather than matched with a wildcard: a
        // new platform payload must not compile until every platform module has
        // taken a position on it.
        ValidatedPlatformSurface::Android { .. }
        | ValidatedPlatformSurface::Win32 { .. }
        | ValidatedPlatformSurface::X11 { .. }
        | ValidatedPlatformSurface::Wayland { .. }
        | ValidatedPlatformSurface::OpenHarmony { .. } => {
            return Err(MIGO_ERROR_UNSUPPORTED_PLATFORM);
        }
    };

    // SAFETY: the host supplies a live CAMetalLayer for this call. Retain it
    // before constructing objects that attach can publish to another thread.
    let layer = unsafe { retain(layer) };
    let surface: SurfaceRef = Arc::new(AppleMetalLayerSurface::from_retained_layer(
        layer.clone(),
        configuration.width_pixels(),
        configuration.height_pixels(),
    ));
    let graphics_platform = match existing {
        Some(PlatformContext::Graphics(graphics_platform)) => graphics_platform.clone(),
        #[cfg(test)]
        Some(PlatformContext::TestOnly) => return Err(MIGO_ERROR_INTERNAL),
        None => {
            platform::apple::presenter::apple_metal_layer_graphics_platform().map_err(|error| {
                tracing::error!("build_target: CAMetalLayer graphics platform: {error:?}");
                MIGO_ERROR_INTERNAL
            })?
        }
    };
    Ok((
        surface,
        graphics_platform.clone(),
        PlatformTarget::MetalLayer { layer },
        PlatformContext::Graphics(graphics_platform),
    ))
}

#[cfg(test)]
pub(crate) fn test_platform_target() -> PlatformTarget {
    PlatformTarget::TestOnly
}

#[cfg(test)]
pub(crate) fn test_platform_context() -> PlatformContext {
    PlatformContext::TestOnly
}

#[cfg(test)]
mod tests {
    use super::*;
    use migo_capi_abi::{
        MIGO_ABI_VERSION_CURRENT, VersionedHeader,
        surface::{
            MIGO_PLATFORM_IOS_CA_METAL_LAYER, MIGO_PLATFORM_IOS_UI_VIEW,
            MIGO_PLATFORM_MACOS_CA_METAL_LAYER, MIGO_PLATFORM_MACOS_NS_VIEW,
            MIGO_PLATFORM_X11_WINDOW, MigoIosMetalLayerDescriptor, MigoIosUiViewDescriptor,
            MigoMacosMetalLayerDescriptor, MigoMacosNsViewDescriptor, MigoSurfaceDescriptor,
            MigoX11WindowDescriptor,
        },
    };
    use std::ffi::c_void;

    const WIDTH: u32 = 640;
    const HEIGHT: u32 = 480;

    #[derive(Default)]
    struct Refcounts {
        retains: std::sync::atomic::AtomicUsize,
        releases: std::sync::atomic::AtomicUsize,
    }

    unsafe fn counted_retain(layer: NonNull<c_void>) -> RetainedMetalLayer {
        unsafe fn retain(layer: NonNull<c_void>) {
            let counts = unsafe { layer.cast::<Refcounts>().as_ref() };
            counts
                .retains
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
        unsafe fn release(layer: NonNull<c_void>) {
            let counts = unsafe { layer.cast::<Refcounts>().as_ref() };
            counts
                .releases
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
        unsafe { RetainedMetalLayer::retain_for_test(layer, retain, release) }
    }

    #[test]
    fn attach_target_retains_before_publication_and_shares_ownership_with_rebuilds() {
        use std::sync::atomic::Ordering::SeqCst;
        let counts = Refcounts::default();
        let descriptor = own_layer(NonNull::from(&counts).as_ptr() as usize);
        let (surface, graphics, target, _) =
            build_target_with_retain(descriptor, None, counted_retain).expect("retained target");
        assert_eq!(
            counts.retains.load(SeqCst),
            1,
            "retain before attach can publish"
        );

        let rebuilt = rebuild_surface(target.clone(), 1024, 768).expect("rebuild");
        let prepared = graphics.prepare_surface(rebuilt.as_ref()).expect("prepare");
        drop(surface);
        drop(target);
        drop(rebuilt);
        assert_eq!(
            counts.releases.load(SeqCst),
            0,
            "EGL target still owns the layer"
        );
        drop(prepared);
        assert_eq!(
            counts.retains.load(SeqCst),
            1,
            "resize shares the original retain"
        );
        assert_eq!(
            counts.releases.load(SeqCst),
            1,
            "release after the final owner"
        );
    }

    #[test]
    fn failed_target_construction_releases_its_native_retain() {
        use std::sync::atomic::Ordering::SeqCst;
        let counts = Refcounts::default();
        let descriptor = own_layer(NonNull::from(&counts).as_ptr() as usize);
        assert_eq!(
            build_target_with_retain(descriptor, Some(&PlatformContext::TestOnly), counted_retain)
                .err(),
            Some(MIGO_ERROR_INTERNAL),
        );
        assert_eq!(counts.retains.load(SeqCst), 1);
        assert_eq!(counts.releases.load(SeqCst), 1);
    }

    // Identity-only tests intentionally use opaque integers on every host,
    // including Apple. Never send those values to the Objective-C runtime.
    fn build_test_target(
        descriptor: SurfaceDescriptorRef,
        existing: Option<&PlatformContext>,
    ) -> Result<
        (
            SurfaceRef,
            GraphicsPlatform,
            PlatformTarget,
            PlatformContext,
        ),
        MigoResult,
    > {
        unsafe fn retain(layer: NonNull<c_void>) -> RetainedMetalLayer {
            unsafe fn no_refcount(_: NonNull<c_void>) {}
            unsafe { RetainedMetalLayer::retain_for_test(layer, no_refcount, no_refcount) }
        }
        build_target_with_retain(descriptor, existing, retain)
    }

    fn envelope(kind: u32, payload_size: usize, payload: *const c_void) -> MigoSurfaceDescriptor {
        MigoSurfaceDescriptor {
            header: VersionedHeader {
                struct_size: size_of::<MigoSurfaceDescriptor>() as u32,
                abi_version: MIGO_ABI_VERSION_CURRENT,
            },
            generation: 1,
            platform_kind: kind,
            flags: 0,
            width_pixels: WIDTH,
            height_pixels: HEIGHT,
            scale_factor: 1.0,
            color_space: 0,
            alpha_mode: 0,
            preferred_presentation_mode: 0,
            capability_flags: 0,
            platform_descriptor_size: payload_size as u32,
            reserved0: 0,
            platform_descriptor: payload,
        }
    }

    /// Parse for exactly the kind under test.
    ///
    /// Not `SurfaceDescriptorRef::parse`, which uses
    /// `MIGO_CAPI_IMPLEMENTED_PLATFORM_KINDS` -- that constant does not carry the
    /// Apple kinds yet, and this module is a reason it eventually may, not a
    /// consumer of the claim.
    fn parse(descriptor: &MigoSurfaceDescriptor) -> SurfaceDescriptorRef {
        // SAFETY: the envelope and its payload are live locals of the caller and
        // announce their own real sizes.
        unsafe {
            SurfaceDescriptorRef::parse_for_platforms(descriptor, 1u64 << descriptor.platform_kind)
        }
        .expect("the payload built here is valid for its own kind")
    }

    fn macos_layer(layer: usize) -> SurfaceDescriptorRef {
        let payload = MigoMacosMetalLayerDescriptor {
            header: VersionedHeader {
                struct_size: size_of::<MigoMacosMetalLayerDescriptor>() as u32,
                abi_version: MIGO_ABI_VERSION_CURRENT,
            },
            platform_kind: MIGO_PLATFORM_MACOS_CA_METAL_LAYER,
            flags: 0,
            ca_metal_layer: layer as *mut c_void,
        };
        parse(&envelope(
            MIGO_PLATFORM_MACOS_CA_METAL_LAYER,
            size_of::<MigoMacosMetalLayerDescriptor>(),
            (&raw const payload).cast(),
        ))
    }

    fn ios_layer(layer: usize) -> SurfaceDescriptorRef {
        let payload = MigoIosMetalLayerDescriptor {
            header: VersionedHeader {
                struct_size: size_of::<MigoIosMetalLayerDescriptor>() as u32,
                abi_version: MIGO_ABI_VERSION_CURRENT,
            },
            platform_kind: MIGO_PLATFORM_IOS_CA_METAL_LAYER,
            flags: 0,
            ca_metal_layer: layer as *mut c_void,
        };
        parse(&envelope(
            MIGO_PLATFORM_IOS_CA_METAL_LAYER,
            size_of::<MigoIosMetalLayerDescriptor>(),
            (&raw const payload).cast(),
        ))
    }

    /// The layer kind THIS build is the platform module for.
    fn own_layer(layer: usize) -> SurfaceDescriptorRef {
        #[cfg(target_os = "macos")]
        return macos_layer(layer);
        #[cfg(not(target_os = "macos"))]
        return ios_layer(layer);
    }

    /// The other Apple platform's layer kind, which this build must refuse.
    fn other_apple_layer(layer: usize) -> SurfaceDescriptorRef {
        #[cfg(target_os = "macos")]
        return ios_layer(layer);
        #[cfg(not(target_os = "macos"))]
        return macos_layer(layer);
    }

    fn ns_view(view: usize) -> SurfaceDescriptorRef {
        let payload = MigoMacosNsViewDescriptor {
            header: VersionedHeader {
                struct_size: size_of::<MigoMacosNsViewDescriptor>() as u32,
                abi_version: MIGO_ABI_VERSION_CURRENT,
            },
            platform_kind: MIGO_PLATFORM_MACOS_NS_VIEW,
            flags: 0,
            ns_view: view as *mut c_void,
        };
        parse(&envelope(
            MIGO_PLATFORM_MACOS_NS_VIEW,
            size_of::<MigoMacosNsViewDescriptor>(),
            (&raw const payload).cast(),
        ))
    }

    fn ui_view(view: usize) -> SurfaceDescriptorRef {
        let payload = MigoIosUiViewDescriptor {
            header: VersionedHeader {
                struct_size: size_of::<MigoIosUiViewDescriptor>() as u32,
                abi_version: MIGO_ABI_VERSION_CURRENT,
            },
            platform_kind: MIGO_PLATFORM_IOS_UI_VIEW,
            flags: 0,
            ui_view: view as *mut c_void,
        };
        parse(&envelope(
            MIGO_PLATFORM_IOS_UI_VIEW,
            size_of::<MigoIosUiViewDescriptor>(),
            (&raw const payload).cast(),
        ))
    }

    fn x11_window(display: usize) -> SurfaceDescriptorRef {
        let payload = MigoX11WindowDescriptor {
            header: VersionedHeader {
                struct_size: size_of::<MigoX11WindowDescriptor>() as u32,
                abi_version: MIGO_ABI_VERSION_CURRENT,
            },
            platform_kind: MIGO_PLATFORM_X11_WINDOW,
            flags: 0,
            display: display as *mut c_void,
            window: 0x2a0_0001,
            screen: 0,
            reserved0: 0,
        };
        parse(&envelope(
            MIGO_PLATFORM_X11_WINDOW,
            size_of::<MigoX11WindowDescriptor>(),
            (&raw const payload).cast(),
        ))
    }

    fn layer_of(target: &PlatformTarget) -> *mut c_void {
        match target {
            PlatformTarget::MetalLayer { layer } => layer.as_ptr(),
            #[cfg(test)]
            PlatformTarget::TestOnly => panic!("expected a layer target"),
        }
    }

    #[test]
    fn this_build_advertises_exactly_its_own_layer_kind() {
        #[cfg(target_os = "macos")]
        let expected = 1u64 << MIGO_PLATFORM_MACOS_CA_METAL_LAYER;
        #[cfg(not(target_os = "macos"))]
        let expected = 1u64 << MIGO_PLATFORM_IOS_CA_METAL_LAYER;
        assert_eq!(supported_platform_kinds(), expected);
    }

    #[test]
    fn capability_mask_is_not_empty() {
        // The published windows-sdk-0.1.0 exported every entry point, loaded
        // cleanly, and reported a mask of zero, so it could attach nothing. A
        // mask of zero is what "no platform layer" looks like from outside, and
        // Apple was in exactly that state until this module existed.
        assert_ne!(supported_platform_kinds(), 0);
    }

    /// Exactly one bit, on every platform, which is the half of the rule that
    /// survives `cfg`.
    ///
    /// The assertion above it restates this build's expected kind and can only
    /// check the arm the host compiles; this one is compiled everywhere and is
    /// what catches a mask that grew a second kind. It is here because a mutation
    /// that made the macOS arm advertise both Apple layers passed the whole suite
    /// on Linux -- advertising a kind no host on this machine can exercise is
    /// precisely what the constant above forbids, and nothing had been checking
    /// it.
    #[test]
    fn exactly_one_layer_kind_is_advertised() {
        assert_eq!(
            supported_platform_kinds().count_ones(),
            1,
            "advertising more than one layer kind claims an ability some host cannot exercise"
        );
    }

    /// The host's layer arrives as an opaque token and comes back out unchanged,
    /// sized by the envelope rather than by anything this module invents.
    #[test]
    fn a_host_owned_layer_becomes_engine_objects_carrying_that_layer() {
        let (surface, graphics_platform, target, context) =
            build_test_target(own_layer(0x1234), None).expect("this build's own layer kind");

        assert_eq!(layer_of(&target) as usize, 0x1234);
        assert_eq!(surface.size(), (WIDTH, HEIGHT));
        assert_eq!(
            graphics_platform.platform_identity(),
            platform::apple::presenter::apple_metal_layer_graphics_platform()
                .expect("CAMetalLayer ANGLE platform")
                .platform_identity(),
            "the layer path must run on the CAMetalLayer graphics platform"
        );
        assert!(matches!(context, PlatformContext::Graphics(_)));
    }

    /// A reattachment reuses the one provider rather than loading ANGLE again.
    #[test]
    fn a_second_attach_reuses_the_graphics_platform_it_was_given() {
        let (_, first, _, context) =
            build_test_target(own_layer(0x1234), None).expect("cold layer target");
        let (_, reused, _, _) = build_test_target(own_layer(0x5678), Some(&context))
            .expect("a second layer must reuse the stored platform");

        assert!(
            Arc::ptr_eq(first.egl_provider(), reused.egl_provider()),
            "a reattachment must not construct a second EGL provider"
        );
    }

    /// The view descriptors, on every host, because the refusal is not
    /// platform-conditional. Handed a plain `CALayer`, ANGLE's Metal backend
    /// allocates its own `CAMetalLayer` sublayer and the host silently loses
    /// `maximumDrawableCount`, `contentsScale` and `presentsWithTransaction`.
    #[test]
    fn a_view_descriptor_is_refused_rather_than_resolved_to_a_layer() {
        assert_eq!(
            super::build_target(ns_view(0x1234), None).err(),
            Some(MIGO_ERROR_UNSUPPORTED_PLATFORM)
        );
        assert_eq!(
            super::build_target(ui_view(0x1234), None).err(),
            Some(MIGO_ERROR_UNSUPPORTED_PLATFORM)
        );
    }

    /// A macOS binary must not attach an iOS layer, or the reverse. The kinds
    /// are distinct so that a host finds out from a capability query rather than
    /// from a renderer that half works.
    #[test]
    fn the_other_apple_platforms_layer_kind_is_refused() {
        assert_eq!(
            super::build_target(other_apple_layer(0x1234), None).err(),
            Some(MIGO_ERROR_UNSUPPORTED_PLATFORM)
        );
    }

    #[test]
    fn a_non_apple_payload_is_refused() {
        assert_eq!(
            super::build_target(x11_window(0x1000), None).err(),
            Some(MIGO_ERROR_UNSUPPORTED_PLATFORM)
        );
    }

    /// A stored context does not buy a bad descriptor a way in.
    ///
    /// The mirror of `linux`'s `a_stored_x11_context_refuses_a_wayland_descriptor`,
    /// and it pins an ORDERING rather than a value: the payload is validated
    /// before `existing` is consulted at all. The plausible defect is a fast path
    /// -- "we already have a graphics platform, so reuse it and skip the work" --
    /// which reads like an optimisation and would let a view descriptor or the
    /// other platform's layer through on every attach after the first. The first
    /// attach would be correctly refused, so the bug would only appear on
    /// reattachment.
    #[test]
    fn a_stored_context_does_not_excuse_a_descriptor_this_build_refuses() {
        let (_, _, _, context) =
            build_test_target(own_layer(0x1234), None).expect("a cold attach to store a context");

        for (what, descriptor) in [
            ("a view", ns_view(0x2222)),
            (
                "the other Apple platform's layer",
                other_apple_layer(0x3333),
            ),
            ("a non-Apple payload", x11_window(0x4444)),
        ] {
            assert_eq!(
                build_test_target(descriptor, Some(&context)).err(),
                Some(MIGO_ERROR_UNSUPPORTED_PLATFORM),
                "{what} was accepted once a context existed"
            );
        }
    }

    /// Identity for a layer is the layer; a resize keeps it and takes the new
    /// size. The host owns the geometry, so the engine reads it rather than
    /// deciding it.
    #[test]
    fn a_rebuild_keeps_the_layer_and_takes_the_new_size() {
        let (_, _, target, _) = build_test_target(own_layer(0x1234), None).expect("layer target");
        let rebuilt = rebuild_surface(target.clone(), 1024, 768).expect("rebuild");

        assert_eq!(rebuilt.size(), (1024, 768));
        assert_eq!(layer_of(&target) as usize, 0x1234);
    }

    /// The test-only seam is refused by both paths that could mistake it for a
    /// real platform. It exists so this crate's Session tests can hold a context
    /// without a native layer, and a build that quietly accepted it would let a
    /// test assert against a platform nobody constructed.
    #[test]
    fn the_test_only_seam_is_never_mistaken_for_a_platform() {
        assert_eq!(
            rebuild_surface(test_platform_target(), WIDTH, HEIGHT).err(),
            Some(MIGO_ERROR_UNSUPPORTED_PLATFORM)
        );
        assert_eq!(
            build_test_target(own_layer(0x1234), Some(&test_platform_context())).err(),
            Some(MIGO_ERROR_INTERNAL)
        );
    }
}
