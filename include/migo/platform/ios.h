#ifndef MIGO_PLATFORM_IOS_H_
#define MIGO_PLATFORM_IOS_H_

#include <migo/surface.h>

/*
 * iOS did not have a native Surface descriptor because the platform candidate
 * assumed the iOS runtime would be a WKWebView container, where WebKit owns
 * the drawing surface and Migo owns nothing. That assumption was wrong for the
 * reason recorded in docs/apple-final-implementation-plan.md: the JIT boundary
 * on iOS is drawn around the process, not around the engine, so the shipping
 * architecture runs content JavaScript inside WebKit's WebContent process and
 * brings rendering back to the host process. Rendering in the host process
 * requires a host-owned Metal surface, so iOS needs the same typed descriptors
 * macOS already declares.
 *
 * ui_view is a UIView*. The convenience path: the Host Kit creates and owns the
 * single CAMetalLayer backing that view, and the layer path below stays the
 * authoritative one for the renderer.
 */
typedef struct MigoIosUiViewDescriptor {
    uint32_t struct_size;
    uint32_t abi_version;
    MigoPlatformKind platform_kind;
    MigoPlatformDescriptorFlags flags;
    void *ui_view;
} MigoIosUiViewDescriptor;

/*
 * ca_metal_layer is a CAMetalLayer*. Kept separate from UIView for the reason
 * macOS keeps NSView and CAMetalLayer separate: a tagless void* would let a
 * host set the wrong kind, compile cleanly, and hand a UIView* to code that
 * calls nextDrawable on it. Migo retains the Objective-C object before attach
 * returns success, keeps it through asynchronous native Surface retirement,
 * and releases its reference before publishing MIGO_SURFACE_RELEASE_RELEASED.
 * The host continues to control the layer's geometry and display link.
 */
typedef struct MigoIosMetalLayerDescriptor {
    uint32_t struct_size;
    uint32_t abi_version;
    MigoPlatformKind platform_kind;
    MigoPlatformDescriptorFlags flags;
    void *ca_metal_layer;
} MigoIosMetalLayerDescriptor;

#endif /* MIGO_PLATFORM_IOS_H_ */
