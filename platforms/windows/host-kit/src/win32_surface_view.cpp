#include <migo/windows/win32_surface_view.hpp>

#include "dom_keys.hpp"

#ifndef WIN32_LEAN_AND_MEAN
#define WIN32_LEAN_AND_MEAN
#endif
#ifndef NOMINMAX
#define NOMINMAX
#endif
// The wide forms of the system's names (IDC_ARROW among them).
#ifndef UNICODE
#define UNICODE
#endif
#include <windows.h>
#include <commctrl.h>
#include <dwmapi.h>
#include <imm.h>
#include <windowsx.h>

#include <array>
#include <chrono>
#include <cmath>
#include <condition_variable>
#include <cstdio>
#include <cstring>
#include <exception>
#include <mutex>
#include <string>
#include <thread>
#include <utility>

namespace migo::windows_host {

static_assert(MIGO_ABI_VERSION_CURRENT == MIGO_ABI_VERSION_1,
              "Win32SurfaceView must be reviewed before targeting a new Migo C ABI");

namespace {

// The release is polled fast at first -- most drivers let go within a frame or
// two -- then at a frame's pace, then slowly once it has stalled.
constexpr UINT kFastReleasePollMs = 4;
constexpr UINT kNormalReleasePollMs = 16;
constexpr UINT kStalledReleasePollMs = 250;
constexpr std::int64_t kFastReleaseWindowMs = 100;
constexpr std::int64_t kReleaseStallThresholdMs = 2000;

constexpr UINT_PTR kReleaseTimer = 1;
// The id the view's watch on its top-level window goes by.
constexpr UINT_PTR kRootWatchId = 0x4D69676F + 11;
// The view's own messages: work done once the message that asked for it is.
constexpr UINT kAttachMessage = WM_APP + 1;
constexpr UINT kMetricsMessage = WM_APP + 2;
constexpr UINT kFrameMessage = WM_APP + 3;

constexpr wchar_t kWindowClass[] = L"MigoWin32SurfaceView";

[[noreturn]] void fatal(const char *why) {
    std::fprintf(stderr, "migo windows host kit: %s\n", why);
    std::fflush(stderr);
    std::terminate();
}

MigoKeyModifiers modifiers() noexcept {
    MigoKeyModifiers held = MIGO_KEY_MODIFIER_NONE;
    if (GetKeyState(VK_CONTROL) < 0) held |= MIGO_KEY_MODIFIER_CONTROL;
    if (GetKeyState(VK_SHIFT) < 0) held |= MIGO_KEY_MODIFIER_SHIFT;
    if (GetKeyState(VK_MENU) < 0) held |= MIGO_KEY_MODIFIER_ALT;
    if (GetKeyState(VK_LWIN) < 0 || GetKeyState(VK_RWIN) < 0) held |= MIGO_KEY_MODIFIER_META;
    return held;
}

// DOM MouseEvent.button ordinals.
std::uint32_t dom_button(UINT message, WPARAM wparam) noexcept {
    switch (message) {
        case WM_MBUTTONDOWN:
        case WM_MBUTTONUP:
            return 1;
        case WM_RBUTTONDOWN:
        case WM_RBUTTONUP:
            return 2;
        case WM_XBUTTONDOWN:
        case WM_XBUTTONUP:
            return GET_XBUTTON_WPARAM(wparam) == XBUTTON1 ? 3 : 4;
        default:
            return 0;
    }
}

// The button a move names: the lowest held -- the primary is the one content
// acts on -- asked of the message, never remembered, so a hover after a drag
// names none.
std::uint32_t dom_button_held(WPARAM keys) noexcept {
    if (keys & MK_LBUTTON) return 0;
    if (keys & MK_MBUTTON) return 1;
    if (keys & MK_RBUTTON) return 2;
    if (keys & MK_XBUTTON1) return 3;
    if (keys & MK_XBUTTON2) return 4;
    return 0;
}

bool is_button_down(UINT message) noexcept {
    return message == WM_LBUTTONDOWN || message == WM_MBUTTONDOWN || message == WM_RBUTTONDOWN ||
           message == WM_XBUTTONDOWN;
}

std::int64_t now_ns() noexcept {
    return std::chrono::duration_cast<std::chrono::nanoseconds>(
               std::chrono::steady_clock::now().time_since_epoch())
        .count();
}

/// `wide` as UTF-8 in `out`; false when it is not valid UTF-16.
bool utf8_of(const std::wstring &wide, std::string &out) {
    out.clear();
    if (wide.empty()) return true;
    const int bytes = WideCharToMultiByte(CP_UTF8, WC_ERR_INVALID_CHARS, wide.data(), static_cast<int>(wide.size()),
                                          nullptr, 0, nullptr, nullptr);
    if (bytes <= 0) return false;
    out.resize(static_cast<std::size_t>(bytes));
    return WideCharToMultiByte(CP_UTF8, WC_ERR_INVALID_CHARS, wide.data(), static_cast<int>(wide.size()), out.data(),
                               bytes, nullptr, nullptr) == bytes;
}

}  // namespace

/// The desktop compositor's frame, on a thread of its own: DwmFlush waits for
/// it, and the view is told on its thread.
class FrameClock final {
public:
    explicit FrameClock(HWND target) : target_(target), thread_([this] { run(); }) {}

    ~FrameClock() {
        {
            const std::lock_guard<std::mutex> lock(mutex_);
            stopping_ = true;
        }
        wake_.notify_one();
        thread_.join();
    }

    FrameClock(const FrameClock &) = delete;
    FrameClock &operator=(const FrameClock &) = delete;

    void request() {
        {
            const std::lock_guard<std::mutex> lock(mutex_);
            requested_ = true;
        }
        wake_.notify_one();
    }

private:
    void run() {
        std::unique_lock<std::mutex> lock(mutex_);
        while (true) {
            wake_.wait(lock, [this] { return requested_ || stopping_; });
            if (stopping_) return;
            requested_ = false;
            lock.unlock();
            // The next frame the compositor puts on screen. Without composition
            // -- never, since Windows 8 -- it returns at once; the view is told
            // either way.
            if (FAILED(DwmFlush())) std::this_thread::sleep_for(std::chrono::milliseconds(16));
            PostMessageW(target_, kFrameMessage, 0, 0);
            lock.lock();
        }
    }

    const HWND target_;
    std::mutex mutex_;
    std::condition_variable wake_;
    bool requested_ = false;
    bool stopping_ = false;
    std::thread thread_;
};

class Win32SurfaceView::Impl final {
public:
    Impl(SurfaceHost &surface_host, HWND parent, Listener listener)
        : host_(surface_host), listener_(std::move(listener)), owner_(std::this_thread::get_id()) {
        static std::once_flag registered;
        std::call_once(registered, [] {
            WNDCLASSEXW window_class{};
            window_class.cbSize = sizeof window_class;
            window_class.lpfnWndProc = &Impl::procedure;
            window_class.hInstance = GetModuleHandleW(nullptr);
            window_class.lpszClassName = kWindowClass;
            // No background: the engine draws every pixel.
            window_class.hbrBackground = nullptr;
            window_class.hCursor = nullptr;
            if (RegisterClassExW(&window_class) == 0) fatal("RegisterClassExW failed");
        });
        // WM_NCCREATE records the handle as the window is made.
        if (CreateWindowExW(0, kWindowClass, L"", WS_CHILD | WS_CLIPSIBLINGS | WS_CLIPCHILDREN, 0, 0, 0, 0, parent,
                            nullptr, GetModuleHandleW(nullptr), this) == nullptr) {
            fatal("CreateWindowExW failed for the game's view");
        }
        cursor_ = LoadCursorW(nullptr, IDC_ARROW);
        // A child is not told when its top-level window is shown, and an App
        // shows its window after the view more often than not: the view
        // watches it, so it attaches once it is in fact on screen.
        root_ = GetAncestor(hwnd_, GA_ROOT);
        if (root_ != nullptr && root_ != hwnd_) {
            watching_root_ =
                SetWindowSubclass(root_, &Impl::rootWatch, kRootWatchId, reinterpret_cast<DWORD_PTR>(this)) != FALSE;
        }
    }

    ~Impl() {
        unlockPointer();
        if (owns_surface_) fatal("Win32SurfaceView destroyed before its Surface reached RELEASED");
        unwatchRoot();
        frame_clock_.reset();
        if (hwnd_ != nullptr) {
            KillTimer(hwnd_, kReleaseTimer);
            SetWindowLongPtrW(hwnd_, GWLP_USERDATA, 0);
            DestroyWindow(hwnd_);
        }
    }

    HWND hwnd() const noexcept { return hwnd_; }

    /// An attach, once the message that may have shown the view is done: the
    /// attach itself looks again at whether it is on screen, at a size.
    void requestAttach() {
        if (hwnd_ == nullptr || owns_surface_ || host_.state() != SurfaceState::Detached || close_pending_ ||
            attach_pending_) {
            return;
        }
        attach_pending_ = true;
        PostMessageW(hwnd_, kAttachMessage, 0, 0);
    }

    /// The top-level window: shown, which shows the view where it was shown already.
    static LRESULT CALLBACK rootWatch(HWND window, UINT message, WPARAM wparam, LPARAM lparam, UINT_PTR,
                                      DWORD_PTR reference) {
        const LRESULT result = DefSubclassProc(window, message, wparam, lparam);
        auto *view = reinterpret_cast<Impl *>(reference);
        if ((message == WM_SHOWWINDOW && wparam != FALSE) ||
            (message == WM_WINDOWPOSCHANGED &&
             (reinterpret_cast<const WINDOWPOS *>(lparam)->flags & SWP_SHOWWINDOW) != 0)) {
            view->requestAttach();
        } else if (message == WM_NCDESTROY) {
            view->watching_root_ = false;
            RemoveWindowSubclass(window, &Impl::rootWatch, kRootWatchId);
        }
        return result;
    }

    void unwatchRoot() {
        if (!watching_root_) return;
        RemoveWindowSubclass(root_, &Impl::rootWatch, kRootWatchId);
        watching_root_ = false;
    }

    // ---- the Surface ------------------------------------------------------------------------------

    MigoResult attachSurface() {
        if (!onOwnerThread()) return MIGO_ERROR_WRONG_THREAD;
        if (hwnd_ == nullptr || owns_surface_ || host_.state() != SurfaceState::Detached || close_pending_) {
            return record(MIGO_ERROR_INVALID_STATE);
        }
        SurfaceMetrics metrics;
        if (const MigoResult measured = currentMetrics(metrics); measured != MIGO_OK) return record(measured);
        const MigoResult result = host_.attach(Win32Target{hwnd_}, metrics);
        if (result == MIGO_OK || host_.state() == SurfaceState::Faulted) {
            owns_surface_ = true;
            owned_generation_ = host_.generation();
        }
        record(result);
        if (result == MIGO_OK && listener_.attached) listener_.attached(owned_generation_);
        return result;
    }

    MigoResult beginDetach() {
        if (!onOwnerThread()) return MIGO_ERROR_WRONG_THREAD;
        if (!ownsAttached()) return record(MIGO_ERROR_INVALID_STATE);
        // The game is going: a lock it holds goes first.
        unlockPointer();
        const MigoResult result = host_.begin_detach();
        record(result);
        if (result == MIGO_OK) {
            metrics_pending_ = false;
            release_stall_reported_ = false;
            release_started_ = std::chrono::steady_clock::now();
            release_interval_ = kFastReleasePollMs;
            SetTimer(hwnd_, kReleaseTimer, release_interval_, nullptr);
        }
        return result;
    }

    MigoResult pollDetach(bool *released) {
        if (!onOwnerThread()) return MIGO_ERROR_WRONG_THREAD;
        if (released == nullptr) return record(MIGO_ERROR_INVALID_ARGUMENT);
        *released = false;
        if (!owns_surface_ || host_.state() != SurfaceState::Retiring || host_.generation() != owned_generation_) {
            return record(MIGO_ERROR_INVALID_STATE);
        }
        const MigoResult result = host_.poll_release(released);
        if (result != MIGO_OK) {
            // An error stops the polling, and leaves the observer to the App's
            // next pollDetach once it has handled the error.
            KillTimer(hwnd_, kReleaseTimer);
            return record(result);
        }
        record(result);
        if (!*released) {
            updateReleaseCadence();
            return result;
        }
        KillTimer(hwnd_, kReleaseTimer);
        const std::uint64_t generation = owned_generation_;
        owned_generation_ = 0;
        owns_surface_ = false;
        const bool closing = close_pending_;
        close_pending_ = false;
        if (listener_.released) listener_.released(generation);
        if (closing) destroyWindow();
        return MIGO_OK;
    }

    void close() {
        if (!onOwnerThread() || hwnd_ == nullptr) return;
        if (!owns_surface_) {
            destroyWindow();
            return;
        }
        switch (host_.state()) {
            case SurfaceState::Attached:
                close_pending_ = true;
                if (beginDetach() != MIGO_OK) close_pending_ = false;
                return;
            case SurfaceState::Retiring:
                close_pending_ = true;
                return;
            case SurfaceState::Detached:
                fatal("Win32SurfaceView's attachment was released outside its view");
            case SurfaceState::Faulted:
                fatal("Win32SurfaceView cannot close after losing its release observer");
        }
    }

    // ---- input settings ---------------------------------------------------------------------------

    void setPointerDelivery(PointerDelivery delivery) noexcept { delivery_ = delivery; }
    PointerDelivery pointerDelivery() const noexcept { return delivery_; }

    bool lockPointer() {
        if (locked_) return true;
        // With the player at the game: the view holds the keyboard -- so Esc,
        // the way out, reaches it -- and the cursor is over it.
        if (!inputIsDeliverable() || GetFocus() != hwnd_) return false;
        POINT cursor{};
        if (!GetCursorPos(&cursor)) return false;
        POINT local = cursor;
        ScreenToClient(hwnd_, &local);
        RECT client{};
        GetClientRect(hwnd_, &client);
        if (!PtInRect(&client, local) || WindowFromPoint(cursor) != hwnd_) return false;
        // The mouse's own motion, before the system's acceleration and clamping
        // to the screen, which a locked pointer must not be.
        RAWINPUTDEVICE mouse{};
        mouse.usUsagePage = 0x01;
        mouse.usUsage = 0x02;
        mouse.hwndTarget = hwnd_;
        if (!RegisterRawInputDevices(&mouse, 1, sizeof mouse)) return false;
        RECT confined = client;
        MapWindowPoints(hwnd_, nullptr, reinterpret_cast<POINT *>(&confined), 2);
        ClipCursor(&confined);
        locked_ = true;
        lock_origin_ = cursor;
        const float scale = currentScale();
        locked_x_ = static_cast<double>(local.x) / scale;
        locked_y_ = static_cast<double>(local.y) / scale;
        last_x_ = static_cast<float>(locked_x_);
        last_y_ = static_cast<float>(locked_y_);
        SetCursor(nullptr);
        if (listener_.pointerLockChanged) listener_.pointerLockChanged(true);
        return true;
    }

    void unlockPointer() {
        if (!locked_) return;
        locked_ = false;
        RAWINPUTDEVICE mouse{};
        mouse.usUsagePage = 0x01;
        mouse.usUsage = 0x02;
        mouse.dwFlags = RIDEV_REMOVE;
        mouse.hwndTarget = nullptr;
        RegisterRawInputDevices(&mouse, 1, sizeof mouse);
        ClipCursor(nullptr);
        // The motion moved content's pointer, not the cursor: it stands where
        // it stood.
        SetCursorPos(lock_origin_.x, lock_origin_.y);
        SetCursor(cursor_);
        if (listener_.pointerLockChanged) listener_.pointerLockChanged(false);
    }

    bool isPointerLocked() const noexcept { return locked_; }

    void setCursor(HCURSOR cursor) noexcept {
        cursor_ = cursor;
        // Shown now when the cursor is over the view; otherwise as it enters.
        POINT at{};
        if (!locked_ && GetCursorPos(&at) && WindowFromPoint(at) == hwnd_) SetCursor(cursor_);
    }

    // ---- frames -----------------------------------------------------------------------------------

    MigoResult requestFrame() {
        if (!onOwnerThread()) return MIGO_ERROR_WRONG_THREAD;
        if (!inputIsDeliverable()) return MIGO_ERROR_INVALID_STATE;
        // Already asked: the engine asks again when it wants another frame,
        // and two boundaries for one request would run content's clock at
        // double speed.
        if (frame_requested_) return MIGO_OK;
        if (frame_clock_ == nullptr) frame_clock_ = std::make_unique<FrameClock>(hwnd_);
        frame_requested_ = true;
        frame_clock_->request();
        return MIGO_OK;
    }

    bool isFramePending() const noexcept { return frame_requested_; }
    std::uint64_t framesDelivered() const noexcept { return frames_delivered_; }

    SurfaceState surfaceState() const noexcept { return owns_surface_ ? host_.state() : SurfaceState::Detached; }
    std::uint64_t generation() const noexcept { return owned_generation_; }
    MigoResult lastError() const noexcept { return last_error_; }
    float scale() const noexcept { return currentScale(); }

private:
    static LRESULT CALLBACK procedure(HWND window, UINT message, WPARAM wparam, LPARAM lparam) {
        if (message == WM_NCCREATE) {
            // The first message, before CreateWindowExW returns the handle.
            auto *created = static_cast<Impl *>(reinterpret_cast<const CREATESTRUCTW *>(lparam)->lpCreateParams);
            created->hwnd_ = window;
            SetWindowLongPtrW(window, GWLP_USERDATA, reinterpret_cast<LONG_PTR>(created));
        }
        auto *view = reinterpret_cast<Impl *>(GetWindowLongPtrW(window, GWLP_USERDATA));
        if (view == nullptr || view->hwnd_ != window) return DefWindowProcW(window, message, wparam, lparam);
        return view->handle(window, message, wparam, lparam);
    }

    LRESULT handle(HWND window, UINT message, WPARAM wparam, LPARAM lparam) {
        switch (message) {
            case WM_WINDOWPOSCHANGED: {
                const auto *position = reinterpret_cast<const WINDOWPOS *>(lparam);
                // Shown at a size: attached once the show is done, so the
                // engine binds the window as the system has placed it.
                if ((position->flags & SWP_SHOWWINDOW) != 0 || (position->flags & SWP_NOSIZE) == 0) {
                    if (IsWindowVisible(hwnd_)) requestAttach();
                    if ((position->flags & SWP_NOSIZE) == 0) scheduleMetrics();
                }
                return DefWindowProcW(window, message, wparam, lparam);
            }
            case kAttachMessage: {
                attach_pending_ = false;
                RECT client{};
                GetClientRect(hwnd_, &client);
                if (IsWindowVisible(hwnd_) && client.right > 0 && client.bottom > 0 && !owns_surface_ &&
                    host_.state() == SurfaceState::Detached && !close_pending_) {
                    (void)attachSurface();
                }
                return 0;
            }
            case kMetricsMessage:
                metrics_pending_ = false;
                if (ownsAttached()) {
                    SurfaceMetrics metrics;
                    const MigoResult measured = currentMetrics(metrics);
                    record(measured == MIGO_OK ? host_.update(metrics) : measured);
                }
                return 0;
            case WM_DPICHANGED_AFTERPARENT:
                scheduleMetrics();
                return 0;
            case kFrameMessage:
                notifyFrameBoundary();
                return 0;
            case WM_TIMER:
                if (wparam == kReleaseTimer) {
                    bool released = false;
                    (void)pollDetach(&released);
                    return 0;
                }
                break;
            case WM_ERASEBKGND:
                // The engine draws every pixel.
                return 1;
            case WM_PAINT:
                ValidateRect(hwnd_, nullptr);
                return 0;
            case WM_SETCURSOR:
                if (LOWORD(lparam) == HTCLIENT) {
                    SetCursor(locked_ ? nullptr : cursor_);
                    return TRUE;
                }
                break;
            case WM_MOUSEACTIVATE:
                // A click on the game gives it the keyboard, as a browser's
                // canvas would.
                SetFocus(hwnd_);
                return MA_ACTIVATE;
            case WM_LBUTTONDOWN:
            case WM_MBUTTONDOWN:
            case WM_RBUTTONDOWN:
            case WM_XBUTTONDOWN:
            case WM_LBUTTONUP:
            case WM_MBUTTONUP:
            case WM_RBUTTONUP:
            case WM_XBUTTONUP:
                mouseButton(message, wparam, lparam);
                return message == WM_XBUTTONDOWN || message == WM_XBUTTONUP ? TRUE : 0;
            case WM_MOUSEMOVE:
                mouseMove(wparam, lparam);
                return 0;
            case WM_MOUSELEAVE:
                tracking_leave_ = false;
                return 0;
            case WM_CAPTURECHANGED:
                // The press went elsewhere: cancelled, not stranded.
                if (reinterpret_cast<HWND>(lparam) != hwnd_ && mouse_pressed_) retractPress();
                return 0;
            case WM_MOUSEWHEEL:
            case WM_MOUSEHWHEEL:
                wheel(message, wparam);
                return 0;
            case WM_INPUT:
                rawInput(lparam);
                return DefWindowProcW(window, message, wparam, lparam);
            case WM_POINTERDOWN:
            case WM_POINTERUPDATE:
            case WM_POINTERUP:
            case WM_POINTERCAPTURECHANGED:
                if (contact(message, wparam)) return 0;
                break;
            case WM_KEYDOWN:
            case WM_SYSKEYDOWN:
            case WM_KEYUP:
            case WM_SYSKEYUP:
                if (key(message, wparam, lparam)) return 0;
                // A system key the game did not take -- Alt+F4 -- is still the
                // system's.
                break;
            case WM_SETFOCUS:
                if (inputIsDeliverable()) reportInput(migo_session_set_focus(host_.session(), 1));
                return 0;
            case WM_KILLFOCUS:
                // Esc no longer reaches the view: a lock it holds goes. The
                // press and the preedit end while content still has focus.
                unlockPointer();
                retractPendingInput();
                if (inputIsDeliverable()) reportInput(migo_session_set_focus(host_.session(), 0));
                return 0;
            case WM_IME_STARTCOMPOSITION:
                return 0;
            case WM_IME_COMPOSITION:
                if (composition(lparam)) return 0;
                break;
            case WM_IME_ENDCOMPOSITION:
                if (composing_) endComposition({});
                return 0;
            case WM_DESTROY:
                // Destroyed by someone else -- its parent going with it.
                if (owns_surface_) fatal("the game's view was destroyed before Migo reported RELEASED");
                unwatchRoot();
                frame_clock_.reset();
                SetWindowLongPtrW(window, GWLP_USERDATA, 0);
                hwnd_ = nullptr;
                return 0;
            default:
                break;
        }
        return DefWindowProcW(window, message, wparam, lparam);
    }

    // ---- the Surface's bookkeeping ------------------------------------------------------------------

    bool onOwnerThread() const noexcept { return std::this_thread::get_id() == owner_; }

    bool ownsAttached() const noexcept {
        return owns_surface_ && host_.state() == SurfaceState::Attached && host_.generation() == owned_generation_;
    }

    bool inputIsDeliverable() const noexcept { return ownsAttached() && host_.session() != nullptr; }

    MigoResult record(MigoResult result) {
        last_error_ = result;
        if (result != MIGO_OK && listener_.surfaceError) listener_.surfaceError(result);
        return result;
    }

    void reportInput(MigoResult result) {
        if (result != MIGO_OK && listener_.inputRejected) listener_.inputRejected(result);
    }

    float currentScale() const noexcept {
        const UINT dpi = hwnd_ != nullptr ? GetDpiForWindow(hwnd_) : 0;
        return dpi == 0 ? 1.0F : static_cast<float>(dpi) / static_cast<float>(USER_DEFAULT_SCREEN_DPI);
    }

    MigoResult currentMetrics(SurfaceMetrics &metrics) const {
        RECT client{};
        if (hwnd_ == nullptr || !GetClientRect(hwnd_, &client) || client.right <= 0 || client.bottom <= 0) {
            return MIGO_ERROR_INVALID_ARGUMENT;
        }
        metrics.width_pixels = static_cast<std::uint32_t>(client.right);
        metrics.height_pixels = static_cast<std::uint32_t>(client.bottom);
        metrics.scale_factor = currentScale();
        metrics.color_space = MIGO_COLOR_SPACE_SRGB;
        metrics.alpha_mode = MIGO_ALPHA_MODE_OPAQUE;
        metrics.presentation_mode = MIGO_PRESENTATION_MODE_DEFAULT;
        metrics.required_capabilities = MIGO_SURFACE_CAPABILITY_NONE;
        return MIGO_OK;
    }

    void scheduleMetrics() {
        if (!ownsAttached() || metrics_pending_) return;
        metrics_pending_ = true;
        PostMessageW(hwnd_, kMetricsMessage, 0, 0);
    }

    void updateReleaseCadence() {
        const auto elapsed = std::chrono::duration_cast<std::chrono::milliseconds>(
                                 std::chrono::steady_clock::now() - release_started_)
                                 .count();
        UINT interval = kFastReleasePollMs;
        if (elapsed >= kReleaseStallThresholdMs) {
            interval = kStalledReleasePollMs;
        } else if (elapsed >= kFastReleaseWindowMs) {
            interval = kNormalReleasePollMs;
        }
        if (interval != release_interval_) {
            release_interval_ = interval;
            SetTimer(hwnd_, kReleaseTimer, interval, nullptr);
        }
        if (elapsed >= kReleaseStallThresholdMs && !release_stall_reported_) {
            release_stall_reported_ = true;
            if (listener_.releaseStalled) listener_.releaseStalled(host_.generation(), elapsed);
        }
    }

    void destroyWindow() {
        if (hwnd_ == nullptr) return;
        frame_clock_.reset();
        KillTimer(hwnd_, kReleaseTimer);
        const HWND window = hwnd_;
        SetWindowLongPtrW(window, GWLP_USERDATA, 0);
        hwnd_ = nullptr;
        DestroyWindow(window);
    }

    void notifyFrameBoundary() {
        if (!frame_requested_) return;
        frame_requested_ = false;
        if (!inputIsDeliverable()) return;
        // When the boundary was observed, on the steady clock: the engine takes
        // it as a monotonic frame time, not a display timestamp.
        const MigoResult result = migo_session_notify_vsync(host_.session(), now_ns());
        if (result == MIGO_OK) ++frames_delivered_;
        reportInput(result);
    }

    // ---- the mouse ----------------------------------------------------------------------------------

    void position(LPARAM lparam) {
        const float scale = currentScale();
        last_x_ = static_cast<float>(GET_X_LPARAM(lparam)) / scale;
        last_y_ = static_cast<float>(GET_Y_LPARAM(lparam)) / scale;
    }

    void deliverPointer(std::uint32_t kind, std::uint32_t button) {
        MigoPointerEvent out{};
        out.struct_size = static_cast<std::uint32_t>(sizeof out);
        out.abi_version = MIGO_ABI_VERSION_1;
        out.event_type = kind;
        out.button = button;
        out.x = last_x_;
        out.y = last_y_;
        out.timestamp_ms = static_cast<double>(GetMessageTime());
        reportInput(migo_session_send_pointer_event(host_.session(), &out));
    }

    // A mouse is one finger, id 0, and only while a button is held: content on
    // a phone has no hover.
    void deliverMouseAsTouch(MigoTouchType kind) {
        MigoTouchPoint &point = points_[0];
        point = MigoTouchPoint{};
        point.id = 0;
        point.x = last_x_;
        point.y = last_y_;
        const bool ended = kind == MIGO_TOUCH_END || kind == MIGO_TOUCH_CANCEL;
        point.pressure = ended ? 0.0F : 1.0F;
        point.flags = MIGO_TOUCH_FLAG_CHANGED | (ended ? MIGO_TOUCH_FLAG_REMOVED : 0U);
        MigoTouchEvent out{};
        out.struct_size = static_cast<std::uint32_t>(sizeof out);
        out.abi_version = MIGO_ABI_VERSION_1;
        out.type = kind;
        out.point_count = 1;
        out.timestamp_ms = static_cast<std::int64_t>(GetMessageTime());
        out.points = points_.data();
        reportInput(migo_session_send_touch(host_.session(), &out));
    }

    void mouseButton(UINT message, WPARAM wparam, LPARAM lparam) {
        if (!inputIsDeliverable()) return;
        // While locked, a press is where content's pointer is, not the hidden cursor.
        if (!locked_) position(lparam);
        const bool pressing = is_button_down(message);
        const std::uint32_t button = dom_button(message, wparam);
        if (pressing) {
            const bool first = !mouse_pressed_;
            mouse_pressed_ = true;
            // Held past the window's edge, the release still comes here.
            SetCapture(hwnd_);
            if (delivery_ != PointerDelivery::TouchOnly) deliverPointer(MIGO_POINTER_EVENT_DOWN, button);
            if (delivery_ != PointerDelivery::MouseOnly && first) deliverMouseAsTouch(MIGO_TOUCH_START);
            return;
        }
        const bool any_held = (wparam & (MK_LBUTTON | MK_MBUTTON | MK_RBUTTON | MK_XBUTTON1 | MK_XBUTTON2)) != 0;
        const bool was_pressed = mouse_pressed_;
        if (!any_held) {
            mouse_pressed_ = false;
            if (GetCapture() == hwnd_) ReleaseCapture();
        }
        if (delivery_ != PointerDelivery::TouchOnly) deliverPointer(MIGO_POINTER_EVENT_UP, button);
        if (delivery_ != PointerDelivery::MouseOnly && was_pressed && !any_held) deliverMouseAsTouch(MIGO_TOUCH_END);
    }

    void mouseMove(WPARAM wparam, LPARAM lparam) {
        if (!tracking_leave_) {
            TRACKMOUSEEVENT track{};
            track.cbSize = sizeof track;
            track.dwFlags = TME_LEAVE;
            track.hwndTrack = hwnd_;
            tracking_leave_ = TrackMouseEvent(&track) != FALSE;
        }
        // Locked, the raw motion moves content's pointer; this is the hidden cursor's.
        if (!inputIsDeliverable() || locked_) return;
        position(lparam);
        if (delivery_ != PointerDelivery::TouchOnly) deliverPointer(MIGO_POINTER_EVENT_MOVE, dom_button_held(wparam));
        if (delivery_ != PointerDelivery::MouseOnly && mouse_pressed_) deliverMouseAsTouch(MIGO_TOUCH_MOVE);
    }

    void rawInput(LPARAM lparam) {
        if (!locked_ || !inputIsDeliverable()) return;
        RAWINPUT input{};
        UINT size = sizeof input;
        if (GetRawInputData(reinterpret_cast<HRAWINPUT>(lparam), RID_INPUT, &input, &size, sizeof(RAWINPUTHEADER)) ==
                static_cast<UINT>(-1) ||
            input.header.dwType != RIM_TYPEMOUSE || (input.data.mouse.usFlags & MOUSE_MOVE_ABSOLUTE) != 0) {
            return;
        }
        const LONG dx = input.data.mouse.lLastX;
        const LONG dy = input.data.mouse.lLastY;
        if (dx == 0 && dy == 0) return;
        // Past the view's edges if the mouse goes there: the pointer is content's.
        const float scale = currentScale();
        locked_x_ += static_cast<double>(dx) / scale;
        locked_y_ += static_cast<double>(dy) / scale;
        last_x_ = static_cast<float>(locked_x_);
        last_y_ = static_cast<float>(locked_y_);
        WPARAM held = 0;
        if (GetKeyState(VK_LBUTTON) < 0) held |= MK_LBUTTON;
        if (GetKeyState(VK_MBUTTON) < 0) held |= MK_MBUTTON;
        if (GetKeyState(VK_RBUTTON) < 0) held |= MK_RBUTTON;
        if (delivery_ != PointerDelivery::TouchOnly) deliverPointer(MIGO_POINTER_EVENT_MOVE, dom_button_held(held));
        if (delivery_ != PointerDelivery::MouseOnly && mouse_pressed_) deliverMouseAsTouch(MIGO_TOUCH_MOVE);
    }

    void wheel(UINT message, WPARAM wparam) {
        if (!inputIsDeliverable()) return;
        // 120 a notch, three lines a notch -- the step every toolkit and browser
        // settled on. The DOM's deltaY is positive scrolling down, Windows' is
        // positive scrolling up; both agree that right is positive.
        constexpr double kPerNotch = WHEEL_DELTA;
        constexpr double kLinesPerNotch = 3.0;
        const double notches = static_cast<double>(GET_WHEEL_DELTA_WPARAM(wparam)) / kPerNotch;
        MigoWheelEvent out{};
        out.struct_size = static_cast<std::uint32_t>(sizeof out);
        out.abi_version = MIGO_ABI_VERSION_1;
        out.delta_mode = MIGO_WHEEL_DELTA_MODE_LINE;
        if (message == WM_MOUSEWHEEL) {
            out.delta_y = -notches * kLinesPerNotch;
        } else {
            out.delta_x = notches * kLinesPerNotch;
        }
        out.timestamp_ms = static_cast<double>(GetMessageTime());
        reportInput(migo_session_send_wheel_event(host_.session(), &out));
    }

    void retractPress() {
        mouse_pressed_ = false;
        if (!inputIsDeliverable() || delivery_ == PointerDelivery::MouseOnly) return;
        deliverMouseAsTouch(MIGO_TOUCH_CANCEL);
    }

    // ---- touch screens and pens --------------------------------------------------------------------

    /// One contact's message: every contact of its frame sent at once, as a
    /// touch screen reports them. False for the mouse, which the mouse
    /// messages carry.
    bool contact(UINT message, WPARAM wparam) {
        const UINT32 id = GET_POINTERID_WPARAM(wparam);
        POINTER_INPUT_TYPE type = PT_POINTER;
        if (!GetPointerType(id, &type) || (type != PT_TOUCH && type != PT_PEN)) return false;
        if (!inputIsDeliverable()) return true;
        UINT32 count = MIGO_TOUCH_MAX_POINTS;
        std::array<POINTER_INFO, MIGO_TOUCH_MAX_POINTS> frame{};
        if (!GetPointerFrameInfo(id, &count, frame.data()) || count == 0) {
            // More contacts than the ABI takes: refused rather than truncated,
            // since a dropped contact is one content is tracking.
            return true;
        }
        std::array<UINT32, MIGO_TOUCH_MAX_POINTS> pressure{};
        if (type == PT_TOUCH) {
            std::array<POINTER_TOUCH_INFO, MIGO_TOUCH_MAX_POINTS> touches{};
            UINT32 touch_count = count;
            if (GetPointerFrameTouchInfo(id, &touch_count, touches.data())) {
                for (UINT32 index = 0; index < touch_count && index < count; ++index) {
                    if (touches[index].touchMask & TOUCH_MASK_PRESSURE) pressure[index] = touches[index].pressure;
                }
            }
        } else {
            std::array<POINTER_PEN_INFO, MIGO_TOUCH_MAX_POINTS> pens{};
            UINT32 pen_count = count;
            if (GetPointerFramePenInfo(id, &pen_count, pens.data())) {
                for (UINT32 index = 0; index < pen_count && index < count; ++index) {
                    if (pens[index].penMask & PEN_MASK_PRESSURE) pressure[index] = pens[index].pressure;
                }
            }
        }
        MigoTouchType kind = MIGO_TOUCH_MOVE;
        if (message == WM_POINTERDOWN) kind = MIGO_TOUCH_START;
        if (message == WM_POINTERUP) kind = MIGO_TOUCH_END;
        if (message == WM_POINTERCAPTURECHANGED) kind = MIGO_TOUCH_CANCEL;
        const float scale = currentScale();
        for (UINT32 index = 0; index < count; ++index) {
            const POINTER_INFO &source = frame[index];
            POINT at = source.ptPixelLocation;
            ScreenToClient(hwnd_, &at);
            MigoTouchPoint &point = points_[index];
            point = MigoTouchPoint{};
            point.id = source.pointerId;
            point.x = static_cast<float>(at.x) / scale;
            point.y = static_cast<float>(at.y) / scale;
            // 0..1024, where the device reports it.
            point.pressure = pressure[index] != 0 ? static_cast<float>(pressure[index]) / 1024.0F : 1.0F;
            point.flags = MIGO_TOUCH_FLAG_NONE;
            const bool removed = (source.pointerFlags & (POINTER_FLAG_UP | POINTER_FLAG_CANCELED)) != 0 ||
                                 (kind == MIGO_TOUCH_CANCEL && source.pointerId == id);
            if (source.pointerId == id || (source.pointerFlags & (POINTER_FLAG_DOWN | POINTER_FLAG_UPDATE)) != 0) {
                point.flags |= MIGO_TOUCH_FLAG_CHANGED;
            }
            if (removed) {
                point.flags |= MIGO_TOUCH_FLAG_REMOVED;
                point.pressure = 0.0F;
            }
        }
        MigoTouchEvent out{};
        out.struct_size = static_cast<std::uint32_t>(sizeof out);
        out.abi_version = MIGO_ABI_VERSION_1;
        out.type = kind;
        out.point_count = count;
        out.timestamp_ms = static_cast<std::int64_t>(GetMessageTime());
        out.points = points_.data();
        reportInput(migo_session_send_touch(host_.session(), &out));
        return true;
    }

    // ---- keys ---------------------------------------------------------------------------------------

    /// True when the key is taken -- the game's, or Esc ending a lock.
    bool key(UINT message, WPARAM wparam, LPARAM lparam) {
        const bool pressing = message == WM_KEYDOWN || message == WM_SYSKEYDOWN;
        const auto virtual_key = static_cast<std::uint32_t>(wparam);
        const bool repeat = pressing && (lparam & (LPARAM{1} << 30)) != 0;
        // Esc is the player's way out of a lock, as in a browser: that Esc, its
        // repeats and its release are not the game's.
        if (virtual_key == VK_ESCAPE && (locked_ || escape_ended_lock_)) {
            if (pressing && locked_) {
                escape_ended_lock_ = true;
                unlockPointer();
            } else if (!pressing) {
                escape_ended_lock_ = false;
            }
            return true;
        }
        if (!inputIsDeliverable()) return false;
        const auto scan = static_cast<std::uint32_t>((lparam >> 16) & 0xFF);
        const bool extended = (lparam & (LPARAM{1} << 24)) != 0;
        std::array<std::uint8_t, 256> state{};
        if (!GetKeyboardState(state.data())) state.fill(0);
        char name[64];
        std::size_t length =
            detail::dom_key(virtual_key, scan, extended, state.data(), GetKeyboardLayout(0), name, sizeof name);
        if (length == 0) {
            constexpr char kUnidentified[] = "Unidentified";
            std::memcpy(name, kUnidentified, sizeof kUnidentified - 1);
            length = sizeof kUnidentified - 1;
        }
        const char *code = detail::dom_code(scan, extended);
        MigoKeyEvent out{};
        out.struct_size = static_cast<std::uint32_t>(sizeof out);
        out.abi_version = MIGO_ABI_VERSION_1;
        out.event_type = pressing ? MIGO_KEY_EVENT_DOWN : MIGO_KEY_EVENT_UP;
        out.key_utf8 = name;
        out.key_length = static_cast<std::uint32_t>(length);
        out.code_utf8 = code;
        out.code_length = static_cast<std::uint32_t>(std::strlen(code));
        out.timestamp_ms = static_cast<double>(GetMessageTime());
        out.modifiers = modifiers();
        out.flags = repeat ? MIGO_KEY_EVENT_FLAG_REPEAT : MIGO_KEY_EVENT_FLAG_NONE;
        reportInput(migo_session_send_key_event(host_.session(), &out));
        // The system keeps its own shortcuts: Alt and F10 still reach the
        // window's menu, Alt+F4 still closes it.
        return message == WM_KEYDOWN || message == WM_KEYUP;
    }

    // ---- the IME ------------------------------------------------------------------------------------

    void deliverComposition(std::uint32_t kind, const std::string &data) {
        MigoCompositionEvent out{};
        out.struct_size = static_cast<std::uint32_t>(sizeof out);
        out.abi_version = MIGO_ABI_VERSION_1;
        out.event_type = kind;
        out.data_utf8 = data.empty() ? nullptr : data.data();
        out.data_length = static_cast<std::uint32_t>(data.size());
        reportInput(migo_session_send_composition_event(host_.session(), &out));
    }

    /// A composition string of the IME's, as UTF-8; empty when there is none.
    static std::string compositionString(HIMC context, DWORD which) {
        const LONG bytes = ImmGetCompositionStringW(context, which, nullptr, 0);
        if (bytes <= 0) return {};
        std::wstring wide(static_cast<std::size_t>(bytes) / sizeof(wchar_t), L'\0');
        ImmGetCompositionStringW(context, which, wide.data(), static_cast<DWORD>(bytes));
        std::string utf8;
        return utf8_of(wide, utf8) ? utf8 : std::string();
    }

    /// The preedit and the commit, which content drawing its own field needs
    /// both of; neither is inferred from the other. True when taken.
    bool composition(LPARAM lparam) {
        if (!inputIsDeliverable()) return false;
        HIMC context = ImmGetContext(hwnd_);
        if (context == nullptr) return false;
        std::string commit;
        std::string preedit;
        if (lparam & GCS_RESULTSTR) commit = compositionString(context, GCS_RESULTSTR);
        if (lparam & GCS_COMPSTR) preedit = compositionString(context, GCS_COMPSTR);
        ImmReleaseContext(hwnd_, context);
        if (!commit.empty()) endComposition(commit);
        if (!preedit.empty()) {
            if (!composing_) {
                composing_ = true;
                deliverComposition(MIGO_COMPOSITION_EVENT_START, preedit);
            }
            deliverComposition(MIGO_COMPOSITION_EVENT_UPDATE, preedit);
        }
        // Taken whole: what the IME committed is not typed again as characters.
        return true;
    }

    /// The composition ends with `commit`; an empty one is a cancellation,
    /// which content must see to clear the preedit it drew.
    void endComposition(const std::string &commit) {
        if (!composing_) {
            composing_ = true;
            deliverComposition(MIGO_COMPOSITION_EVENT_START, {});
        }
        composing_ = false;
        deliverComposition(MIGO_COMPOSITION_EVENT_END, commit);
    }

    void retractPendingInput() {
        if (!inputIsDeliverable()) {
            mouse_pressed_ = false;
            composing_ = false;
            return;
        }
        if (mouse_pressed_) retractPress();
        if (composing_) {
            // A focus loss mid-composition is the DOM's cancellation.
            composing_ = false;
            deliverComposition(MIGO_COMPOSITION_EVENT_END, {});
        }
    }

    SurfaceHost &host_;
    const Listener listener_;
    const std::thread::id owner_;
    HWND hwnd_ = nullptr;
    HCURSOR cursor_ = nullptr;

    bool owns_surface_ = false;
    std::uint64_t owned_generation_ = 0;
    bool close_pending_ = false;
    bool attach_pending_ = false;
    HWND root_ = nullptr;
    bool watching_root_ = false;
    bool metrics_pending_ = false;
    MigoResult last_error_ = MIGO_OK;

    std::chrono::steady_clock::time_point release_started_{};
    UINT release_interval_ = kFastReleasePollMs;
    bool release_stall_reported_ = false;

    PointerDelivery delivery_ = PointerDelivery::TouchAndMouse;
    std::array<MigoTouchPoint, MIGO_TOUCH_MAX_POINTS> points_{};
    float last_x_ = 0.0F;
    float last_y_ = 0.0F;
    bool mouse_pressed_ = false;
    bool tracking_leave_ = false;
    bool composing_ = false;

    bool locked_ = false;
    bool escape_ended_lock_ = false;
    POINT lock_origin_{};
    double locked_x_ = 0.0;
    double locked_y_ = 0.0;

    std::unique_ptr<FrameClock> frame_clock_;
    bool frame_requested_ = false;
    std::uint64_t frames_delivered_ = 0;
};

// ---- the public face ------------------------------------------------------------------------------

Win32SurfaceView::Win32SurfaceView(SurfaceHost &surface_host, HWND__ *parent, Listener listener)
    : impl_(std::make_unique<Impl>(surface_host, parent, std::move(listener))) {}

Win32SurfaceView::~Win32SurfaceView() = default;

HWND__ *Win32SurfaceView::hwnd() const noexcept { return impl_->hwnd(); }
MigoResult Win32SurfaceView::attachSurface() { return impl_->attachSurface(); }
MigoResult Win32SurfaceView::beginDetach() { return impl_->beginDetach(); }
MigoResult Win32SurfaceView::pollDetach(bool *released) { return impl_->pollDetach(released); }
void Win32SurfaceView::close() { impl_->close(); }
void Win32SurfaceView::setPointerDelivery(PointerDelivery delivery) noexcept { impl_->setPointerDelivery(delivery); }
Win32SurfaceView::PointerDelivery Win32SurfaceView::pointerDelivery() const noexcept { return impl_->pointerDelivery(); }
bool Win32SurfaceView::lockPointer() { return impl_->lockPointer(); }
void Win32SurfaceView::unlockPointer() { impl_->unlockPointer(); }
bool Win32SurfaceView::isPointerLocked() const noexcept { return impl_->isPointerLocked(); }
void Win32SurfaceView::setCursor(HICON__ *cursor) noexcept { impl_->setCursor(cursor); }
MigoResult Win32SurfaceView::requestFrame() { return impl_->requestFrame(); }
bool Win32SurfaceView::isFramePending() const noexcept { return impl_->isFramePending(); }
std::uint64_t Win32SurfaceView::framesDelivered() const noexcept { return impl_->framesDelivered(); }
SurfaceState Win32SurfaceView::surfaceState() const noexcept { return impl_->surfaceState(); }
std::uint64_t Win32SurfaceView::generation() const noexcept { return impl_->generation(); }
MigoResult Win32SurfaceView::lastError() const noexcept { return impl_->lastError(); }
float Win32SurfaceView::scale() const noexcept { return impl_->scale(); }

}  // namespace migo::windows_host
