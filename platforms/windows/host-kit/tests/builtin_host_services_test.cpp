// The Host Kit's built-in services, through the Managed session, against the
// strict fake of the C ABI: the clipboard, the window, the screen, the scopes a
// desktop decides, and the debugging aids.

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

#include <chrono>
#include <cstdio>
#include <optional>
#include <string>

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

void pump(int ms) { (void)pump_until([] { return false; }, ms); }

/// The App's window: its one child, the game's view, fills it.
LRESULT CALLBACK frame_procedure(HWND window, UINT message, WPARAM wparam, LPARAM lparam) {
    if (message == WM_SIZE) {
        if (HWND view = GetWindow(window, GW_CHILD); view != nullptr) {
            MoveWindow(view, 0, 0, LOWORD(lparam), HIWORD(lparam), TRUE);
        }
    }
    return DefWindowProcW(window, message, wparam, lparam);
}

HWND make_frame() {
    static bool registered = false;
    if (!registered) {
        WNDCLASSEXW frame_class{};
        frame_class.cbSize = sizeof frame_class;
        frame_class.lpfnWndProc = frame_procedure;
        frame_class.hInstance = GetModuleHandleW(nullptr);
        frame_class.lpszClassName = L"MigoBuiltInTestFrame";
        RegisterClassExW(&frame_class);
        registered = true;
    }
    return CreateWindowExW(0, L"MigoBuiltInTestFrame", L"built-in services test", WS_OVERLAPPEDWINDOW, 40, 40, 700,
                           500, nullptr, nullptr, GetModuleHandleW(nullptr), nullptr);
}

SIZE client_of(HWND window) {
    RECT client{};
    GetClientRect(window, &client);
    return SIZE{client.right, client.bottom};
}

std::optional<fake_migo::HostServiceAnswer> answer_to(std::uint64_t call_id) {
    for (const auto &answer : fake_migo::host_service_answers()) {
        if (answer.call_id == call_id) return answer;
    }
    return std::nullopt;
}

bool answered_ok(std::uint64_t call_id) {
    const auto answer = answer_to(call_id);
    return answer.has_value() && answer->status == MIGO_HOST_SERVICE_STATUS_OK;
}

std::string failure_of(std::uint64_t call_id) {
    const auto answer = answer_to(call_id);
    return answer.has_value() && answer->status != MIGO_HOST_SERVICE_STATUS_OK ? answer->message : std::string();
}

/// Whether the service posted `event` with exactly `payload`.
bool posted(MigoHostService service, std::uint32_t event, const std::string &payload) {
    for (const auto &record : fake_migo::host_service_events()) {
        if (record.service == service && record.event == event && record.payload == payload) return true;
    }
    return false;
}

std::uint64_t g_next_call = 1;

/// Asks `service` as content does and waits for the answer; its call id.
std::uint64_t ask(MigoHostService service, std::uint32_t method, const Json &request) {
    const std::uint64_t call_id = g_next_call++;
    fake_migo::deliver_host_service_call_from_engine_thread(service, method, call_id, request.dump());
    CHECK(pump_until([call_id] { return answer_to(call_id).has_value(); }));
    return call_id;
}

/// Tells `service` as content does a command, which has no answer.
void tell(MigoHostService service, std::uint32_t method, const Json &request) {
    fake_migo::deliver_host_service_call_from_engine_thread(service, method, 0, request.dump());
    pump(50);
}

/// A game in a window, its view filling it at 640 x 360.
struct Running {
    HWND frame = make_frame();
    bool closed = false;
    std::optional<bool> aids_changed;
    std::optional<Win32ManagedSession> session;

    Running() {
        Win32ManagedSession::Listener listener;
        listener.closed = [this] { closed = true; };
        listener.debugAidsChanged = [this](bool on) { aids_changed = on; };
        session.emplace(*fake_migo::engine(), frame, Win32ManagedSession::Options(), std::move(listener));
        // The frame's client area at 640 x 360, which its view fills.
        RECT outer{};
        GetWindowRect(frame, &outer);
        const SIZE client = client_of(frame);
        SetWindowPos(frame, nullptr, 0, 0, (outer.right - outer.left) - client.cx + 640,
                     (outer.bottom - outer.top) - client.cy + 360, SWP_NOMOVE | SWP_NOZORDER);
        ShowWindow(frame, SW_SHOWNORMAL);
        ShowWindow(session->view()->hwnd(), SW_SHOW);
        CHECK(pump_until([this] { return session->view()->surfaceState() == SurfaceState::Attached; }));
        CHECK(client_of(session->view()->hwnd()).cx == 640 && client_of(session->view()->hwnd()).cy == 360);
    }

    [[nodiscard]] SIZE game() const { return client_of(session->view()->hwnd()); }

    ~Running() {
        (void)session->close();
        if (session->view()->surfaceState() == SurfaceState::Retiring) {
            fake_migo::set_release_status(session->view()->generation(), MIGO_SURFACE_RELEASE_RELEASED);
        }
        CHECK(pump_until([this] { return closed; }));
        session.reset();
        DestroyWindow(frame);
    }
};

void it_declares_what_it_answers() {
    fake_migo::reset();
    Running running;
    const std::uint64_t declared = fake_migo::installed_callbacks().host_services;
    for (MigoHostService service : {MIGO_HOST_SERVICE_CLIPBOARD, MIGO_HOST_SERVICE_WINDOW, MIGO_HOST_SERVICE_SCREEN,
                                    MIGO_HOST_SERVICE_PERMISSION}) {
        CHECK((declared & hostServiceBit(service)) != 0);
    }
    CHECK(fake_migo::installed_callbacks().on_host_service_call != nullptr);
    // A method the Host Kit has no answer for is refused, not left waiting.
    CHECK(failure_of(ask(MIGO_HOST_SERVICE_SCREEN, MIGO_SCREEN_GET_RECORDING_STATE, Json::object())) ==
          "not supported");
}

void the_clipboard_is_the_systems() {
    fake_migo::reset();
    Running running;
    const std::string text = "copied \xE5\xA4\x8D\xE5\x88\xB6 \xE2\x9C\x93";
    CHECK(answered_ok(ask(MIGO_HOST_SERVICE_CLIPBOARD, MIGO_CLIPBOARD_SET_CLIPBOARD_DATA,
                          Json::object().set("data", text))));
    // On the system's clipboard, as text another App reads.
    std::wstring held;
    if (OpenClipboard(nullptr)) {
        if (HANDLE data = GetClipboardData(CF_UNICODETEXT); data != nullptr) {
            if (const auto *chars = static_cast<const wchar_t *>(GlobalLock(data)); chars != nullptr) {
                held = chars;
                GlobalUnlock(data);
            }
        }
        CloseClipboard();
    }
    CHECK(held == L"copied 复制 ✓");
    const std::uint64_t read = ask(MIGO_HOST_SERVICE_CLIPBOARD, MIGO_CLIPBOARD_GET_CLIPBOARD_DATA, Json::object());
    CHECK(answered_ok(read));
    CHECK(answer_to(read).has_value() && answer_to(read)->payload == Json::object().set("data", text).dump());
}

void set_window_size_sizes_the_game() {
    fake_migo::reset();
    Running running;
    CHECK(answered_ok(ask(MIGO_HOST_SERVICE_WINDOW, MIGO_WINDOW_SET_WINDOW_SIZE,
                          Json::object().set("width", 800).set("height", 600))));
    CHECK(running.game().cx == 800 && running.game().cy == 600);
    CHECK(!posted(MIGO_HOST_SERVICE_SCREEN, MIGO_SCREEN_EVENT_DEVICE_ORIENTATION_CHANGE, "{\"value\":\"portrait\"}"));
    // Taller than wide: the screen has turned, as content sees it.
    CHECK(answered_ok(ask(MIGO_HOST_SERVICE_WINDOW, MIGO_WINDOW_SET_WINDOW_SIZE,
                          Json::object().set("width", 400).set("height", 640))));
    CHECK(running.game().cx == 400 && running.game().cy == 640);
    CHECK(posted(MIGO_HOST_SERVICE_SCREEN, MIGO_SCREEN_EVENT_DEVICE_ORIENTATION_CHANGE, "{\"value\":\"portrait\"}"));
    CHECK(failure_of(ask(MIGO_HOST_SERVICE_WINDOW, MIGO_WINDOW_SET_WINDOW_SIZE,
                         Json::object().set("width", -1).set("height", 600))) ==
          "width and height must be positive numbers");
}

void set_device_orientation_turns_the_window() {
    fake_migo::reset();
    Running running;
    CHECK(answered_ok(ask(MIGO_HOST_SERVICE_SCREEN, MIGO_SCREEN_SET_DEVICE_ORIENTATION,
                          Json::object().set("value", "portrait"))));
    CHECK(running.game().cx == 360 && running.game().cy == 640);
    CHECK(posted(MIGO_HOST_SERVICE_SCREEN, MIGO_SCREEN_EVENT_DEVICE_ORIENTATION_CHANGE, "{\"value\":\"portrait\"}"));
    // Already so: nothing to turn.
    CHECK(answered_ok(ask(MIGO_HOST_SERVICE_SCREEN, MIGO_SCREEN_SET_DEVICE_ORIENTATION,
                          Json::object().set("value", "portrait"))));
    CHECK(running.game().cx == 360 && running.game().cy == 640);
    CHECK(answered_ok(ask(MIGO_HOST_SERVICE_SCREEN, MIGO_SCREEN_SET_DEVICE_ORIENTATION,
                          Json::object().set("value", "landscape"))));
    CHECK(running.game().cx == 640 && running.game().cy == 360);
    CHECK(posted(MIGO_HOST_SERVICE_SCREEN, MIGO_SCREEN_EVENT_DEVICE_ORIENTATION_CHANGE, "{\"value\":\"landscape\"}"));
    CHECK(failure_of(ask(MIGO_HOST_SERVICE_SCREEN, MIGO_SCREEN_SET_DEVICE_ORIENTATION,
                         Json::object().set("value", "upside-down"))) == "value must be portrait or landscape");
}

void the_windows_state_is_reported() {
    fake_migo::reset();
    Running running;
    ShowWindow(running.frame, SW_MAXIMIZE);
    CHECK(pump_until([] {
        return posted(MIGO_HOST_SERVICE_WINDOW, MIGO_WINDOW_EVENT_WINDOW_STATE_CHANGE, "{\"state\":\"maximize\"}");
    }));
    // Maximized, the window is the screen's to size.
    CHECK(failure_of(ask(MIGO_HOST_SERVICE_WINDOW, MIGO_WINDOW_SET_WINDOW_SIZE,
                         Json::object().set("width", 300).set("height", 300))) == "the window is maximized");
    ShowWindow(running.frame, SW_RESTORE);
    CHECK(pump_until([] {
        return posted(MIGO_HOST_SERVICE_WINDOW, MIGO_WINDOW_EVENT_WINDOW_STATE_CHANGE, "{\"state\":\"normalize\"}");
    }));
    ShowWindow(running.frame, SW_MINIMIZE);
    CHECK(pump_until([] {
        return posted(MIGO_HOST_SERVICE_WINDOW, MIGO_WINDOW_EVENT_WINDOW_STATE_CHANGE, "{\"state\":\"minimize\"}");
    }));
    ShowWindow(running.frame, SW_RESTORE);
    pump(50);
}

void a_pointer_lock_refused_is_said_to_be() {
    fake_migo::reset();
    Running running;
    // The cursor is not over the game: no lock, and content hears so.
    tell(MIGO_HOST_SERVICE_WINDOW, MIGO_WINDOW_REQUEST_POINTER_LOCK, Json::object());
    CHECK(posted(MIGO_HOST_SERVICE_WINDOW, MIGO_WINDOW_EVENT_POINTER_LOCK_CHANGE, "{\"locked\":false}"));
    CHECK(!running.session->view()->isPointerLocked());
}

void the_window_can_be_kept_out_of_captures() {
    fake_migo::reset();
    Running running;
    CHECK(answered_ok(ask(MIGO_HOST_SERVICE_SCREEN, MIGO_SCREEN_SET_VISUAL_EFFECT_ON_CAPTURE,
                          Json::object().set("visualEffect", "hidden"))));
    DWORD affinity = 0;
    CHECK(GetWindowDisplayAffinity(running.frame, &affinity) && affinity == 0x11);
    CHECK(answered_ok(ask(MIGO_HOST_SERVICE_SCREEN, MIGO_SCREEN_SET_VISUAL_EFFECT_ON_CAPTURE,
                          Json::object().set("visualEffect", "none"))));
    CHECK(GetWindowDisplayAffinity(running.frame, &affinity) && affinity == WDA_NONE);
    CHECK(answered_ok(ask(MIGO_HOST_SERVICE_SCREEN, MIGO_SCREEN_SET_VISUAL_EFFECT_ON_CAPTURE,
                          Json::object().set("visualEffect", "hidden"))));
    // Closing gives the window back as it was.
    (void)running.session->close();
    CHECK(GetWindowDisplayAffinity(running.frame, &affinity) && affinity == WDA_NONE);
}

void the_desktop_decides_its_scopes() {
    fake_migo::reset();
    Running running;
    CHECK(answered_ok(ask(MIGO_HOST_SERVICE_PERMISSION, MIGO_PERMISSION_REQUEST_SCOPE,
                          Json::object().set("scope", "scope.writePhotosAlbum").set("scopeId", MIGO_SCOPE_WRITE_PHOTOS_ALBUM))));
    CHECK(fake_migo::scope_state(MIGO_SCOPE_WRITE_PHOTOS_ALBUM) == MIGO_SCOPE_STATE_GRANTED);
    CHECK(failure_of(ask(MIGO_HOST_SERVICE_PERMISSION, MIGO_PERMISSION_REQUEST_SCOPE,
                         Json::object().set("scope", "scope.camera").set("scopeId", MIGO_SCOPE_CAMERA))) == "auth deny");
    CHECK(fake_migo::scope_state(MIGO_SCOPE_CAMERA) == MIGO_SCOPE_STATE_UNKNOWN);
}

HWND find_panel(HWND owner) {
    HWND panel = nullptr;
    while ((panel = FindWindowExW(nullptr, panel, L"MigoDebugPanel", nullptr)) != nullptr) {
        if (GetWindow(panel, GW_OWNER) == owner) return panel;
    }
    return nullptr;
}

void set_enable_debug_turns_the_aids() {
    fake_migo::reset();
    Running running;
    tell(MIGO_HOST_SERVICE_SCREEN, MIGO_SCREEN_SET_ENABLE_DEBUG, Json::object().set("enableDebug", true));
    CHECK(running.session->debugAids());
    CHECK(running.aids_changed == std::optional<bool>(true));
    CHECK((fake_migo::reported_host_app_flags() & MIGO_HOST_APP_INFO_FLAG_DEBUG) != 0);
    // The panel: over the top 40% of the game, taking no input.
    const HWND panel = find_panel(running.frame);
    CHECK(panel != nullptr);
    if (panel != nullptr) {
        CHECK(IsWindowVisible(panel));
        RECT placed{};
        GetWindowRect(panel, &placed);
        POINT origin{0, 0};
        ClientToScreen(running.session->view()->hwnd(), &origin);
        CHECK(placed.left == origin.x && placed.top == origin.y);
        CHECK(placed.right - placed.left == 640 && placed.bottom - placed.top == 144);
        const LONG_PTR style = GetWindowLongPtrW(panel, GWL_EXSTYLE);
        CHECK((style & WS_EX_TRANSPARENT) != 0 && (style & WS_EX_NOACTIVATE) != 0);
        // It follows the game.
        SetWindowPos(running.frame, nullptr, 120, 90, 0, 0, SWP_NOSIZE | SWP_NOZORDER);
        pump(50);
        GetWindowRect(panel, &placed);
        origin = POINT{0, 0};
        ClientToScreen(running.session->view()->hwnd(), &origin);
        CHECK(placed.left == origin.x && placed.top == origin.y);
    }
    fake_migo::write_console(1, "hello from the game");
    pump(600);
    tell(MIGO_HOST_SERVICE_SCREEN, MIGO_SCREEN_SET_ENABLE_DEBUG, Json::object().set("enableDebug", false));
    CHECK(!running.session->debugAids());
    CHECK(running.aids_changed == std::optional<bool>(false));
    CHECK((fake_migo::reported_host_app_flags() & MIGO_HOST_APP_INFO_FLAG_DEBUG) == 0);
    CHECK(find_panel(running.frame) == nullptr);
}

}  // namespace

int main() {
    SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    SetCursorPos(GetSystemMetrics(SM_CXVIRTUALSCREEN) - 1, GetSystemMetrics(SM_CYVIRTUALSCREEN) - 1);
    it_declares_what_it_answers();
    the_clipboard_is_the_systems();
    set_window_size_sizes_the_game();
    set_device_orientation_turns_the_window();
    the_windows_state_is_reported();
    a_pointer_lock_refused_is_said_to_be();
    the_window_can_be_kept_out_of_captures();
    the_desktop_decides_its_scopes();
    set_enable_debug_turns_the_aids();
    if (g_failures != 0) {
        std::fprintf(stderr, "%d Win32BuiltInHostServices assertion(s) failed\n", g_failures);
        return 1;
    }
    std::puts("Win32BuiltInHostServices contract: PASS");
    return 0;
}
