#include "debug_panel.hpp"
#include "win32_text.hpp"

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

#include <migo/windows/json.hpp>

#include <algorithm>
#include <cmath>
#include <cstdio>
#include <exception>
#include <mutex>
#include <string_view>

namespace migo::windows_host {

namespace {

constexpr wchar_t kPanelClass[] = L"MigoDebugPanel";
constexpr UINT_PTR kRefreshTimer = 1;
constexpr UINT kRefreshMs = 500;
constexpr UINT_PTR kFollowId = 0x4D69676F + 3;
// How opaque the panel is: the game shows through it.
constexpr BYTE kOpacity = 210;
constexpr int kMargin = 6;
// The Session's buffer is measured, then copied; lines written between the two
// make the copy too small, so it is measured again, this many times at most.
constexpr int kCopyAttempts = 4;

[[noreturn]] void fatal(const char *why) {
    std::fprintf(stderr, "migo windows host kit: %s\n", why);
    std::fflush(stderr);
    std::terminate();
}

wchar_t mark_of(int level) noexcept {
    switch (level) {
        case 3: return L'E';
        case 2: return L'W';
        case 0: return L'D';
        default: return L'I';
    }
}

COLORREF colour_of(int level) noexcept {
    switch (level) {
        case 3: return RGB(0xf0, 0x54, 0x4f);
        case 2: return RGB(0xff, 0xb8, 0x4d);
        case 0: return RGB(0x9e, 0x9e, 0x9e);
        default: return RGB(0xe0, 0xe0, 0xe0);
    }
}

}  // namespace

struct DebugPanelProcedures {
    static LRESULT CALLBACK procedure(HWND window, UINT message, WPARAM wparam, LPARAM lparam);
    static LRESULT CALLBACK follow(HWND window, UINT message, WPARAM wparam, LPARAM lparam, UINT_PTR id,
                                   DWORD_PTR reference);
};

DebugPanel::DebugPanel(MigoSession *session, Win32SurfaceView &view) : session_(session), view_(view) {
    static std::once_flag registered;
    std::call_once(registered, [] {
        WNDCLASSEXW panel{};
        panel.cbSize = sizeof panel;
        panel.lpfnWndProc = &DebugPanelProcedures::procedure;
        panel.hInstance = GetModuleHandleW(nullptr);
        panel.hCursor = LoadCursorW(nullptr, IDC_ARROW);
        panel.lpszClassName = kPanelClass;
        if (RegisterClassExW(&panel) == 0) fatal("RegisterClassExW failed for the debugging panel");
    });
    HWND view_window = view_.hwnd();
    root_ = GetAncestor(view_window, GA_ROOT);
    // Layered and transparent: the mouse goes through it to the game; a tool
    // window, never activated: the keyboard stays the game's.
    window_ = CreateWindowExW(WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW, kPanelClass,
                              L"", WS_POPUP, 0, 0, 1, 1, root_, nullptr, GetModuleHandleW(nullptr), this);
    if (window_ == nullptr) fatal("CreateWindowExW failed for the debugging panel");
    SetLayeredWindowAttributes(window_, 0, kOpacity, LWA_ALPHA);
    makeFont(GetDpiForWindow(view_window));
    if (root_ != nullptr && root_ != view_window) {
        following_root_ = SetWindowSubclass(root_, &DebugPanelProcedures::follow, kFollowId,
                                            reinterpret_cast<DWORD_PTR>(this)) != FALSE;
    }
    following_view_ = SetWindowSubclass(view_window, &DebugPanelProcedures::follow, kFollowId,
                                        reinterpret_cast<DWORD_PTR>(this)) != FALSE;
    SetTimer(window_, kRefreshTimer, kRefreshMs, nullptr);
    refresh();
    place();
}

DebugPanel::~DebugPanel() {
    if (following_root_) RemoveWindowSubclass(root_, &DebugPanelProcedures::follow, kFollowId);
    if (following_view_ && view_.hwnd() != nullptr) {
        RemoveWindowSubclass(view_.hwnd(), &DebugPanelProcedures::follow, kFollowId);
    }
    if (window_ != nullptr) {
        // No message reaches this object once the window is gone.
        SetWindowLongPtrW(window_, GWLP_USERDATA, 0);
        DestroyWindow(window_);
    }
    if (font_ != nullptr) DeleteObject(font_);
}

LRESULT CALLBACK DebugPanelProcedures::procedure(HWND window, UINT message, WPARAM wparam, LPARAM lparam) {
    if (message == WM_NCCREATE) {
        const auto *create = reinterpret_cast<const CREATESTRUCTW *>(lparam);
        SetWindowLongPtrW(window, GWLP_USERDATA, reinterpret_cast<LONG_PTR>(create->lpCreateParams));
    }
    auto *panel = reinterpret_cast<DebugPanel *>(GetWindowLongPtrW(window, GWLP_USERDATA));
    if (panel != nullptr) {
        switch (message) {
            case WM_TIMER:
                if (wparam == kRefreshTimer) {
                    panel->refresh();
                    return 0;
                }
                break;
            case WM_PAINT:
                panel->paint();
                return 0;
            case WM_ERASEBKGND:
                return 1;
            case WM_MOUSEACTIVATE:
                return MA_NOACTIVATE;
            case WM_NCHITTEST:
                return HTTRANSPARENT;
            default:
                break;
        }
    }
    return DefWindowProcW(window, message, wparam, lparam);
}

LRESULT CALLBACK DebugPanelProcedures::follow(HWND window, UINT message, WPARAM wparam, LPARAM lparam, UINT_PTR,
                                              DWORD_PTR reference) {
    const LRESULT result = DefSubclassProc(window, message, wparam, lparam);
    auto *panel = reinterpret_cast<DebugPanel *>(reference);
    switch (message) {
        case WM_WINDOWPOSCHANGED:
        case WM_SHOWWINDOW:
            panel->place();
            break;
        case WM_DPICHANGED_AFTERPARENT:
        case WM_DPICHANGED:
            panel->makeFont(GetDpiForWindow(panel->view_.hwnd()));
            panel->place();
            break;
        case WM_NCDESTROY:
            if (window == panel->root_) {
                panel->following_root_ = false;
            } else {
                panel->following_view_ = false;
            }
            RemoveWindowSubclass(window, &DebugPanelProcedures::follow, kFollowId);
            break;
        default:
            break;
    }
    return result;
}

void DebugPanel::makeFont(unsigned dpi) {
    if (dpi == 0) dpi = USER_DEFAULT_SCREEN_DPI;
    if (dpi == font_dpi_ && font_ != nullptr) return;
    HFONT font = CreateFontW(-MulDiv(9, static_cast<int>(dpi), 72), 0, 0, 0, FW_NORMAL, FALSE, FALSE, FALSE,
                             DEFAULT_CHARSET, OUT_DEFAULT_PRECIS, CLIP_DEFAULT_PRECIS, CLEARTYPE_QUALITY,
                             FIXED_PITCH | FF_MODERN, L"Consolas");
    if (font == nullptr) return;
    if (font_ != nullptr) DeleteObject(font_);
    font_ = font;
    font_dpi_ = dpi;
}

void DebugPanel::refresh() {
    readConsole();
    // The rate is the change in frames delivered since the last sample.
    const std::uint64_t delivered = view_.framesDelivered();
    const auto now = std::chrono::steady_clock::now();
    if (sampled_ && now > sampled_at_ && delivered >= sampled_frames_) {
        const double seconds = std::chrono::duration<double>(now - sampled_at_).count();
        frames_per_second_ = static_cast<double>(delivered - sampled_frames_) / seconds;
    }
    sampled_ = true;
    sampled_frames_ = delivered;
    sampled_at_ = now;
    InvalidateRect(window_, nullptr, FALSE);
}

void DebugPanel::readConsole() {
    std::size_t length = 0;
    MigoResult result = migo_session_copy_console_logs(session_, cursor_, nullptr, 0, &length);
    for (int attempt = 0; attempt < kCopyAttempts && result == MIGO_ERROR_INVALID_ARGUMENT; ++attempt) {
        buffer_.resize(length + 1);
        result = migo_session_copy_console_logs(session_, cursor_, buffer_.data(), buffer_.size(), &length);
    }
    if (result != MIGO_OK) return;
    const std::optional<Json> read = Json::parse(std::string_view(buffer_.data(), length));
    if (!read.has_value()) return;
    for (const Json &log : (*read)["logs"].items()) {
        const int level = static_cast<int>(log["l"].numberOr(1));
        const std::wstring text = wideOf(log["m"].string());
        // A line content wrote with breaks in it is shown as the lines it holds.
        std::size_t start = 0;
        bool first = true;
        while (start <= text.size()) {
            std::size_t end = text.find(L'\n', start);
            if (end == std::wstring::npos) end = text.size();
            std::wstring shown = first ? std::wstring(1, mark_of(level)) + L' ' : std::wstring(L"  ");
            shown.append(text, start, end - start);
            if (!shown.empty() && shown.back() == L'\r') shown.pop_back();
            lines_.push_back(Line{level, std::move(shown)});
            first = false;
            start = end + 1;
        }
    }
    while (lines_.size() > kLineLimit) lines_.pop_front();
    cursor_ = static_cast<std::uint64_t>((*read)["cursor"].numberOr(static_cast<double>(cursor_)));
}

/// Over the top 40% of the game, its full width; hidden while the game is.
void DebugPanel::place() {
    HWND view_window = view_.hwnd();
    if (view_window == nullptr || !IsWindowVisible(view_window)) {
        ShowWindow(window_, SW_HIDE);
        return;
    }
    RECT client{};
    GetClientRect(view_window, &client);
    POINT origin{0, 0};
    ClientToScreen(view_window, &origin);
    const int width = std::max(1, static_cast<int>(client.right));
    const int height = std::max(1, static_cast<int>(std::lround(static_cast<double>(client.bottom) * 0.4)));
    SetWindowPos(window_, nullptr, origin.x, origin.y, width, height,
                 SWP_NOACTIVATE | SWP_NOZORDER | SWP_NOOWNERZORDER | SWP_SHOWWINDOW);
}

void DebugPanel::paint() {
    PAINTSTRUCT painting;
    HDC target = BeginPaint(window_, &painting);
    RECT area{};
    GetClientRect(window_, &area);
    // Drawn off screen and copied once: the panel redraws twice a second over
    // a game that redraws every frame, and must not flicker.
    HDC canvas = CreateCompatibleDC(target);
    HBITMAP bitmap = CreateCompatibleBitmap(target, std::max(1L, area.right), std::max(1L, area.bottom));
    HGDIOBJ old_bitmap = SelectObject(canvas, bitmap);
    HBRUSH background = CreateSolidBrush(RGB(20, 20, 20));
    FillRect(canvas, &area, background);
    DeleteObject(background);
    HGDIOBJ old_font = SelectObject(canvas, font_ != nullptr ? static_cast<HGDIOBJ>(font_) : GetStockObject(ANSI_FIXED_FONT));
    SetBkMode(canvas, TRANSPARENT);
    TEXTMETRICW metrics{};
    GetTextMetricsW(canvas, &metrics);
    const int line_height = std::max(1, static_cast<int>(metrics.tmHeight + metrics.tmExternalLeading));

    wchar_t header[64];
    if (frames_per_second_.has_value()) {
        std::swprintf(header, sizeof header / sizeof header[0], L"%.1f frames/s", *frames_per_second_);
    } else {
        std::swprintf(header, sizeof header / sizeof header[0], L"-- frames/s");
    }
    SetTextColor(canvas, RGB(0xff, 0xff, 0xff));
    RECT header_box{kMargin, kMargin / 2, area.right - kMargin, kMargin / 2 + line_height};
    DrawTextW(canvas, header, -1, &header_box, DT_SINGLELINE | DT_NOPREFIX | DT_LEFT | DT_TOP);

    // The newest lines that fit, the newest at the bottom.
    const int top = header_box.bottom + 2;
    const int fits = std::max(0, static_cast<int>((area.bottom - top - kMargin / 2) / line_height));
    const std::size_t shown = std::min(lines_.size(), static_cast<std::size_t>(fits));
    int y = top;
    for (std::size_t index = lines_.size() - shown; index < lines_.size(); ++index) {
        const Line &line = lines_[index];
        SetTextColor(canvas, colour_of(line.level));
        RECT box{kMargin, y, area.right - kMargin, y + line_height};
        DrawTextW(canvas, line.text.c_str(), static_cast<int>(line.text.size()), &box,
                  DT_SINGLELINE | DT_NOPREFIX | DT_LEFT | DT_TOP | DT_END_ELLIPSIS);
        y += line_height;
    }

    BitBlt(target, 0, 0, area.right, area.bottom, canvas, 0, 0, SRCCOPY);
    SelectObject(canvas, old_font);
    SelectObject(canvas, old_bitmap);
    DeleteObject(bitmap);
    DeleteDC(canvas);
    EndPaint(window_, &painting);
}

}  // namespace migo::windows_host
