#ifndef MIGO_PLATFORM_OPENHARMONY_H_
#define MIGO_PLATFORM_OPENHARMONY_H_

#include <migo/surface.h>

/*
 * native_window is an OHNativeWindow*. The OpenHarmony implementation takes its
 * own native-object reference before attach returns success and releases that
 * reference before the release observer reaches MIGO_SURFACE_RELEASE_RELEASED.
 */
typedef struct MigoOpenHarmonyNativeWindowDescriptor {
    uint32_t struct_size;
    uint32_t abi_version;
    MigoPlatformKind platform_kind;
    MigoPlatformDescriptorFlags flags;
    void *native_window;
} MigoOpenHarmonyNativeWindowDescriptor;

#endif /* MIGO_PLATFORM_OPENHARMONY_H_ */
