#include <migo/linux/surface_host.hpp>

#include <cmath>
#include <exception>
#include <limits>

namespace migo::linux_host {

static_assert(MIGO_ABI_VERSION_CURRENT == MIGO_ABI_VERSION_1,
              "SurfaceHost must be reviewed before targeting a new Migo C ABI");

SurfaceHost::SurfaceHost(MigoSession *session, std::uint64_t first_generation) noexcept
    : session_(session),
      owner_thread_(std::this_thread::get_id()),
      next_generation_(first_generation) {}

SurfaceHost::~SurfaceHost() noexcept {
    if (state_ != SurfaceState::Detached || attachment_ != nullptr || release_ != nullptr) {
        std::terminate();
    }
}

bool SurfaceHost::metrics_are_valid(const SurfaceMetrics &metrics) noexcept {
    constexpr auto kMaxDimension =
        static_cast<std::uint32_t>(std::numeric_limits<std::int32_t>::max());
    return metrics.width_pixels != 0 && metrics.height_pixels != 0 &&
           metrics.width_pixels <= kMaxDimension && metrics.height_pixels <= kMaxDimension &&
           std::isfinite(metrics.scale_factor) && metrics.scale_factor > 0.0F;
}

bool SurfaceHost::is_owner_thread() const noexcept {
    return std::this_thread::get_id() == owner_thread_;
}

void SurfaceHost::consume_generation(std::uint64_t generation) noexcept {
    generation_ = generation;
    next_generation_ = generation == std::numeric_limits<std::uint64_t>::max()
                           ? 0
                           : generation + 1;
}

MigoResult SurfaceHost::attach(const X11Target &target,
                               const SurfaceMetrics &metrics) noexcept {
    if (!is_owner_thread()) return MIGO_ERROR_WRONG_THREAD;
    if (target.display == nullptr || target.window == 0 || target.screen < 0) {
        return MIGO_ERROR_INVALID_ARGUMENT;
    }

    MigoX11WindowDescriptor descriptor{};
    descriptor.struct_size = static_cast<std::uint32_t>(sizeof(descriptor));
    descriptor.abi_version = MIGO_ABI_VERSION_CURRENT;
    descriptor.platform_kind = MIGO_PLATFORM_X11_WINDOW;
    descriptor.flags = MIGO_PLATFORM_DESCRIPTOR_FLAG_NONE;
    descriptor.display = target.display;
    descriptor.window = target.window;
    descriptor.screen = target.screen;
    return attach_descriptor(MIGO_PLATFORM_X11_WINDOW, &descriptor,
                             static_cast<std::uint32_t>(sizeof(descriptor)), metrics);
}

MigoResult SurfaceHost::attach(const WaylandTarget &target,
                               const SurfaceMetrics &metrics) noexcept {
    if (!is_owner_thread()) return MIGO_ERROR_WRONG_THREAD;
    if (target.display == nullptr || target.surface == nullptr) {
        return MIGO_ERROR_INVALID_ARGUMENT;
    }

    MigoWaylandSurfaceDescriptor descriptor{};
    descriptor.struct_size = static_cast<std::uint32_t>(sizeof(descriptor));
    descriptor.abi_version = MIGO_ABI_VERSION_CURRENT;
    descriptor.platform_kind = MIGO_PLATFORM_WAYLAND_SURFACE;
    descriptor.flags = MIGO_PLATFORM_DESCRIPTOR_FLAG_NONE;
    descriptor.display = target.display;
    descriptor.surface = target.surface;
    return attach_descriptor(MIGO_PLATFORM_WAYLAND_SURFACE, &descriptor,
                             static_cast<std::uint32_t>(sizeof(descriptor)), metrics);
}

MigoResult SurfaceHost::attach_descriptor(MigoPlatformKind platform_kind,
                                          const void *platform_descriptor,
                                          std::uint32_t platform_descriptor_size,
                                          const SurfaceMetrics &metrics) noexcept {
    if (session_ == nullptr || platform_descriptor == nullptr || !metrics_are_valid(metrics)) {
        return MIGO_ERROR_INVALID_ARGUMENT;
    }
    if (state_ != SurfaceState::Detached || attachment_ != nullptr || release_ != nullptr ||
        next_generation_ == 0) {
        return MIGO_ERROR_INVALID_STATE;
    }

    const std::uint64_t candidate_generation = next_generation_;
    MigoSurfaceDescriptor descriptor{};
    descriptor.struct_size = static_cast<std::uint32_t>(sizeof(descriptor));
    descriptor.abi_version = MIGO_ABI_VERSION_CURRENT;
    descriptor.generation = candidate_generation;
    descriptor.platform_kind = platform_kind;
    descriptor.flags = MIGO_SURFACE_DESCRIPTOR_FLAG_NONE;
    descriptor.width_pixels = metrics.width_pixels;
    descriptor.height_pixels = metrics.height_pixels;
    descriptor.scale_factor = metrics.scale_factor;
    descriptor.color_space = metrics.color_space;
    descriptor.alpha_mode = metrics.alpha_mode;
    descriptor.preferred_presentation_mode = metrics.presentation_mode;
    descriptor.capability_flags = metrics.required_capabilities;
    descriptor.platform_descriptor_size = platform_descriptor_size;
    descriptor.platform_descriptor = platform_descriptor;

    MigoSurfaceAttachment *attachment = nullptr;
    const MigoResult result = migo_session_attach_surface(session_, &descriptor, &attachment);
    if (result != MIGO_OK) {
        if (attachment != nullptr) {
            // A non-OK result is specified to leave this null. Preserve the
            // unexpected handle and fail closed: guessing that it is safe to
            // discard could let the host destroy a target still leased by a
            // mismatched or corrupt runtime.
            attachment_ = attachment;
            state_ = SurfaceState::Faulted;
        }
        return result;
    }

    consume_generation(candidate_generation);
    if (attachment == nullptr) {
        state_ = SurfaceState::Faulted;
        return MIGO_ERROR_INTERNAL;
    }

    attachment_ = attachment;
    attached_capabilities_ = metrics.required_capabilities;
    state_ = SurfaceState::Attached;
    return MIGO_OK;
}

MigoResult SurfaceHost::update(const SurfaceMetrics &metrics) noexcept {
    if (!is_owner_thread()) return MIGO_ERROR_WRONG_THREAD;
    if (state_ != SurfaceState::Attached || attachment_ == nullptr) {
        return MIGO_ERROR_INVALID_STATE;
    }
    if (!metrics_are_valid(metrics)) return MIGO_ERROR_INVALID_ARGUMENT;
    if (metrics.required_capabilities != attached_capabilities_) {
        return MIGO_ERROR_INVALID_ARGUMENT;
    }

    MigoSurfaceMetrics update{};
    update.struct_size = static_cast<std::uint32_t>(sizeof(update));
    update.abi_version = MIGO_ABI_VERSION_CURRENT;
    update.generation = generation_;
    update.width_pixels = metrics.width_pixels;
    update.height_pixels = metrics.height_pixels;
    update.scale_factor = metrics.scale_factor;
    update.color_space = metrics.color_space;
    update.alpha_mode = metrics.alpha_mode;
    update.preferred_presentation_mode = metrics.presentation_mode;
    update.flags = MIGO_SURFACE_DESCRIPTOR_FLAG_NONE;
    return migo_surface_update(attachment_, &update);
}

MigoResult SurfaceHost::begin_detach() noexcept {
    if (!is_owner_thread()) return MIGO_ERROR_WRONG_THREAD;
    if (state_ != SurfaceState::Attached || attachment_ == nullptr) {
        return MIGO_ERROR_INVALID_STATE;
    }

    MigoSurfaceRelease *release = nullptr;
    const MigoResult result = migo_surface_begin_detach(attachment_, &release);
    if (result != MIGO_OK) {
        if (release != nullptr) {
            // The ABI says failure consumes nothing and writes null. Owning
            // both pointers is intentionally unrecoverable rather than
            // silently choosing the wrong lifetime interpretation.
            release_ = release;
            state_ = SurfaceState::Faulted;
        }
        return result;
    }

    attachment_ = nullptr;
    if (release == nullptr) {
        state_ = SurfaceState::Faulted;
        return MIGO_ERROR_INTERNAL;
    }

    release_ = release;
    state_ = SurfaceState::Retiring;
    return MIGO_OK;
}

MigoResult SurfaceHost::poll_release(bool *released) noexcept {
    if (!is_owner_thread()) return MIGO_ERROR_WRONG_THREAD;
    if (released == nullptr) return MIGO_ERROR_INVALID_ARGUMENT;
    *released = false;
    if (state_ != SurfaceState::Retiring || release_ == nullptr) {
        return MIGO_ERROR_INVALID_STATE;
    }

    MigoSurfaceReleaseStatus status{};
    status.struct_size = static_cast<std::uint32_t>(sizeof(status));
    status.abi_version = MIGO_ABI_VERSION_CURRENT;
    const MigoResult query_result = migo_surface_release_query(release_, &status);
    if (query_result != MIGO_OK) return query_result;
    if (status.struct_size != static_cast<std::uint32_t>(sizeof(status)) ||
        status.abi_version != MIGO_ABI_VERSION_CURRENT || status.reserved0 != 0) {
        return MIGO_ERROR_INTERNAL;
    }
    if (status.generation != generation_) return MIGO_ERROR_INTERNAL;
    if (status.state == MIGO_SURFACE_RELEASE_PENDING) return MIGO_OK;
    if (status.state != MIGO_SURFACE_RELEASE_RELEASED) return MIGO_ERROR_INTERNAL;

    const MigoResult destroy_result = migo_surface_release_destroy(release_);
    if (destroy_result != MIGO_OK) return destroy_result;
    release_ = nullptr;
    state_ = SurfaceState::Detached;
    generation_ = 0;
    attached_capabilities_ = MIGO_SURFACE_CAPABILITY_NONE;
    *released = true;
    return MIGO_OK;
}

}  // namespace migo::linux_host
