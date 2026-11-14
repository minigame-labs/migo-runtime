#include "video_controls.hpp"

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
#include <d2d1.h>
#include <windowsx.h>
#include <wrl/client.h>

#include <algorithm>
#include <cmath>
#include <cstdio>
#include <exception>
#include <mutex>
#include <optional>

namespace migo::windows_host {

namespace {

using Microsoft::WRL::ComPtr;

constexpr wchar_t kOverlayClass[] = L"MigoVideoControls";
constexpr wchar_t kBarClass[] = L"MigoVideoBar";
constexpr wchar_t kProgressClass[] = L"MigoVideoProgress";
// What is not a control: neither drawn nor hit.
constexpr COLORREF kKey = RGB(255, 0, 254);
constexpr COLORREF kShade = RGB(24, 24, 24);
constexpr UINT_PTR kHideBarTimer = 1;
constexpr UINT_PTR kClickTimer = 2;
constexpr UINT kBarShownMs = 3000;
// In logical pixels.
constexpr int kBarHeight = 36;
constexpr int kCenterSize = 56;
constexpr int kClockWidth = 52;
constexpr int kDragHeight = 22;
constexpr int kDragWidth = 120;
// The controls' ids.
constexpr int kCenterPlay = 1;
constexpr int kBarPlay = 2;
constexpr int kFullScreen = 3;
constexpr UINT_PTR kButtonWatch = 0x4D69676F + 10;

enum class Glyph { Play, CenterPlay, Pause, EnterFullScreen, ExitFullScreen };

[[noreturn]] void fatal(const char *why) {
    std::fprintf(stderr, "migo windows host kit: %s\n", why);
    std::fflush(stderr);
    std::terminate();
}

D2D1_COLOR_F d2d(COLORREF color, float alpha = 1) {
    return D2D1::ColorF(static_cast<float>(GetRValue(color)) / 255.0F, static_cast<float>(GetGValue(color)) / 255.0F,
                        static_cast<float>(GetBValue(color)) / 255.0F, alpha);
}

}  // namespace

std::wstring videoClock(double seconds) {
    const long long whole = std::isfinite(seconds) ? static_cast<long long>(std::floor(std::max(0.0, seconds))) : 0;
    wchar_t text[32];
    if (whole >= 3600) {
        std::swprintf(text, 32, L"%lld:%02lld:%02lld", whole / 3600, whole / 60 % 60, whole % 60);
    } else {
        std::swprintf(text, 32, L"%lld:%02lld", whole / 60, whole % 60);
    }
    return text;
}

class VideoControls::Impl final {
public:
    Impl(HWND owner, VideoControlled &player) : player_(player) {
        static std::once_flag registered;
        std::call_once(registered, [] {
            for (const auto &[name, procedure] :
                 {std::pair<const wchar_t *, WNDPROC>{kOverlayClass, &Impl::overlayProcedure},
                  {kBarClass, &Impl::barProcedure},
                  {kProgressClass, &Impl::progressProcedure}}) {
                WNDCLASSEXW kind{};
                kind.cbSize = sizeof kind;
                kind.lpfnWndProc = procedure;
                kind.hInstance = GetModuleHandleW(nullptr);
                kind.hCursor = LoadCursorW(nullptr, IDC_HAND);
                kind.lpszClassName = name;
                if (RegisterClassExW(&kind) == 0) fatal("RegisterClassExW failed for the video controls");
            }
        });
        D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, d2d_.GetAddressOf());
        shade_ = CreateSolidBrush(kShade);
        overlay_ = CreateWindowExW(WS_EX_LAYERED | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW, kOverlayClass, L"",
                                   WS_POPUP | WS_CLIPCHILDREN, 0, 0, 1, 1, owner, nullptr, GetModuleHandleW(nullptr),
                                   this);
        if (overlay_ == nullptr) fatal("CreateWindowExW failed for the video controls");
        SetLayeredWindowAttributes(overlay_, kKey, 0, LWA_COLORKEY);
        center_ = button(overlay_, L"play", kCenterPlay);
        bar_ = CreateWindowExW(0, kBarClass, L"", WS_CHILD | WS_CLIPCHILDREN, 0, 0, 1, 1, overlay_, nullptr,
                               GetModuleHandleW(nullptr), this);
        barPlay_ = button(bar_, L"play or pause", kBarPlay);
        fullScreen_ = button(bar_, L"full screen", kFullScreen);
        time_ = CreateWindowExW(0, L"STATIC", L"0:00", WS_CHILD | WS_VISIBLE | SS_CENTER | SS_CENTERIMAGE, 0, 0, 1, 1,
                                bar_, nullptr, GetModuleHandleW(nullptr), nullptr);
        total_ = CreateWindowExW(0, L"STATIC", L"0:00", WS_CHILD | WS_VISIBLE | SS_CENTER | SS_CENTERIMAGE, 0, 0, 1, 1,
                                 bar_, nullptr, GetModuleHandleW(nullptr), nullptr);
        progress_ = CreateWindowExW(0, kProgressClass, L"progress", WS_CHILD | WS_VISIBLE, 0, 0, 1, 1, bar_, nullptr,
                                    GetModuleHandleW(nullptr), this);
        dragTime_ = CreateWindowExW(0, L"STATIC", L"", WS_CHILD | SS_CENTER | SS_CENTERIMAGE, 0, 0, 1, 1, overlay_,
                                    nullptr, GetModuleHandleW(nullptr), nullptr);
    }

    ~Impl() {
        SetWindowLongPtrW(overlay_, GWLP_USERDATA, 0);
        DestroyWindow(overlay_);
        if (font_ != nullptr) DeleteObject(font_);
        if (shade_ != nullptr) DeleteObject(shade_);
    }

    void configure(const Json &options) {
        controls_ = options["controls"].boolOr(true);
        showProgress_ = options["showProgress"].boolOr(true);
        showProgressInBar_ = options["showProgressInControlMode"].boolOr(true);
        showCenter_ = options["showCenterPlayBtn"].boolOr(true);
        playGesture_ = options["enablePlayGesture"].boolOr(false);
        progressGesture_ = options["enableProgressGesture"].boolOr(true);
        if (!controls_) barShown_ = false;
        refresh();
    }

    bool takesInput() const {
        return controls_ || playGesture_ || progressGesture_ || (showCenter_ && !player_.isPlaying());
    }

    void refresh() {
        const bool playing = player_.isPlaying();
        const double duration = player_.duration();
        const double now = seekTarget_.value_or(player_.position());
        ShowWindow(center_, showCenter_ && !playing && !seekTarget_.has_value() ? SW_SHOWNA : SW_HIDE);
        ShowWindow(bar_, controls_ && barShown_ ? SW_SHOWNA : SW_HIDE);
        ShowWindow(progress_, showProgressInBar_ ? SW_SHOWNA : SW_HIDE);
        ShowWindow(dragTime_, seekTarget_.has_value() ? SW_SHOWNA : SW_HIDE);
        if (playing != barPlayShowsPause_ || player_.isFullScreen() != fullScreenShowsExit_) {
            barPlayShowsPause_ = playing;
            fullScreenShowsExit_ = player_.isFullScreen();
            InvalidateRect(barPlay_, nullptr, FALSE);
            InvalidateRect(fullScreen_, nullptr, FALSE);
        }
        setText(time_, videoClock(now));
        setText(total_, videoClock(duration));
        setText(dragTime_, videoClock(now) + L" / " + videoClock(duration));
        InvalidateRect(progress_, nullptr, FALSE);
        // The progress line, drawn on the overlay itself.
        InvalidateRect(overlay_, nullptr, FALSE);
        if (playing && barShown_) {
            if (!hideScheduled_) {
                hideScheduled_ = true;
                SetTimer(overlay_, kHideBarTimer, kBarShownMs, nullptr);
            }
        }
        if (const bool takes = takesInput(); takes != tookInput_) {
            tookInput_ = takes;
            player_.inputChanged();
        }
    }

    void place(HWND player, int left, int top, int width, int height, float scale, bool shown) {
        if (!shown || width <= 0 || height <= 0) {
            ShowWindow(overlay_, SW_HIDE);
            return;
        }
        if (scale != scale_ || font_ == nullptr) makeFont(scale);
        size_ = SIZE{width, height};
        // Above its player, and no higher: what the App shows over the game stays over it.
        SetWindowPos(overlay_, nullptr, left, top, width, height,
                     SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOOWNERZORDER | SWP_SHOWWINDOW);
        SetWindowPos(player, overlay_, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_NOOWNERZORDER);
        const int bar = px(kBarHeight);
        const int center = px(kCenterSize);
        const int clock = px(kClockWidth);
        MoveWindow(center_, (width - center) / 2, (height - center) / 2, center, center, TRUE);
        MoveWindow(bar_, 0, height - bar, width, bar, TRUE);
        MoveWindow(barPlay_, 0, 0, bar, bar, TRUE);
        MoveWindow(fullScreen_, width - bar, 0, bar, bar, TRUE);
        MoveWindow(time_, bar, 0, clock, bar, TRUE);
        MoveWindow(total_, width - bar - clock, 0, clock, bar, TRUE);
        MoveWindow(progress_, bar + clock, 0, std::max(0, width - 2 * (bar + clock)), bar, TRUE);
        MoveWindow(dragTime_, (width - px(kDragWidth)) / 2, (height - px(kDragHeight)) / 2, px(kDragWidth),
                   px(kDragHeight), TRUE);
        refresh();
    }

    bool mouse(UINT message, WPARAM wparam, LPARAM lparam) {
        const POINT at{GET_X_LPARAM(lparam), GET_Y_LPARAM(lparam)};
        switch (message) {
            case WM_LBUTTONDOWN:
                pressedAt_ = at;
                dragging_ = false;
                return true;
            case WM_MOUSEMOVE: {
                if (!pressedAt_.has_value() || (wparam & MK_LBUTTON) == 0) return false;
                const int across = at.x - pressedAt_->x;
                const int down = at.y - pressedAt_->y;
                if (!dragging_) {
                    // Sideways only: a drag more up than across is not a seek.
                    if (!progressGesture_ || player_.duration() <= 0 || size_.cx <= 0 ||
                        std::abs(across) < GetSystemMetrics(SM_CXDRAG) || std::abs(across) <= std::abs(down)) {
                        return true;
                    }
                    dragging_ = true;
                    dragFrom_ = player_.position();
                }
                const double duration = player_.duration();
                seekTarget_ = std::clamp(dragFrom_ + static_cast<double>(across) / size_.cx * duration, 0.0, duration);
                refresh();
                return true;
            }
            case WM_LBUTTONUP:
                if (!pressedAt_.has_value()) return true;
                pressedAt_.reset();
                if (dragging_) {
                    dragging_ = false;
                    seekEnded();
                    return true;
                }
                // With the play gesture, a click waits until it is not one of two.
                if (playGesture_) {
                    SetTimer(overlay_, kClickTimer, GetDoubleClickTime(), nullptr);
                } else {
                    toggleBar();
                }
                return true;
            case WM_LBUTTONDBLCLK:
                if (!playGesture_) return mouse(WM_LBUTTONDOWN, wparam, lparam);
                KillTimer(overlay_, kClickTimer);
                // Its release is not a click of its own.
                pressedAt_.reset();
                player_.togglePlay();
                return true;
            default:
                return false;
        }
    }

    HWND hwnd() const noexcept { return overlay_; }

private:
    int px(int logical) const { return static_cast<int>(std::lround(static_cast<float>(logical) * scale_)); }

    HWND button(HWND parent, const wchar_t *name, int id) {
        HWND made = CreateWindowExW(0, L"BUTTON", name, WS_CHILD | WS_VISIBLE | BS_OWNERDRAW, 0, 0, 1, 1, parent,
                                    reinterpret_cast<HMENU>(static_cast<INT_PTR>(id)), GetModuleHandleW(nullptr),
                                    nullptr);
        // The keyboard is the game's: a click on a control gives it straight back.
        SetWindowSubclass(made, &Impl::keepFocus, kButtonWatch, 0);
        return made;
    }

    static LRESULT CALLBACK keepFocus(HWND window, UINT message, WPARAM wparam, LPARAM lparam, UINT_PTR, DWORD_PTR) {
        if (message == WM_SETFOCUS && wparam != 0 && IsWindow(reinterpret_cast<HWND>(wparam))) {
            SetFocus(reinterpret_cast<HWND>(wparam));
            return 0;
        }
        if (message == WM_NCDESTROY) RemoveWindowSubclass(window, &Impl::keepFocus, kButtonWatch);
        return DefSubclassProc(window, message, wparam, lparam);
    }

    static void setText(HWND window, const std::wstring &text) {
        wchar_t now[64] = {};
        GetWindowTextW(window, now, 64);
        if (text != now) SetWindowTextW(window, text.c_str());
    }

    void makeFont(float scale) {
        scale_ = scale;
        NONCLIENTMETRICSW metrics{};
        metrics.cbSize = sizeof metrics;
        SystemParametersInfoForDpi(SPI_GETNONCLIENTMETRICS, sizeof metrics, &metrics, 0,
                                   static_cast<UINT>(std::lround(96.0F * scale)));
        HFONT font = CreateFontIndirectW(&metrics.lfMessageFont);
        if (font == nullptr) return;
        for (HWND label : {time_, total_, dragTime_}) SendMessageW(label, WM_SETFONT, reinterpret_cast<WPARAM>(font), TRUE);
        if (font_ != nullptr) DeleteObject(font_);
        font_ = font;
    }

    void toggleBar() {
        if (!controls_) return;
        setBarShown(!barShown_);
    }

    void setBarShown(bool shown) {
        barShown_ = shown;
        KillTimer(overlay_, kHideBarTimer);
        hideScheduled_ = false;
        refresh();
    }

    void seekEnded() {
        if (!seekTarget_.has_value()) return;
        const double target = *seekTarget_;
        seekTarget_.reset();
        player_.seekTo(target);
        refresh();
    }

    double fraction() const {
        const double duration = player_.duration();
        const double now = seekTarget_.value_or(player_.position());
        return duration > 0 ? std::clamp(now / duration, 0.0, 1.0) : 0.0;
    }

    // ---- drawing ----------------------------------------------------------------------------------

    /// `glyph` into `box` on `context`, `background` around it -- the key,
    /// where nothing is to be drawn.
    void drawGlyph(HDC context, const RECT &box, Glyph glyph, COLORREF background) {
        if (d2d_ == nullptr) return;
        if (target_ == nullptr) {
            const D2D1_RENDER_TARGET_PROPERTIES properties = D2D1::RenderTargetProperties(
                D2D1_RENDER_TARGET_TYPE_DEFAULT, D2D1::PixelFormat(DXGI_FORMAT_B8G8R8A8_UNORM, D2D1_ALPHA_MODE_IGNORE));
            if (FAILED(d2d_->CreateDCRenderTarget(&properties, target_.GetAddressOf()))) return;
        }
        if (FAILED(target_->BindDC(context, &box))) return;
        const float s = static_cast<float>(std::min(box.right - box.left, box.bottom - box.top));
        const float x0 = (static_cast<float>(box.right - box.left) - s) / 2;
        const float y0 = (static_cast<float>(box.bottom - box.top) - s) / 2;
        ComPtr<ID2D1SolidColorBrush> white;
        ComPtr<ID2D1SolidColorBrush> dark;
        target_->BeginDraw();
        target_->Clear(d2d(background));
        target_->CreateSolidColorBrush(D2D1::ColorF(1, 1, 1, 1), white.GetAddressOf());
        target_->CreateSolidColorBrush(d2d(RGB(40, 40, 40)), dark.GetAddressOf());
        const auto point = [x0, y0, s](float fx, float fy) { return D2D1::Point2F(x0 + fx * s, y0 + fy * s); };
        const auto triangle = [&](float left, float top, float right, float bottom) {
            ComPtr<ID2D1PathGeometry> path;
            ComPtr<ID2D1GeometrySink> sink;
            if (FAILED(d2d_->CreatePathGeometry(path.GetAddressOf())) || FAILED(path->Open(sink.GetAddressOf()))) return;
            sink->BeginFigure(point(left, top), D2D1_FIGURE_BEGIN_FILLED);
            sink->AddLine(point(right, (top + bottom) / 2));
            sink->AddLine(point(left, bottom));
            sink->EndFigure(D2D1_FIGURE_END_CLOSED);
            if (SUCCEEDED(sink->Close())) target_->FillGeometry(path.Get(), white.Get());
        };
        switch (glyph) {
            case Glyph::CenterPlay:
                target_->FillEllipse(D2D1::Ellipse(point(0.5F, 0.5F), s / 2, s / 2), dark.Get());
                triangle(0.40F, 0.30F, 0.72F, 0.70F);
                break;
            case Glyph::Play:
                triangle(0.32F, 0.25F, 0.75F, 0.75F);
                break;
            case Glyph::Pause:
                target_->FillRectangle(D2D1::RectF(x0 + 0.30F * s, y0 + 0.25F * s, x0 + 0.43F * s, y0 + 0.75F * s), white.Get());
                target_->FillRectangle(D2D1::RectF(x0 + 0.57F * s, y0 + 0.25F * s, x0 + 0.70F * s, y0 + 0.75F * s), white.Get());
                break;
            case Glyph::EnterFullScreen:
            case Glyph::ExitFullScreen: {
                // A corner at each of the four, pointing out to enter, in to leave.
                const bool enter = glyph == Glyph::EnterFullScreen;
                const float stroke = std::max(1.5F, s * 0.06F);
                for (const auto &[cx, cy] : {std::pair<float, float>{0.32F, 0.32F}, {0.68F, 0.32F}, {0.32F, 0.68F}, {0.68F, 0.68F}}) {
                    const float dx = (cx < 0.5F ? 1.0F : -1.0F) * (enter ? 1.0F : -1.0F) * 0.12F;
                    const float dy = (cy < 0.5F ? 1.0F : -1.0F) * (enter ? 1.0F : -1.0F) * 0.12F;
                    target_->DrawLine(point(cx + dx, cy), point(cx, cy), white.Get(), stroke);
                    target_->DrawLine(point(cx, cy), point(cx, cy + dy), white.Get(), stroke);
                }
                break;
            }
        }
        if (target_->EndDraw() == static_cast<HRESULT>(D2DERR_RECREATE_TARGET)) target_.Reset();
    }

    void drawButton(const DRAWITEMSTRUCT &item) {
        switch (item.CtlID) {
            case kCenterPlay:
                drawGlyph(item.hDC, item.rcItem, Glyph::CenterPlay, kKey);
                break;
            case kBarPlay:
                drawGlyph(item.hDC, item.rcItem, barPlayShowsPause_ ? Glyph::Pause : Glyph::Play, kShade);
                break;
            case kFullScreen:
                drawGlyph(item.hDC, item.rcItem, fullScreenShowsExit_ ? Glyph::ExitFullScreen : Glyph::EnterFullScreen,
                          kShade);
                break;
            default:
                break;
        }
    }

    void paintOverlay() {
        PAINTSTRUCT painting;
        HDC context = BeginPaint(overlay_, &painting);
        RECT area{};
        GetClientRect(overlay_, &area);
        HBRUSH key = CreateSolidBrush(kKey);
        FillRect(context, &area, key);
        DeleteObject(key);
        // While the bar is hidden, a thin line of how far it has played.
        if (showProgress_ && !(controls_ && barShown_) && player_.duration() > 0) {
            const int line = std::max(1, px(2));
            RECT track{0, area.bottom - line, area.right, area.bottom};
            HBRUSH dim = CreateSolidBrush(RGB(96, 96, 96));
            FillRect(context, &track, dim);
            DeleteObject(dim);
            track.right = static_cast<LONG>(std::lround(area.right * fraction()));
            HBRUSH bright = CreateSolidBrush(RGB(235, 235, 235));
            FillRect(context, &track, bright);
            DeleteObject(bright);
        }
        EndPaint(overlay_, &painting);
    }

    void paintProgress() {
        PAINTSTRUCT painting;
        HDC context = BeginPaint(progress_, &painting);
        RECT area{};
        GetClientRect(progress_, &area);
        FillRect(context, &area, shade_);
        const int thick = std::max(2, px(3));
        const int middle = area.bottom / 2;
        const int inset = px(8);
        RECT track{inset, middle - thick / 2, std::max<LONG>(inset, area.right - inset), middle - thick / 2 + thick};
        HBRUSH dim = CreateSolidBrush(RGB(110, 110, 110));
        FillRect(context, &track, dim);
        DeleteObject(dim);
        const LONG reached = track.left + static_cast<LONG>(std::lround((track.right - track.left) * fraction()));
        RECT played{track.left, track.top, reached, track.bottom};
        HBRUSH bright = CreateSolidBrush(RGB(235, 235, 235));
        FillRect(context, &played, bright);
        const int knob = px(6);
        RECT thumb{reached - knob, middle - knob, reached + knob, middle + knob};
        HGDIOBJ previous_brush = SelectObject(context, bright);
        HGDIOBJ previous_pen = SelectObject(context, GetStockObject(NULL_PEN));
        Ellipse(context, thumb.left, thumb.top, thumb.right, thumb.bottom);
        SelectObject(context, previous_pen);
        SelectObject(context, previous_brush);
        DeleteObject(bright);
        EndPaint(progress_, &painting);
    }

    /// A press or a drag along the progress bar: where it goes.
    void slide(int x, bool done) {
        RECT area{};
        GetClientRect(progress_, &area);
        const int inset = px(8);
        const double width = std::max<LONG>(1, area.right - 2 * inset);
        const double duration = player_.duration();
        if (duration <= 0) return;
        const double target = std::clamp((x - inset) / width, 0.0, 1.0) * duration;
        if (done) {
            seekTarget_.reset();
            player_.seekTo(target);
        } else {
            seekTarget_ = target;
        }
        refresh();
    }

    // ---- the windows' procedures ---------------------------------------------------------------------

    static Impl *of(HWND window) { return reinterpret_cast<Impl *>(GetWindowLongPtrW(window, GWLP_USERDATA)); }

    static void remember(HWND window, UINT message, LPARAM lparam) {
        if (message == WM_NCCREATE) {
            const auto *create = reinterpret_cast<const CREATESTRUCTW *>(lparam);
            SetWindowLongPtrW(window, GWLP_USERDATA, reinterpret_cast<LONG_PTR>(create->lpCreateParams));
        }
    }

    /// A control pressed: what it drives.
    void command(int id) {
        switch (id) {
            case kCenterPlay:
            case kBarPlay:
                player_.togglePlay();
                break;
            case kFullScreen:
                player_.toggleFullScreen();
                break;
            default:
                break;
        }
        // The bar stays while it is used.
        if (barShown_) setBarShown(true);
    }

    static LRESULT CALLBACK overlayProcedure(HWND window, UINT message, WPARAM wparam, LPARAM lparam) {
        remember(window, message, lparam);
        Impl *impl = of(window);
        if (impl != nullptr) {
            switch (message) {
                case WM_PAINT:
                    impl->paintOverlay();
                    return 0;
                case WM_ERASEBKGND:
                    return 1;
                case WM_MOUSEACTIVATE:
                    return MA_NOACTIVATE;
                case WM_COMMAND:
                    if (HIWORD(wparam) == BN_CLICKED) impl->command(LOWORD(wparam));
                    return 0;
                case WM_DRAWITEM:
                    impl->drawButton(*reinterpret_cast<const DRAWITEMSTRUCT *>(lparam));
                    return TRUE;
                case WM_CTLCOLORSTATIC:
                    SetTextColor(reinterpret_cast<HDC>(wparam), RGB(255, 255, 255));
                    SetBkColor(reinterpret_cast<HDC>(wparam), kShade);
                    return reinterpret_cast<LRESULT>(impl->shade_);
                case WM_TIMER:
                    KillTimer(window, wparam);
                    if (wparam == kHideBarTimer) {
                        impl->hideScheduled_ = false;
                        if (impl->player_.isPlaying() && !impl->seekTarget_.has_value()) impl->setBarShown(false);
                    } else if (wparam == kClickTimer) {
                        impl->toggleBar();
                    }
                    return 0;
                default:
                    break;
            }
        }
        return DefWindowProcW(window, message, wparam, lparam);
    }

    static LRESULT CALLBACK barProcedure(HWND window, UINT message, WPARAM wparam, LPARAM lparam) {
        remember(window, message, lparam);
        Impl *impl = of(window);
        if (impl != nullptr) {
            switch (message) {
                case WM_ERASEBKGND: {
                    RECT area{};
                    GetClientRect(window, &area);
                    FillRect(reinterpret_cast<HDC>(wparam), &area, impl->shade_);
                    return 1;
                }
                case WM_COMMAND:
                    if (HIWORD(wparam) == BN_CLICKED) impl->command(LOWORD(wparam));
                    return 0;
                case WM_DRAWITEM:
                    impl->drawButton(*reinterpret_cast<const DRAWITEMSTRUCT *>(lparam));
                    return TRUE;
                case WM_CTLCOLORSTATIC:
                    SetTextColor(reinterpret_cast<HDC>(wparam), RGB(255, 255, 255));
                    SetBkColor(reinterpret_cast<HDC>(wparam), kShade);
                    return reinterpret_cast<LRESULT>(impl->shade_);
                default:
                    break;
            }
        }
        return DefWindowProcW(window, message, wparam, lparam);
    }

    static LRESULT CALLBACK progressProcedure(HWND window, UINT message, WPARAM wparam, LPARAM lparam) {
        remember(window, message, lparam);
        Impl *impl = of(window);
        if (impl != nullptr) {
            switch (message) {
                case WM_PAINT:
                    impl->paintProgress();
                    return 0;
                case WM_ERASEBKGND:
                    return 1;
                case WM_LBUTTONDOWN:
                    SetCapture(window);
                    impl->slide(GET_X_LPARAM(lparam), false);
                    return 0;
                case WM_MOUSEMOVE:
                    if (GetCapture() == window) impl->slide(GET_X_LPARAM(lparam), false);
                    return 0;
                case WM_LBUTTONUP:
                    if (GetCapture() == window) {
                        ReleaseCapture();
                        impl->slide(GET_X_LPARAM(lparam), true);
                    }
                    return 0;
                default:
                    break;
            }
        }
        return DefWindowProcW(window, message, wparam, lparam);
    }

    VideoControlled &player_;
    ComPtr<ID2D1Factory> d2d_;
    ComPtr<ID2D1DCRenderTarget> target_;
    HBRUSH shade_ = nullptr;
    HFONT font_ = nullptr;
    float scale_ = 1;
    SIZE size_{0, 0};

    HWND overlay_ = nullptr;
    HWND center_ = nullptr;
    HWND bar_ = nullptr;
    HWND barPlay_ = nullptr;
    HWND fullScreen_ = nullptr;
    HWND time_ = nullptr;
    HWND total_ = nullptr;
    HWND progress_ = nullptr;
    HWND dragTime_ = nullptr;

    bool controls_ = true;
    bool showProgress_ = true;
    bool showProgressInBar_ = true;
    bool showCenter_ = true;
    bool playGesture_ = false;
    bool progressGesture_ = true;
    bool barShown_ = false;
    bool hideScheduled_ = false;
    bool tookInput_ = false;
    bool barPlayShowsPause_ = false;
    bool fullScreenShowsExit_ = false;
    std::optional<POINT> pressedAt_;
    bool dragging_ = false;
    double dragFrom_ = 0;
    /// A drag across the picture, or along the progress bar, under way: where it would seek to.
    std::optional<double> seekTarget_;
};

VideoControls::VideoControls(HWND__ *owner, VideoControlled &player) : impl_(std::make_unique<Impl>(owner, player)) {}
VideoControls::~VideoControls() = default;
void VideoControls::configure(const Json &options) { impl_->configure(options); }
bool VideoControls::takesInput() const { return impl_->takesInput(); }
void VideoControls::refresh() { impl_->refresh(); }
void VideoControls::place(HWND__ *player, int left, int top, int width, int height, float scale, bool shown) {
    impl_->place(player, left, top, width, height, scale, shown);
}
bool VideoControls::mouse(unsigned message, std::uintptr_t wparam, std::intptr_t lparam) {
    return impl_->mouse(message, wparam, lparam);
}
HWND__ *VideoControls::hwnd() const noexcept { return impl_->hwnd(); }

}  // namespace migo::windows_host
