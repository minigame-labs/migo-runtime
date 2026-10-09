/*
 * An old client, compiled against this library's current headers.
 *
 * The structs below are declared here rather than included, because that is the
 * whole point: an already-compiled host carries the shape its own headers had,
 * and this file is the only place that shape still exists. Including
 * `session.h` and checking it against itself would pass no matter what the
 * library did.
 *
 * What C can prove is layout: that the old shape is still a byte-exact prefix
 * of the current one, which is what makes a short `struct_size` meaningful at
 * all. If a field is ever inserted rather than appended, every assertion below
 * fires at build time -- at the moment the mistake is made, rather than when a
 * shipped host meets the new library.
 *
 * What C cannot prove here is behaviour: this lane compiles, it does not link
 * or run. The runtime half -- that a 72-byte caller is accepted, that its
 * absent fields read as absent rather than as its neighbouring bytes, and that
 * a larger struct is refused -- lives in `capi`'s own tests, which build a
 * truncated buffer with poisoned trailing bytes.
 */

#include <migo/migo.h>

#include <stddef.h>
#include <stdint.h>

/*
 * MigoHostCallbacks as it stood before the soft keyboard was appended: through
 * on_request_frame, 72 bytes on LP64. A host built then still writes exactly
 * this.
 */
typedef struct OldHostCallbacks {
    uint32_t struct_size;
    uint32_t abi_version;
    void *user_data;
    void *dispatcher_data;
    MigoDispatchFn dispatch;
    MigoOnReadyFn on_ready;
    MigoOnErrorFn on_error;
    MigoOnExitRequestedFn on_exit_requested;
    MigoOnSurfaceLostFn on_surface_lost;
    MigoOnRequestFrameFn on_request_frame;
} OldHostCallbacks;

/*
 * Every field the old client knows must sit at the same offset it always did.
 * An insertion anywhere above on_request_frame would shift the rest, and the
 * library would read that client's on_error as its on_exit_requested -- a
 * mismatch no size check can detect, because the size would still be 72.
 */
#define MIGO_SAME_OFFSET(field)                                                                  \
    MIGO_STATIC_ASSERT(offsetof(OldHostCallbacks, field) == offsetof(MigoHostCallbacks, field),  \
                       "MigoHostCallbacks." #field " moved; appended fields must only append")

MIGO_SAME_OFFSET(struct_size);
MIGO_SAME_OFFSET(abi_version);
MIGO_SAME_OFFSET(user_data);
MIGO_SAME_OFFSET(dispatcher_data);
MIGO_SAME_OFFSET(dispatch);
MIGO_SAME_OFFSET(on_ready);
MIGO_SAME_OFFSET(on_error);
MIGO_SAME_OFFSET(on_exit_requested);
MIGO_SAME_OFFSET(on_surface_lost);
MIGO_SAME_OFFSET(on_request_frame);

/*
 * The old struct must still be no larger than the current one. A library that
 * shrank would be reading past what it believes it has.
 */
MIGO_STATIC_ASSERT(sizeof(OldHostCallbacks) <= sizeof(MigoHostCallbacks),
                   "the current MigoHostCallbacks must still contain the old one");

#if MIGO_LP64
MIGO_STATIC_ASSERT(sizeof(OldHostCallbacks) == 72, "the pre-keyboard LP64 shape was 72 bytes");
/*
 * The documented floor: header, the two opaque tokens and the dispatcher. Every
 * field past it is an optional callback, which is what lets a shorter client be
 * accepted rather than merely tolerated.
 */
MIGO_STATIC_ASSERT(offsetof(MigoHostCallbacks, dispatch) + sizeof(MigoDispatchFn) == 32,
                   "the minimum accepted MigoHostCallbacks is 32 bytes on LP64");
#endif

/*
 * MigoHostCallbacks as it stood before the host-service channel was appended:
 * through on_game_log, 128 bytes on LP64 and 68 on ILP32. A host built then
 * announces that size and must be read as supplying no host services -- which
 * the library can only do if the channel begins exactly where that struct
 * ended.
 */
typedef struct DeviceEraHostCallbacks {
    uint32_t struct_size;
    uint32_t abi_version;
    void *user_data;
    void *dispatcher_data;
    MigoDispatchFn dispatch;
    MigoOnReadyFn on_ready;
    MigoOnErrorFn on_error;
    MigoOnExitRequestedFn on_exit_requested;
    MigoOnSurfaceLostFn on_surface_lost;
    MigoOnRequestFrameFn on_request_frame;
    MigoOnShowKeyboardFn on_show_keyboard;
    MigoOnHideKeyboardFn on_hide_keyboard;
    MigoOnUpdateKeyboardFn on_update_keyboard;
    MigoOnSurfaceReleasedFn on_surface_released;
    MigoOnVibrateFn on_vibrate;
    MigoOnKeepScreenOnFn on_keep_screen_on;
    MigoOnGameLogFn on_game_log;
} DeviceEraHostCallbacks;

#define MIGO_DEVICE_ERA_SAME_OFFSET(field)                                                       \
    MIGO_STATIC_ASSERT(offsetof(DeviceEraHostCallbacks, field) ==                                 \
                           offsetof(MigoHostCallbacks, field),                                    \
                       "MigoHostCallbacks." #field " moved; appended fields must only append")

MIGO_DEVICE_ERA_SAME_OFFSET(on_show_keyboard);
MIGO_DEVICE_ERA_SAME_OFFSET(on_hide_keyboard);
MIGO_DEVICE_ERA_SAME_OFFSET(on_update_keyboard);
MIGO_DEVICE_ERA_SAME_OFFSET(on_surface_released);
MIGO_DEVICE_ERA_SAME_OFFSET(on_vibrate);
MIGO_DEVICE_ERA_SAME_OFFSET(on_keep_screen_on);
MIGO_DEVICE_ERA_SAME_OFFSET(on_game_log);

MIGO_STATIC_ASSERT(sizeof(DeviceEraHostCallbacks) ==
                       offsetof(MigoHostCallbacks, on_host_service_call),
                   "the host-service channel must begin exactly where the old struct ended");

#if MIGO_LP64
MIGO_STATIC_ASSERT(sizeof(DeviceEraHostCallbacks) == 128,
                   "the pre-channel LP64 shape was 128 bytes");
#else
MIGO_STATIC_ASSERT(sizeof(DeviceEraHostCallbacks) == 68,
                   "the pre-channel ILP32 shape was 68 bytes");
#endif

/*
 * MigoKeyEvent as it stood before modifiers and repeat were appended: 48 bytes
 * on LP64, 40 on ILP32. A host built then still writes exactly this and
 * announces that size, and the library must read its absent tail as zero --
 * no modifier held, not an auto-repeat -- rather than rejecting it.
 */
typedef struct OldKeyEvent {
    uint32_t struct_size;
    uint32_t abi_version;
    MigoKeyEventType event_type;
    uint32_t key_length;
    const char *key_utf8;
    const char *code_utf8;
    uint32_t code_length;
    uint32_t reserved0;
    double timestamp_ms;
} OldKeyEvent;

#define MIGO_KEY_SAME_OFFSET(field)                                                              \
    MIGO_STATIC_ASSERT(offsetof(OldKeyEvent, field) == offsetof(MigoKeyEvent, field),            \
                       "MigoKeyEvent." #field " moved; appended fields must only append")

MIGO_KEY_SAME_OFFSET(struct_size);
MIGO_KEY_SAME_OFFSET(abi_version);
MIGO_KEY_SAME_OFFSET(event_type);
MIGO_KEY_SAME_OFFSET(key_length);
MIGO_KEY_SAME_OFFSET(key_utf8);
MIGO_KEY_SAME_OFFSET(code_utf8);
MIGO_KEY_SAME_OFFSET(code_length);
MIGO_KEY_SAME_OFFSET(reserved0);
MIGO_KEY_SAME_OFFSET(timestamp_ms);

MIGO_STATIC_ASSERT(sizeof(OldKeyEvent) <= sizeof(MigoKeyEvent),
                   "the current MigoKeyEvent must still contain the old one");
MIGO_STATIC_ASSERT(sizeof(OldKeyEvent) == offsetof(MigoKeyEvent, modifiers),
                   "the appended tail must begin exactly where the old struct ended");

#if MIGO_LP64
MIGO_STATIC_ASSERT(sizeof(OldKeyEvent) == 48, "the pre-modifier LP64 shape was 48 bytes");
#else
MIGO_STATIC_ASSERT(sizeof(OldKeyEvent) == 40, "the pre-modifier ILP32 shape was 40 bytes");
#endif

/*
 * A compiled-and-linked old client would do exactly this: fill the shape it
 * knows, announce its own size, and hand over a pointer. Kept as a function so
 * the compiler checks the call is well typed against the current header.
 */
MigoResult migo_old_client_installs(MigoSession *session, MigoDispatchFn dispatch);

MigoResult migo_old_client_installs(MigoSession *session, MigoDispatchFn dispatch) {
    OldHostCallbacks old;
    for (size_t i = 0; i < sizeof old; ++i) ((unsigned char *)&old)[i] = 0;
    old.struct_size = (uint32_t)sizeof old;
    old.abi_version = MIGO_ABI_VERSION_CURRENT;
    old.dispatch = dispatch;

    /* The cast is what an old client's machine code does implicitly: it passes
     * the address of storage that is smaller than the library's struct. */
    return migo_session_set_host_callbacks(session, (const MigoHostCallbacks *)&old);
}
