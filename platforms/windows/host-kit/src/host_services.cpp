#include <migo/windows/host_services.hpp>

#include <utility>

namespace migo::windows_host {

std::shared_ptr<HostServiceChannel> HostServiceChannel::open(MigoSession *session) {
    return std::shared_ptr<HostServiceChannel>(new HostServiceChannel(session));
}

void HostServiceChannel::close() noexcept {
    const std::lock_guard<std::mutex> lock(mutex_);
    session_ = nullptr;
}

HostServiceReply HostServiceChannel::reply(std::uint64_t call_id) { return HostServiceReply(shared_from_this(), call_id); }

MigoResult HostServiceChannel::post(MigoHostService service, std::uint32_t event, const Json &payload) {
    const std::string json = payload.dump();
    const std::lock_guard<std::mutex> lock(mutex_);
    if (session_ == nullptr) return MIGO_OK;
    return migo_session_post_host_service_event(session_, service, event, json.c_str(),
                                                static_cast<std::uint32_t>(json.size()));
}

MigoResult HostServiceChannel::setScopeState(MigoScope scope, MigoScopeState state) {
    const std::lock_guard<std::mutex> lock(mutex_);
    if (session_ == nullptr) return MIGO_OK;
    return migo_session_set_scope_state(session_, scope, state);
}

MigoResult HostServiceChannel::postRecorderFrame(std::uint32_t recording_id, std::string_view bytes, bool last) {
    MigoRecorderFrame frame{};
    frame.struct_size = static_cast<std::uint32_t>(sizeof frame);
    frame.abi_version = MIGO_ABI_VERSION_1;
    frame.recording_id = recording_id;
    frame.is_last_frame = last ? 1U : 0U;
    frame.data = reinterpret_cast<const std::uint8_t *>(bytes.data());
    frame.length = static_cast<std::uint32_t>(bytes.size());
    const std::lock_guard<std::mutex> lock(mutex_);
    if (session_ == nullptr) return MIGO_OK;
    return migo_session_post_recorder_frame(session_, &frame);
}

MigoResult HostServiceChannel::complete(std::uint64_t call_id, const MigoHostServiceResult &result) {
    // Held across the call so close() cannot return -- and the Session be
    // destroyed -- while an answer is on its way in.
    const std::lock_guard<std::mutex> lock(mutex_);
    if (session_ == nullptr) return MIGO_OK;
    return migo_session_complete_host_service_call(session_, call_id, &result);
}

HostServiceReply::HostServiceReply(std::shared_ptr<HostServiceChannel> channel, std::uint64_t call_id)
    : channel_(std::move(channel)), call_id_(call_id), answered_(std::make_shared<std::atomic<bool>>(false)) {}

MigoResult HostServiceReply::answer(const MigoHostServiceResult &result) {
    if (call_id_ == 0) return MIGO_OK;
    if (answered_->exchange(true, std::memory_order_acq_rel)) return MIGO_ERROR_INVALID_STATE;
    return channel_->complete(call_id_, result);
}

MigoResult HostServiceReply::succeed(const Json &result) {
    const std::string json = result.dump();
    MigoHostServiceResult outcome{};
    outcome.struct_size = static_cast<std::uint32_t>(sizeof outcome);
    outcome.abi_version = MIGO_ABI_VERSION_1;
    outcome.status = MIGO_HOST_SERVICE_STATUS_OK;
    outcome.payload_json_utf8 = json.c_str();
    outcome.payload_length = static_cast<std::uint32_t>(json.size());
    return answer(outcome);
}

MigoResult HostServiceReply::fail(std::string_view reason, std::optional<std::int32_t> error_code) {
    // Refused before the answer is spent: the library refuses a failure with no
    // reason, and spending the one answer on that would leave content waiting.
    if (reason.empty()) return MIGO_ERROR_INVALID_ARGUMENT;
    MigoHostServiceResult outcome{};
    outcome.struct_size = static_cast<std::uint32_t>(sizeof outcome);
    outcome.abi_version = MIGO_ABI_VERSION_1;
    outcome.status = MIGO_HOST_SERVICE_STATUS_FAIL;
    outcome.message_utf8 = reason.data();
    outcome.message_length = static_cast<std::uint32_t>(reason.size());
    if (error_code.has_value()) {
        outcome.flags = MIGO_HOST_SERVICE_RESULT_FLAG_ERROR_CODE;
        outcome.error_code = *error_code;
    }
    return answer(outcome);
}

}  // namespace migo::windows_host
