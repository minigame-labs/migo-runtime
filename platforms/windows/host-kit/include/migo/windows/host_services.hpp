#ifndef MIGO_WINDOWS_HOST_SERVICES_HPP_
#define MIGO_WINDOWS_HOST_SERVICES_HPP_

#include <migo/migo.h>
#include <migo/windows/json.hpp>

#include <atomic>
#include <cstdint>
#include <memory>
#include <mutex>
#include <optional>
#include <string>
#include <string_view>

namespace migo::windows_host {

/// The bit a service takes in `MigoHostCallbacks.host_services`.
[[nodiscard]] constexpr std::uint64_t hostServiceBit(MigoHostService service) noexcept {
    return std::uint64_t{1} << service;
}

class HostServiceReply;

/// The way back to one Session for its host-service answers -- the Linux Host
/// Kit's `MigoQtHostServiceChannel`, the same contract.
///
/// A dialog can be answered after the game that asked has gone, so an answer
/// does not hold the Session: it holds this channel, which is closed before
/// the Session is destroyed. An answer or an event through a closed channel
/// reaches nobody, as the C ABI does for a call whose content has gone. Any
/// thread.
class HostServiceChannel final : public std::enable_shared_from_this<HostServiceChannel> {
public:
    [[nodiscard]] static std::shared_ptr<HostServiceChannel> open(MigoSession *session);

    HostServiceChannel(const HostServiceChannel &) = delete;
    HostServiceChannel &operator=(const HostServiceChannel &) = delete;

    /// Nothing made through the channel reaches the Session from now. Waits for
    /// one in progress, so once it returns the Session can be destroyed.
    void close() noexcept;

    /// The answer to the call `call_id` names; a command (call id 0) has none.
    [[nodiscard]] HostServiceReply reply(std::uint64_t call_id);

    /// Reports one of a declared service's events
    /// (`migo_session_post_host_service_event`); `payload` is an object.
    /// MIGO_OK once closed: there is no running content left to tell.
    MigoResult post(MigoHostService service, std::uint32_t event, const Json &payload);

    /// Records the standing decision for one of content's scopes
    /// (`migo_session_set_scope_state`). MIGO_OK once closed.
    MigoResult setScopeState(MigoScope scope, MigoScopeState state);

    /// Delivers one frame of a recording (`migo_session_post_recorder_frame`);
    /// `last` flags its last. MIGO_OK once closed.
    MigoResult postRecorderFrame(std::uint32_t recording_id, std::string_view bytes, bool last);

private:
    explicit HostServiceChannel(MigoSession *session) noexcept : session_(session) {}

    friend class HostServiceReply;
    MigoResult complete(std::uint64_t call_id, const MigoHostServiceResult &result);

    std::mutex mutex_;
    MigoSession *session_;
};

/// The answer to one request content made of the host: given once, from any
/// thread. The first `succeed` or `fail` completes the call; a later one
/// returns MIGO_ERROR_INVALID_STATE without reaching the Session. Copies share
/// the one answer. A command has none: `expectsAnswer()` is false and
/// answering it does nothing.
class HostServiceReply final {
public:
    [[nodiscard]] bool expectsAnswer() const noexcept { return call_id_ != 0; }

    /// Completes the call with `result`, an object of the method's fields.
    MigoResult succeed(const Json &result = Json::object());

    /// Fails the call. `reason` is required -- an empty one is refused with
    /// MIGO_ERROR_INVALID_ARGUMENT and leaves the call unanswered -- and does
    /// not name the API: content reads "<api>:fail <reason>". `error_code` is
    /// the API's numeric code, where it has one.
    MigoResult fail(std::string_view reason, std::optional<std::int32_t> error_code = std::nullopt);

private:
    friend class HostServiceChannel;
    HostServiceReply(std::shared_ptr<HostServiceChannel> channel, std::uint64_t call_id);

    MigoResult answer(const MigoHostServiceResult &result);

    std::shared_ptr<HostServiceChannel> channel_;
    std::uint64_t call_id_;
    std::shared_ptr<std::atomic<bool>> answered_;
};

/// Something that answers host services: the App's own integrations, or the
/// Host Kit's built-in ones.
class HostServiceProvider {
public:
    virtual ~HostServiceProvider() = default;

    /// The services answered, one `hostServiceBit` per MIGO_HOST_SERVICE_*.
    /// Read once, when the Session's callback table is installed.
    [[nodiscard]] virtual std::uint64_t services() const noexcept = 0;

    /// One request for a service this provider answers, on the GUI thread.
    /// `request` is the method's options. A call is answered through `reply`,
    /// now or later; a command is carried out.
    virtual void call(MigoHostService service, std::uint32_t method, const Json &request, HostServiceReply reply) = 0;
};

}  // namespace migo::windows_host

#endif  // MIGO_WINDOWS_HOST_SERVICES_HPP_
