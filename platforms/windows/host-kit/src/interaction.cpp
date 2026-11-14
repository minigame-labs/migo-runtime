#include "interaction.hpp"
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
#include <d2d1.h>
#include <dwrite.h>
#include <wrl/client.h>

#include <algorithm>
#include <cmath>
#include <cstdio>
#include <exception>
#include <mutex>
#include <optional>
#include <string>
#include <string_view>
#include <utility>
#include <vector>

namespace migo::windows_host {

namespace {

using Microsoft::WRL::ComPtr;

constexpr wchar_t kIndicatorClass[] = L"MigoIndicator";
constexpr UINT_PTR kExpiryTimer = 1;
constexpr UINT_PTR kSpinTimer = 2;
// The spinner turns once a second, redrawn thirty times in it.
constexpr UINT kSpinMs = 33;
constexpr float kTurnPerSecond = 360.0F;
constexpr UINT_PTR kWatchId = 0x4D69676F + 4;
constexpr double kToastMs = 1500;

// The card, in device-independent pixels.
constexpr float kIconSize = 40;
constexpr float kTextWidth = 280;
constexpr float kPaddingX = 20;
constexpr float kPaddingY = 16;
constexpr float kSpacing = 10;
constexpr float kMinimumWithIcon = 88;
constexpr float kCorner = 8;
constexpr float kStroke = 3.5F;
constexpr float kTextSize = 14;

// The dialogs, in device-independent pixels.
constexpr int kDialogMargin = 16;
constexpr int kDialogGap = 8;
constexpr int kModalWidth = 340;
constexpr int kSheetWidth = 280;
constexpr int kButtonHeight = 28;
constexpr int kButtonMinWidth = 88;
constexpr int kItemHeight = 32;
constexpr int kFirstItem = 100;
constexpr int kTitleId = 10;
constexpr int kContentId = 11;
constexpr int kFieldId = 12;

const wchar_t kConfirm[] = L"确定";
const wchar_t kCancel[] = L"取消";

[[noreturn]] void fatal(const char *why) {
    std::fprintf(stderr, "migo windows host kit: %s\n", why);
    std::fflush(stderr);
    std::terminate();
}

std::wstring text_of(HWND window) {
    const int length = GetWindowTextLengthW(window);
    if (length <= 0) return {};
    std::wstring text(static_cast<std::size_t>(length) + 1, L'\0');
    const int copied = GetWindowTextW(window, text.data(), length + 1);
    text.resize(static_cast<std::size_t>(std::max(0, copied)));
    return text;
}

/// What the player does to the game: while a mask is up, none of it reaches the game.
bool is_input(UINT message) noexcept {
    if (message >= WM_MOUSEFIRST && message <= WM_MOUSELAST) return true;
    if (message >= WM_KEYFIRST && message <= WM_KEYLAST) return true;
    if (message >= WM_IME_STARTCOMPOSITION && message <= WM_IME_KEYLAST) return true;
    if (message == WM_INPUT || message == WM_TOUCH) return true;
    // The pointer's frames; not its activation or capture notices.
    return message >= 0x0245 /* WM_POINTERUPDATE */ && message <= 0x024F /* WM_POINTERHWHEEL */ &&
           message != 0x024B /* WM_POINTERACTIVATE */ && message != 0x024C /* WM_POINTERCAPTURECHANGED */;
}

int scaled(int dips, UINT dpi) noexcept { return MulDiv(dips, static_cast<int>(dpi), USER_DEFAULT_SCREEN_DPI); }

// ---- the dialogs' keyboard ------------------------------------------------------------------------

/// The open dialogs on this thread, and the hook that gives them the keyboard's
/// dialog navigation: a modeless dialog has it only from a message loop that
/// asks IsDialogMessage, and the loop is the App's.
struct DialogKeyboard {
    std::vector<HWND> dialogs;
    HHOOK hook = nullptr;
};

thread_local DialogKeyboard t_keyboard;

LRESULT CALLBACK dialog_keys(int code, WPARAM wparam, LPARAM lparam) {
    if (code == HC_ACTION && wparam == PM_REMOVE) {
        auto *message = reinterpret_cast<MSG *>(lparam);
        if ((message->message >= WM_KEYFIRST && message->message <= WM_KEYLAST)) {
            for (HWND dialog : t_keyboard.dialogs) {
                if (IsDialogMessageW(dialog, message)) {
                    // Handled: what the App's loop goes on to dispatch is nothing.
                    message->message = WM_NULL;
                    message->wParam = 0;
                    message->lParam = 0;
                    break;
                }
            }
        }
    }
    return CallNextHookEx(nullptr, code, wparam, lparam);
}

void keyboard_add(HWND dialog) {
    if (t_keyboard.dialogs.empty()) {
        t_keyboard.hook = SetWindowsHookExW(WH_GETMESSAGE, &dialog_keys, nullptr, GetCurrentThreadId());
    }
    t_keyboard.dialogs.push_back(dialog);
}

void keyboard_remove(HWND dialog) {
    auto &dialogs = t_keyboard.dialogs;
    dialogs.erase(std::remove(dialogs.begin(), dialogs.end(), dialog), dialogs.end());
    if (dialogs.empty() && t_keyboard.hook != nullptr) {
        UnhookWindowsHookEx(t_keyboard.hook);
        t_keyboard.hook = nullptr;
    }
}

}  // namespace

class Interaction::Impl final {
public:
    explicit Impl(Win32SurfaceView &view) : view_(view), root_(GetAncestor(view.hwnd(), GA_ROOT)) {
        static std::once_flag registered;
        std::call_once(registered, [] {
            WNDCLASSEXW indicator{};
            indicator.cbSize = sizeof indicator;
            indicator.lpfnWndProc = &Impl::indicatorProcedure;
            indicator.hInstance = GetModuleHandleW(nullptr);
            indicator.hCursor = LoadCursorW(nullptr, IDC_ARROW);
            indicator.lpszClassName = kIndicatorClass;
            if (RegisterClassExW(&indicator) == 0) fatal("RegisterClassExW failed for the indicator");
        });
        watching_view_ = SetWindowSubclass(view_.hwnd(), &Impl::watch, kWatchId, reinterpret_cast<DWORD_PTR>(this));
        if (root_ != nullptr && root_ != view_.hwnd()) {
            watching_root_ = SetWindowSubclass(root_, &Impl::watch, kWatchId, reinterpret_cast<DWORD_PTR>(this));
        }
    }

    ~Impl() {
        hideIndicator();
        // A dialog still up goes with the service that showed it, declined.
        while (!dialogs_.empty()) dismiss(*dialogs_.back());
        if (watching_view_ && view_.hwnd() != nullptr) RemoveWindowSubclass(view_.hwnd(), &Impl::watch, kWatchId);
        if (watching_root_) RemoveWindowSubclass(root_, &Impl::watch, kWatchId);
    }

    void call(std::uint32_t method, const Json &request, HostServiceReply reply) {
        switch (method) {
            case MIGO_INTERACTION_SHOW_TOAST:
                showIndicator(Shown::Toast, wideOf(request["title"].string()), iconOf(request["icon"].stringOr("success")),
                              request["mask"].boolOr(false), request["duration"].numberOr(kToastMs));
                return;
            case MIGO_INTERACTION_HIDE_TOAST:
                if (shown_ == Shown::Toast) hideIndicator();
                return;
            case MIGO_INTERACTION_SHOW_LOADING:
                showIndicator(Shown::Loading, wideOf(request["title"].string()), Icon::Loading,
                              request["mask"].boolOr(false), std::nullopt);
                return;
            case MIGO_INTERACTION_HIDE_LOADING:
                if (shown_ == Shown::Loading) hideIndicator();
                return;
            case MIGO_INTERACTION_SHOW_MODAL:
                showModal(request, std::move(reply));
                return;
            case MIGO_INTERACTION_SHOW_ACTION_SHEET:
                showActionSheet(request, std::move(reply));
                return;
            default:
                (void)reply.fail("not supported");
                return;
        }
    }

private:
    enum class Shown { None, Toast, Loading };
    enum class Icon { None, Success, Error, Loading };

    static Icon iconOf(const std::string &name) noexcept {
        if (name == "none") return Icon::None;
        if (name == "error") return Icon::Error;
        if (name == "loading") return Icon::Loading;
        return Icon::Success;
    }

    // ---- the indicator --------------------------------------------------------------------------------

    void showIndicator(Shown kind, std::wstring title, Icon icon, bool mask, std::optional<double> duration_ms) {
        hideIndicator();
        // Over the game, taking none of its input: a mask keeps that from the
        // game, not the card.
        indicator_ = CreateWindowExW(WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW,
                                     kIndicatorClass, title.c_str(), WS_POPUP, 0, 0, 1, 1, root_, nullptr,
                                     GetModuleHandleW(nullptr), this);
        if (indicator_ == nullptr) return;
        shown_ = kind;
        icon_ = icon;
        masked_ = mask;
        title_ = std::move(title);
        angle_ = 0;
        present();
        if (duration_ms.has_value()) {
            const double ms = std::isfinite(*duration_ms) ? std::clamp(*duration_ms, 0.0, 86400000.0) : kToastMs;
            SetTimer(indicator_, kExpiryTimer, static_cast<UINT>(std::max(1.0, ms)), nullptr);
        }
        if (icon_ == Icon::Loading) SetTimer(indicator_, kSpinTimer, kSpinMs, nullptr);
    }

    void hideIndicator() {
        shown_ = Shown::None;
        masked_ = false;
        title_.clear();
        if (indicator_ != nullptr) {
            HWND gone = indicator_;
            indicator_ = nullptr;
            SetWindowLongPtrW(gone, GWLP_USERDATA, 0);
            DestroyWindow(gone);
        }
    }

    static LRESULT CALLBACK indicatorProcedure(HWND window, UINT message, WPARAM wparam, LPARAM lparam) {
        if (message == WM_NCCREATE) {
            const auto *create = reinterpret_cast<const CREATESTRUCTW *>(lparam);
            SetWindowLongPtrW(window, GWLP_USERDATA, reinterpret_cast<LONG_PTR>(create->lpCreateParams));
        }
        auto *impl = reinterpret_cast<Impl *>(GetWindowLongPtrW(window, GWLP_USERDATA));
        switch (message) {
            case WM_TIMER:
                if (impl != nullptr && wparam == kExpiryTimer) {
                    impl->hideIndicator();
                    return 0;
                }
                if (impl != nullptr && wparam == kSpinTimer) {
                    impl->angle_ = std::fmod(impl->angle_ + kTurnPerSecond * static_cast<float>(kSpinMs) / 1000.0F,
                                             360.0F);
                    impl->present();
                    return 0;
                }
                break;
            case WM_MOUSEACTIVATE:
                return MA_NOACTIVATE;
            case WM_NCHITTEST:
                return HTTRANSPARENT;
            default:
                break;
        }
        return DefWindowProcW(window, message, wparam, lparam);
    }

    bool ensureDrawing() {
        if (d2d_ == nullptr &&
            FAILED(D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, d2d_.GetAddressOf()))) {
            return false;
        }
        if (dwrite_ == nullptr &&
            FAILED(DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED, __uuidof(IDWriteFactory),
                                       reinterpret_cast<IUnknown **>(dwrite_.GetAddressOf())))) {
            return false;
        }
        if (text_format_ == nullptr) {
            if (FAILED(dwrite_->CreateTextFormat(L"Segoe UI", nullptr, DWRITE_FONT_WEIGHT_NORMAL,
                                                 DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_STRETCH_NORMAL, kTextSize,
                                                 L"", text_format_.GetAddressOf()))) {
                return false;
            }
            text_format_->SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER);
            text_format_->SetWordWrapping(DWRITE_WORD_WRAPPING_WRAP);
        }
        if (stroke_ == nullptr) {
            const D2D1_STROKE_STYLE_PROPERTIES rounded = D2D1::StrokeStyleProperties(
                D2D1_CAP_STYLE_ROUND, D2D1_CAP_STYLE_ROUND, D2D1_CAP_STYLE_ROUND, D2D1_LINE_JOIN_ROUND);
            if (FAILED(d2d_->CreateStrokeStyle(rounded, nullptr, 0, stroke_.GetAddressOf()))) return false;
        }
        if (target_ == nullptr) {
            const D2D1_RENDER_TARGET_PROPERTIES properties = D2D1::RenderTargetProperties(
                D2D1_RENDER_TARGET_TYPE_DEFAULT,
                D2D1::PixelFormat(DXGI_FORMAT_B8G8R8A8_UNORM, D2D1_ALPHA_MODE_PREMULTIPLIED));
            if (FAILED(d2d_->CreateDCRenderTarget(&properties, target_.GetAddressOf()))) return false;
        }
        return true;
    }

    /// The card drawn at its size, centred over the game; hidden while the game is.
    void present() {
        if (indicator_ == nullptr) return;
        HWND view_window = view_.hwnd();
        if (view_window == nullptr || !IsWindowVisible(view_window)) {
            ShowWindow(indicator_, SW_HIDE);
            return;
        }
        if (!ensureDrawing()) return;
        const UINT dpi = std::max<UINT>(GetDpiForWindow(view_window), USER_DEFAULT_SCREEN_DPI / 4);
        const float scale = static_cast<float>(dpi) / static_cast<float>(USER_DEFAULT_SCREEN_DPI);

        ComPtr<IDWriteTextLayout> layout;
        DWRITE_TEXT_METRICS text{};
        if (!title_.empty() &&
            SUCCEEDED(dwrite_->CreateTextLayout(title_.c_str(), static_cast<UINT32>(title_.size()), text_format_.Get(),
                                                kTextWidth, 4096.0F, layout.GetAddressOf()))) {
            layout->GetMetrics(&text);
        }
        const bool has_icon = icon_ != Icon::None;
        const bool has_text = layout != nullptr;
        float content_width = has_text ? std::ceil(text.width) : 0.0F;
        if (has_icon) content_width = std::max(content_width, kMinimumWithIcon);
        const float width = content_width + 2 * kPaddingX;
        const float height = 2 * kPaddingY + (has_icon ? kIconSize : 0.0F) + (has_icon && has_text ? kSpacing : 0.0F) +
                             (has_text ? std::ceil(text.height) : 0.0F);
        const LONG pixel_width = std::max(1L, std::lround(width * scale));
        const LONG pixel_height = std::max(1L, std::lround(height * scale));

        RECT client{};
        GetClientRect(view_window, &client);
        POINT centre{client.right / 2, client.bottom / 2};
        ClientToScreen(view_window, &centre);
        POINT position{centre.x - pixel_width / 2, centre.y - pixel_height / 2};
        SIZE size{pixel_width, pixel_height};

        HDC screen = GetDC(nullptr);
        HDC canvas = CreateCompatibleDC(screen);
        BITMAPINFO format{};
        format.bmiHeader.biSize = sizeof format.bmiHeader;
        format.bmiHeader.biWidth = pixel_width;
        format.bmiHeader.biHeight = -pixel_height;
        format.bmiHeader.biPlanes = 1;
        format.bmiHeader.biBitCount = 32;
        format.bmiHeader.biCompression = BI_RGB;
        void *pixels = nullptr;
        HBITMAP bitmap = CreateDIBSection(screen, &format, DIB_RGB_COLORS, &pixels, nullptr, 0);
        if (bitmap != nullptr) {
            HGDIOBJ previous = SelectObject(canvas, bitmap);
            const RECT bounds{0, 0, pixel_width, pixel_height};
            if (draw(canvas, bounds, scale, width, height, layout.Get())) {
                POINT origin{0, 0};
                BLENDFUNCTION blend{AC_SRC_OVER, 0, 255, AC_SRC_ALPHA};
                UpdateLayeredWindow(indicator_, screen, &position, &size, canvas, &origin, 0, &blend, ULW_ALPHA);
                SetWindowPos(indicator_, nullptr, 0, 0, 0, 0,
                             SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOOWNERZORDER | SWP_NOACTIVATE |
                                 SWP_SHOWWINDOW);
            }
            SelectObject(canvas, previous);
            DeleteObject(bitmap);
        }
        DeleteDC(canvas);
        ReleaseDC(nullptr, screen);
    }

    bool draw(HDC canvas, const RECT &bounds, float scale, float width, float height, IDWriteTextLayout *layout) {
        target_->SetDpi(96.0F * scale, 96.0F * scale);
        if (FAILED(target_->BindDC(canvas, &bounds))) return false;
        target_->BeginDraw();
        target_->Clear(D2D1::ColorF(0, 0, 0, 0));
        ComPtr<ID2D1SolidColorBrush> card;
        ComPtr<ID2D1SolidColorBrush> ink;
        target_->CreateSolidColorBrush(D2D1::ColorF(32.0F / 255, 32.0F / 255, 32.0F / 255, 0.9F), card.GetAddressOf());
        target_->CreateSolidColorBrush(D2D1::ColorF(1, 1, 1, 1), ink.GetAddressOf());
        if (card != nullptr && ink != nullptr) {
            target_->FillRoundedRectangle(D2D1::RoundedRect(D2D1::RectF(0, 0, width, height), kCorner, kCorner),
                                          card.Get());
            float y = kPaddingY;
            if (icon_ != Icon::None) {
                drawIcon(D2D1::Point2F((width - kIconSize) / 2, y), ink.Get());
                y += kIconSize + (layout != nullptr ? kSpacing : 0.0F);
            }
            if (layout != nullptr) {
                // The layout centres its lines in the text's width, which is
                // centred in the card.
                target_->DrawTextLayout(D2D1::Point2F((width - kTextWidth) / 2, y), layout, ink.Get());
            }
        }
        const HRESULT drawn = target_->EndDraw();
        if (drawn == static_cast<HRESULT>(D2DERR_RECREATE_TARGET)) {
            // The device went away: made again on the next frame.
            target_.Reset();
            return false;
        }
        return SUCCEEDED(drawn);
    }

    void drawIcon(D2D1_POINT_2F at, ID2D1Brush *ink) {
        const auto point = [at](float x, float y) { return D2D1::Point2F(at.x + x, at.y + y); };
        switch (icon_) {
            case Icon::Success:
                target_->DrawLine(point(9, 21), point(17, 29), ink, kStroke, stroke_.Get());
                target_->DrawLine(point(17, 29), point(31, 12), ink, kStroke, stroke_.Get());
                break;
            case Icon::Error:
                target_->DrawLine(point(12, 12), point(28, 28), ink, kStroke, stroke_.Get());
                target_->DrawLine(point(28, 12), point(12, 28), ink, kStroke, stroke_.Get());
                break;
            case Icon::Loading: {
                // Three quarters of a ring, turning.
                constexpr float kRadius = 15;
                constexpr float kPi = 3.14159265358979F;
                const float start = angle_ * kPi / 180.0F;
                const float end = start + 1.5F * kPi;
                const D2D1_POINT_2F centre = point(kIconSize / 2, kIconSize / 2);
                ComPtr<ID2D1PathGeometry> ring;
                ComPtr<ID2D1GeometrySink> sink;
                if (FAILED(d2d_->CreatePathGeometry(ring.GetAddressOf())) || FAILED(ring->Open(sink.GetAddressOf()))) {
                    break;
                }
                sink->BeginFigure(D2D1::Point2F(centre.x + kRadius * std::cos(start), centre.y + kRadius * std::sin(start)),
                                  D2D1_FIGURE_BEGIN_HOLLOW);
                sink->AddArc(D2D1::ArcSegment(
                    D2D1::Point2F(centre.x + kRadius * std::cos(end), centre.y + kRadius * std::sin(end)),
                    D2D1::SizeF(kRadius, kRadius), 0, D2D1_SWEEP_DIRECTION_CLOCKWISE, D2D1_ARC_SIZE_LARGE));
                sink->EndFigure(D2D1_FIGURE_END_OPEN);
                if (SUCCEEDED(sink->Close())) target_->DrawGeometry(ring.Get(), ink, kStroke, stroke_.Get());
                break;
            }
            case Icon::None:
                break;
        }
    }

    // ---- what the game's windows tell it ---------------------------------------------------------------

    static LRESULT CALLBACK watch(HWND window, UINT message, WPARAM wparam, LPARAM lparam, UINT_PTR,
                                  DWORD_PTR reference) {
        auto *impl = reinterpret_cast<Impl *>(reference);
        // Masked, the game hears nothing the player does; the system still
        // does (Alt+F4, the window menu).
        if (impl->masked_ && window == impl->view_.hwnd() && is_input(message)) {
            return DefWindowProcW(window, message, wparam, lparam);
        }
        const LRESULT result = DefSubclassProc(window, message, wparam, lparam);
        switch (message) {
            case WM_WINDOWPOSCHANGED:
            case WM_SHOWWINDOW:
            case WM_DPICHANGED:
            case WM_DPICHANGED_AFTERPARENT:
                impl->present();
                break;
            case WM_NCDESTROY:
                if (window == impl->root_) {
                    impl->watching_root_ = false;
                } else {
                    impl->watching_view_ = false;
                }
                RemoveWindowSubclass(window, &Impl::watch, kWatchId);
                break;
            default:
                break;
        }
        return result;
    }

    // ---- the dialogs ----------------------------------------------------------------------------------

    struct Dialog {
        enum class Kind { Modal, Sheet };
        Impl *owner = nullptr;
        Kind kind = Kind::Modal;
        HostServiceReply reply;
        HWND window = nullptr;
        bool editable = false;
        std::wstring title;
        std::wstring content;
        std::wstring placeholder;
        std::wstring confirm_text;
        std::wstring cancel_text;
        bool show_cancel = true;
        std::vector<std::wstring> items;
        HFONT font = nullptr;
        HFONT heading = nullptr;
        bool answered = false;

        explicit Dialog(HostServiceReply answer) : reply(std::move(answer)) {}
    };

    void showModal(const Json &request, HostServiceReply reply) {
        auto dialog = std::make_unique<Dialog>(std::move(reply));
        dialog->kind = Dialog::Kind::Modal;
        dialog->title = wideOf(request["title"].string());
        dialog->content = wideOf(request["content"].string());
        dialog->editable = request["editable"].boolOr(false);
        dialog->placeholder = wideOf(request["placeholderText"].string());
        dialog->show_cancel = request["showCancel"].boolOr(true);
        dialog->confirm_text = request.contains("confirmText") ? wideOf(request["confirmText"].string()) : kConfirm;
        dialog->cancel_text = request.contains("cancelText") ? wideOf(request["cancelText"].string()) : kCancel;
        if (dialog->confirm_text.empty()) dialog->confirm_text = kConfirm;
        if (dialog->cancel_text.empty()) dialog->cancel_text = kCancel;
        open(std::move(dialog));
    }

    void showActionSheet(const Json &request, HostServiceReply reply) {
        const Json::Array &items = request["itemList"].items();
        if (items.empty()) {
            (void)reply.fail("itemList is empty");
            return;
        }
        auto dialog = std::make_unique<Dialog>(std::move(reply));
        dialog->kind = Dialog::Kind::Sheet;
        dialog->content = wideOf(request["alertText"].string());
        for (const Json &item : items) dialog->items.push_back(wideOf(item.string()));
        dialog->cancel_text = kCancel;
        open(std::move(dialog));
    }

    void open(std::unique_ptr<Dialog> dialog) {
        // An empty dialog template: the controls are laid out at the window's
        // DPI, in the system's message font, once it exists.
        alignas(DWORD) WORD layout[16] = {};
        auto *shape = reinterpret_cast<DLGTEMPLATE *>(layout);
        shape->style = WS_POPUP | WS_CAPTION | WS_SYSMENU | DS_MODALFRAME;
        shape->dwExtendedStyle = WS_EX_DLGMODALFRAME;
        Dialog *raw = dialog.get();
        raw->owner = this;
        dialogs_.push_back(std::move(dialog));
        // Modal to the App's window: disabled before the dialog takes the focus.
        if (dialogs_.size() == 1 && root_ != nullptr) EnableWindow(root_, FALSE);
        HWND window = CreateDialogIndirectParamW(GetModuleHandleW(nullptr), shape, root_, &Impl::dialogProcedure,
                                                 reinterpret_cast<LPARAM>(raw));
        if (window == nullptr) {
            HostServiceReply reply = std::move(raw->reply);
            forget(*raw);
            (void)reply.fail("the dialog could not be shown");
            return;
        }
        keyboard_add(window);
        ShowWindow(window, SW_SHOW);
    }

    static INT_PTR CALLBACK dialogProcedure(HWND window, UINT message, WPARAM wparam, LPARAM lparam) {
        if (message == WM_INITDIALOG) {
            auto *dialog = reinterpret_cast<Dialog *>(lparam);
            SetWindowLongPtrW(window, DWLP_USER, lparam);
            dialog->window = window;
            dialog->owner->build(*dialog);
            // The focus is set: the field, or the default button.
            return FALSE;
        }
        auto *dialog = reinterpret_cast<Dialog *>(GetWindowLongPtrW(window, DWLP_USER));
        if (dialog == nullptr) return FALSE;
        switch (message) {
            case WM_COMMAND: {
                const int id = LOWORD(wparam);
                if (HIWORD(wparam) != BN_CLICKED && id != IDOK && id != IDCANCEL) return FALSE;
                if (id == IDOK && dialog->kind == Dialog::Kind::Modal) {
                    dialog->owner->finish(*dialog, true, -1);
                } else if (id == IDCANCEL) {
                    dialog->owner->finish(*dialog, false, -1);
                } else if (id >= kFirstItem && id < kFirstItem + static_cast<int>(dialog->items.size())) {
                    dialog->owner->finish(*dialog, true, id - kFirstItem);
                } else {
                    return FALSE;
                }
                return TRUE;
            }
            case WM_CLOSE:
                dialog->owner->finish(*dialog, false, -1);
                return TRUE;
            case WM_DPICHANGED: {
                const auto *suggested = reinterpret_cast<const RECT *>(lparam);
                dialog->owner->layOut(*dialog, HIWORD(wparam));
                SetWindowPos(window, nullptr, suggested->left, suggested->top, 0, 0,
                             SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE);
                return TRUE;
            }
            default:
                return FALSE;
        }
    }

    HWND control(Dialog &dialog, const wchar_t *kind, const std::wstring &text, DWORD style, int id,
                 DWORD extended = 0) {
        return CreateWindowExW(extended, kind, text.c_str(), WS_CHILD | WS_VISIBLE | style, 0, 0, 1, 1, dialog.window,
                               reinterpret_cast<HMENU>(static_cast<INT_PTR>(id)), GetModuleHandleW(nullptr), nullptr);
    }

    void build(Dialog &dialog) {
        // The App's name in the caption, as a Windows dialog has it.
        std::wstring caption = root_ != nullptr ? text_of(root_) : std::wstring();
        SetWindowTextW(dialog.window, caption.c_str());
        if (dialog.kind == Dialog::Kind::Modal) {
            if (!dialog.title.empty()) control(dialog, L"STATIC", dialog.title, SS_LEFT | SS_NOPREFIX, kTitleId);
            if (dialog.editable) {
                HWND field = control(dialog, L"EDIT", dialog.content, WS_TABSTOP | ES_LEFT | ES_AUTOHSCROLL, kFieldId,
                                     WS_EX_CLIENTEDGE);
                if (!dialog.placeholder.empty()) {
                    SendMessageW(field, EM_SETCUEBANNER, TRUE, reinterpret_cast<LPARAM>(dialog.placeholder.c_str()));
                }
            } else if (!dialog.content.empty()) {
                control(dialog, L"STATIC", dialog.content, SS_LEFT | SS_NOPREFIX, kContentId);
            }
            control(dialog, L"BUTTON", dialog.confirm_text, WS_TABSTOP | BS_DEFPUSHBUTTON, IDOK);
            if (dialog.show_cancel) control(dialog, L"BUTTON", dialog.cancel_text, WS_TABSTOP | BS_PUSHBUTTON, IDCANCEL);
        } else {
            if (!dialog.content.empty()) control(dialog, L"STATIC", dialog.content, SS_LEFT | SS_NOPREFIX, kContentId);
            for (std::size_t index = 0; index < dialog.items.size(); ++index) {
                control(dialog, L"BUTTON", dialog.items[index], WS_TABSTOP | BS_PUSHBUTTON,
                        kFirstItem + static_cast<int>(index));
            }
            control(dialog, L"BUTTON", dialog.cancel_text, WS_TABSTOP | BS_PUSHBUTTON, IDCANCEL);
        }
        layOut(dialog, GetDpiForWindow(dialog.window));
        centre(dialog);
        HWND focus = GetDlgItem(dialog.window, dialog.editable ? kFieldId
                                               : dialog.kind == Dialog::Kind::Modal ? IDOK
                                                                                    : kFirstItem);
        if (focus != nullptr) {
            SetFocus(focus);
            if (dialog.editable) SendMessageW(focus, EM_SETSEL, 0, -1);
        }
    }

    /// The text's height in `width` pixels, wrapped, in `font`.
    static int heightOf(HWND window, HFONT font, const std::wstring &text, int width) {
        HDC context = GetDC(window);
        HGDIOBJ previous = SelectObject(context, font);
        RECT box{0, 0, width, 0};
        DrawTextW(context, text.c_str(), static_cast<int>(text.size()), &box,
                  DT_CALCRECT | DT_WORDBREAK | DT_NOPREFIX | DT_EDITCONTROL);
        SelectObject(context, previous);
        ReleaseDC(window, context);
        return box.bottom;
    }

    static int widthOf(HWND window, HFONT font, const std::wstring &text) {
        HDC context = GetDC(window);
        HGDIOBJ previous = SelectObject(context, font);
        SIZE extent{};
        GetTextExtentPoint32W(context, text.c_str(), static_cast<int>(text.size()), &extent);
        SelectObject(context, previous);
        ReleaseDC(window, context);
        return extent.cx;
    }

    /// The controls placed at `dpi`, in the system's message font at it.
    void layOut(Dialog &dialog, UINT dpi) {
        if (dpi == 0) dpi = USER_DEFAULT_SCREEN_DPI;
        NONCLIENTMETRICSW metrics{};
        metrics.cbSize = sizeof metrics;
        SystemParametersInfoForDpi(SPI_GETNONCLIENTMETRICS, sizeof metrics, &metrics, 0, dpi);
        HFONT font = CreateFontIndirectW(&metrics.lfMessageFont);
        LOGFONTW bold = metrics.lfMessageFont;
        bold.lfWeight = FW_SEMIBOLD;
        bold.lfHeight = MulDiv(bold.lfHeight, 5, 4);
        HFONT heading = CreateFontIndirectW(&bold);
        if (dialog.font != nullptr) DeleteObject(dialog.font);
        if (dialog.heading != nullptr) DeleteObject(dialog.heading);
        dialog.font = font;
        dialog.heading = heading;

        const int margin = scaled(kDialogMargin, dpi);
        const int gap = scaled(kDialogGap, dpi);
        const int width = scaled(dialog.kind == Dialog::Kind::Modal ? kModalWidth : kSheetWidth, dpi);
        const int inner = width - 2 * margin;
        int y = margin;
        const auto place = [&](int id, HFONT with, int height) {
            HWND child = GetDlgItem(dialog.window, id);
            if (child == nullptr) return;
            SendMessageW(child, WM_SETFONT, reinterpret_cast<WPARAM>(with), FALSE);
            SetWindowPos(child, nullptr, margin, y, inner, height, SWP_NOZORDER | SWP_NOACTIVATE);
            y += height + gap;
        };
        if (dialog.kind == Dialog::Kind::Modal) {
            if (!dialog.title.empty()) place(kTitleId, heading, heightOf(dialog.window, heading, dialog.title, inner));
            if (dialog.editable) {
                place(kFieldId, font, -metrics.lfMessageFont.lfHeight + scaled(12, dpi));
            } else if (!dialog.content.empty()) {
                place(kContentId, font, heightOf(dialog.window, font, dialog.content, inner));
            }
            // The buttons on the right, the affirmative first, as Windows has them.
            y += gap;
            const int button_height = scaled(kButtonHeight, dpi);
            const auto button_width = [&](const std::wstring &text) {
                return std::max(scaled(kButtonMinWidth, dpi), widthOf(dialog.window, font, text) + scaled(24, dpi));
            };
            int right = width - margin;
            for (int id : {IDCANCEL, IDOK}) {
                HWND button = GetDlgItem(dialog.window, id);
                if (button == nullptr) continue;
                const int button_wide = button_width(id == IDOK ? dialog.confirm_text : dialog.cancel_text);
                SendMessageW(button, WM_SETFONT, reinterpret_cast<WPARAM>(font), FALSE);
                SetWindowPos(button, nullptr, right - button_wide, y, button_wide, button_height,
                             SWP_NOZORDER | SWP_NOACTIVATE);
                right -= button_wide + gap;
            }
            y += button_height + margin;
        } else {
            if (!dialog.content.empty()) place(kContentId, font, heightOf(dialog.window, font, dialog.content, inner));
            for (std::size_t index = 0; index < dialog.items.size(); ++index) {
                place(kFirstItem + static_cast<int>(index), font, scaled(kItemHeight, dpi));
            }
            y += gap;
            place(IDCANCEL, font, scaled(kItemHeight, dpi));
            y += margin - gap;
        }
        RECT outer{0, 0, width, y};
        AdjustWindowRectExForDpi(&outer, static_cast<DWORD>(GetWindowLongPtrW(dialog.window, GWL_STYLE)), FALSE,
                                 static_cast<DWORD>(GetWindowLongPtrW(dialog.window, GWL_EXSTYLE)), dpi);
        SetWindowPos(dialog.window, nullptr, 0, 0, outer.right - outer.left, outer.bottom - outer.top,
                     SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE);
        InvalidateRect(dialog.window, nullptr, TRUE);
    }

    /// Over the middle of the game, kept on its screen's work area.
    void centre(Dialog &dialog) {
        RECT outer{};
        GetWindowRect(dialog.window, &outer);
        const LONG width = outer.right - outer.left;
        const LONG height = outer.bottom - outer.top;
        RECT client{};
        GetClientRect(view_.hwnd(), &client);
        POINT middle{client.right / 2, client.bottom / 2};
        ClientToScreen(view_.hwnd(), &middle);
        LONG x = middle.x - width / 2;
        LONG y = middle.y - height / 2;
        MONITORINFO monitor{};
        monitor.cbSize = sizeof monitor;
        if (GetMonitorInfoW(MonitorFromPoint(middle, MONITOR_DEFAULTTONEAREST), &monitor)) {
            x = std::clamp(x, monitor.rcWork.left, std::max(monitor.rcWork.left, monitor.rcWork.right - width));
            y = std::clamp(y, monitor.rcWork.top, std::max(monitor.rcWork.top, monitor.rcWork.bottom - height));
        }
        SetWindowPos(dialog.window, nullptr, x, y, 0, 0, SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE);
    }

    /// The player answered -- or declined, `chosen` false. A modal confirms or
    /// cancels; a sheet names the item tapped, or fails "cancel".
    void finish(Dialog &dialog, bool chosen, int item) {
        if (dialog.answered) return;
        dialog.answered = true;
        HostServiceReply reply = std::move(dialog.reply);
        Json answer;
        bool declined_sheet = false;
        if (dialog.kind == Dialog::Kind::Modal) {
            answer = Json::object().set("confirm", chosen).set("cancel", !chosen);
            if (dialog.editable) {
                HWND field = GetDlgItem(dialog.window, kFieldId);
                answer.set("content", field != nullptr ? utf8Of(text_of(field)) : std::string());
            }
        } else if (chosen) {
            answer = Json::object().set("tapIndex", item);
        } else {
            declined_sheet = true;
        }
        close(dialog);
        // Answered once the dialog is gone: content acting on it finds the
        // App's window its own again.
        if (declined_sheet) {
            (void)reply.fail("cancel");
        } else {
            (void)reply.succeed(answer);
        }
    }

    void dismiss(Dialog &dialog) { finish(dialog, false, -1); }

    void close(Dialog &dialog) {
        HWND window = dialog.window;
        // The App's window is enabled before the dialog goes, so the system
        // gives it back the activation rather than to some other App.
        if (dialogs_.size() == 1 && root_ != nullptr) EnableWindow(root_, TRUE);
        if (window != nullptr) {
            keyboard_remove(window);
            SetWindowLongPtrW(window, DWLP_USER, 0);
            DestroyWindow(window);
        }
        forget(dialog);
    }

    /// The dialog's record dropped, its fonts freed; the App's window enabled
    /// once none is left.
    void forget(Dialog &dialog) {
        if (dialog.font != nullptr) DeleteObject(dialog.font);
        if (dialog.heading != nullptr) DeleteObject(dialog.heading);
        const auto found = std::find_if(dialogs_.begin(), dialogs_.end(),
                                        [&dialog](const std::unique_ptr<Dialog> &open) { return open.get() == &dialog; });
        if (found != dialogs_.end()) dialogs_.erase(found);
        if (dialogs_.empty() && root_ != nullptr) EnableWindow(root_, TRUE);
    }

    Win32SurfaceView &view_;
    const HWND root_;
    bool watching_view_ = false;
    bool watching_root_ = false;

    HWND indicator_ = nullptr;
    Shown shown_ = Shown::None;
    Icon icon_ = Icon::None;
    bool masked_ = false;
    std::wstring title_;
    float angle_ = 0;

    ComPtr<ID2D1Factory> d2d_;
    ComPtr<IDWriteFactory> dwrite_;
    ComPtr<IDWriteTextFormat> text_format_;
    ComPtr<ID2D1StrokeStyle> stroke_;
    ComPtr<ID2D1DCRenderTarget> target_;

    std::vector<std::unique_ptr<Dialog>> dialogs_;
};

Interaction::Interaction(Win32SurfaceView &view) : impl_(std::make_unique<Impl>(view)) {}
Interaction::~Interaction() = default;
void Interaction::call(std::uint32_t method, const Json &request, HostServiceReply reply) {
    impl_->call(method, request, std::move(reply));
}

}  // namespace migo::windows_host
