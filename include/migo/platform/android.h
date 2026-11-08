#ifndef MIGO_PLATFORM_ANDROID_H_
#define MIGO_PLATFORM_ANDROID_H_

#include <migo/surface.h>

/*
 * native_window is an ANativeWindow*. Migo acquires its own strong reference
 * before attach returns success and releases that reference before the release
 * observer reaches MIGO_SURFACE_RELEASE_RELEASED.
 */
typedef struct MigoAndroidNativeWindowDescriptor {
    uint32_t struct_size;
    uint32_t abi_version;
    MigoPlatformKind platform_kind;
    MigoPlatformDescriptorFlags flags;
    void *native_window;
} MigoAndroidNativeWindowDescriptor;

/*
 * Compatibility entry point for the former JNI-dependent audio backend.
 * The current native Oboe backend needs no JavaVM; new hosts need not call it.
 * Neither pointer is retained or dereferenced. Repeated calls are harmless.
 *
 * Returns MIGO_ERROR_INVALID_ARGUMENT if vm is NULL.
 */
MIGO_API MigoResult MIGO_CALL migo_android_init_context(void *vm, void *activity);

#endif /* MIGO_PLATFORM_ANDROID_H_ */
