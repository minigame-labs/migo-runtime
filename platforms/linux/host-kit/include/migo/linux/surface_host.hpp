#ifndef MIGO_LINUX_SURFACE_HOST_HPP_
#define MIGO_LINUX_SURFACE_HOST_HPP_

#include <migo/migo.h>
#include <migo/platform/wayland.h>
#include <migo/platform/x11.h>

#include <cstdint>
#include <thread>

namespace migo::linux_host {

enum class SurfaceState : std::uint8_t {
    Detached,
    Attached,
    Retiring,
    Faulted,
};

struct SurfaceMetrics {
    std::uint32_t width_pixels = 0;
    std::uint32_t height_pixels = 0;
    float scale_factor = 1.0F;
    MigoColorSpace color_space = MIGO_COLOR_SPACE_SRGB;
    MigoAlphaMode alpha_mode = MIGO_ALPHA_MODE_OPAQUE;
    MigoPresentationMode presentation_mode = MIGO_PRESENTATION_MODE_DEFAULT;
    // Attach-time requirements. The C ABI does not allow capability changes
    // through MigoSurfaceMetrics, so update() deliberately leaves this field
    // unchanged in the runtime.
    MigoSurfaceCapabilities required_capabilities = MIGO_SURFACE_CAPABILITY_NONE;
};

struct X11Target {
    void *display = nullptr;
    std::uintptr_t window = 0;
    std::int32_t screen = 0;
};

struct WaylandTarget {
    void *display = nullptr;
    void *surface = nullptr;
};

/// Control-path owner of one Migo Surface attachment.
///
/// This type never owns the native X11/Wayland objects or the Session. The
/// caller keeps both alive through asynchronous retirement. One controller
/// stays at a stable address for the Session's full Surface history, so it is
/// neither copyable nor movable and replacement views cannot reset generation
/// identity. Destroying a live controller is a programming error and
/// terminates, matching `std::thread`'s
/// fail-fast treatment of a still-joinable thread: silently leaking the only
/// attachment/release handle would make safe native-window teardown impossible.
/// The same applies to `Faulted`: a successful ABI transition that returned no
/// required handle, or a failed transition that nevertheless returned an
/// ownership handle, has made safe target teardown unknowable.
/// `first_generation` must be non-zero and greater than the Session's last
/// accepted generation when adopting a Session previously driven without this
/// controller; new Sessions use the default value 1. Construct and call the
/// controller on the Session's owner thread; foreign-thread calls fail before
/// entering the C ABI.
class SurfaceHost final {
public:
    explicit SurfaceHost(MigoSession *session, std::uint64_t first_generation = 1) noexcept;
    ~SurfaceHost() noexcept;

    SurfaceHost(const SurfaceHost &) = delete;
    SurfaceHost &operator=(const SurfaceHost &) = delete;
    SurfaceHost(SurfaceHost &&) = delete;
    SurfaceHost &operator=(SurfaceHost &&) = delete;

    [[nodiscard]] MigoResult attach(const X11Target &target,
                                    const SurfaceMetrics &metrics) noexcept;
    [[nodiscard]] MigoResult attach(const WaylandTarget &target,
                                    const SurfaceMetrics &metrics) noexcept;
    [[nodiscard]] MigoResult update(const SurfaceMetrics &metrics) noexcept;
    [[nodiscard]] MigoResult begin_detach() noexcept;
    [[nodiscard]] MigoResult poll_release(bool *released) noexcept;

    [[nodiscard]] SurfaceState state() const noexcept { return state_; }
    [[nodiscard]] std::uint64_t generation() const noexcept { return generation_; }

    /// The Session this controller drives, for the input path.
    ///
    /// Input is addressed to the Session, not to the attachment, but it is only
    /// meaningful while a Surface is attached -- the ABI answers
    /// `MIGO_ERROR_INVALID_STATE` otherwise. Reading the Session from here
    /// rather than having the App hand the same pointer to a second object
    /// removes the possibility of the two disagreeing, which would deliver
    /// input to a different Session than the one being drawn to.
    [[nodiscard]] MigoSession *session() const noexcept { return session_; }

private:
    [[nodiscard]] MigoResult attach_descriptor(
        MigoPlatformKind platform_kind,
        const void *platform_descriptor,
        std::uint32_t platform_descriptor_size,
        const SurfaceMetrics &metrics) noexcept;
    [[nodiscard]] static bool metrics_are_valid(const SurfaceMetrics &metrics) noexcept;
    [[nodiscard]] bool is_owner_thread() const noexcept;
    void consume_generation(std::uint64_t generation) noexcept;

    MigoSession *session_ = nullptr;
    const std::thread::id owner_thread_;
    MigoSurfaceAttachment *attachment_ = nullptr;
    MigoSurfaceRelease *release_ = nullptr;
    SurfaceState state_ = SurfaceState::Detached;
    std::uint64_t generation_ = 0;
    std::uint64_t next_generation_ = 1;
    MigoSurfaceCapabilities attached_capabilities_ = MIGO_SURFACE_CAPABILITY_NONE;
};

}  // namespace migo::linux_host

#endif  // MIGO_LINUX_SURFACE_HOST_HPP_
