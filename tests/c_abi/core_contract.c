#include <migo/migo.h>

#include <stddef.h>
#include <stdint.h>

#define MIGO_CHECK_PREFIX(TYPE)                                                \
    _Static_assert(offsetof(TYPE, struct_size) == 0, #TYPE " size prefix");   \
    _Static_assert(offsetof(TYPE, abi_version) == 4, #TYPE " ABI prefix")

_Static_assert(MIGO_C_ABI_CANDIDATE == 1, "candidate marker");
/*
 * The macro answers "does a linkable runtime exist for this target", so the
 * assertion checks the rule rather than a constant: desktop Linux and Android
 * each ship one, every other target does not. Asserting a fixed 0 here would
 * have to be relaxed the moment any platform gained an implementation, which is
 * exactly when the check is worth having.
 *
 * The three target macros are asserted mutually exclusive first. They are not
 * independent facts: Android and OpenHarmony are Linux kernels and define
 * __linux__ too, so a classifier written in terms of __linux__ alone answers
 * for all three at once. Getting that wrong routes a host to the wrong surface
 * descriptor and the wrong platform library, on a target that compiles cleanly
 * either way.
 */
_Static_assert(MIGO_PLATFORM_IS_ANDROID + MIGO_PLATFORM_IS_OPENHARMONY +
                       MIGO_PLATFORM_IS_LINUX_GNU <= 1,
               "at most one Linux-kernel target may claim a translation unit");
#if defined(__ANDROID__)
_Static_assert(MIGO_PLATFORM_IS_ANDROID == 1, "Android must classify as Android");
_Static_assert(MIGO_PLATFORM_IS_LINUX_GNU == 0, "Android is not desktop Linux");
_Static_assert(MIGO_C_ABI_HAS_RUNTIME == 1, "Android ships a static runtime");
#elif defined(__OHOS__) || defined(__OHOS_FAMILY__)
_Static_assert(MIGO_PLATFORM_IS_OPENHARMONY == 1, "OpenHarmony must classify as itself");
_Static_assert(MIGO_PLATFORM_IS_LINUX_GNU == 0, "OpenHarmony is not desktop Linux");
_Static_assert(MIGO_C_ABI_HAS_RUNTIME == 1, "OpenHarmony ships a static runtime");
#elif defined(__linux__) && defined(__GLIBC__)
_Static_assert(MIGO_PLATFORM_IS_LINUX_GNU == 1, "glibc Linux is the desktop target");
_Static_assert(MIGO_C_ABI_HAS_RUNTIME == 1, "desktop Linux ships a runtime");
#elif defined(_WIN32) && !defined(__CYGWIN__)
_Static_assert(MIGO_PLATFORM_IS_WINDOWS == 1, "Windows must classify as Windows");
_Static_assert(MIGO_PLATFORM_IS_LINUX_GNU == 0, "Windows is not desktop Linux");
_Static_assert(MIGO_C_ABI_HAS_RUNTIME == 1, "Windows ships migo.lib with a Win32 surface backend");
#else
_Static_assert(MIGO_PLATFORM_IS_WINDOWS == 0, "only _WIN32 targets classify as Windows");
_Static_assert(MIGO_C_ABI_HAS_RUNTIME == 0, "no runtime on unclassified targets");
#endif
_Static_assert(MIGO_ABI_VERSION_1 == UINT32_C(1), "ABI version value");
/* 72, not 64: on_request_frame was appended in the frame-pacing slice. Appending
 * is what keeps this compatible -- an older host's struct_size still describes a
 * valid prefix, and such a host simply does not drive frames. Growing the struct
 * anywhere but the end would silently reinterpret existing fields. */
_Static_assert(sizeof(MigoResult) == 4, "fixed-width result");
_Static_assert(MIGO_OK == INT32_C(0), "success value");
_Static_assert(MIGO_ERROR_INVALID_ARGUMENT == -INT32_C(1), "invalid argument value");
_Static_assert(MIGO_ERROR_UNSUPPORTED_ABI == -INT32_C(2), "unsupported ABI value");
_Static_assert(MIGO_ERROR_UNSUPPORTED_PLATFORM == -INT32_C(3),
               "unsupported platform value");
_Static_assert(MIGO_ERROR_UNSUPPORTED_CAPABILITY == -INT32_C(4),
               "unsupported capability value");
_Static_assert(MIGO_ERROR_INVALID_STATE == -INT32_C(5), "invalid state value");
_Static_assert(MIGO_ERROR_WRONG_THREAD == -INT32_C(6), "wrong thread value");
_Static_assert(MIGO_ERROR_STALE_SURFACE == -INT32_C(7), "stale Surface value");
_Static_assert(MIGO_ERROR_CANCELLED == -INT32_C(8), "cancelled value");
_Static_assert(MIGO_ERROR_DISPATCH_REJECTED == -INT32_C(9),
               "dispatch rejection value");
_Static_assert(MIGO_ERROR_OUT_OF_MEMORY == -INT32_C(10), "out-of-memory value");
_Static_assert(MIGO_ERROR_INTERNAL == -INT32_C(11), "internal error value");

_Static_assert(MIGO_ERROR_WOULD_BLOCK == -INT32_C(12), "would-block value");

/* Must match Rust's TouchPoint, which asserts 20 bytes on its side. A mismatch
 * here would corrupt every touch the host sends, silently. */
_Static_assert(sizeof(MigoTouchPoint) == 20, "touch point layout");
_Static_assert(offsetof(MigoTouchPoint, id) == 0, "touch point id offset");
_Static_assert(offsetof(MigoTouchPoint, x) == 4, "touch point x offset");
_Static_assert(offsetof(MigoTouchPoint, y) == 8, "touch point y offset");
_Static_assert(offsetof(MigoTouchPoint, pressure) == 12, "touch point pressure offset");
_Static_assert(offsetof(MigoTouchPoint, flags) == 16, "touch point flags offset");
MIGO_CHECK_PREFIX(MigoTouchEvent);
_Static_assert(MIGO_TOUCH_MAX_POINTS == 10, "inline array capacity");
_Static_assert(MIGO_TOUCH_START == UINT32_C(0), "touch start value");
_Static_assert(MIGO_TOUCH_MOVE == UINT32_C(1), "touch move value");
_Static_assert(MIGO_TOUCH_END == UINT32_C(2), "touch end value");
_Static_assert(MIGO_TOUCH_CANCEL == UINT32_C(3), "touch cancel value");

#ifdef MIGO_ERROR_ALREADY_DETACHED
#error "a consumed attachment cannot safely report already-detached through the old pointer"
#endif

MIGO_CHECK_PREFIX(MigoError);
MIGO_CHECK_PREFIX(MigoEngineConfig);
MIGO_CHECK_PREFIX(MigoSessionConfig);
MIGO_CHECK_PREFIX(MigoPlatformSurfaceDescriptor);
MIGO_CHECK_PREFIX(MigoSurfaceMetrics);
MIGO_CHECK_PREFIX(MigoSurfaceDescriptor);
MIGO_CHECK_PREFIX(MigoHostCallbacks);

_Static_assert(offsetof(MigoSurfaceDescriptor, generation) == 8,
               "generation is naturally aligned");
_Static_assert(offsetof(MigoSurfaceDescriptor, platform_descriptor) == 64,
               "platform payload pointer is append-safe");
_Static_assert(sizeof(MigoSurfaceMetrics) == 48, "metrics v1 layout");

/* Mirrors capi-abi's assertions on MigoSurfaceReleaseStatus. The Rust side
 * spells the first eight bytes as a nested VersionedHeader and this side spells
 * them flat; the offsets are what make those two spellings the same record. */
MIGO_CHECK_PREFIX(MigoSurfaceReleaseStatus);
_Static_assert(sizeof(MigoSurfaceReleaseStatus) == 24, "release status v1 layout");
_Static_assert(offsetof(MigoSurfaceReleaseStatus, generation) == 8,
               "release status generation offset");
_Static_assert(offsetof(MigoSurfaceReleaseStatus, state) == 16,
               "release status state offset");
_Static_assert(MIGO_SURFACE_RELEASE_PENDING == UINT32_C(0), "pending state value");
_Static_assert(MIGO_SURFACE_RELEASE_RELEASED == UINT32_C(1), "released state value");

#if UINTPTR_MAX == UINT64_MAX
_Static_assert(sizeof(MigoError) == 32, "LP64 error layout");
_Static_assert(sizeof(MigoSurfaceDescriptor) == 72, "LP64 Surface layout");
_Static_assert(sizeof(MigoHostCallbacks) == 128, "LP64 callback layout");
_Static_assert(offsetof(MigoHostCallbacks, on_surface_released) == 96,
               "release wakeup stays where it was appended");
_Static_assert(offsetof(MigoHostCallbacks, on_game_log) == 120,
               "the device callbacks are the append-only tail");
#elif UINTPTR_MAX == UINT32_MAX
_Static_assert(sizeof(MigoError) == 28, "ILP32 error layout");
_Static_assert(sizeof(MigoSurfaceDescriptor) ==
                   (_Alignof(uint64_t) == 8 ? 72 : 68),
               "ILP32 Surface layout follows the target uint64_t alignment");
/* 8 bytes of header plus twelve pointers. This was wrong from the commit that
 * appended on_request_frame until 2026-07-21: the LP64 line was updated and
 * this one was not, and the soft-keyboard callbacks were then added on top of
 * the wrong base. Nothing caught it because every lane ran on an LP64 host, so
 * this branch had never once been compiled. */
_Static_assert(sizeof(MigoHostCallbacks) == 68, "ILP32 callback layout");
_Static_assert(offsetof(MigoHostCallbacks, on_surface_released) == 52,
               "ILP32 release wakeup offset");
_Static_assert(offsetof(MigoHostCallbacks, on_game_log) == 64,
               "ILP32 device callbacks tail offset");
#else
#error "unsupported pointer width"
#endif

void migo_core_c_teardown_contract(MigoSession *session, MigoEngine *engine) {
    (void)migo_session_destroy(session);
    (void)migo_engine_destroy(engine);
    /* Native display/window teardown and library unload are legal only here. */
}

int migo_core_c_contract(void) {
    MigoEngineConfig engine_config = {0};
    MigoSessionConfig session_config = {0};
    MigoSurfaceDescriptor surface = {0};
    MigoHostCallbacks callbacks = {0};
    MigoOnSurfaceReleasedFn release_wakeup = callbacks.on_surface_released;

    engine_config.struct_size = (uint32_t)sizeof(engine_config);
    engine_config.abi_version = MIGO_ABI_VERSION_1;
    session_config.struct_size = (uint32_t)sizeof(session_config);
    session_config.abi_version = MIGO_ABI_VERSION_1;
    surface.struct_size = (uint32_t)sizeof(surface);
    surface.abi_version = MIGO_ABI_VERSION_1;
    callbacks.struct_size = (uint32_t)sizeof(callbacks);
    callbacks.abi_version = MIGO_ABI_VERSION_1;

    MigoResult(MIGO_CALL *attach_fn)(MigoSession *, const MigoSurfaceDescriptor *,
                                     MigoSurfaceAttachment **) =
        &migo_session_attach_surface;
    MigoResult(MIGO_CALL *update_fn)(MigoSurfaceAttachment *,
                                     const MigoSurfaceMetrics *) =
        &migo_surface_update;
    MigoResult(MIGO_CALL *begin_detach_fn)(MigoSurfaceAttachment *,
                                           MigoSurfaceRelease **) =
        &migo_surface_begin_detach;
    MigoResult(MIGO_CALL *release_query_fn)(const MigoSurfaceRelease *,
                                            MigoSurfaceReleaseStatus *) =
        &migo_surface_release_query;
    MigoResult(MIGO_CALL *release_destroy_fn)(MigoSurfaceRelease *) =
        &migo_surface_release_destroy;
    MigoResult(MIGO_CALL *engine_create_fn)(const MigoEngineConfig *, MigoEngine **) =
        &migo_engine_create;
    MigoResult(MIGO_CALL *engine_destroy_fn)(MigoEngine *) = &migo_engine_destroy;
    MigoResult(MIGO_CALL *session_create_fn)(MigoEngine *, const MigoSessionConfig *,
                                             MigoSession **) = &migo_session_create;
    MigoResult(MIGO_CALL *set_callbacks_fn)(MigoSession *, const MigoHostCallbacks *) =
        &migo_session_set_host_callbacks;
    MigoResult(MIGO_CALL *set_lifecycle_fn)(MigoSession *, MigoLifecycleState) =
        &migo_session_set_lifecycle;
    MigoResult(MIGO_CALL *set_visibility_fn)(MigoSession *, uint8_t) =
        &migo_session_set_visibility;
    MigoResult(MIGO_CALL *set_focus_fn)(MigoSession *, uint8_t) =
        &migo_session_set_focus;
    MigoResult(MIGO_CALL *destroy_fn)(MigoSession *) = &migo_session_destroy;
    MigoResult(MIGO_CALL *network_fn)(MigoSession *, MigoNetworkType, uint8_t) =
        &migo_session_set_network_status;
    MigoResult(MIGO_CALL *battery_fn)(MigoSession *, uint32_t, MigoBatteryFlags) =
        &migo_session_set_battery_status;

    return (int)(engine_config.struct_size + session_config.struct_size +
                 surface.struct_size + callbacks.struct_size +
                 (release_wakeup != NULL) +
                 (attach_fn != NULL) + (update_fn != NULL) +
                 (begin_detach_fn != NULL) + (release_query_fn != NULL) +
                 (release_destroy_fn != NULL) +
                 (engine_create_fn != NULL) + (engine_destroy_fn != NULL) +
                 (session_create_fn != NULL) + (set_callbacks_fn != NULL) +
                 (set_lifecycle_fn != NULL) + (set_visibility_fn != NULL) +
                 (set_focus_fn != NULL) + (destroy_fn != NULL) +
                 (network_fn != NULL) + (battery_fn != NULL));
}
