#include <migo/windows/builtin_host_services.hpp>

#include "background.hpp"
#include "brightness.hpp"
#include "geolocation.hpp"
#include "images.hpp"
#include "interaction.hpp"
#include "recorder.hpp"
#include "videos.hpp"

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
#include <commctrl.h>

#include <chrono>
#include <cmath>
#include <cstdio>
#include <cstring>
#include <string>
#include <string_view>
#include <utility>

namespace migo::windows_host {

namespace {

// The ids the services' watches on the App's windows go by.
constexpr UINT_PTR kWindowWatchId = 0x4D69676F + 1;
constexpr UINT_PTR kViewWatchId = 0x4D69676F + 2;
// WDA_EXCLUDEFROMCAPTURE, which older SDKs do not name.
constexpr DWORD kExcludeFromCapture = 0x00000011;
// How long setting the clipboard waits for another App to let go of it.
constexpr int kClipboardAttempts = 10;
constexpr DWORD kClipboardRetryMs = 10;

std::string utf8_of(std::wstring_view wide) {
    if (wide.empty()) return {};
    const int bytes = WideCharToMultiByte(CP_UTF8, 0, wide.data(), static_cast<int>(wide.size()), nullptr, 0, nullptr,
                                          nullptr);
    if (bytes <= 0) return {};
    std::string out(static_cast<std::size_t>(bytes), '\0');
    WideCharToMultiByte(CP_UTF8, 0, wide.data(), static_cast<int>(wide.size()), out.data(), bytes, nullptr, nullptr);
    return out;
}

std::wstring wide_of(std::string_view utf8) {
    if (utf8.empty()) return {};
    const int units = MultiByteToWideChar(CP_UTF8, 0, utf8.data(), static_cast<int>(utf8.size()), nullptr, 0);
    if (units <= 0) return {};
    std::wstring out(static_cast<std::size_t>(units), L'\0');
    MultiByteToWideChar(CP_UTF8, 0, utf8.data(), static_cast<int>(utf8.size()), out.data(), units);
    return out;
}

/// The screen's orientation on a desktop: the game's shape. Square is
/// landscape, as a phone's screen is never square.
const char *orientation_of(LONG width, LONG height) noexcept { return height > width ? "portrait" : "landscape"; }

/// A window's state in the platform's words.
const char *state_of(HWND window) noexcept {
    if (IsIconic(window)) return "minimize";
    if (IsZoomed(window)) return "maximize";
    return "normalize";
}

/// A CSS cursor keyword as the system's cursor: null hides it; what Windows
/// has no cursor for -- context-menu, cell, the zooms -- is the arrow.
HCURSOR cursor_of(std::string_view keyword) noexcept {
    struct Named {
        std::string_view keyword;
        LPCWSTR cursor;
    };
    static const Named kCursors[] = {
        {"pointer", IDC_HAND},      {"text", IDC_IBEAM},        {"vertical-text", IDC_IBEAM},
        {"crosshair", IDC_CROSS},   {"wait", IDC_WAIT},         {"progress", IDC_APPSTARTING},
        {"help", IDC_HELP},         {"move", IDC_SIZEALL},      {"all-scroll", IDC_SIZEALL},
        {"not-allowed", IDC_NO},    {"no-drop", IDC_NO},        {"grab", IDC_HAND},
        {"grabbing", IDC_SIZEALL},  {"n-resize", IDC_SIZENS},   {"s-resize", IDC_SIZENS},
        {"ns-resize", IDC_SIZENS},  {"row-resize", IDC_SIZENS}, {"e-resize", IDC_SIZEWE},
        {"w-resize", IDC_SIZEWE},   {"ew-resize", IDC_SIZEWE},  {"col-resize", IDC_SIZEWE},
        {"ne-resize", IDC_SIZENESW}, {"sw-resize", IDC_SIZENESW}, {"nesw-resize", IDC_SIZENESW},
        {"nw-resize", IDC_SIZENWSE}, {"se-resize", IDC_SIZENWSE}, {"nwse-resize", IDC_SIZENWSE},
    };
    if (keyword == "none") return nullptr;
    for (const Named &named : kCursors) {
        if (named.keyword == keyword) return LoadCursorW(nullptr, named.cursor);
    }
    return LoadCursorW(nullptr, IDC_ARROW);
}

}  // namespace

class Win32BuiltInHostServices::Impl final {
public:
    Impl(Win32SurfaceView &view, std::shared_ptr<HostServiceChannel> channel, std::function<void(bool)> debug_aids,
         const FileLocations &files)
        : view_(view), channel_(std::move(channel)), debug_aids_(std::move(debug_aids)), interaction_(view),
          videos_(view, channel_),
          images_(GetAncestor(view.hwnd(), GA_ROOT), placesOf(files),
                  [this](HWND__ *window, void *file, const std::string &url, const std::string &poster) {
                      return videos_.embed(window, file, url, poster);
                  }),
          recorder_(channel_, placesOf(files).staging) {
        root_ = GetAncestor(view_.hwnd(), GA_ROOT);
        if (root_ != nullptr) {
            reported_state_ = state_of(root_);
            watching_root_ = SetWindowSubclass(root_, &Impl::rootWatch, kWindowWatchId, reinterpret_cast<DWORD_PTR>(this));
        }
        RECT client{};
        GetClientRect(view_.hwnd(), &client);
        reported_orientation_ = orientation_of(client.right, client.bottom);
        watching_view_ = SetWindowSubclass(view_.hwnd(), &Impl::viewWatch, kViewWatchId, reinterpret_cast<DWORD_PTR>(this));
    }

    ~Impl() {
        // The game's lock, its cursor and its capture affinity go with the game.
        view_.unlockPointer();
        view_.setCursor(LoadCursorW(nullptr, IDC_ARROW));
        if (owned_cursor_ != nullptr) DestroyCursor(owned_cursor_);
        if (watching_root_) RemoveWindowSubclass(root_, &Impl::rootWatch, kWindowWatchId);
        if (watching_view_ && view_.hwnd() != nullptr) RemoveWindowSubclass(view_.hwnd(), &Impl::viewWatch, kViewWatchId);
        if (excluded_from_capture_ && root_ != nullptr) SetWindowDisplayAffinity(root_, WDA_NONE);
    }

    void call(MigoHostService service, std::uint32_t method, const Json &request, HostServiceReply reply) {
        switch (service) {
            case MIGO_HOST_SERVICE_INTERACTION:
                interaction_.call(method, request, std::move(reply));
                return;
            case MIGO_HOST_SERVICE_IMAGE:
                images_.call(method, request, std::move(reply));
                return;
            case MIGO_HOST_SERVICE_LOCATION:
                location(method, request, std::move(reply));
                return;
            case MIGO_HOST_SERVICE_VIDEO:
                videos_.call(method, request, std::move(reply));
                return;
            case MIGO_HOST_SERVICE_RECORDER:
                // Commands, every one: heard through the recorder's events.
                recorder_.call(method, request);
                return;
            case MIGO_HOST_SERVICE_CLIPBOARD:
                clipboard(method, request, std::move(reply));
                return;
            case MIGO_HOST_SERVICE_WINDOW:
                window(method, request, std::move(reply));
                return;
            case MIGO_HOST_SERVICE_SCREEN:
                screen(method, request, std::move(reply));
                return;
            case MIGO_HOST_SERVICE_PERMISSION:
                permission(method, request, std::move(reply));
                return;
            default:
                (void)reply.fail("not supported");
                return;
        }
    }

    void pointerLockChanged(bool locked) {
        (void)channel_->post(MIGO_HOST_SERVICE_WINDOW, MIGO_WINDOW_EVENT_POINTER_LOCK_CHANGE,
                             Json::object().set("locked", locked));
    }

private:
    // ---- the clipboard ----------------------------------------------------------------------------

    bool openClipboard() {
        for (int attempt = 0; attempt < kClipboardAttempts; ++attempt) {
            if (OpenClipboard(view_.hwnd())) return true;
            Sleep(kClipboardRetryMs);
        }
        return false;
    }

    void clipboard(std::uint32_t method, const Json &request, HostServiceReply reply) {
        if (method == MIGO_CLIPBOARD_SET_CLIPBOARD_DATA) {
            const std::wstring text = wide_of(request["data"].string());
            if (!openClipboard()) {
                (void)reply.fail("the clipboard is held by another application");
                return;
            }
            EmptyClipboard();
            const std::size_t bytes = (text.size() + 1) * sizeof(wchar_t);
            HGLOBAL memory = GlobalAlloc(GMEM_MOVEABLE, bytes);
            bool placed = false;
            if (memory != nullptr) {
                if (void *target = GlobalLock(memory); target != nullptr) {
                    std::memcpy(target, text.c_str(), bytes);
                    GlobalUnlock(memory);
                    // The clipboard owns the memory once it takes it.
                    placed = SetClipboardData(CF_UNICODETEXT, memory) != nullptr;
                }
                if (!placed) GlobalFree(memory);
            }
            CloseClipboard();
            if (placed) {
                (void)reply.succeed();
            } else {
                (void)reply.fail("the clipboard did not take the text");
            }
            return;
        }
        if (method == MIGO_CLIPBOARD_GET_CLIPBOARD_DATA) {
            if (!openClipboard()) {
                (void)reply.fail("the clipboard is held by another application");
                return;
            }
            std::string data;
            if (HANDLE held = GetClipboardData(CF_UNICODETEXT); held != nullptr) {
                if (const auto *text = static_cast<const wchar_t *>(GlobalLock(held)); text != nullptr) {
                    data = utf8_of(text);
                    GlobalUnlock(held);
                }
            }
            CloseClipboard();
            (void)reply.succeed(Json::object().set("data", std::move(data)));
            return;
        }
        (void)reply.fail("not supported");
    }

    // ---- the window ---------------------------------------------------------------------------------

    void window(std::uint32_t method, const Json &request, HostServiceReply reply) {
        switch (method) {
            case MIGO_WINDOW_SET_WINDOW_SIZE:
                setWindowSize(request, std::move(reply));
                return;
            case MIGO_WINDOW_SET_CURSOR:
                setCursor(request);
                return;
            case MIGO_WINDOW_REQUEST_POINTER_LOCK:
                // A lock taken is reported by the view; a refusal -- the player
                // is not at the game -- here. A lock already held is no change.
                if (!view_.lockPointer()) pointerLockChanged(false);
                return;
            case MIGO_WINDOW_EXIT_POINTER_LOCK:
                view_.unlockPointer();
                return;
            default:
                (void)reply.fail("not supported");
                return;
        }
    }

    /// Why the window cannot be sized or turned now, or null when it can.
    const char *windowRefusal() const noexcept {
        if (root_ == nullptr) return "there is no window to size";
        if (IsIconic(root_)) return "the window is minimized";
        if (IsZoomed(root_)) return "the window is maximized";
        return nullptr;
    }

    /// The window grown or shrunk by what makes the view `width` x `height`
    /// pixels; false, and why, when it cannot be.
    bool resizeForView(LONG width, LONG height, const char *&why) {
        RECT client{};
        RECT outer{};
        GetClientRect(view_.hwnd(), &client);
        GetWindowRect(root_, &outer);
        const LONG new_width = (outer.right - outer.left) + (width - client.right);
        const LONG new_height = (outer.bottom - outer.top) + (height - client.bottom);
        if (new_width <= 0 || new_height <= 0) {
            why = "the window cannot be that size";
            return false;
        }
        return SetWindowPos(root_, nullptr, 0, 0, new_width, new_height, SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE) !=
               FALSE;
    }

    void setWindowSize(const Json &request, HostServiceReply reply) {
        const double width = request["width"].numberOr(-1);
        const double height = request["height"].numberOr(-1);
        if (!std::isfinite(width) || !std::isfinite(height) || width <= 0 || height <= 0 || width > 32767 ||
            height > 32767) {
            (void)reply.fail("width and height must be positive numbers");
            return;
        }
        if (const char *refused = windowRefusal(); refused != nullptr) {
            (void)reply.fail(refused);
            return;
        }
        const char *why = "the window could not be sized";
        if (!resizeForView(static_cast<LONG>(std::lround(width)), static_cast<LONG>(std::lround(height)), why)) {
            (void)reply.fail(why);
            return;
        }
        (void)reply.succeed();
    }

    void setCursor(const Json &request) {
        if (request.contains("keyword")) {
            const std::string &keyword = request["keyword"].string();
            if (keyword == "default" || keyword == "auto") {
                view_.setCursor(LoadCursorW(nullptr, IDC_ARROW));
            } else {
                view_.setCursor(cursor_of(keyword));
            }
            // The system's cursors are shared; one read from a file is ours.
            if (owned_cursor_ != nullptr) {
                DestroyCursor(owned_cursor_);
                owned_cursor_ = nullptr;
            }
            return;
        }
        // A cursor file (.cur, .ani), its hotspot its own.
        const std::wstring path = wide_of(request["path"].string());
        HCURSOR loaded = static_cast<HCURSOR>(
            LoadImageW(nullptr, path.c_str(), IMAGE_CURSOR, 0, 0, LR_LOADFROMFILE | LR_DEFAULTSIZE));
        if (loaded == nullptr) {
            std::fprintf(stderr, "migo windows host kit: setCursor: %s is not a cursor the system reads; unchanged\n",
                         request["path"].string().c_str());
            return;
        }
        view_.setCursor(loaded);
        if (owned_cursor_ != nullptr) DestroyCursor(owned_cursor_);
        owned_cursor_ = loaded;
    }

    // ---- the screen ---------------------------------------------------------------------------------

    void screen(std::uint32_t method, const Json &request, HostServiceReply reply) {
        switch (method) {
            case MIGO_SCREEN_SET_DEVICE_ORIENTATION:
                setOrientation(request, std::move(reply));
                return;
            case MIGO_SCREEN_SET_ENABLE_DEBUG:
                if (debug_aids_) debug_aids_(request["enableDebug"].boolOr(false));
                return;
            case MIGO_SCREEN_SET_VISUAL_EFFECT_ON_CAPTURE: {
                const std::string &effect = request["visualEffect"].string();
                if (effect != "none" && effect != "hidden") {
                    (void)reply.fail("visualEffect must be none or hidden");
                    return;
                }
                const bool hidden = effect == "hidden";
                if (root_ == nullptr || !SetWindowDisplayAffinity(root_, hidden ? kExcludeFromCapture : WDA_NONE)) {
                    (void)reply.fail("this Windows cannot keep a window out of screenshots");
                    return;
                }
                excluded_from_capture_ = hidden;
                (void)reply.succeed();
                return;
            }
            case MIGO_SCREEN_GET_BRIGHTNESS:
                // Asked of the monitor over its cable, or of WMI: off the GUI thread.
                runInBackground([window = view_.hwnd(), reply]() mutable {
                    if (const std::optional<double> value = screenBrightness(window)) {
                        (void)reply.succeed(Json::object().set("value", *value));
                    } else {
                        (void)reply.fail(kNoBrightness);
                    }
                });
                return;
            case MIGO_SCREEN_SET_BRIGHTNESS: {
                const double value = request["value"].numberOr(std::nan(""));
                if (!std::isfinite(value)) {
                    (void)reply.fail("value is not a number");
                    return;
                }
                runInBackground([window = view_.hwnd(), value, reply]() mutable {
                    const std::string error = setScreenBrightness(window, value);
                    (void)(error.empty() ? reply.succeed() : reply.fail(error));
                });
                return;
            }
            default:
                // A screenshot, a recording: Windows tells an App of neither.
                // The observers are commands, and a command's failure reaches
                // no one.
                (void)reply.fail("not supported");
                return;
        }
    }

    void setOrientation(const Json &request, HostServiceReply reply) {
        const std::string &value = request["value"].string();
        if (value != "portrait" && value != "landscape") {
            (void)reply.fail("value must be portrait or landscape");
            return;
        }
        if (const char *refused = windowRefusal(); refused != nullptr) {
            (void)reply.fail(refused);
            return;
        }
        RECT client{};
        GetClientRect(view_.hwnd(), &client);
        if (value == orientation_of(client.right, client.bottom)) {
            (void)reply.succeed();
            return;
        }
        // The window's frame comes with it: turned, it must fit the work area.
        RECT outer{};
        GetWindowRect(root_, &outer);
        const LONG turned_width = (outer.right - outer.left) - client.right + client.bottom;
        const LONG turned_height = (outer.bottom - outer.top) - client.bottom + client.right;
        MONITORINFO monitor{};
        monitor.cbSize = sizeof monitor;
        if (GetMonitorInfoW(MonitorFromWindow(root_, MONITOR_DEFAULTTONEAREST), &monitor) &&
            (turned_width > monitor.rcWork.right - monitor.rcWork.left ||
             turned_height > monitor.rcWork.bottom - monitor.rcWork.top)) {
            (void)reply.fail("the screen cannot hold the window turned");
            return;
        }
        const char *why = "the window could not be turned";
        if (!resizeForView(client.bottom, client.right, why)) {
            (void)reply.fail(why);
            return;
        }
        (void)reply.succeed();
    }

    // ---- permission -------------------------------------------------------------------------------

    // ---- location ---------------------------------------------------------------------------------

    void location(std::uint32_t method, const Json &request, HostServiceReply reply) {
        // As long as the Android SDK waits for a position.
        constexpr std::chrono::milliseconds kWait{10'000};
        switch (method) {
            case MIGO_LOCATION_GET_LOCATION: {
                const bool high = request["isHighAccuracy"].boolOr(false);
                const double expiry = request["highAccuracyExpireTime"].numberOr(0);
                const bool altitude = request["altitude"].boolOr(false);
                const std::chrono::milliseconds wait =
                    high && std::isfinite(expiry) && expiry >= 1
                        ? std::chrono::milliseconds(static_cast<std::int64_t>(std::min(expiry, 3.6e6)))
                        : kWait;
                Geolocation::locate(high ? Geolocation::Accuracy::Exact : Geolocation::Accuracy::Street, wait,
                                    [reply, altitude](const Geolocation::Outcome &outcome) mutable {
                                        if (!outcome.position.has_value()) {
                                            (void)reply.fail(outcome.error);
                                            return;
                                        }
                                        const Geolocation::Position &at = *outcome.position;
                                        (void)reply.succeed(
                                            Json::object()
                                                .set("latitude", at.latitude)
                                                .set("longitude", at.longitude)
                                                .set("speed", at.speed.value_or(0))
                                                .set("accuracy", at.accuracy)
                                                .set("altitude", altitude ? at.altitude.value_or(0) : 0.0)
                                                .set("verticalAccuracy", at.altitude_accuracy.value_or(0))
                                                .set("horizontalAccuracy", at.accuracy));
                                    });
                return;
            }
            case MIGO_LOCATION_GET_FUZZY_LOCATION:
                Geolocation::locate(Geolocation::Accuracy::City, kWait,
                                    [reply](const Geolocation::Outcome &outcome) mutable {
                                        if (!outcome.position.has_value()) {
                                            (void)reply.fail(outcome.error);
                                            return;
                                        }
                                        (void)reply.succeed(Json::object()
                                                                .set("latitude", outcome.position->latitude)
                                                                .set("longitude", outcome.position->longitude));
                                    });
                return;
            default:
                (void)reply.fail("not supported");
                return;
        }
    }

    void permission(std::uint32_t method, const Json &request, HostServiceReply reply) {
        if (method != MIGO_PERMISSION_REQUEST_SCOPE) {
            (void)reply.fail("not supported");
            return;
        }
        const auto scope = static_cast<MigoScope>(request["scopeId"].numberOr(-1));
        switch (scope) {
            case MIGO_SCOPE_USER_LOCATION:
            case MIGO_SCOPE_USER_LOCATION_BACKGROUND:
                // Windows' privacy settings decide, as the player keeps them.
                Geolocation::decide([channel = channel_, scope, reply](const Geolocation::Outcome &outcome) mutable {
                    if (outcome.allowed) {
                        (void)channel->setScopeState(scope, MIGO_SCOPE_STATE_GRANTED);
                        (void)reply.succeed();
                    } else if (outcome.denied) {
                        (void)channel->setScopeState(scope, MIGO_SCOPE_STATE_DENIED);
                        (void)reply.fail("auth deny");
                    } else {
                        // Nobody decided: there is no location service to ask.
                        (void)reply.fail(outcome.error);
                    }
                });
                return;
            case MIGO_SCOPE_RECORD:
                // A desktop does not gate an App's microphone by scope; the
                // privacy settings' switch is the player's, and a recording it
                // keeps from the App fails as it starts.
            case MIGO_SCOPE_WRITE_PHOTOS_ALBUM:
                // Recorded before the answer, so content acting on a grant finds it.
                (void)channel_->setScopeState(scope, MIGO_SCOPE_STATE_GRANTED);
                (void)reply.succeed();
                return;
            default:
                (void)reply.fail("auth deny");
                return;
        }
    }

    // ---- what the windows report ----------------------------------------------------------------------

    static LRESULT CALLBACK rootWatch(HWND window, UINT message, WPARAM wparam, LPARAM lparam, UINT_PTR,
                                      DWORD_PTR reference) {
        const LRESULT result = DefSubclassProc(window, message, wparam, lparam);
        auto *impl = reinterpret_cast<Impl *>(reference);
        if (message == WM_SIZE) {
            const char *state = state_of(window);
            if (std::strcmp(state, impl->reported_state_) != 0) {
                impl->reported_state_ = state;
                (void)impl->channel_->post(MIGO_HOST_SERVICE_WINDOW, MIGO_WINDOW_EVENT_WINDOW_STATE_CHANGE,
                                           Json::object().set("state", state));
            }
        } else if (message == WM_NCDESTROY) {
            impl->watching_root_ = false;
            RemoveWindowSubclass(window, &Impl::rootWatch, kWindowWatchId);
        }
        return result;
    }

    /// The game turns when its view does, whatever turned it -- the window
    /// dragged to the other shape, the App's layout, or setDeviceOrientation --
    /// and content hears it once it crosses, as a turn on a phone, not on every
    /// resize.
    static LRESULT CALLBACK viewWatch(HWND window, UINT message, WPARAM wparam, LPARAM lparam, UINT_PTR,
                                      DWORD_PTR reference) {
        const LRESULT result = DefSubclassProc(window, message, wparam, lparam);
        auto *impl = reinterpret_cast<Impl *>(reference);
        if (message == WM_SIZE && LOWORD(lparam) > 0 && HIWORD(lparam) > 0) {
            const char *orientation = orientation_of(LOWORD(lparam), HIWORD(lparam));
            if (std::strcmp(orientation, impl->reported_orientation_) != 0) {
                impl->reported_orientation_ = orientation;
                (void)impl->channel_->post(MIGO_HOST_SERVICE_SCREEN, MIGO_SCREEN_EVENT_DEVICE_ORIENTATION_CHANGE,
                                           Json::object().set("value", orientation));
            }
        } else if (message == WM_NCDESTROY) {
            impl->watching_view_ = false;
            RemoveWindowSubclass(window, &Impl::viewWatch, kViewWatchId);
        }
        return result;
    }

    Win32SurfaceView &view_;
    const std::shared_ptr<HostServiceChannel> channel_;
    const std::function<void(bool)> debug_aids_;
    HWND root_ = nullptr;
    bool watching_root_ = false;
    bool watching_view_ = false;
    const char *reported_state_ = "normalize";
    const char *reported_orientation_ = "landscape";
    HCURSOR owned_cursor_ = nullptr;
    bool excluded_from_capture_ = false;
    Interaction interaction_;
    // Before the image service, so they outlive the viewer that shows a video with them.
    Videos videos_;
    Images images_;
    Recorder recorder_;

public:
    void setRunning(bool running) { videos_.setRunning(running); }

private:

    static Images::Places placesOf(const FileLocations &files) {
        Images::Places places = Images::Places::system();
        if (!files.staging.empty()) places.staging = files.staging.wstring();
        if (!files.album.empty()) places.album = files.album.wstring();
        return places;
    }
};

Win32BuiltInHostServices::Win32BuiltInHostServices(Win32SurfaceView &view, std::shared_ptr<HostServiceChannel> channel,
                                                   std::function<void(bool on)> debug_aids, FileLocations files)
    : impl_(std::make_unique<Impl>(view, std::move(channel), std::move(debug_aids), files)) {}

Win32BuiltInHostServices::~Win32BuiltInHostServices() = default;

std::uint64_t Win32BuiltInHostServices::services() const noexcept {
    std::uint64_t builtIn = 0;
    builtIn |= hostServiceBit(MIGO_HOST_SERVICE_INTERACTION);
    builtIn |= hostServiceBit(MIGO_HOST_SERVICE_IMAGE);
    builtIn |= hostServiceBit(MIGO_HOST_SERVICE_LOCATION);
    builtIn |= hostServiceBit(MIGO_HOST_SERVICE_RECORDER);
    builtIn |= hostServiceBit(MIGO_HOST_SERVICE_VIDEO);
    builtIn |= hostServiceBit(MIGO_HOST_SERVICE_CLIPBOARD);
    builtIn |= hostServiceBit(MIGO_HOST_SERVICE_WINDOW);
    builtIn |= hostServiceBit(MIGO_HOST_SERVICE_SCREEN);
    builtIn |= hostServiceBit(MIGO_HOST_SERVICE_PERMISSION);
    return builtIn;
}

void Win32BuiltInHostServices::call(MigoHostService service, std::uint32_t method, const Json &request,
                                    HostServiceReply reply) {
    impl_->call(service, method, request, std::move(reply));
}

void Win32BuiltInHostServices::pointerLockChanged(bool locked) { impl_->pointerLockChanged(locked); }

void Win32BuiltInHostServices::setRunning(bool running) { impl_->setRunning(running); }

}  // namespace migo::windows_host
