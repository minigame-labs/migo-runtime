// The Managed session against the strict fake of the C ABI: what it creates and
// installs, engine callbacks run on the GUI thread, the content loaded once the
// view attaches, the window's state reported, the App's host services, and the
// teardown's three steps.

#include "fake_migo.hpp"

#include <migo/windows/managed_session.hpp>

#ifndef WIN32_LEAN_AND_MEAN
#define WIN32_LEAN_AND_MEAN
#endif
#ifndef NOMINMAX
#define NOMINMAX
#endif
#ifndef UNICODE
#define UNICODE
#endif
#include <windows.h>

#include <algorithm>
#include <chrono>
#include <cstdio>
#include <optional>
#include <string>
#include <thread>
#include <vector>

namespace {

int g_failures = 0;

#define CHECK(condition)                                                                        \
    do {                                                                                        \
        if (!(condition)) {                                                                     \
            std::fprintf(stderr, "%s:%d: CHECK failed: %s\n", __FILE__, __LINE__, #condition); \
            ++g_failures;                                                                       \
        }                                                                                       \
    } while (false)

using migo::windows_host::hostServiceBit;
using migo::windows_host::HostServiceProvider;
using migo::windows_host::HostServiceReply;
using migo::windows_host::Json;
using migo::windows_host::SurfaceState;
using migo::windows_host::Win32ManagedSession;

template <typename Done>
bool pump_until(Done done, int ms = 3000) {
    const auto deadline = std::chrono::steady_clock::now() + std::chrono::milliseconds(ms);
    while (!done()) {
        MSG message;
        while (PeekMessageW(&message, nullptr, 0, 0, PM_REMOVE)) {
            TranslateMessage(&message);
            DispatchMessageW(&message);
        }
        if (done()) return true;
        if (std::chrono::steady_clock::now() > deadline) return false;
        MsgWaitForMultipleObjects(0, nullptr, FALSE, 5, QS_ALLINPUT);
    }
    return true;
}

HWND make_frame() {
    static bool registered = false;
    if (!registered) {
        WNDCLASSEXW frame_class{};
        frame_class.cbSize = sizeof frame_class;
        frame_class.lpfnWndProc = DefWindowProcW;
        frame_class.hInstance = GetModuleHandleW(nullptr);
        frame_class.lpszClassName = L"MigoSessionTestFrame";
        RegisterClassExW(&frame_class);
        registered = true;
    }
    HWND frame = CreateWindowExW(0, L"MigoSessionTestFrame", L"session test", WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN,
                                 0, 0, 900, 700, nullptr, nullptr, GetModuleHandleW(nullptr), nullptr);
    ShowWindow(frame, SW_SHOWNORMAL);
    return frame;
}

std::optional<fake_migo::HostServiceAnswer> answer_to(std::uint64_t call_id) {
    for (const auto &answer : fake_migo::host_service_answers()) {
        if (answer.call_id == call_id) return answer;
    }
    return std::nullopt;
}

bool reported(const char *state) {
    const auto &reports = fake_migo::window_reports();
    return std::find(reports.begin(), reports.end(), state) != reports.end();
}

/// The App's clipboard, answering on the GUI thread.
class Clipboard final : public HostServiceProvider {
public:
    std::thread::id answered_on;
    std::uint64_t services() const noexcept override { return hostServiceBit(MIGO_HOST_SERVICE_CLIPBOARD); }
    void call(MigoHostService, std::uint32_t method, const Json &request, HostServiceReply reply) override {
        answered_on = std::this_thread::get_id();
        if (method == MIGO_CLIPBOARD_SET_CLIPBOARD_DATA) {
            data = request["data"].string();
            (void)reply.succeed();
        } else {
            (void)reply.succeed(Json::object().set("data", data));
        }
    }
    std::string data = "held";
};

/// A session in a frame, its view shown at 640 x 360; closed on the way out.
struct Running {
    HWND frame = make_frame();
    Clipboard clipboard;
    bool ready = false;
    std::thread::id ready_on;
    std::string failure;
    bool exit_asked = false;
    bool closed = false;
    std::optional<Win32ManagedSession> session;

    explicit Running(std::string app_id = {}) {
        Win32ManagedSession::Options options;
        options.appId = std::move(app_id);
        options.appServices = &clipboard;
        Win32ManagedSession::Listener listener;
        listener.contentReady = [this] {
            ready = true;
            ready_on = std::this_thread::get_id();
        };
        listener.contentFailed = [this](MigoResult, const std::string &message) { failure = message; };
        listener.exitRequested = [this] { exit_asked = true; };
        listener.closed = [this] { closed = true; };
        session.emplace(*fake_migo::engine(), frame, std::move(options), std::move(listener));
    }

    void show() {
        MoveWindow(session->view()->hwnd(), 0, 0, 640, 360, TRUE);
        ShowWindow(session->view()->hwnd(), SW_SHOW);
        CHECK(pump_until([this] { return session->view()->surfaceState() == SurfaceState::Attached; }));
    }

    ~Running() {
        if (session.has_value() && !session->isClosed()) {
            (void)session->close();
            if (session->view() != nullptr && session->view()->surfaceState() == SurfaceState::Retiring) {
                fake_migo::set_release_status(session->view()->generation(), MIGO_SURFACE_RELEASE_RELEASED);
            }
            CHECK(pump_until([this] { return closed; }));
        }
        session.reset();
        DestroyWindow(frame);
    }
};

void it_creates_the_session_and_installs_its_callbacks() {
    fake_migo::reset();
    Running running;
    CHECK(running.session->isValid());
    CHECK(fake_migo::calls().session_create == 1);
    CHECK(fake_migo::calls().set_callbacks == 1);
    const MigoHostCallbacks &installed = fake_migo::installed_callbacks();
    CHECK(installed.dispatch != nullptr && installed.on_ready != nullptr && installed.on_request_frame != nullptr);
    CHECK(installed.on_keep_screen_on != nullptr);
    // The pads' motors are the Host Kit's to drive.
    CHECK(installed.on_gamepad_rumble != nullptr);
    // The App's services, and the Host Kit's own.
    CHECK((installed.host_services & hostServiceBit(MIGO_HOST_SERVICE_CLIPBOARD)) != 0);
    CHECK((installed.host_services & hostServiceBit(MIGO_HOST_SERVICE_WINDOW)) != 0);
    // Not named: the executable's name.
    CHECK(fake_migo::reported_host_app_id() == "migo-windows-managed-session-test");
}

void an_app_id_given_is_the_one_reported() {
    fake_migo::reset();
    Running running("com.example.game");
    CHECK(fake_migo::reported_host_app_id() == "com.example.game");
}

void content_is_loaded_once_the_view_attaches() {
    fake_migo::reset();
    Running running;
    CHECK(running.session->loadContent("game-1") == MIGO_OK);
    CHECK(fake_migo::calls().load_content == 0);
    CHECK(running.session->loadContent("game-2") == MIGO_ERROR_INVALID_STATE);
    running.show();
    CHECK(fake_migo::calls().load_content == 1);
    CHECK(fake_migo::loaded_content_id() == "game-1");
}

void engine_callbacks_run_on_the_gui_thread() {
    fake_migo::reset();
    Running running;
    running.show();
    fake_migo::deliver_ready_from_engine_thread();
    fake_migo::deliver_error_from_engine_thread(MIGO_ERROR_INTERNAL, "boom");
    CHECK(pump_until([&running] { return running.ready && !running.failure.empty(); }));
    CHECK(running.ready_on == std::this_thread::get_id());
    CHECK(running.failure == "boom");
    // A frame asked for is one frame, paced by the view.
    fake_migo::deliver_frame_request_from_engine_thread();
    CHECK(pump_until([] { return !fake_migo::vsyncs().empty(); }));
}

void the_windows_state_is_the_games() {
    fake_migo::reset();
    Running running;
    running.show();
    ShowWindow(running.frame, SW_MINIMIZE);
    CHECK(pump_until([] { return reported("paused") && reported("hidden"); }));
    ShowWindow(running.frame, SW_RESTORE);
    CHECK(pump_until([] { return reported("running") && reported("shown"); }));
}

void the_apps_services_answer_on_the_gui_thread() {
    fake_migo::reset();
    Running running;
    running.show();
    fake_migo::deliver_host_service_call_from_engine_thread(MIGO_HOST_SERVICE_CLIPBOARD,
                                                            MIGO_CLIPBOARD_SET_CLIPBOARD_DATA, 0, "{\"data\":\"copied\"}");
    fake_migo::deliver_host_service_call_from_engine_thread(MIGO_HOST_SERVICE_CLIPBOARD,
                                                            MIGO_CLIPBOARD_GET_CLIPBOARD_DATA, 7, "{}");
    CHECK(pump_until([] { return answer_to(7).has_value(); }));
    CHECK(answer_to(7).has_value() && answer_to(7)->status == MIGO_HOST_SERVICE_STATUS_OK);
    CHECK(answer_to(7).has_value() && answer_to(7)->payload == "{\"data\":\"copied\"}");
    CHECK(running.clipboard.answered_on == std::this_thread::get_id());
}

void closing_retires_the_view_then_destroys_the_session() {
    fake_migo::reset();
    Running running;
    running.show();
    CHECK(running.session->close() == MIGO_OK);
    CHECK(running.session->close() == MIGO_OK);
    CHECK(!running.closed);
    CHECK(fake_migo::session_is_alive());
    // The door is shut: a task handed over now goes back to the engine.
    const MigoHostCallbacks &installed = fake_migo::installed_callbacks();
    CHECK(installed.dispatch(installed.dispatcher_data, [](void *) {}, nullptr) == MIGO_ERROR_INVALID_STATE);
    fake_migo::set_release_status(running.session->view()->generation(), MIGO_SURFACE_RELEASE_RELEASED);
    CHECK(pump_until([&running] { return running.closed; }));
    CHECK(!fake_migo::session_is_alive());
    CHECK(running.session->isClosed());
}

HWND keyboard_field(HWND owner) {
    HWND window = nullptr;
    while ((window = FindWindowExW(nullptr, window, L"MigoKeyboardField", nullptr)) != nullptr) {
        if (GetWindow(window, GW_OWNER) == owner) return window;
    }
    return nullptr;
}

std::wstring text_of(HWND window) {
    wchar_t text[256] = {};
    GetWindowTextW(window, text, 256);
    return text;
}

bool heard_keyboard(MigoKeyboardEventType type, const std::string &value) {
    for (const auto &event : fake_migo::keyboard_events()) {
        if (event.event_type == type && event.value == value) return true;
    }
    return false;
}

MigoKeyboardShowOptions keyboard_options(MigoKeyboardFlags flags, std::uint32_t max_length, MigoKeyboardType type) {
    MigoKeyboardShowOptions options{};
    options.struct_size = static_cast<std::uint32_t>(sizeof options);
    options.abi_version = MIGO_ABI_VERSION_1;
    options.flags = flags;
    options.max_length = max_length;
    options.keyboard_type = type;
    return options;
}

void show_keyboard_opens_a_field_over_the_game() {
    fake_migo::reset();
    Running running;
    running.show();
    fake_migo::deliver_show_keyboard_from_engine_thread(
        keyboard_options(MIGO_KEYBOARD_FLAG_NONE, 6, MIGO_KEYBOARD_TYPE_TEXT), "hi");
    HWND field = nullptr;
    CHECK(pump_until([&] { return (field = keyboard_field(running.frame)) != nullptr && IsWindowVisible(field); }));
    if (field == nullptr) return;
    const HWND editor = FindWindowExW(field, nullptr, L"Edit", nullptr);
    CHECK(editor != nullptr && text_of(editor) == L"hi");
    // Along the bottom of the game, its full width; content is told its height.
    RECT placed{};
    GetWindowRect(field, &placed);
    POINT corner{0, 360};
    ClientToScreen(running.session->view()->hwnd(), &corner);
    CHECK(placed.left == corner.x && placed.bottom == corner.y && placed.right - placed.left == 640);
    CHECK(!fake_migo::keyboard_events().empty() &&
          fake_migo::keyboard_events().front().event_type == MIGO_KEYBOARD_EVENT_HEIGHT_CHANGE &&
          fake_migo::keyboard_events().front().height_css_px > 0);
    if (editor == nullptr) return;
    // The player's typing is reported, whole; the limit holds.
    for (wchar_t typed : {L'!', L'?', L'a', L'b', L'c'}) SendMessageW(editor, WM_CHAR, typed, 1);
    CHECK(text_of(editor) == L"hi!?ab");
    CHECK(heard_keyboard(MIGO_KEYBOARD_EVENT_INPUT, "hi!"));
    CHECK(heard_keyboard(MIGO_KEYBOARD_EVENT_INPUT, "hi!?ab"));
    // Content's own correction is not reported back to it.
    const std::size_t heard = fake_migo::keyboard_events().size();
    fake_migo::deliver_update_keyboard_from_engine_thread("fixed");
    CHECK(pump_until([editor] { return text_of(editor) == L"fixed"; }));
    CHECK(fake_migo::keyboard_events().size() == heard);
    // Enter confirms, and the field goes with what it held.
    SendMessageW(editor, WM_KEYDOWN, VK_RETURN, 1);
    CHECK(heard_keyboard(MIGO_KEYBOARD_EVENT_CONFIRM, "fixed"));
    CHECK(heard_keyboard(MIGO_KEYBOARD_EVENT_COMPLETE, "fixed"));
    CHECK(!IsWindowVisible(field));
}

void escape_and_hide_keyboard_put_the_field_away() {
    fake_migo::reset();
    Running running;
    running.show();
    fake_migo::deliver_show_keyboard_from_engine_thread(
        keyboard_options(MIGO_KEYBOARD_FLAG_CONFIRM_HOLD, 0, MIGO_KEYBOARD_TYPE_NUMBER), "12");
    HWND field = nullptr;
    CHECK(pump_until([&] { return (field = keyboard_field(running.frame)) != nullptr && IsWindowVisible(field); }));
    if (field == nullptr) return;
    const HWND editor = FindWindowExW(field, nullptr, L"Edit", nullptr);
    CHECK(editor != nullptr && (GetWindowLongPtrW(editor, GWL_STYLE) & ES_NUMBER) != 0);
    if (editor == nullptr) return;
    // A confirm content holds keeps the field up.
    SendMessageW(editor, WM_KEYDOWN, VK_RETURN, 1);
    CHECK(heard_keyboard(MIGO_KEYBOARD_EVENT_CONFIRM, "12"));
    CHECK(IsWindowVisible(field));
    SendMessageW(editor, WM_KEYDOWN, VK_ESCAPE, 1);
    CHECK(heard_keyboard(MIGO_KEYBOARD_EVENT_COMPLETE, "12"));
    CHECK(!IsWindowVisible(field));

    fake_migo::deliver_show_keyboard_from_engine_thread(
        keyboard_options(MIGO_KEYBOARD_FLAG_MULTIPLE, 0, MIGO_KEYBOARD_TYPE_TEXT), "line");
    CHECK(pump_until([&] { return IsWindowVisible(field) != FALSE; }));
    fake_migo::deliver_hide_keyboard_from_engine_thread();
    CHECK(pump_until([&] { return !IsWindowVisible(field); }));
    CHECK(heard_keyboard(MIGO_KEYBOARD_EVENT_COMPLETE, "line"));
}

void the_machine_is_reported_as_windows_tells_it() {
    fake_migo::reset();
    Running running;
    // The network, at once: this machine's, through the Network List Manager.
    CHECK(!fake_migo::network_reports().empty());
    if (!fake_migo::network_reports().empty()) {
        const auto &network = fake_migo::network_reports().back();
        CHECK(network.type != MIGO_NETWORK_NONE || network.connected == 0);
    }
    // The battery, where there is one.
    SYSTEM_POWER_STATUS power{};
    GetSystemPowerStatus(&power);
    const bool has_battery = (power.BatteryFlag & BATTERY_FLAG_NO_BATTERY) == 0 && power.BatteryFlag != BATTERY_FLAG_UNKNOWN &&
                             power.BatteryLifePercent <= 100;
    CHECK(fake_migo::battery_reports().empty() == !has_battery);
    // What a desktop App is not gated on; the privacy settings decide the rest when asked.
    const MigoAppAuthorizeSetting &granted = fake_migo::reported_app_authorization();
    CHECK(granted.album == MIGO_AUTHORIZATION_AUTHORIZED && granted.notification == MIGO_AUTHORIZATION_AUTHORIZED);
    CHECK(granted.camera == MIGO_AUTHORIZATION_NOT_DETERMINED && granted.location == MIGO_AUTHORIZATION_NOT_DETERMINED);
}

void gamepads_are_announced_as_the_view_attaches() {
    fake_migo::reset();
    Running running;
    // Nothing to announce to before the Surface.
    CHECK(fake_migo::gamepad_announcements().empty());
    running.show();
    // This machine's pads, whichever it has: each announced connected, with a
    // layout the engine holds.
    for (const auto &announced : fake_migo::gamepad_announcements()) {
        CHECK(announced.connected);
        CHECK(announced.info.axis_count <= MIGO_GAMEPAD_MAX_AXES && announced.info.button_count <= MIGO_GAMEPAD_MAX_BUTTONS);
    }
    // A rumble for a pad there is not is nothing.
    MigoGamepadRumble rumble{};
    rumble.struct_size = static_cast<std::uint32_t>(sizeof rumble);
    rumble.abi_version = MIGO_ABI_VERSION_1;
    rumble.index = 99;
    rumble.duration_ms = 100;
    rumble.strong_magnitude = 1;
    fake_migo::installed_callbacks().on_gamepad_rumble(fake_migo::installed_callbacks().user_data,
                                                       fake_migo::session(), &rumble);
}

void a_session_never_shown_closes_at_once() {
    fake_migo::reset();
    Running running;
    CHECK(running.session->close() == MIGO_OK);
    CHECK(running.closed);
    CHECK(!fake_migo::session_is_alive());
}

}  // namespace

int main() {
    SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    SetCursorPos(GetSystemMetrics(SM_CXVIRTUALSCREEN) - 1, GetSystemMetrics(SM_CYVIRTUALSCREEN) - 1);
    it_creates_the_session_and_installs_its_callbacks();
    an_app_id_given_is_the_one_reported();
    content_is_loaded_once_the_view_attaches();
    engine_callbacks_run_on_the_gui_thread();
    the_windows_state_is_the_games();
    the_apps_services_answer_on_the_gui_thread();
    closing_retires_the_view_then_destroys_the_session();
    a_session_never_shown_closes_at_once();
    show_keyboard_opens_a_field_over_the_game();
    escape_and_hide_keyboard_put_the_field_away();
    the_machine_is_reported_as_windows_tells_it();
    gamepads_are_announced_as_the_view_attaches();
    if (g_failures != 0) {
        std::fprintf(stderr, "%d Win32ManagedSession assertion(s) failed\n", g_failures);
        return 1;
    }
    std::puts("Win32ManagedSession contract: PASS");
    return 0;
}
