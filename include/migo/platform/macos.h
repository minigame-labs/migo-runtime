#ifndef MIGO_PLATFORM_MACOS_H_
#define MIGO_PLATFORM_MACOS_H_

#include <migo/surface.h>

/*
 * ns_view is an NSView*. A future implementation retains it before attach
 * returns success and releases it before the release observer reaches
 * MIGO_SURFACE_RELEASE_RELEASED.
 */
typedef struct MigoMacosNsViewDescriptor {
    uint32_t struct_size;
    uint32_t abi_version;
    MigoPlatformKind platform_kind;
    MigoPlatformDescriptorFlags flags;
    void *ns_view;
} MigoMacosNsViewDescriptor;

/*
 * ca_metal_layer is a CAMetalLayer*. Keeping this separate from NSView avoids
 * a tagless pointer and permits a compositor-owned layer integration. Migo
 * retains it before attach returns success, keeps it through asynchronous
 * native Surface retirement, and releases its reference before publishing
 * MIGO_SURFACE_RELEASE_RELEASED. The host controls geometry and the display link.
 */
typedef struct MigoMacosMetalLayerDescriptor {
    uint32_t struct_size;
    uint32_t abi_version;
    MigoPlatformKind platform_kind;
    MigoPlatformDescriptorFlags flags;
    void *ca_metal_layer;
} MigoMacosMetalLayerDescriptor;

#endif /* MIGO_PLATFORM_MACOS_H_ */
