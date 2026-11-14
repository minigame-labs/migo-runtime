#include "keyboard_field.hpp"
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

#include <algorithm>
#include <cstdio>
#include <exception>
#include <mutex>

namespace migo::windows_host {

namespace {

constexpr wchar_t kFieldClass[] = L"MigoKeyboardField";
constexpr UINT_PTR kFollowId = 0x4D69676F + 5;
constexpr UINT_PTR kEditorId = 0x4D69676F + 6;
constexpr int kEditorControl = 1;
// Several lines show this many at once.
constexpr int kVisibleLines = 4;
// The field's inner margin, in device-independent pixels.
constexpr int kPadding = 4;

[[noreturn]] void fatal(const char *why) {
    std::fprintf(stderr, "migo windows host kit: %s\n", why);
    std::fflush(stderr);
    std::terminate();
}

/// Content's line breaks are "\n"; the edit control's are "\r\n".
std::wstring to_control(const std::wstring &text) {
    std::wstring out;
    out.reserve(text.size());
    for (std::size_t index = 0; index < text.size(); ++index) {
        if (text[index] == L'\n' && (index == 0 || text[index - 1] != L'\r')) out.push_back(L'\r');
        out.push_back(text[index]);
    }
    return out;
}

std::wstring from_control(const std::wstring &text) {
    std::wstring out;
    out.reserve(text.size());
    for (std::size_t index = 0; index < text.size(); ++index) {
        if (text[index] == L'\r' && index + 1 < text.size() && text[index + 1] == L'\n') continue;
        out.push_back(text[index]);
    }
    return out;
}

}  // namespace

struct KeyboardFieldProcedures {
    static LRESULT CALLBACK window(HWND window, UINT message, WPARAM wparam, LPARAM lparam) {
        if (message == WM_NCCREATE) {
            const auto *create = reinterpret_cast<const CREATESTRUCTW *>(lparam);
            SetWindowLongPtrW(window, GWLP_USERDATA, reinterpret_cast<LONG_PTR>(create->lpCreateParams));
        }
        auto *field = reinterpret_cast<KeyboardField *>(GetWindowLongPtrW(window, GWLP_USERDATA));
        if (field != nullptr) {
            switch (message) {
                case WM_COMMAND:
                    if (LOWORD(wparam) == kEditorControl && HIWORD(wparam) == EN_CHANGE) {
                        field->edited();
                        return 0;
                    }
                    break;
                case WM_SETFOCUS:
                    // The keyboard is the field's, not its frame's.
                    if (field->editor_ != nullptr) SetFocus(field->editor_);
                    return 0;
                case WM_ACTIVATE:
                    // The player turned to another window: the field goes with
                    // the focus, as a phone's keyboard does.
                    if (LOWORD(wparam) == WA_INACTIVE && field->showing_) field->finish(true);
                    break;
                default:
                    break;
            }
        }
        return DefWindowProcW(window, message, wparam, lparam);
    }

    /// The edit control's keys: Enter confirms a one-line field, Esc puts the
    /// field away, and the focus leaving it is the player turning away.
    static LRESULT CALLBACK editor(HWND window, UINT message, WPARAM wparam, LPARAM lparam, UINT_PTR,
                                   DWORD_PTR reference) {
        auto *field = reinterpret_cast<KeyboardField *>(reference);
        switch (message) {
            case WM_GETDLGCODE:
                // Every key is the field's: a host's dialog navigation must not take Enter or Esc.
                return DefSubclassProc(window, message, wparam, lparam) | DLGC_WANTALLKEYS;
            case WM_KEYDOWN:
                if (wparam == VK_ESCAPE && field->showing_) {
                    field->finish(true);
                    return 0;
                }
                if (wparam == VK_RETURN && field->showing_ && !field->multiline_) {
                    field->send(MIGO_KEYBOARD_EVENT_CONFIRM, field->text());
                    if (!field->hold_) field->finish(true);
                    return 0;
                }
                break;
            case WM_CHAR:
                // The Enter and Esc already acted on: the control would only beep.
                if ((wparam == L'\r' && !field->multiline_) || wparam == 0x1B) return 0;
                break;
            case WM_KILLFOCUS: {
                const LRESULT result = DefSubclassProc(window, message, wparam, lparam);
                // To another window -- the game, the App, another App -- not to
                // the field's own context menu, which takes no focus.
                if (field->showing_ && reinterpret_cast<HWND>(wparam) != field->window_) field->finish(true);
                return result;
            }
            case WM_NCDESTROY:
                RemoveWindowSubclass(window, &KeyboardFieldProcedures::editor, kEditorId);
                break;
            default:
                break;
        }
        return DefSubclassProc(window, message, wparam, lparam);
    }

    /// The game's windows: the field follows the game as it moves and resizes.
    static LRESULT CALLBACK follow(HWND window, UINT message, WPARAM wparam, LPARAM lparam, UINT_PTR,
                                   DWORD_PTR reference) {
        const LRESULT result = DefSubclassProc(window, message, wparam, lparam);
        auto *field = reinterpret_cast<KeyboardField *>(reference);
        switch (message) {
            case WM_WINDOWPOSCHANGED:
                if (field->showing_) field->place();
                break;
            case WM_NCDESTROY:
                if (window == field->root_) {
                    field->following_root_ = false;
                } else {
                    field->following_view_ = false;
                }
                RemoveWindowSubclass(window, &KeyboardFieldProcedures::follow, kFollowId);
                break;
            default:
                break;
        }
        return result;
    }
};

KeyboardField::KeyboardField(MigoSession *session, Win32SurfaceView &view) : session_(session), view_(view) {
    static std::once_flag registered;
    std::call_once(registered, [] {
        WNDCLASSEXW field{};
        field.cbSize = sizeof field;
        field.lpfnWndProc = &KeyboardFieldProcedures::window;
        field.hInstance = GetModuleHandleW(nullptr);
        field.hCursor = LoadCursorW(nullptr, IDC_IBEAM);
        field.hbrBackground = reinterpret_cast<HBRUSH>(COLOR_WINDOW + 1);
        field.lpszClassName = kFieldClass;
        if (RegisterClassExW(&field) == 0) fatal("RegisterClassExW failed for the keyboard field");
    });
    root_ = GetAncestor(view_.hwnd(), GA_ROOT);
    // Over the game's window, which keeps it above itself; owned by it, so a
    // window that goes first takes the field with it.
    window_ = CreateWindowExW(WS_EX_TOOLWINDOW, kFieldClass, L"", WS_POPUP | WS_CLIPCHILDREN, 0, 0, 1, 1, root_, nullptr,
                              GetModuleHandleW(nullptr), this);
    if (window_ == nullptr) fatal("CreateWindowExW failed for the keyboard field");
    following_view_ =
        SetWindowSubclass(view_.hwnd(), &KeyboardFieldProcedures::follow, kFollowId, reinterpret_cast<DWORD_PTR>(this));
    if (root_ != nullptr && root_ != view_.hwnd()) {
        following_root_ =
            SetWindowSubclass(root_, &KeyboardFieldProcedures::follow, kFollowId, reinterpret_cast<DWORD_PTR>(this));
    }
}

KeyboardField::~KeyboardField() {
    finish(false);
    if (following_view_ && view_.hwnd() != nullptr) {
        RemoveWindowSubclass(view_.hwnd(), &KeyboardFieldProcedures::follow, kFollowId);
    }
    if (following_root_) RemoveWindowSubclass(root_, &KeyboardFieldProcedures::follow, kFollowId);
    if (window_ != nullptr) {
        SetWindowLongPtrW(window_, GWLP_USERDATA, 0);
        DestroyWindow(window_);
    }
    if (font_ != nullptr) DeleteObject(font_);
}

void KeyboardField::makeEditor(bool multiline, bool number) {
    if (editor_ != nullptr && multiline == multiline_ && number == number_) return;
    if (editor_ != nullptr) DestroyWindow(editor_);
    // One line, or several that scroll; digits only for a number keyboard.
    DWORD style = WS_CHILD | WS_VISIBLE | ES_LEFT;
    style |= multiline ? (ES_MULTILINE | ES_WANTRETURN | ES_AUTOVSCROLL | WS_VSCROLL) : ES_AUTOHSCROLL;
    if (number) style |= ES_NUMBER;
    editor_ = CreateWindowExW(0, L"EDIT", L"", style, 0, 0, 1, 1, window_,
                              reinterpret_cast<HMENU>(static_cast<INT_PTR>(kEditorControl)), GetModuleHandleW(nullptr),
                              nullptr);
    if (editor_ == nullptr) fatal("CreateWindowExW failed for the keyboard field's editor");
    SetWindowSubclass(editor_, &KeyboardFieldProcedures::editor, kEditorId, reinterpret_cast<DWORD_PTR>(this));
    multiline_ = multiline;
    number_ = number;
    font_dpi_ = 0;
}

void KeyboardField::show(const MigoKeyboardShowOptions &options) {
    const bool number = options.keyboard_type == MIGO_KEYBOARD_TYPE_NUMBER;
    // Digits are one line, as a number keyboard's field is.
    const bool multiline = (options.flags & MIGO_KEYBOARD_FLAG_MULTIPLE) != 0 && !number;
    makeEditor(multiline, number);
    hold_ = (options.flags & MIGO_KEYBOARD_FLAG_CONFIRM_HOLD) != 0;
    max_length_ = options.max_length;
    // The limit is in UTF-16 code units, the control's own; none is its largest.
    SendMessageW(editor_, EM_SETLIMITTEXT,
                 max_length_ > 0 && max_length_ < 0x7FFFFFFEU ? static_cast<WPARAM>(max_length_) : 0, 0);
    std::wstring initial = options.default_value_utf8 == nullptr
                               ? std::wstring()
                               : wideOf(std::string_view(options.default_value_utf8, options.default_value_length));
    if (max_length_ > 0 && initial.size() > max_length_) initial.resize(max_length_);
    setText(initial);
    place();
    const bool was_showing = showing_;
    showing_ = true;
    ShowWindow(window_, SW_SHOW);
    SetForegroundWindow(window_);
    SetFocus(editor_);
    // In CSS pixels: the view's pixels over its scale.
    if (!was_showing) {
        const float scale = std::max(view_.scale(), 0.25F);
        send(MIGO_KEYBOARD_EVENT_HEIGHT_CHANGE, std::wstring(), static_cast<double>(height_) / scale);
    }
}

void KeyboardField::hide() { finish(true); }

void KeyboardField::replaceText(std::string_view utf8) {
    if (!showing_) return;
    setText(wideOf(utf8));
}

std::wstring KeyboardField::text() const {
    if (editor_ == nullptr) return {};
    const int length = GetWindowTextLengthW(editor_);
    std::wstring text(static_cast<std::size_t>(std::max(0, length)) + 1, L'\0');
    const int copied = GetWindowTextW(editor_, text.data(), length + 1);
    text.resize(static_cast<std::size_t>(std::max(0, copied)));
    return multiline_ ? from_control(text) : text;
}

void KeyboardField::setText(const std::wstring &text) {
    quiet_ = true;
    SetWindowTextW(editor_, multiline_ ? to_control(text).c_str() : text.c_str());
    quiet_ = false;
    // The caret at the end, where typing goes on.
    const LRESULT end = GetWindowTextLengthW(editor_);
    SendMessageW(editor_, EM_SETSEL, static_cast<WPARAM>(end), end);
    SendMessageW(editor_, EM_SCROLLCARET, 0, 0);
}

/// Along the bottom of the game, its full width, in the system's message font.
void KeyboardField::place() {
    HWND view_window = view_.hwnd();
    if (view_window == nullptr || editor_ == nullptr) return;
    const UINT dpi = std::max<UINT>(GetDpiForWindow(view_window), USER_DEFAULT_SCREEN_DPI / 4);
    if (dpi != font_dpi_) {
        NONCLIENTMETRICSW metrics{};
        metrics.cbSize = sizeof metrics;
        SystemParametersInfoForDpi(SPI_GETNONCLIENTMETRICS, sizeof metrics, &metrics, 0, dpi);
        HFONT font = CreateFontIndirectW(&metrics.lfMessageFont);
        if (font != nullptr) {
            SendMessageW(editor_, WM_SETFONT, reinterpret_cast<WPARAM>(font), TRUE);
            if (font_ != nullptr) DeleteObject(font_);
            font_ = font;
            font_dpi_ = dpi;
        }
    }
    HDC context = GetDC(editor_);
    HGDIOBJ previous = SelectObject(context, font_);
    TEXTMETRICW metrics{};
    GetTextMetricsW(context, &metrics);
    SelectObject(context, previous);
    ReleaseDC(editor_, context);
    const int line = static_cast<int>(metrics.tmHeight + metrics.tmExternalLeading);
    const int padding = MulDiv(kPadding, static_cast<int>(dpi), USER_DEFAULT_SCREEN_DPI);
    height_ = (multiline_ ? line * kVisibleLines : line) + 2 * padding + 2 * GetSystemMetricsForDpi(SM_CYEDGE, dpi);
    RECT client{};
    GetClientRect(view_window, &client);
    POINT origin{0, std::max(0L, client.bottom - height_)};
    ClientToScreen(view_window, &origin);
    SetWindowPos(window_, HWND_TOP, origin.x, origin.y, client.right, height_, SWP_NOACTIVATE | SWP_NOOWNERZORDER);
    SetWindowPos(editor_, nullptr, padding, padding, std::max(1L, client.right - 2 * padding), height_ - 2 * padding,
                 SWP_NOZORDER | SWP_NOACTIVATE);
}

void KeyboardField::finish(bool report) {
    if (!showing_) return;
    // Before the window goes: hiding it moves the focus, which comes back here.
    showing_ = false;
    const std::wstring left = text();
    const bool had_keyboard = GetForegroundWindow() == window_ || GetFocus() == editor_;
    ShowWindow(window_, SW_HIDE);
    // The keys go back to the game the field took them from; a player who
    // turned to another window keeps it.
    if (had_keyboard && root_ != nullptr) {
        SetForegroundWindow(root_);
        SetFocus(view_.hwnd());
    }
    if (!report) return;
    send(MIGO_KEYBOARD_EVENT_HEIGHT_CHANGE, std::wstring(), 0.0);
    send(MIGO_KEYBOARD_EVENT_COMPLETE, left);
}

void KeyboardField::edited() {
    // The player's edits only: content's own text is not one.
    if (!showing_ || quiet_) return;
    send(MIGO_KEYBOARD_EVENT_INPUT, text());
}

void KeyboardField::send(MigoKeyboardEventType type, const std::wstring &value, double height) {
    const std::string utf8 = utf8Of(value);
    MigoKeyboardEvent event{};
    event.struct_size = static_cast<std::uint32_t>(sizeof event);
    event.abi_version = MIGO_ABI_VERSION_1;
    event.event_type = type;
    event.value_length = static_cast<std::uint32_t>(utf8.size());
    event.value_utf8 = utf8.c_str();
    event.height_css_px = height;
    // Refused only without a Surface, when there is no content to tell.
    (void)migo_session_send_keyboard_event(session_, &event);
}

}  // namespace migo::windows_host
