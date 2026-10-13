#ifndef MIGO_SURFACE_H_
#define MIGO_SURFACE_H_

#include <stddef.h> /* offsetof */
#include <migo/types.h>

typedef uint32_t MigoPlatformKind;
#define MIGO_PLATFORM_UNKNOWN 0U
#define MIGO_PLATFORM_ANDROID_NATIVE_WINDOW 1U
#define MIGO_PLATFORM_WIN32_HWND 2U
#define MIGO_PLATFORM_WINUI_SWAP_CHAIN_PANEL 3U
#define MIGO_PLATFORM_MACOS_NS_VIEW 4U
#define MIGO_PLATFORM_MACOS_CA_METAL_LAYER 5U
#define MIGO_PLATFORM_X11_WINDOW 6U
#define MIGO_PLATFORM_WAYLAND_SURFACE 7U
#define MIGO_PLATFORM_OPENHARMONY_NATIVE_WINDOW 8U
#define MIGO_PLATFORM_IOS_UI_VIEW 9U
#define MIGO_PLATFORM_IOS_CA_METAL_LAYER 10U

typedef uint32_t MigoSurfaceDescriptorFlags;
#define MIGO_SURFACE_DESCRIPTOR_FLAG_NONE 0U

typedef uint32_t MigoPlatformDescriptorFlags;
#define MIGO_PLATFORM_DESCRIPTOR_FLAG_NONE 0U

typedef uint32_t MigoColorSpace;
#define MIGO_COLOR_SPACE_UNSPECIFIED 0U
#define MIGO_COLOR_SPACE_SRGB 1U
#define MIGO_COLOR_SPACE_DISPLAY_P3 2U
#define MIGO_COLOR_SPACE_EXTENDED_SRGB 3U

typedef uint32_t MigoAlphaMode;
#define MIGO_ALPHA_MODE_UNSPECIFIED 0U
#define MIGO_ALPHA_MODE_OPAQUE 1U
#define MIGO_ALPHA_MODE_PREMULTIPLIED 2U
#define MIGO_ALPHA_MODE_POSTMULTIPLIED 3U

typedef uint32_t MigoPresentationMode;
#define MIGO_PRESENTATION_MODE_DEFAULT 0U
#define MIGO_PRESENTATION_MODE_FIFO 1U
#define MIGO_PRESENTATION_MODE_MAILBOX 2U
#define MIGO_PRESENTATION_MODE_IMMEDIATE 3U

typedef uint64_t MigoSurfaceCapabilities;
#define MIGO_SURFACE_CAPABILITY_NONE 0ULL
#define MIGO_SURFACE_CAPABILITY_WIDE_COLOR (1ULL << 0)
#define MIGO_SURFACE_CAPABILITY_TRANSPARENT (1ULL << 1)
#define MIGO_SURFACE_CAPABILITY_MAILBOX_PRESENT (1ULL << 2)

typedef uint32_t MigoSurfaceLossReason;
#define MIGO_SURFACE_LOSS_UNKNOWN 0U
#define MIGO_SURFACE_LOSS_HOST_DESTROYED 1U
#define MIGO_SURFACE_LOSS_DEVICE_LOST 2U
#define MIGO_SURFACE_LOSS_PLATFORM_ERROR 3U

/* Common prefix repeated by every strongly typed platform descriptor. */
typedef struct MigoPlatformSurfaceDescriptor {
    uint32_t struct_size;
    uint32_t abi_version;
    MigoPlatformKind platform_kind;
    MigoPlatformDescriptorFlags flags;
} MigoPlatformSurfaceDescriptor;

typedef struct MigoSurfaceMetrics {
    uint32_t struct_size;
    uint32_t abi_version;
    uint64_t generation;
    uint32_t width_pixels;
    uint32_t height_pixels;
    float scale_factor;
    MigoColorSpace color_space;
    MigoAlphaMode alpha_mode;
    MigoPresentationMode preferred_presentation_mode;
    MigoSurfaceDescriptorFlags flags;
    uint32_t reserved0;
} MigoSurfaceMetrics;

MIGO_STATIC_ASSERT(sizeof(MigoSurfaceMetrics) == 48, "MigoSurfaceMetrics size changed");
MIGO_STATIC_ASSERT(offsetof(MigoSurfaceMetrics, struct_size) == 0,
                   "every versioned struct must begin with struct_size");
MIGO_STATIC_ASSERT(offsetof(MigoSurfaceMetrics, scale_factor) == 24, "MigoSurfaceMetrics.scale_factor moved");

/*
 * platform_descriptor is borrowed for this call only. The implementation must
 * copy the descriptor and acquire any ref-counted native target before attach
 * returns success. platform_descriptor_size must equal the typed descriptor's
 * struct_size for ABI v1; the duplicate value is an intentional envelope versus
 * payload bounds/integrity cross-check. Every reserved field supplied by the
 * caller must be zero.
 *
 * generation numbers the host's attachments and starts at 1. Each attach must
 * carry one strictly greater than any this Session has already accepted -- that
 * is what lets the engine discard work naming a window the host has since
 * replaced -- and a repeated value is refused with MIGO_ERROR_STALE_SURFACE. A
 * refused attach consumes nothing, so a retry may offer the same value again.
 * Any platform that destroys and recreates the window during normal operation,
 * which Android does on every trip through the background, therefore needs a
 * counter rather than a constant. MigoSurfaceMetrics.generation is the mirror
 * rule: an update names the attachment it updates, so it carries that
 * attachment's own generation rather than the next one.
 */
typedef struct MigoSurfaceDescriptor {
    uint32_t struct_size;
    uint32_t abi_version;
    uint64_t generation;
    MigoPlatformKind platform_kind;
    MigoSurfaceDescriptorFlags flags;
    uint32_t width_pixels;
    uint32_t height_pixels;
    float scale_factor;
    MigoColorSpace color_space;
    MigoAlphaMode alpha_mode;
    MigoPresentationMode preferred_presentation_mode;
    MigoSurfaceCapabilities capability_flags;
    uint32_t platform_descriptor_size;
    uint32_t reserved0;
    const void *platform_descriptor;
} MigoSurfaceDescriptor;

MIGO_STATIC_ASSERT(offsetof(MigoSurfaceDescriptor, struct_size) == 0,
                   "every versioned struct must begin with struct_size");
#if MIGO_LP64
MIGO_STATIC_ASSERT(sizeof(MigoSurfaceDescriptor) == 72, "MigoSurfaceDescriptor LP64 size changed");
MIGO_STATIC_ASSERT(offsetof(MigoSurfaceDescriptor, capability_flags) == 48,
                   "MigoSurfaceDescriptor.capability_flags moved");
MIGO_STATIC_ASSERT(offsetof(MigoSurfaceDescriptor, platform_descriptor) == 64,
                   "MigoSurfaceDescriptor.platform_descriptor moved");
#endif

/*
 * Opaque observer of one asynchronous native-Surface release.
 *
 * It owns no Surface resource lease. Once it reports RELEASED it may outlive a
 * later successful destruction of the Session that produced it. A pending
 * observer cannot: migo_session_destroy refuses while any release is pending.
 */
typedef struct MigoSurfaceRelease MigoSurfaceRelease;

/* Level-triggered, not edge-triggered: a late first query still observes a
 * release that already completed. An edge would be missable exactly once, on
 * the path where missing it means destroying a window the GPU still reads. */
typedef uint32_t MigoSurfaceReleaseState;
#define MIGO_SURFACE_RELEASE_PENDING 0U
#define MIGO_SURFACE_RELEASE_RELEASED 1U

typedef struct MigoSurfaceReleaseStatus {
    uint32_t struct_size;
    uint32_t abi_version;
    uint64_t generation;
    MigoSurfaceReleaseState state;
    uint32_t reserved0;
} MigoSurfaceReleaseStatus;

MIGO_STATIC_ASSERT(sizeof(MigoSurfaceReleaseStatus) == 24,
                   "MigoSurfaceReleaseStatus size changed");
MIGO_STATIC_ASSERT(offsetof(MigoSurfaceReleaseStatus, struct_size) == 0,
                   "every versioned struct must begin with struct_size");
MIGO_STATIC_ASSERT(offsetof(MigoSurfaceReleaseStatus, generation) == 8,
                   "MigoSurfaceReleaseStatus.generation moved");
MIGO_STATIC_ASSERT(offsetof(MigoSurfaceReleaseStatus, state) == 16,
                   "MigoSurfaceReleaseStatus.state moved");

MIGO_BEGIN_DECLS

/*
 * Attach a native Surface to a Session and publish it for presentation.
 *
 * The first successful attach fixes the Session's graphics platform identity.
 * Later native-target replacement is supported only within that identity:
 * Android in the same process, X11 on the same server using the Session's
 * private render connection, Wayland on the same wl_display, or HWND under the
 * same ANGLE device. A different
 * backend/display returns MIGO_ERROR_INVALID_STATE synchronously, publishes no
 * attachment, and enqueues no render command; the Session remains retryable.
 *
 * out_attachment is cleared to NULL before anything else is validated, so on
 * every failure it is NULL and a caller can branch on the handle alone.
 *
 * Two rejections are deliberately distinct, because they call for different
 * responses. A value this ABI does not define at all is a caller bug and gives
 * MIGO_ERROR_INVALID_ARGUMENT. A value this ABI does define but this build
 * does not implement gives MIGO_ERROR_UNSUPPORTED_CAPABILITY, and the caller's
 * recovery is to ask for less -- capability bits are requirements, never hints,
 * so the engine refuses rather than quietly downgrading.
 *
 *   MIGO_ERROR_INVALID_ARGUMENT  session, descriptor, or out_attachment was
 *                                NULL; descriptor's struct_size is smaller
 *                                than the minimum versioned record; a field is
 *                                out of range (generation zero, a width or
 *                                height that is zero or above INT32_MAX, a
 *                                scale factor that is not finite and positive,
 *                                a non-zero flags or reserved field); a color
 *                                space, alpha mode, or presentation mode this
 *                                ABI does not define; platform_kind is not a
 *                                defined MIGO_PLATFORM_* value; or the
 *                                platform payload disagrees with platform_kind
 *                                or carries a NULL native window
 *   MIGO_ERROR_UNSUPPORTED_ABI   descriptor.abi_version does not match this
 *                                engine build, or struct_size claims a record
 *                                larger than this build knows
 *   MIGO_ERROR_UNSUPPORTED_CAPABILITY
 *                                a defined request this build does not
 *                                implement: MIGO_COLOR_SPACE_DISPLAY_P3 or
 *                                MIGO_COLOR_SPACE_EXTENDED_SRGB, premultiplied
 *                                or postmultiplied alpha, mailbox or immediate
 *                                presentation, or any non-zero capability_flags
 *   MIGO_ERROR_UNSUPPORTED_PLATFORM
 *                                platform_kind is defined but not attachable
 *                                by this build -- the same fact
 *                                migo_query_capabilities reports through
 *                                platform_kinds
 *   MIGO_ERROR_STALE_SURFACE     descriptor.generation is not newer than the
 *                                newest attach this Session has already
 *                                accepted
 *   MIGO_ERROR_INVALID_STATE     the Session is already closed, another
 *                                Surface transition is running, an attachment
 *                                is already active, or the descriptor names a
 *                                different backend/display than the first
 *                                successful attach fixed
 *   MIGO_ERROR_INTERNAL          the Session state lock was poisoned, or the
 *                                host-side lease or dispatch failed; rare, and
 *                                logged when it happens
 */
MIGO_API MigoResult MIGO_CALL migo_session_attach_surface(
    MigoSession *session,
    const MigoSurfaceDescriptor *descriptor,
    MigoSurfaceAttachment **out_attachment);

/*
 * Report a resize or a presentation-parameter change on an already-attached
 * Surface. Synchronous: by the time this returns, either the new metrics are
 * committed and a resize command is enqueued to the host, or nothing changed.
 *
 * metrics.generation names the attachment the update is for: it is the
 * generation that attachment was attached with (MigoSurfaceDescriptor's), the
 * same for every update until the next attach -- not a count of updates. A
 * host that coalesces resize events on its own thread can still hold one for
 * a window it has since replaced; that update names the old attachment and is
 * refused as stale rather than resizing the new one.
 *
 *   MIGO_ERROR_INVALID_ARGUMENT  attachment or metrics was NULL; metrics'
 *                                struct_size is smaller than the minimum
 *                                versioned record; a field is out of range
 *                                (zero width/height, non-finite scale
 *                                factor); or metrics.generation is zero or
 *                                newer than the attachment's
 *   MIGO_ERROR_UNSUPPORTED_ABI   metrics.abi_version does not match this
 *                                engine build, or struct_size claims a
 *                                record larger than this build knows
 *   MIGO_ERROR_INVALID_STATE     another Surface transition (attach, update,
 *                                or detach) is already running on this
 *                                Session, or the Session has no live host
 *   MIGO_ERROR_STALE_SURFACE     attachment is not the Session's active
 *                                attachment, has already been lost, or
 *                                metrics.generation is older than the
 *                                attachment's
 *   MIGO_ERROR_INTERNAL          the host-side lease or dispatch failed;
 *                                rare, and logged when it happens
 *
 * Callable from the thread that owns the Session; concurrent calls through
 * the same Session are the caller's to serialize.
 */
MIGO_API MigoResult MIGO_CALL migo_surface_update(
    MigoSurfaceAttachment *attachment,
    const MigoSurfaceMetrics *metrics);

/*
 * Begin retiring one attachment. This is the irreversible presentation
 * boundary, and it is asynchronous because the GPU cannot be made to forget a
 * Surface synchronously: driver-side references outlive the call.
 *
 * attachment is a unique handle and must not be copied into independently owned
 * aliases. MIGO_OK consumes it -- the pointer is invalid on return, no future
 * GPU operation or presentation references its generation, and *out_release
 * owns a new observer the caller must eventually destroy. out_release is set to
 * NULL before any fallible work, so it is always well-defined to read.
 *
 * A non-OK result changes nothing and consumes nothing:
 *   MIGO_ERROR_INVALID_ARGUMENT  attachment or out_release was NULL
 *   MIGO_ERROR_INVALID_STATE     another Surface transition is already running,
 *                                or the Session has no live host
 *   MIGO_ERROR_STALE_SURFACE     attachment is not the Session's active one
 *
 * This call must not wait for another turn of the host dispatcher. It is
 * callable from the thread that owns the Session; concurrent calls through the
 * same Session are the caller's to serialize.
 *
 * THE HOST MUST NOT DESTROY THE NATIVE WINDOW HERE. Returning MIGO_OK means
 * retirement started, not that the driver is finished. Keep the native
 * resource and its event loop alive until migo_surface_release_query reports
 * MIGO_SURFACE_RELEASE_RELEASED; destroying it earlier is a use-after-free
 * inside the driver, which the engine cannot detect or prevent.
 */
MIGO_API MigoResult MIGO_CALL migo_surface_begin_detach(
    MigoSurfaceAttachment *attachment,
    MigoSurfaceRelease **out_release);

/*
 * Read the authoritative release state. Never blocks, so it is safe to poll
 * from a UI thread or an event-loop idle handler.
 *
 * A release that has reached RELEASED stays valid and queryable after the
 * owning Session is destroyed. Session destruction refuses while it is still
 * PENDING.
 *
 * out_status IS CALLER-OWNED AND ITS HEADER IS AN INPUT. Set struct_size and
 * abi_version before the call: struct_size is what bounds the write into your
 * storage, so a record that arrives holding zeros -- which is what an
 * uninitialised struct holds in C and what a default-initialised one holds in
 * Swift -- is refused rather than filled in. Failing that way rather than
 * writing sizeof(this build's record) is the only behaviour that stays safe
 * when a host built against an older header calls a newer library.
 *
 * Returns MIGO_ERROR_INVALID_ARGUMENT if either pointer is NULL or
 * out_status->struct_size is below the minimum record this ABI defines, and
 * MIGO_ERROR_UNSUPPORTED_ABI if its abi_version is not the current one or its
 * struct_size is larger than the record this build knows how to fill.
 * out_status is written only on MIGO_OK, never partially, and the header you
 * supplied is preserved rather than overwritten.
 */
MIGO_API MigoResult MIGO_CALL migo_surface_release_query(
    const MigoSurfaceRelease *release,
    MigoSurfaceReleaseStatus *out_status);

/*
 * Destroy a completed observer. MIGO_OK consumes the handle and its pointer
 * becomes invalid.
 *
 * Returns MIGO_ERROR_INVALID_STATE while the release is still
 * MIGO_SURFACE_RELEASE_PENDING, leaving ownership with the caller. Refusing is
 * the point: destroying the observer early would discard the only means of
 * learning when the native resource became safe to free, and the host would
 * have nothing left to wait on.
 */
MIGO_API MigoResult MIGO_CALL
migo_surface_release_destroy(MigoSurfaceRelease *release);

MIGO_END_DECLS

#endif /* MIGO_SURFACE_H_ */
