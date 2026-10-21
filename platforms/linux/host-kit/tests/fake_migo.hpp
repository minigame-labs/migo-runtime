#ifndef MIGO_LINUX_HOST_KIT_TESTS_FAKE_MIGO_HPP_
#define MIGO_LINUX_HOST_KIT_TESTS_FAKE_MIGO_HPP_

#include <migo/input.h>
#include <migo/migo.h>
#include <migo/platform/wayland.h>
#include <migo/platform/x11.h>

#include <functional>
#include <string>
#include <vector>

namespace fake_migo {

struct Calls {
    int attach = 0;
    int update = 0;
    int begin_detach = 0;
    int query = 0;
    int destroy_release = 0;
    int touch = 0;
    int pointer = 0;
    int wheel = 0;
    int key = 0;
    int composition = 0;
    int focus = 0;
    int vsync = 0;
    int session_create = 0;
    int session_destroy = 0;
    int set_callbacks = 0;
    int load_content = 0;
};

/// One delivered input event, flattened.
///
/// The payload is recorded, not just the count: a bridge that sends the right
/// number of events with physical pixels instead of CSS pixels, or with `code`
/// in the `key` field, passes every counting assertion while being wrong in the
/// way that actually reaches a game.
struct TouchRecord {
    MigoTouchType type = 0;
    std::vector<MigoTouchPoint> points;
    int64_t timestamp_ms = 0;
};

struct PointerRecord {
    MigoPointerEventType event_type = 0;
    uint32_t button = 0;
    float x = 0.0F;
    float y = 0.0F;
    double timestamp_ms = 0.0;
};

struct WheelRecord {
    double delta_x = 0.0;
    double delta_y = 0.0;
    double delta_z = 0.0;
    MigoWheelDeltaMode delta_mode = 0;
    double timestamp_ms = 0.0;
};

struct KeyRecord {
    MigoKeyEventType event_type = 0;
    std::string key;
    std::string code;
    MigoKeyModifiers modifiers = 0;
    MigoKeyEventFlags flags = 0;
    double timestamp_ms = 0.0;
};

struct CompositionRecord {
    MigoCompositionEventType event_type = 0;
    std::string data;
};

const std::vector<TouchRecord> &touches() noexcept;
const std::vector<PointerRecord> &pointers() noexcept;
const std::vector<WheelRecord> &wheels() noexcept;
const std::vector<KeyRecord> &keys() noexcept;
const std::vector<CompositionRecord> &compositions() noexcept;
const std::vector<uint8_t> &focus_changes() noexcept;
const std::vector<int64_t> &vsyncs() noexcept;

/// Make every input entry point report a full queue, so a caller that ignores
/// the result can be caught.
void set_input_result(MigoResult result) noexcept;

MigoEngine *engine() noexcept;
/// The callback table the host installed, copied.
const MigoHostCallbacks &installed_callbacks() noexcept;
/// True while a Session handle the fake handed out has not been destroyed.
bool session_is_alive() noexcept;
const std::string &loaded_content_id() noexcept;

void set_session_create_result(MigoResult result) noexcept;
void set_set_callbacks_result(MigoResult result) noexcept;
void set_load_content_result(MigoResult result) noexcept;
void set_session_destroy_result(MigoResult result) noexcept;

/// What the Session was created with: its network policy and launch options,
/// empty where the host passed NULL.
const std::string &created_network_policy() noexcept;
const std::string &created_launch_options() noexcept;
/// The host app's identifier the host last reported, empty when it reported none.
const std::string &reported_host_app_id() noexcept;
/// The flags of the host's last report about itself (MIGO_HOST_APP_INFO_FLAG_*).
uint32_t reported_host_app_flags() noexcept;

/// What the host reported of the battery and the network, in order.
struct BatteryRecord {
    uint32_t level_percent = 0;
    MigoBatteryFlags flags = 0;
};
struct NetworkRecord {
    MigoNetworkType type = 0;
    uint8_t connected = 0;
};
const std::vector<BatteryRecord> &battery_reports() noexcept;
const std::vector<NetworkRecord> &network_reports() noexcept;
/// What the host said of content's window, in order: "hidden", "shown"
/// (migo_session_set_visibility), "paused", "running" (migo_session_set_lifecycle).
const std::vector<std::string> &window_reports() noexcept;
/// The system switches last reported (MIGO_SYSTEM_SETTING_FLAG_*), and how many times.
uint32_t reported_system_settings() noexcept;
int system_settings_reports() noexcept;
/// The App's authorisations last reported; struct_size 0 before any report.
const MigoAppAuthorizeSetting &reported_app_authorization() noexcept;
/// The memory warnings the host passed on, in order.
const std::vector<MigoMemoryWarningLevel> &memory_warnings() noexcept;
/// The standing decision the host recorded for `scope`; UNKNOWN until one.
MigoScopeState scope_state(MigoScope scope) noexcept;
/// The motion readings the host delivered, in order. A copy: they arrive on
/// the sensors' own thread.
std::vector<MigoSensorSample> sensor_samples();

/// One characteristic value the host delivered.
struct BleValue {
    std::string device;
    std::string service;
    std::string characteristic;
    std::string value;
};
std::vector<BleValue> ble_values();

/// The gamepads the host announced and withdrew, and its samples.
struct GamepadAnnouncement {
    MigoGamepadInfo info{};
    std::string id;
    std::string mapping;
    bool connected = false;
};
const std::vector<GamepadAnnouncement> &gamepad_announcements() noexcept;
struct GamepadSample {
    uint32_t index = 0;
    std::vector<float> axes;
    std::vector<MigoGamepadButton> buttons;
};
const std::vector<GamepadSample> &gamepad_samples() noexcept;

/// A line content writes to its console, which the host's debugging panel
/// reads back (migo_session_copy_console_logs): level 0 debug .. 3 error.
void write_console(int level, const std::string &text);

/// One answer the host gave a host-service call.
struct HostServiceAnswer {
    uint64_t call_id = 0;
    MigoHostServiceStatus status = 0;
    std::string message;
    std::string payload;
    bool has_error_code = false;
    int32_t error_code = 0;
};

/// One event the host posted for a service it declared.
struct HostServiceEvent {
    MigoHostService service = 0;
    uint32_t event = 0;
    std::string payload;
};

/// The answers so far. A copy: the host answers from any thread, as the
/// library lets it, while a test reads them on its own.
std::vector<HostServiceAnswer> host_service_answers();
const std::vector<HostServiceEvent> &host_service_events() noexcept;

/// Runs `take` on each answer as the host completes the call, on the thread
/// that completes it and before the answer is recorded: where the library takes
/// the files a result names into the Session (none, unless a test says so).
void set_host_service_completion(std::function<void(const HostServiceAnswer &)> take);

/// Open the call `call_id` of `service` as the library does on delivering it,
/// for a test that hands the request to a service itself.
void expect_host_service_call(MigoHostService service, uint64_t call_id) noexcept;

/// Deliver content's request of a host service the way the library does:
/// through the host's dispatcher, from an engine thread. A call (call_id != 0)
/// stays open until the host completes it, once; a command (0) is not answered.
void deliver_host_service_call_from_engine_thread(MigoHostService service, uint32_t method,
                                                  uint64_t call_id,
                                                  const std::string &payload_json) noexcept;

/// Drive the engine-side callbacks the way the real library would: through the
/// host's dispatcher, from a thread that is not the GUI thread.
void deliver_ready_from_engine_thread() noexcept;
void deliver_error_from_engine_thread(MigoResult code, const char *message) noexcept;
void deliver_frame_request_from_engine_thread() noexcept;
void deliver_keep_screen_on_from_engine_thread(bool keep_on) noexcept;

void reset() noexcept;

MigoSession *session() noexcept;
MigoSurfaceAttachment *attachment() noexcept;
MigoSurfaceRelease *release() noexcept;

const Calls &calls() noexcept;
const MigoSurfaceDescriptor &last_surface() noexcept;
const MigoSurfaceMetrics &last_metrics() noexcept;
const MigoSurfaceReleaseStatus &last_release_query() noexcept;
const MigoX11WindowDescriptor &last_x11() noexcept;
const MigoWaylandSurfaceDescriptor &last_wayland() noexcept;

void set_attach_result(MigoResult result) noexcept;
void set_attach_returns_null(bool returns_null) noexcept;
void set_attach_writes_handle_on_error(bool writes_handle) noexcept;
void set_update_result(MigoResult result) noexcept;
void set_begin_detach_result(MigoResult result) noexcept;
void set_begin_detach_returns_null(bool returns_null) noexcept;
void set_begin_detach_writes_handle_on_error(bool writes_handle) noexcept;
void set_query_result(MigoResult result) noexcept;
void set_release_status(uint64_t generation, MigoSurfaceReleaseState state) noexcept;
void set_release_status_metadata(uint32_t struct_size, uint32_t abi_version,
                                 uint32_t reserved0) noexcept;
void set_destroy_result(MigoResult result) noexcept;

}  // namespace fake_migo

#endif  // MIGO_LINUX_HOST_KIT_TESTS_FAKE_MIGO_HPP_
