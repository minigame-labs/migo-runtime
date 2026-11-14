// The Win32 view, driven as the system drives it: real windows on the runner's
// desktop, and the messages a mouse, a wheel and a keyboard send them, against
// the strict fake of the C ABI.

#include "fake_migo.hpp"

#include <migo/windows/win32_surface_view.hpp>

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
#include <cmath>
#include <cstdint>
#include <cstdio>
#include <string>
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

using migo::windows_host::SurfaceHost;
using migo::windows_host::SurfaceState;
using migo::windows_host::Win32SurfaceView;

/// Messages dispatched until `done`, or `ms` pass.
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

void pump() {
    (void)pump_until([] { return false; }, 50);
}

/// The App's window, which the view is a child of.
HWND make_frame() {
    static bool registered = false;
    if (!registered) {
        WNDCLASSEXW frame_class{};
        frame_class.cbSize = sizeof frame_class;
        frame_class.lpfnWndProc = DefWindowProcW;
        frame_class.hInstance = GetModuleHandleW(nullptr);
        frame_class.lpszClassName = L"MigoViewTestFrame";
        RegisterClassExW(&frame_class);
        registered = true;
    }
    HWND frame = CreateWindowExW(0, L"MigoViewTestFrame", L"view test", WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN, 0, 0,
                                 900, 700, nullptr, nullptr, GetModuleHandleW(nullptr), nullptr);
    ShowWindow(frame, SW_SHOWNORMAL);
    return frame;
}

/// A view shown in a frame at 640 x 360, attached.
struct Shown {
    HWND frame = make_frame();
    SurfaceHost host{fake_migo::session()};
    std::vector<std::uint64_t> attached;
    std::vector<std::uint64_t> released;
    Win32SurfaceView view{host, frame, listener()};

    Shown() {
        MoveWindow(view.hwnd(), 0, 0, 640, 360, TRUE);
        ShowWindow(view.hwnd(), SW_SHOW);
        CHECK(pump_until([this] { return view.surfaceState() == SurfaceState::Attached; }));
    }

    ~Shown() {
        if (view.surfaceState() == SurfaceState::Attached) {
            view.close();
            fake_migo::set_release_status(view.generation(), MIGO_SURFACE_RELEASE_RELEASED);
            CHECK(pump_until([this] { return view.hwnd() == nullptr; }));
        }
        DestroyWindow(frame);
    }

    Win32SurfaceView::Listener listener() {
        Win32SurfaceView::Listener heard;
        heard.attached = [this](std::uint64_t generation) { attached.push_back(generation); };
        heard.released = [this](std::uint64_t generation) { released.push_back(generation); };
        return heard;
    }

    LRESULT send(UINT message, WPARAM wparam, LPARAM lparam) { return SendMessageW(view.hwnd(), message, wparam, lparam); }
};

bool close_to(float actual, float expected) { return std::fabs(actual - expected) < 0.001F; }

void it_attaches_once_shown_at_a_size() {
    fake_migo::reset();
    Shown shown;
    CHECK(fake_migo::calls().attach == 1);
    CHECK(shown.attached == std::vector<std::uint64_t>{1});
    CHECK(fake_migo::last_surface().platform_kind == MIGO_PLATFORM_WIN32_HWND);
    CHECK(fake_migo::last_surface().width_pixels == 640);
    CHECK(fake_migo::last_surface().height_pixels == 360);
    CHECK(close_to(fake_migo::last_surface().scale_factor,
               static_cast<float>(GetDpiForWindow(shown.view.hwnd())) / 96.0F));
    CHECK(fake_migo::last_win32().hwnd == shown.view.hwnd());
}

void it_attaches_once_its_window_is_shown_after_it() {
    fake_migo::reset();
    // The order an App most often shows them in: the view, then its window.
    HWND frame = CreateWindowExW(0, L"MigoViewTestFrame", L"view test", WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN, 0, 0,
                                 900, 700, nullptr, nullptr, GetModuleHandleW(nullptr), nullptr);
    SurfaceHost host{fake_migo::session()};
    {
        Win32SurfaceView view{host, frame, Win32SurfaceView::Listener()};
        MoveWindow(view.hwnd(), 0, 0, 640, 360, TRUE);
        ShowWindow(view.hwnd(), SW_SHOW);
        pump();
        // Not on screen yet: nothing to bind.
        CHECK(view.surfaceState() == SurfaceState::Detached);
        ShowWindow(frame, SW_SHOWNORMAL);
        CHECK(pump_until([&view] { return view.surfaceState() == SurfaceState::Attached; }));
        CHECK(fake_migo::calls().attach == 1);
        view.close();
        fake_migo::set_release_status(view.generation(), MIGO_SURFACE_RELEASE_RELEASED);
        CHECK(pump_until([&view] { return view.hwnd() == nullptr; }));
    }
    DestroyWindow(frame);
}

void a_resize_updates_the_surface_once() {
    fake_migo::reset();
    Shown shown;
    const int updates = fake_migo::calls().update;
    MoveWindow(shown.view.hwnd(), 0, 0, 700, 400, TRUE);
    MoveWindow(shown.view.hwnd(), 0, 0, 720, 405, TRUE);
    MoveWindow(shown.view.hwnd(), 0, 0, 800, 450, TRUE);
    CHECK(pump_until([updates] { return fake_migo::calls().update > updates; }));
    pump();
    CHECK(fake_migo::calls().update == updates + 1);
    CHECK(fake_migo::last_metrics().width_pixels == 800);
    CHECK(fake_migo::last_metrics().height_pixels == 450);
    CHECK(fake_migo::last_metrics().generation == 1);
}

/// The runner's own cursor kept off the windows under test: one resting over a
/// window as it appears makes the system send it a move of its own.
void cursor_away() {
    SetCursorPos(GetSystemMetrics(SM_CXVIRTUALSCREEN) - 1, GetSystemMetrics(SM_CYVIRTUALSCREEN) - 1);
}

void the_mouse_is_a_touch_while_pressed_and_a_pointer_always() {
    fake_migo::reset();
    cursor_away();
    Shown shown;
    pump();
    const float scale = shown.view.scale();
    const std::size_t pointers_before = fake_migo::pointers().size();
    const std::size_t touches_before = fake_migo::touches().size();
    shown.send(WM_LBUTTONDOWN, MK_LBUTTON, MAKELPARAM(10, 20));
    shown.send(WM_MOUSEMOVE, MK_LBUTTON, MAKELPARAM(30, 40));
    shown.send(WM_LBUTTONUP, 0, MAKELPARAM(30, 40));
    shown.send(WM_MOUSEMOVE, 0, MAKELPARAM(50, 60));
    const std::vector<fake_migo::PointerRecord> pointers(fake_migo::pointers().begin() + pointers_before,
                                                         fake_migo::pointers().end());
    CHECK(pointers.size() == 4);
    if (pointers.size() == 4) {
        CHECK(pointers[0].event_type == MIGO_POINTER_EVENT_DOWN && pointers[0].button == 0);
        CHECK(close_to(pointers[0].x, 10.0F / scale) && close_to(pointers[0].y, 20.0F / scale));
        CHECK(pointers[1].event_type == MIGO_POINTER_EVENT_MOVE && pointers[1].button == 0);
        CHECK(pointers[2].event_type == MIGO_POINTER_EVENT_UP);
        CHECK(pointers[3].event_type == MIGO_POINTER_EVENT_MOVE && close_to(pointers[3].x, 50.0F / scale));
    }
    const std::vector<fake_migo::TouchRecord> touches(fake_migo::touches().begin() + touches_before,
                                                      fake_migo::touches().end());
    CHECK(touches.size() == 3);
    if (touches.size() == 3) {
        CHECK(touches[0].type == MIGO_TOUCH_START && touches[0].points.size() == 1);
        CHECK(touches[1].type == MIGO_TOUCH_MOVE && close_to(touches[1].points[0].x, 30.0F / scale));
        CHECK(touches[2].type == MIGO_TOUCH_END);
        CHECK((touches[2].points[0].flags & MIGO_TOUCH_FLAG_REMOVED) != 0);
    }

    // The right button is the DOM's 2; content listening for one stream hears only it.
    shown.view.setPointerDelivery(Win32SurfaceView::PointerDelivery::MouseOnly);
    shown.send(WM_RBUTTONDOWN, MK_RBUTTON, MAKELPARAM(5, 5));
    shown.send(WM_RBUTTONUP, 0, MAKELPARAM(5, 5));
    CHECK(fake_migo::touches().size() == touches_before + 3);
    CHECK(fake_migo::pointers().size() == pointers_before + 6);
    CHECK(fake_migo::pointers().size() == pointers_before + 6 && fake_migo::pointers()[pointers_before + 4].button == 2);
    shown.view.setPointerDelivery(Win32SurfaceView::PointerDelivery::TouchOnly);
    shown.send(WM_LBUTTONDOWN, MK_LBUTTON, MAKELPARAM(5, 5));
    shown.send(WM_LBUTTONUP, 0, MAKELPARAM(5, 5));
    CHECK(fake_migo::pointers().size() == pointers_before + 6);
    CHECK(fake_migo::touches().size() == touches_before + 5);
}

void the_wheel_is_three_lines_a_notch() {
    fake_migo::reset();
    Shown shown;
    shown.send(WM_MOUSEWHEEL, MAKEWPARAM(0, WHEEL_DELTA), 0);
    shown.send(WM_MOUSEHWHEEL, MAKEWPARAM(0, WHEEL_DELTA / 2), 0);
    const auto &wheels = fake_migo::wheels();
    CHECK(wheels.size() == 2);
    if (wheels.size() == 2) {
        CHECK(wheels[0].delta_mode == MIGO_WHEEL_DELTA_MODE_LINE);
        CHECK(wheels[0].delta_y == -3.0 && wheels[0].delta_x == 0.0);
        CHECK(wheels[1].delta_x == 1.5 && wheels[1].delta_y == 0.0);
    }
}

LPARAM key_lparam(std::uint32_t scan, bool extended, bool repeat, bool up) {
    LPARAM value = 1 | (static_cast<LPARAM>(scan) << 16);
    if (extended) value |= LPARAM{1} << 24;
    if (repeat || up) value |= LPARAM{1} << 30;
    if (up) value |= LPARAM{1} << 31;
    return value;
}

void keys_are_named_as_the_dom_names_them() {
    fake_migo::reset();
    Shown shown;
    shown.send(WM_KEYDOWN, 'A', key_lparam(0x1E, false, false, false));
    shown.send(WM_KEYDOWN, 'A', key_lparam(0x1E, false, true, false));
    shown.send(WM_KEYUP, 'A', key_lparam(0x1E, false, false, true));
    shown.send(WM_KEYDOWN, VK_LEFT, key_lparam(0x4B, true, false, false));
    shown.send(WM_KEYDOWN, VK_RETURN, key_lparam(0x1C, true, false, false));
    shown.send(WM_KEYDOWN, VK_F5, key_lparam(0x3F, false, false, false));
    shown.send(WM_KEYDOWN, VK_LSHIFT, key_lparam(0x2A, false, false, false));
    const auto &keys = fake_migo::keys();
    CHECK(keys.size() == 7);
    if (keys.size() == 7) {
        // The runner's layout is US English: the key that types "a" there.
        CHECK(keys[0].event_type == MIGO_KEY_EVENT_DOWN && keys[0].key == "a" && keys[0].code == "KeyA");
        CHECK(keys[0].flags == MIGO_KEY_EVENT_FLAG_NONE);
        CHECK(keys[1].flags == MIGO_KEY_EVENT_FLAG_REPEAT);
        CHECK(keys[2].event_type == MIGO_KEY_EVENT_UP && keys[2].code == "KeyA");
        CHECK(keys[3].key == "ArrowLeft" && keys[3].code == "ArrowLeft");
        CHECK(keys[4].key == "Enter" && keys[4].code == "NumpadEnter");
        CHECK(keys[5].key == "F5" && keys[5].code == "F5");
        CHECK(keys[6].key == "Shift" && keys[6].code == "ShiftLeft");
    }
}

void focus_is_reported_and_its_loss_ends_a_press() {
    fake_migo::reset();
    Shown shown;
    SetFocus(shown.view.hwnd());
    pump();
    CHECK(!fake_migo::focus_changes().empty() && fake_migo::focus_changes().back() == 1);
    shown.send(WM_LBUTTONDOWN, MK_LBUTTON, MAKELPARAM(10, 10));
    SetFocus(shown.frame);
    pump();
    CHECK(fake_migo::focus_changes().back() == 0);
    CHECK(!fake_migo::touches().empty() && fake_migo::touches().back().type == MIGO_TOUCH_CANCEL);
}

void a_frame_request_is_one_frame() {
    fake_migo::reset();
    Shown shown;
    CHECK(shown.view.requestFrame() == MIGO_OK);
    CHECK(shown.view.requestFrame() == MIGO_OK);
    CHECK(shown.view.isFramePending());
    CHECK(pump_until([] { return !fake_migo::vsyncs().empty(); }));
    pump();
    CHECK(fake_migo::vsyncs().size() == 1);
    CHECK(shown.view.framesDelivered() == 1);
    CHECK(shown.view.requestFrame() == MIGO_OK);
    CHECK(pump_until([] { return fake_migo::vsyncs().size() == 2; }));
    CHECK(fake_migo::vsyncs()[1] > fake_migo::vsyncs()[0]);
}

void a_detach_is_released_and_a_close_destroys_the_window() {
    fake_migo::reset();
    Shown shown;
    CHECK(shown.view.beginDetach() == MIGO_OK);
    CHECK(shown.view.surfaceState() == SurfaceState::Retiring);
    // The library says, of the generation retiring, that the driver still holds it.
    fake_migo::set_release_status(shown.view.generation(), MIGO_SURFACE_RELEASE_PENDING);
    pump();
    CHECK(shown.released.empty());
    fake_migo::set_release_status(shown.view.generation(), MIGO_SURFACE_RELEASE_RELEASED);
    CHECK(pump_until([&shown] { return !shown.released.empty(); }));
    CHECK(shown.released == std::vector<std::uint64_t>{1});
    CHECK(shown.view.surfaceState() == SurfaceState::Detached);
    CHECK(IsWindow(shown.view.hwnd()));

    // Attached again, then closed: detached, released, and the window gone.
    CHECK(shown.view.attachSurface() == MIGO_OK);
    CHECK(shown.view.generation() == 2);
    const HWND window = shown.view.hwnd();
    shown.view.close();
    fake_migo::set_release_status(shown.view.generation(), MIGO_SURFACE_RELEASE_RELEASED);
    CHECK(pump_until([&shown] { return shown.view.hwnd() == nullptr; }));
    CHECK(!IsWindow(window));
}

void a_lock_needs_the_player_at_the_game() {
    fake_migo::reset();
    Shown shown;
    // Not holding the keyboard: refused.
    SetFocus(shown.frame);
    pump();
    CHECK(!shown.view.lockPointer());
    CHECK(!shown.view.isPointerLocked());
    // Esc without a lock is the game's.
    shown.send(WM_KEYDOWN, VK_ESCAPE, key_lparam(0x01, false, false, false));
    CHECK(!fake_migo::keys().empty() && fake_migo::keys().back().key == "Escape");
}

}  // namespace

int main() {
    SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    cursor_away();
    it_attaches_once_shown_at_a_size();
    it_attaches_once_its_window_is_shown_after_it();
    a_resize_updates_the_surface_once();
    the_mouse_is_a_touch_while_pressed_and_a_pointer_always();
    the_wheel_is_three_lines_a_notch();
    keys_are_named_as_the_dom_names_them();
    focus_is_reported_and_its_loss_ends_a_press();
    a_frame_request_is_one_frame();
    a_detach_is_released_and_a_close_destroys_the_window();
    a_lock_needs_the_player_at_the_game();
    if (g_failures != 0) {
        std::fprintf(stderr, "%d Win32SurfaceView assertion(s) failed\n", g_failures);
        return 1;
    }
    std::puts("Win32SurfaceView contract: PASS");
    return 0;
}
