#include "fake_migo.hpp"

#include <cstring>
#include <map>
#include <memory>
#include <mutex>
#include <thread>

namespace {

fake_migo::Calls g_calls;
MigoSurfaceDescriptor g_surface{};
MigoSurfaceMetrics g_metrics{};
MigoSurfaceReleaseStatus g_release_query{};
MigoX11WindowDescriptor g_x11{};
MigoWaylandSurfaceDescriptor g_wayland{};
MigoResult g_attach_result = MIGO_OK;
bool g_attach_returns_null = false;
bool g_attach_writes_handle_on_error = false;
MigoResult g_update_result = MIGO_OK;
MigoResult g_begin_detach_result = MIGO_OK;
bool g_begin_detach_returns_null = false;
bool g_begin_detach_writes_handle_on_error = false;
MigoResult g_query_result = MIGO_OK;
MigoSurfaceReleaseStatus g_release_status{};
MigoResult g_destroy_result = MIGO_OK;
MigoResult g_input_result = MIGO_OK;
std::vector<fake_migo::TouchRecord> g_touches;
std::vector<fake_migo::PointerRecord> g_pointers;
std::vector<fake_migo::WheelRecord> g_wheels;
std::vector<fake_migo::KeyRecord> g_keys;
std::vector<fake_migo::CompositionRecord> g_compositions;
std::vector<uint8_t> g_focus_changes;
std::vector<int64_t> g_vsyncs;
MigoHostCallbacks g_callbacks{};
bool g_session_alive = false;
std::string g_content_id;
MigoResult g_session_create_result = MIGO_OK;
MigoResult g_set_callbacks_result = MIGO_OK;
MigoResult g_load_content_result = MIGO_OK;
/// Whether the Session has its host: the library makes it on the first attach, and content needs it.
bool g_has_host = false;
MigoResult g_session_destroy_result = MIGO_OK;
std::string g_network_policy;
std::string g_host_app_id;
uint32_t g_host_app_flags = 0;
/// What content wrote to its console, as the library keeps it: each line's
/// JSON, in order; a line's cursor is its index plus one.
std::vector<std::string> g_console;
std::vector<fake_migo::BatteryRecord> g_battery;
std::vector<fake_migo::NetworkRecord> g_network;
std::vector<std::string> g_window;
uint32_t g_system_settings = 0;
int g_system_settings_reports = 0;
MigoAppAuthorizeSetting g_app_authorization{};
std::vector<MigoMemoryWarningLevel> g_memory_warnings;
std::string g_launch_options;
/// Guards the calls and their answers: the host completes a call from any thread.
std::mutex g_answers_mutex;
std::vector<fake_migo::HostServiceAnswer> g_answers;
std::vector<fake_migo::HostServiceEvent> g_events;
/// Calls delivered and not yet completed, with the service each is of.
std::map<uint64_t, MigoHostService> g_open_calls;
std::function<void(const fake_migo::HostServiceAnswer &)> g_completion;
constexpr uintptr_t kEngineToken = 0x4040;

constexpr uintptr_t kSessionToken = 0x1010;
constexpr uintptr_t kAttachmentToken = 0x2020;
constexpr uintptr_t kReleaseToken = 0x3030;

}  // namespace

namespace fake_migo {

void reset() noexcept {
    g_calls = {};
    g_network_policy.clear();
    g_host_app_id.clear();
    g_host_app_flags = 0;
    g_console.clear();
    g_battery.clear();
    g_network.clear();
    g_window.clear();
    g_system_settings = 0;
    g_system_settings_reports = 0;
    g_app_authorization = {};
    g_memory_warnings.clear();
    g_launch_options.clear();
    {
        const std::lock_guard<std::mutex> lock(g_answers_mutex);
        g_answers.clear();
        g_open_calls.clear();
        g_completion = nullptr;
    }
    g_events.clear();
    g_surface = {};
    g_metrics = {};
    g_release_query = {};
    g_x11 = {};
    g_wayland = {};
    g_attach_result = MIGO_OK;
    g_attach_returns_null = false;
    g_attach_writes_handle_on_error = false;
    g_update_result = MIGO_OK;
    g_begin_detach_result = MIGO_OK;
    g_begin_detach_returns_null = false;
    g_begin_detach_writes_handle_on_error = false;
    g_query_result = MIGO_OK;
    g_release_status = {};
    g_release_status.struct_size = sizeof(g_release_status);
    g_release_status.abi_version = MIGO_ABI_VERSION_CURRENT;
    g_release_status.state = MIGO_SURFACE_RELEASE_PENDING;
    g_destroy_result = MIGO_OK;
    g_input_result = MIGO_OK;
    g_touches.clear();
    g_pointers.clear();
    g_wheels.clear();
    g_keys.clear();
    g_compositions.clear();
    g_focus_changes.clear();
    g_vsyncs.clear();
    g_callbacks = {};
    g_session_alive = false;
    g_content_id.clear();
    g_session_create_result = MIGO_OK;
    g_set_callbacks_result = MIGO_OK;
    g_load_content_result = MIGO_OK;
    g_has_host = false;
    g_session_destroy_result = MIGO_OK;
    // Reserved, not merely cleared: the allocation test measures the delivery
    // path by differencing a delivered burst against an undelivered one, and a
    // vector growing here would land in that difference and be read as the
    // adapter allocating per event. `clear()` keeps capacity, so this only ever
    // allocates once per process.
    constexpr std::size_t kRecordCapacity = 1024;
    g_touches.reserve(kRecordCapacity);
    g_pointers.reserve(kRecordCapacity);
    g_wheels.reserve(kRecordCapacity);
    g_keys.reserve(kRecordCapacity);
    g_compositions.reserve(kRecordCapacity);
    g_focus_changes.reserve(kRecordCapacity);
    g_vsyncs.reserve(kRecordCapacity);
}

MigoSession *session() noexcept {
    return reinterpret_cast<MigoSession *>(kSessionToken);
}

MigoSurfaceAttachment *attachment() noexcept {
    return reinterpret_cast<MigoSurfaceAttachment *>(kAttachmentToken);
}

MigoSurfaceRelease *release() noexcept {
    return reinterpret_cast<MigoSurfaceRelease *>(kReleaseToken);
}

const Calls &calls() noexcept { return g_calls; }
const MigoSurfaceDescriptor &last_surface() noexcept { return g_surface; }
const MigoSurfaceMetrics &last_metrics() noexcept { return g_metrics; }
const MigoSurfaceReleaseStatus &last_release_query() noexcept { return g_release_query; }
const MigoX11WindowDescriptor &last_x11() noexcept { return g_x11; }
const MigoWaylandSurfaceDescriptor &last_wayland() noexcept { return g_wayland; }

void set_attach_result(MigoResult result) noexcept { g_attach_result = result; }
void set_attach_returns_null(bool returns_null) noexcept { g_attach_returns_null = returns_null; }
void set_attach_writes_handle_on_error(bool writes_handle) noexcept {
    g_attach_writes_handle_on_error = writes_handle;
}
void set_update_result(MigoResult result) noexcept { g_update_result = result; }
void set_begin_detach_result(MigoResult result) noexcept { g_begin_detach_result = result; }
void set_begin_detach_returns_null(bool returns_null) noexcept {
    g_begin_detach_returns_null = returns_null;
}
void set_begin_detach_writes_handle_on_error(bool writes_handle) noexcept {
    g_begin_detach_writes_handle_on_error = writes_handle;
}
void set_query_result(MigoResult result) noexcept { g_query_result = result; }
void set_release_status(uint64_t generation, MigoSurfaceReleaseState state) noexcept {
    g_release_status.generation = generation;
    g_release_status.state = state;
}
void set_release_status_metadata(uint32_t struct_size, uint32_t abi_version,
                                 uint32_t reserved0) noexcept {
    g_release_status.struct_size = struct_size;
    g_release_status.abi_version = abi_version;
    g_release_status.reserved0 = reserved0;
}
void set_destroy_result(MigoResult result) noexcept { g_destroy_result = result; }
void set_input_result(MigoResult result) noexcept { g_input_result = result; }

const std::vector<TouchRecord> &touches() noexcept { return g_touches; }
const std::vector<PointerRecord> &pointers() noexcept { return g_pointers; }
const std::vector<WheelRecord> &wheels() noexcept { return g_wheels; }
const std::vector<KeyRecord> &keys() noexcept { return g_keys; }
const std::vector<CompositionRecord> &compositions() noexcept { return g_compositions; }
const std::vector<uint8_t> &focus_changes() noexcept { return g_focus_changes; }
const std::vector<int64_t> &vsyncs() noexcept { return g_vsyncs; }
MigoEngine *engine() noexcept { return reinterpret_cast<MigoEngine *>(kEngineToken); }
const MigoHostCallbacks &installed_callbacks() noexcept { return g_callbacks; }
bool session_is_alive() noexcept { return g_session_alive; }
const std::string &loaded_content_id() noexcept { return g_content_id; }
const std::string &created_network_policy() noexcept { return g_network_policy; }
const std::string &reported_host_app_id() noexcept { return g_host_app_id; }
uint32_t reported_host_app_flags() noexcept { return g_host_app_flags; }
const std::vector<BatteryRecord> &battery_reports() noexcept { return g_battery; }
const std::vector<NetworkRecord> &network_reports() noexcept { return g_network; }
const std::vector<std::string> &window_reports() noexcept { return g_window; }
uint32_t reported_system_settings() noexcept { return g_system_settings; }
int system_settings_reports() noexcept { return g_system_settings_reports; }
const MigoAppAuthorizeSetting &reported_app_authorization() noexcept { return g_app_authorization; }
const std::vector<MigoMemoryWarningLevel> &memory_warnings() noexcept { return g_memory_warnings; }

void write_console(int level, const std::string &text) {
    // Plain text only: the tests write no character JSON would need escaped.
    g_console.push_back("{\"l\":" + std::to_string(level) + ",\"t\":0,\"m\":\"" + text + "\"}");
}
const std::string &created_launch_options() noexcept { return g_launch_options; }
std::vector<HostServiceAnswer> host_service_answers() {
    const std::lock_guard<std::mutex> lock(g_answers_mutex);
    return g_answers;
}
void set_host_service_completion(std::function<void(const HostServiceAnswer &)> take) {
    const std::lock_guard<std::mutex> lock(g_answers_mutex);
    g_completion = std::move(take);
}
const std::vector<HostServiceEvent> &host_service_events() noexcept { return g_events; }
void set_session_create_result(MigoResult result) noexcept { g_session_create_result = result; }
void set_set_callbacks_result(MigoResult result) noexcept { g_set_callbacks_result = result; }
void set_load_content_result(MigoResult result) noexcept { g_load_content_result = result; }
void set_session_destroy_result(MigoResult result) noexcept { g_session_destroy_result = result; }

namespace {

/// Run one engine-side callback the way the real library does: hand it to the
/// host's dispatcher from a thread that is not the GUI thread. A test that
/// called the callback directly would never exercise the marshalling, which is
/// the part that can go wrong.
void dispatch_from_engine_thread(void (*body)(const MigoHostCallbacks &)) {
    if (g_callbacks.dispatch == nullptr) return;
    static void (*s_body)(const MigoHostCallbacks &) = nullptr;
    s_body = body;
    std::thread engine_thread([] {
        g_callbacks.dispatch(
            g_callbacks.dispatcher_data,
            [](void *) {
                if (s_body != nullptr) s_body(g_callbacks);
            },
            nullptr);
    });
    engine_thread.join();
}

MigoResult g_pending_error_code = MIGO_OK;
std::string g_pending_error_message;

}  // namespace

void deliver_ready_from_engine_thread() noexcept {
    dispatch_from_engine_thread([](const MigoHostCallbacks &callbacks) {
        if (callbacks.on_ready != nullptr) {
            callbacks.on_ready(callbacks.user_data, session());
        }
    });
}

void deliver_error_from_engine_thread(MigoResult code, const char *message) noexcept {
    g_pending_error_code = code;
    g_pending_error_message = message != nullptr ? message : "";
    dispatch_from_engine_thread([](const MigoHostCallbacks &callbacks) {
        if (callbacks.on_error == nullptr) return;
        MigoError error{};
        error.struct_size = sizeof(error);
        error.abi_version = MIGO_ABI_VERSION_CURRENT;
        error.code = g_pending_error_code;
        error.message_utf8 = g_pending_error_message.c_str();
        error.message_length = static_cast<uint32_t>(g_pending_error_message.size());
        callbacks.on_error(callbacks.user_data, session(), &error);
    });
}

namespace {

/// One request on its way to the host, owned by the task that carries it:
/// consecutive deliveries are queued before any runs, so each carries its own.
struct PendingCall {
    MigoHostService service = 0;
    uint32_t method = 0;
    uint64_t call_id = 0;
    std::string payload;
};

}  // namespace

void deliver_host_service_call_from_engine_thread(MigoHostService service, uint32_t method,
                                                  uint64_t call_id,
                                                  const std::string &payload_json) noexcept {
    if (call_id != 0) {
        const std::lock_guard<std::mutex> lock(g_answers_mutex);
        g_open_calls[call_id] = service;
    }
    if (g_callbacks.dispatch == nullptr) return;
    auto *pending = new PendingCall{service, method, call_id, payload_json};
    std::thread engine_thread([pending] {
        const MigoResult accepted = g_callbacks.dispatch(
            g_callbacks.dispatcher_data,
            [](void *context) {
                const std::unique_ptr<PendingCall> call(static_cast<PendingCall *>(context));
                if (g_callbacks.on_host_service_call == nullptr) return;
                MigoHostServiceCall request{};
                request.struct_size = sizeof(request);
                request.abi_version = MIGO_ABI_VERSION_CURRENT;
                request.call_id = call->call_id;
                request.service = call->service;
                request.method = call->method;
                request.payload_json_utf8 = call->payload.c_str();
                request.payload_length = static_cast<uint32_t>(call->payload.size());
                g_callbacks.on_host_service_call(g_callbacks.user_data, fake_migo::session(), &request);
            },
            pending);
        // Refused, the task is the deliverer's again.
        if (accepted != MIGO_OK) delete pending;
    });
    engine_thread.join();
}

void deliver_frame_request_from_engine_thread() noexcept {
    dispatch_from_engine_thread([](const MigoHostCallbacks &callbacks) {
        if (callbacks.on_request_frame != nullptr) {
            callbacks.on_request_frame(callbacks.user_data, session());
        }
    });
}

void deliver_keep_screen_on_from_engine_thread(bool keep_on) noexcept {
    // Each delivery is its own task: a capture by value outlives this call.
    if (keep_on) {
        dispatch_from_engine_thread([](const MigoHostCallbacks &callbacks) {
            if (callbacks.on_keep_screen_on != nullptr) callbacks.on_keep_screen_on(callbacks.user_data, session(), 1);
        });
    } else {
        dispatch_from_engine_thread([](const MigoHostCallbacks &callbacks) {
            if (callbacks.on_keep_screen_on != nullptr) callbacks.on_keep_screen_on(callbacks.user_data, session(), 0);
        });
    }
}

}  // namespace fake_migo

extern "C" MigoResult MIGO_CALL
migo_session_attach_surface(MigoSession *session, const MigoSurfaceDescriptor *descriptor,
                            MigoSurfaceAttachment **out_attachment) {
    ++g_calls.attach;
    if (out_attachment != nullptr) *out_attachment = nullptr;
    if (session == nullptr || descriptor == nullptr || out_attachment == nullptr) {
        return MIGO_ERROR_INVALID_ARGUMENT;
    }
    g_surface = *descriptor;
    if (descriptor->platform_descriptor != nullptr) {
        if (descriptor->platform_kind == MIGO_PLATFORM_X11_WINDOW) {
            std::memcpy(&g_x11, descriptor->platform_descriptor, sizeof(g_x11));
            g_surface.platform_descriptor = &g_x11;
        } else if (descriptor->platform_kind == MIGO_PLATFORM_WAYLAND_SURFACE) {
            std::memcpy(&g_wayland, descriptor->platform_descriptor, sizeof(g_wayland));
            g_surface.platform_descriptor = &g_wayland;
        }
    }
    if (g_attach_result == MIGO_OK && !g_attach_returns_null) g_has_host = true;
    if ((g_attach_result == MIGO_OK && !g_attach_returns_null) ||
        (g_attach_result != MIGO_OK && g_attach_writes_handle_on_error)) {
        *out_attachment = fake_migo::attachment();
    }
    return g_attach_result;
}

extern "C" MigoResult MIGO_CALL
migo_surface_update(MigoSurfaceAttachment *attachment, const MigoSurfaceMetrics *metrics) {
    ++g_calls.update;
    if (attachment == nullptr || metrics == nullptr) return MIGO_ERROR_INVALID_ARGUMENT;
    g_metrics = *metrics;
    return g_update_result;
}

extern "C" MigoResult MIGO_CALL
migo_surface_begin_detach(MigoSurfaceAttachment *attachment, MigoSurfaceRelease **out_release) {
    ++g_calls.begin_detach;
    if (out_release != nullptr) *out_release = nullptr;
    if (attachment == nullptr || out_release == nullptr) return MIGO_ERROR_INVALID_ARGUMENT;
    if ((g_begin_detach_result == MIGO_OK && !g_begin_detach_returns_null) ||
        (g_begin_detach_result != MIGO_OK && g_begin_detach_writes_handle_on_error)) {
        *out_release = fake_migo::release();
    }
    return g_begin_detach_result;
}

extern "C" MigoResult MIGO_CALL
migo_surface_release_query(const MigoSurfaceRelease *release,
                           MigoSurfaceReleaseStatus *out_status) {
    ++g_calls.query;
    if (release == nullptr || out_status == nullptr) return MIGO_ERROR_INVALID_ARGUMENT;
    g_release_query = *out_status;
    if (g_query_result == MIGO_OK) *out_status = g_release_status;
    return g_query_result;
}

extern "C" MigoResult MIGO_CALL migo_surface_release_destroy(MigoSurfaceRelease *release) {
    ++g_calls.destroy_release;
    if (release == nullptr) return MIGO_ERROR_INVALID_ARGUMENT;
    return g_destroy_result;
}

extern "C" MigoResult MIGO_CALL migo_session_send_touch(MigoSession *session,
                                                        const MigoTouchEvent *event) {
    ++g_calls.touch;
    if (session == nullptr || event == nullptr) return MIGO_ERROR_INVALID_ARGUMENT;
    if (event->struct_size != sizeof(*event)) return MIGO_ERROR_INVALID_ARGUMENT;
    if (event->points == nullptr || event->point_count == 0) return MIGO_ERROR_INVALID_ARGUMENT;
    fake_migo::TouchRecord record;
    record.type = event->type;
    record.timestamp_ms = event->timestamp_ms;
    record.points.assign(event->points, event->points + event->point_count);
    g_touches.push_back(std::move(record));
    return g_input_result;
}

extern "C" MigoResult MIGO_CALL migo_session_send_pointer_event(MigoSession *session,
                                                                const MigoPointerEvent *event) {
    ++g_calls.pointer;
    if (session == nullptr || event == nullptr) return MIGO_ERROR_INVALID_ARGUMENT;
    if (event->struct_size != sizeof(*event)) return MIGO_ERROR_INVALID_ARGUMENT;
    g_pointers.push_back({event->event_type, event->button, event->x, event->y,
                          event->timestamp_ms});
    return g_input_result;
}

extern "C" MigoResult MIGO_CALL migo_session_send_wheel_event(MigoSession *session,
                                                              const MigoWheelEvent *event) {
    ++g_calls.wheel;
    if (session == nullptr || event == nullptr) return MIGO_ERROR_INVALID_ARGUMENT;
    if (event->struct_size != sizeof(*event)) return MIGO_ERROR_INVALID_ARGUMENT;
    if (event->reserved0 != 0) return MIGO_ERROR_INVALID_ARGUMENT;
    g_wheels.push_back({event->delta_x, event->delta_y, event->delta_z, event->delta_mode,
                        event->timestamp_ms});
    return g_input_result;
}

extern "C" MigoResult MIGO_CALL migo_session_send_key_event(MigoSession *session,
                                                            const MigoKeyEvent *event) {
    ++g_calls.key;
    if (session == nullptr || event == nullptr) return MIGO_ERROR_INVALID_ARGUMENT;
    if (event->struct_size != sizeof(*event)) return MIGO_ERROR_INVALID_ARGUMENT;
    if (event->reserved0 != 0) return MIGO_ERROR_INVALID_ARGUMENT;
    // The real library rejects an empty code, so the fake must too: a bridge
    // that could not identify the physical key must say "Unidentified" rather
    // than send nothing.
    if (event->code_utf8 == nullptr || event->code_length == 0) {
        return MIGO_ERROR_INVALID_ARGUMENT;
    }
    fake_migo::KeyRecord record;
    record.event_type = event->event_type;
    if (event->key_utf8 != nullptr) record.key.assign(event->key_utf8, event->key_length);
    record.code.assign(event->code_utf8, event->code_length);
    record.modifiers = event->modifiers;
    record.flags = event->flags;
    record.timestamp_ms = event->timestamp_ms;
    g_keys.push_back(std::move(record));
    return g_input_result;
}

extern "C" MigoResult MIGO_CALL
migo_session_send_composition_event(MigoSession *session, const MigoCompositionEvent *event) {
    ++g_calls.composition;
    if (session == nullptr || event == nullptr) return MIGO_ERROR_INVALID_ARGUMENT;
    if (event->struct_size != sizeof(*event)) return MIGO_ERROR_INVALID_ARGUMENT;
    fake_migo::CompositionRecord record;
    record.event_type = event->event_type;
    if (event->data_utf8 != nullptr) record.data.assign(event->data_utf8, event->data_length);
    g_compositions.push_back(std::move(record));
    return g_input_result;
}

extern "C" MigoResult MIGO_CALL migo_session_set_focus(MigoSession *session, uint8_t focused) {
    ++g_calls.focus;
    if (session == nullptr) return MIGO_ERROR_INVALID_ARGUMENT;
    g_focus_changes.push_back(focused);
    return g_input_result;
}

extern "C" MigoResult MIGO_CALL migo_session_notify_vsync(MigoSession *session,
                                                          int64_t frame_time_nanos) {
    ++g_calls.vsync;
    if (session == nullptr) return MIGO_ERROR_INVALID_ARGUMENT;
    g_vsyncs.push_back(frame_time_nanos);
    return g_input_result;
}

extern "C" MigoResult MIGO_CALL migo_session_create(MigoEngine *engine,
                                                    const MigoSessionConfig *config,
                                                    MigoSession **out_session) {
    ++g_calls.session_create;
    if (out_session != nullptr) *out_session = nullptr;
    if (engine == nullptr || config == nullptr || out_session == nullptr) {
        return MIGO_ERROR_INVALID_ARGUMENT;
    }
    if (config->struct_size != sizeof(*config)) return MIGO_ERROR_INVALID_ARGUMENT;
    if (g_session_create_result != MIGO_OK) return g_session_create_result;
    g_network_policy = config->network_policy_json_utf8 != nullptr ? config->network_policy_json_utf8 : "";
    g_launch_options = config->launch_options_json_utf8 != nullptr ? config->launch_options_json_utf8 : "";
    g_session_alive = true;
    *out_session = fake_migo::session();
    return MIGO_OK;
}

extern "C" MigoResult MIGO_CALL
migo_session_set_host_callbacks(MigoSession *session, const MigoHostCallbacks *callbacks) {
    ++g_calls.set_callbacks;
    if (session == nullptr || callbacks == nullptr) return MIGO_ERROR_INVALID_ARGUMENT;
    if (callbacks->struct_size > sizeof(*callbacks)) return MIGO_ERROR_UNSUPPORTED_ABI;
    // A non-null callback requires a non-null dispatcher: the real library
    // refuses otherwise, and a fake that accepted it would let a host ship a
    // table whose callbacks can never be delivered.
    const bool has_callback = callbacks->on_ready != nullptr || callbacks->on_error != nullptr ||
                              callbacks->on_exit_requested != nullptr ||
                              callbacks->on_request_frame != nullptr;
    if (has_callback && callbacks->dispatch == nullptr) return MIGO_ERROR_INVALID_ARGUMENT;
    // All three keyboard callbacks or none.
    const int keyboard_installed = (callbacks->on_show_keyboard != nullptr ? 1 : 0) +
                                   (callbacks->on_hide_keyboard != nullptr ? 1 : 0) +
                                   (callbacks->on_update_keyboard != nullptr ? 1 : 0);
    if (keyboard_installed != 0 && keyboard_installed != 3) return MIGO_ERROR_INVALID_ARGUMENT;
    // The host-service channel: the callback and the services it answers, both or neither, and only
    // services the header defines.
    if ((callbacks->on_host_service_call != nullptr) != (callbacks->host_services != 0)) {
        return MIGO_ERROR_INVALID_ARGUMENT;
    }
    if ((callbacks->host_services >> (MIGO_HOST_SERVICE_UPDATE + 1)) != 0) return MIGO_ERROR_INVALID_ARGUMENT;
    if (callbacks->on_host_service_call != nullptr && callbacks->dispatch == nullptr) {
        return MIGO_ERROR_INVALID_ARGUMENT;
    }
    if (g_set_callbacks_result != MIGO_OK) return g_set_callbacks_result;
    g_callbacks = *callbacks;
    return MIGO_OK;
}

extern "C" MigoResult MIGO_CALL migo_session_load_content(MigoSession *session,
                                                          const MigoContentDescriptor *content) {
    ++g_calls.load_content;
    if (session == nullptr || content == nullptr) return MIGO_ERROR_INVALID_ARGUMENT;
    if (content->struct_size != sizeof(*content)) return MIGO_ERROR_INVALID_ARGUMENT;
    if (content->content_id_utf8 == nullptr || content->entry_utf8 == nullptr) {
        return MIGO_ERROR_INVALID_ARGUMENT;
    }
    // As the library: content needs a render target, and there is none before the first attach.
    if (!g_has_host) return MIGO_ERROR_INVALID_STATE;
    if (g_load_content_result != MIGO_OK) return g_load_content_result;
    g_content_id = content->content_id_utf8;
    return MIGO_OK;
}

extern "C" MigoResult MIGO_CALL migo_session_destroy(MigoSession *session) {
    ++g_calls.session_destroy;
    if (session == nullptr) return MIGO_ERROR_INVALID_ARGUMENT;
    if (g_session_destroy_result != MIGO_OK) return g_session_destroy_result;
    g_session_alive = false;
    return MIGO_OK;
}

extern "C" MigoResult MIGO_CALL migo_session_complete_host_service_call(
    MigoSession *session, uint64_t call_id, const MigoHostServiceResult *result) {
    // As strict as the library: an open call of a declared service, answered
    // once, a failure with a reason.
    if (session != fake_migo::session() || !g_session_alive || result == nullptr) {
        return MIGO_ERROR_INVALID_ARGUMENT;
    }
    std::function<void(const fake_migo::HostServiceAnswer &)> take;
    {
        const std::lock_guard<std::mutex> lock(g_answers_mutex);
        const auto open = g_open_calls.find(call_id);
        if (open == g_open_calls.end()) return MIGO_ERROR_INVALID_ARGUMENT;
        if ((g_callbacks.host_services & (uint64_t{1} << open->second)) == 0) return MIGO_ERROR_INVALID_ARGUMENT;
        if (result->status == MIGO_HOST_SERVICE_STATUS_FAIL && result->message_length == 0) {
            return MIGO_ERROR_INVALID_ARGUMENT;
        }
        g_open_calls.erase(open);
        take = g_completion;
    }
    fake_migo::HostServiceAnswer answer;
    answer.call_id = call_id;
    answer.status = result->status;
    if (result->message_utf8 != nullptr) answer.message.assign(result->message_utf8, result->message_length);
    if (result->payload_json_utf8 != nullptr) answer.payload.assign(result->payload_json_utf8, result->payload_length);
    answer.has_error_code = (result->flags & MIGO_HOST_SERVICE_RESULT_FLAG_ERROR_CODE) != 0;
    answer.error_code = result->error_code;
    // Before the answer is seen, as the library takes the files before content hears of them.
    if (take) take(answer);
    const std::lock_guard<std::mutex> lock(g_answers_mutex);
    g_answers.push_back(std::move(answer));
    return MIGO_OK;
}

extern "C" MigoResult MIGO_CALL migo_session_post_host_service_event(
    MigoSession *session, MigoHostService service, uint32_t event, const char *payload_json_utf8,
    uint32_t payload_length) {
    if (session != fake_migo::session() || !g_session_alive || payload_json_utf8 == nullptr) {
        return MIGO_ERROR_INVALID_ARGUMENT;
    }
    if ((g_callbacks.host_services & (uint64_t{1} << service)) == 0) return MIGO_ERROR_INVALID_ARGUMENT;
    g_events.push_back({service, event, std::string(payload_json_utf8, payload_length)});
    return MIGO_OK;
}

extern "C" MigoResult MIGO_CALL migo_session_set_host_app_info(MigoSession *session, const MigoHostAppInfo *info) {
    if (session != fake_migo::session() || !g_session_alive || info == nullptr) return MIGO_ERROR_INVALID_ARGUMENT;
    if (info->struct_size != sizeof(*info) || info->benchmark_level < -2) return MIGO_ERROR_INVALID_ARGUMENT;
    g_host_app_id = info->app_id_utf8 != nullptr ? info->app_id_utf8 : "";
    g_host_app_flags = info->flags;
    return MIGO_OK;
}

extern "C" MigoResult MIGO_CALL migo_session_set_visibility(MigoSession *session, uint8_t visible) {
    if (session != fake_migo::session() || !g_session_alive || visible > 1) return MIGO_ERROR_INVALID_ARGUMENT;
    g_window.emplace_back(visible != 0 ? "shown" : "hidden");
    return MIGO_OK;
}

extern "C" MigoResult MIGO_CALL migo_session_set_lifecycle(MigoSession *session, MigoLifecycleState state) {
    if (session != fake_migo::session() || !g_session_alive) return MIGO_ERROR_INVALID_ARGUMENT;
    if (state == MIGO_LIFECYCLE_CREATED) return MIGO_ERROR_INVALID_STATE;
    if (state != MIGO_LIFECYCLE_RUNNING && state != MIGO_LIFECYCLE_PAUSED) return MIGO_ERROR_INVALID_ARGUMENT;
    g_window.emplace_back(state == MIGO_LIFECYCLE_RUNNING ? "running" : "paused");
    return MIGO_OK;
}

extern "C" MigoResult MIGO_CALL migo_session_set_system_settings(MigoSession *session, MigoSystemSettingFlags flags) {
    constexpr MigoSystemSettingFlags kKnown = MIGO_SYSTEM_SETTING_FLAG_BLUETOOTH_ENABLED |
                                              MIGO_SYSTEM_SETTING_FLAG_LOCATION_ENABLED |
                                              MIGO_SYSTEM_SETTING_FLAG_WIFI_ENABLED;
    if (session != fake_migo::session() || !g_session_alive || (flags & ~kKnown) != 0) return MIGO_ERROR_INVALID_ARGUMENT;
    g_system_settings = flags;
    ++g_system_settings_reports;
    return MIGO_OK;
}

extern "C" MigoResult MIGO_CALL migo_session_set_app_authorize_setting(MigoSession *session,
                                                                     const MigoAppAuthorizeSetting *setting) {
    if (session != fake_migo::session() || !g_session_alive || setting == nullptr ||
        setting->struct_size != sizeof(*setting)) {
        return MIGO_ERROR_INVALID_ARGUMENT;
    }
    g_app_authorization = *setting;
    return MIGO_OK;
}

extern "C" MigoResult MIGO_CALL migo_session_post_memory_warning(MigoSession *session, MigoMemoryWarningLevel level) {
    if (session != fake_migo::session() || !g_session_alive) return MIGO_ERROR_INVALID_ARGUMENT;
    if (level != MIGO_MEMORY_WARNING_MODERATE && level != MIGO_MEMORY_WARNING_LOW &&
        level != MIGO_MEMORY_WARNING_CRITICAL) {
        return MIGO_ERROR_INVALID_ARGUMENT;
    }
    g_memory_warnings.push_back(level);
    return MIGO_OK;
}

extern "C" MigoResult MIGO_CALL migo_session_set_battery_status(MigoSession *session, uint32_t level_percent,
                                                              MigoBatteryFlags flags) {
    if (session != fake_migo::session() || !g_session_alive || level_percent > 100) return MIGO_ERROR_INVALID_ARGUMENT;
    if ((flags & ~(MIGO_BATTERY_FLAG_CHARGING | MIGO_BATTERY_FLAG_LOW_POWER_MODE)) != 0) return MIGO_ERROR_INVALID_ARGUMENT;
    g_battery.push_back({level_percent, flags});
    return MIGO_OK;
}

extern "C" MigoResult MIGO_CALL migo_session_set_network_status(MigoSession *session, MigoNetworkType type,
                                                              uint8_t connected) {
    if (session != fake_migo::session() || !g_session_alive || type > MIGO_NETWORK_5G || connected > 1 ||
        (type == MIGO_NETWORK_NONE && connected != 0)) {
        return MIGO_ERROR_INVALID_ARGUMENT;
    }
    g_network.push_back({type, connected});
    return MIGO_OK;
}

extern "C" MigoResult MIGO_CALL migo_session_copy_console_logs(MigoSession *session, uint64_t since_cursor,
                                                             char *buffer, size_t capacity, size_t *out_length) {
    if (session != fake_migo::session() || !g_session_alive || out_length == nullptr) return MIGO_ERROR_INVALID_ARGUMENT;
    std::string json = "{\"logs\":[";
    for (size_t index = since_cursor; index < g_console.size(); ++index) {
        if (index > since_cursor) json += ",";
        json += g_console[index];
    }
    json += "],\"cursor\":" + std::to_string(g_console.size()) + "}";
    *out_length = json.size();
    if (buffer == nullptr || capacity < json.size() + 1) return MIGO_ERROR_INVALID_ARGUMENT;
    std::memcpy(buffer, json.c_str(), json.size() + 1);
    return MIGO_OK;
}
