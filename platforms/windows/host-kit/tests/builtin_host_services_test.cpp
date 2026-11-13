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
#include <cstdlib>
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
    CHECK((declared & hostServiceBit(MIGO_HOST_SERVICE_INTERACTION)) != 0);
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

/// A top-level window of `kind` the App's window owns, or null.
HWND find_owned(const wchar_t *kind, HWND owner) {
    HWND window = nullptr;
    while ((window = FindWindowExW(nullptr, window, kind, nullptr)) != nullptr) {
        if (GetWindow(window, GW_OWNER) == owner) return window;
    }
    return nullptr;
}

HWND find_panel(HWND owner) { return find_owned(L"MigoDebugPanel", owner); }

std::wstring text_of(HWND window) {
    wchar_t text[256] = {};
    GetWindowTextW(window, text, 256);
    return text;
}

/// Hands content's call to the service without waiting for its answer.
std::uint64_t deliver(MigoHostService service, std::uint32_t method, const Json &request) {
    const std::uint64_t call_id = g_next_call++;
    fake_migo::deliver_host_service_call_from_engine_thread(service, method, call_id, request.dump());
    return call_id;
}

/// The player clicks `button`, as BM_CLICK does once its dialog is active.
void click(HWND button) {
    SetActiveWindow(GetAncestor(button, GA_ROOT));
    SendMessageW(button, BM_CLICK, 0, 0);
}

/// What the screen shows at `point` (screen pixels), layered windows included.
COLORREF screen_at(POINT point) {
    HDC screen = GetDC(nullptr);
    HDC copy = CreateCompatibleDC(screen);
    HBITMAP pixel = CreateCompatibleBitmap(screen, 1, 1);
    HGDIOBJ previous = SelectObject(copy, pixel);
    BitBlt(copy, 0, 0, 1, 1, screen, point.x, point.y, SRCCOPY | CAPTUREBLT);
    const COLORREF colour = GetPixel(copy, 0, 0);
    SelectObject(copy, previous);
    DeleteObject(pixel);
    DeleteDC(copy);
    ReleaseDC(nullptr, screen);
    return colour;
}

void a_toast_is_a_card_over_the_game() {
    fake_migo::reset();
    Running running;
    tell(MIGO_HOST_SERVICE_INTERACTION, MIGO_INTERACTION_SHOW_TOAST,
         Json::object().set("title", "Saved").set("icon", "success").set("duration", 400));
    const HWND card = find_owned(L"MigoIndicator", running.frame);
    CHECK(card != nullptr);
    if (card != nullptr) {
        CHECK(IsWindowVisible(card));
        CHECK(text_of(card) == L"Saved");
        // Over the middle of the game, taking none of its input.
        RECT placed{};
        GetWindowRect(card, &placed);
        POINT middle{320, 180};
        ClientToScreen(running.session->view()->hwnd(), &middle);
        CHECK(std::abs((placed.left + placed.right) / 2 - middle.x) <= 1);
        CHECK(std::abs((placed.top + placed.bottom) / 2 - middle.y) <= 1);
        CHECK((GetWindowLongPtrW(card, GWL_EXSTYLE) & WS_EX_TRANSPARENT) != 0);
        // Drawn: the card's dark face, inside its padding.
        const COLORREF face = screen_at(POINT{placed.left + 6, (placed.top + placed.bottom) / 2});
        CHECK(GetRValue(face) < 80 && GetGValue(face) < 80 && GetBValue(face) < 80);
    }
    // Gone after its duration.
    CHECK(pump_until([&running] { return find_owned(L"MigoIndicator", running.frame) == nullptr; }, 2000));
}

void loading_and_toasts_take_turns() {
    fake_migo::reset();
    Running running;
    const auto shown = [&running]() -> std::wstring {
        const HWND card = find_owned(L"MigoIndicator", running.frame);
        return card != nullptr ? text_of(card) : std::wstring(L"<none>");
    };
    tell(MIGO_HOST_SERVICE_INTERACTION, MIGO_INTERACTION_SHOW_LOADING, Json::object().set("title", "Loading"));
    CHECK(shown() == L"Loading");
    // A loading indicator stays: hiding a toast leaves it.
    pump(200);
    tell(MIGO_HOST_SERVICE_INTERACTION, MIGO_INTERACTION_HIDE_TOAST, Json::object());
    CHECK(shown() == L"Loading");
    // One card at a time: a toast replaces it, and hiding loading leaves the toast.
    tell(MIGO_HOST_SERVICE_INTERACTION, MIGO_INTERACTION_SHOW_TOAST,
         Json::object().set("title", "Done").set("duration", 60000));
    CHECK(shown() == L"Done");
    tell(MIGO_HOST_SERVICE_INTERACTION, MIGO_INTERACTION_HIDE_LOADING, Json::object());
    CHECK(shown() == L"Done");
    tell(MIGO_HOST_SERVICE_INTERACTION, MIGO_INTERACTION_HIDE_TOAST, Json::object());
    CHECK(shown() == L"<none>");
}

void a_mask_keeps_the_player_from_the_game() {
    fake_migo::reset();
    Running running;
    const HWND view = running.session->view()->hwnd();
    const auto heard = [] { return fake_migo::touches().size() + fake_migo::pointers().size(); };
    tell(MIGO_HOST_SERVICE_INTERACTION, MIGO_INTERACTION_SHOW_LOADING,
         Json::object().set("title", "Wait").set("mask", true));
    const std::size_t before = heard();
    SendMessageW(view, WM_LBUTTONDOWN, MK_LBUTTON, MAKELPARAM(100, 100));
    SendMessageW(view, WM_LBUTTONUP, 0, MAKELPARAM(100, 100));
    CHECK(heard() == before);
    tell(MIGO_HOST_SERVICE_INTERACTION, MIGO_INTERACTION_HIDE_LOADING, Json::object());
    SendMessageW(view, WM_LBUTTONDOWN, MK_LBUTTON, MAKELPARAM(100, 100));
    SendMessageW(view, WM_LBUTTONUP, 0, MAKELPARAM(100, 100));
    CHECK(heard() > before);
}

void a_modal_is_modal_to_the_apps_window() {
    fake_migo::reset();
    Running running;
    const std::uint64_t call = deliver(MIGO_HOST_SERVICE_INTERACTION, MIGO_INTERACTION_SHOW_MODAL,
                                       Json::object().set("title", "Quit?").set("content", "Progress is saved.")
                                           .set("confirmText", "Yes"));
    HWND dialog = nullptr;
    CHECK(pump_until([&] { return (dialog = find_owned(L"#32770", running.frame)) != nullptr; }));
    if (dialog == nullptr) return;
    CHECK(!IsWindowEnabled(running.frame));
    CHECK(FindWindowExW(dialog, nullptr, L"Static", L"Quit?") != nullptr);
    CHECK(FindWindowExW(dialog, nullptr, L"Static", L"Progress is saved.") != nullptr);
    CHECK(FindWindowExW(dialog, nullptr, L"Button", L"\u53D6\u6D88") != nullptr);
    const HWND yes = FindWindowExW(dialog, nullptr, L"Button", L"Yes");
    CHECK(yes != nullptr);
    if (yes != nullptr) click(yes);
    CHECK(pump_until([call] { return answer_to(call).has_value(); }));
    CHECK(answered_ok(call));
    CHECK(answer_to(call).has_value() && answer_to(call)->payload == "{\"confirm\":true,\"cancel\":false}");
    CHECK(IsWindowEnabled(running.frame));
    CHECK(find_owned(L"#32770", running.frame) == nullptr);
}

void an_editable_modal_answers_what_was_typed() {
    fake_migo::reset();
    Running running;
    const std::uint64_t call = deliver(MIGO_HOST_SERVICE_INTERACTION, MIGO_INTERACTION_SHOW_MODAL,
                                       Json::object().set("title", "Name").set("content", "Player").set("editable", true)
                                           .set("placeholderText", "Your name"));
    HWND dialog = nullptr;
    CHECK(pump_until([&] { return (dialog = find_owned(L"#32770", running.frame)) != nullptr; }));
    if (dialog == nullptr) return;
    const HWND field = FindWindowExW(dialog, nullptr, L"Edit", nullptr);
    CHECK(field != nullptr && text_of(field) == L"Player");
    if (field != nullptr) {
        SetWindowTextW(field, L"Ada");
        // Enter, through the dialog's keyboard: the default button.
        PostMessageW(field, WM_KEYDOWN, VK_RETURN, 0);
    }
    CHECK(pump_until([call] { return answer_to(call).has_value(); }));
    CHECK(answer_to(call).has_value() &&
          answer_to(call)->payload == "{\"confirm\":true,\"cancel\":false,\"content\":\"Ada\"}");
}

void escape_declines_a_modal() {
    fake_migo::reset();
    Running running;
    const std::uint64_t call = deliver(MIGO_HOST_SERVICE_INTERACTION, MIGO_INTERACTION_SHOW_MODAL,
                                       Json::object().set("content", "Sure?").set("showCancel", false));
    HWND dialog = nullptr;
    CHECK(pump_until([&] { return (dialog = find_owned(L"#32770", running.frame)) != nullptr; }));
    if (dialog == nullptr) return;
    CHECK(FindWindowExW(dialog, nullptr, L"Button", L"\u53D6\u6D88") == nullptr);
    PostMessageW(FindWindowExW(dialog, nullptr, L"Button", L"\u786E\u5B9A"), WM_KEYDOWN, VK_ESCAPE, 0);
    CHECK(pump_until([call] { return answer_to(call).has_value(); }));
    CHECK(answer_to(call).has_value() && answer_to(call)->payload == "{\"confirm\":false,\"cancel\":true}");
    CHECK(IsWindowEnabled(running.frame));
}

void an_action_sheet_answers_the_item_tapped() {
    fake_migo::reset();
    Running running;
    Json items;
    items.push("Share").push("Save").push("Report");
    const std::uint64_t call = deliver(MIGO_HOST_SERVICE_INTERACTION, MIGO_INTERACTION_SHOW_ACTION_SHEET,
                                       Json::object().set("itemList", items).set("alertText", "Photo"));
    HWND dialog = nullptr;
    CHECK(pump_until([&] { return (dialog = find_owned(L"#32770", running.frame)) != nullptr; }));
    if (dialog == nullptr) return;
    CHECK(FindWindowExW(dialog, nullptr, L"Static", L"Photo") != nullptr);
    const HWND save = FindWindowExW(dialog, nullptr, L"Button", L"Save");
    CHECK(save != nullptr);
    if (save != nullptr) click(save);
    CHECK(pump_until([call] { return answer_to(call).has_value(); }));
    CHECK(answer_to(call).has_value() && answer_to(call)->payload == "{\"tapIndex\":1}");

    const std::uint64_t declined = deliver(MIGO_HOST_SERVICE_INTERACTION, MIGO_INTERACTION_SHOW_ACTION_SHEET,
                                           Json::object().set("itemList", items));
    CHECK(pump_until([&] { return (dialog = find_owned(L"#32770", running.frame)) != nullptr; }));
    if (dialog != nullptr) click(FindWindowExW(dialog, nullptr, L"Button", L"\u53D6\u6D88"));
    CHECK(pump_until([declined] { return answer_to(declined).has_value(); }));
    CHECK(failure_of(declined) == "cancel");

    CHECK(failure_of(ask(MIGO_HOST_SERVICE_INTERACTION, MIGO_INTERACTION_SHOW_ACTION_SHEET,
                         Json::object().set("itemList", Json(Json::Array{})))) == "itemList is empty");
}

void closing_takes_a_dialog_down_with_the_game() {
    fake_migo::reset();
    Running running;
    const std::uint64_t call =
        deliver(MIGO_HOST_SERVICE_INTERACTION, MIGO_INTERACTION_SHOW_MODAL, Json::object().set("content", "Still here?"));
    CHECK(pump_until([&running] { return find_owned(L"#32770", running.frame) != nullptr; }));
    (void)running.session->close();
    CHECK(find_owned(L"#32770", running.frame) == nullptr);
    CHECK(IsWindowEnabled(running.frame));
    // Declined, as the game went.
    CHECK(answer_to(call).has_value() && answer_to(call)->payload == "{\"confirm\":false,\"cancel\":true}");
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
    a_toast_is_a_card_over_the_game();
    loading_and_toasts_take_turns();
    a_mask_keeps_the_player_from_the_game();
    a_modal_is_modal_to_the_apps_window();
    an_editable_modal_answers_what_was_typed();
    escape_declines_a_modal();
    an_action_sheet_answers_the_item_tapped();
    closing_takes_a_dialog_down_with_the_game();
    if (g_failures != 0) {
        std::fprintf(stderr, "%d Win32BuiltInHostServices assertion(s) failed\n", g_failures);
        return 1;
    }
    std::puts("Win32BuiltInHostServices contract: PASS");
    return 0;
}
