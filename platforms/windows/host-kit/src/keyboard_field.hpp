#ifndef MIGO_WINDOWS_HOST_KIT_KEYBOARD_FIELD_HPP_
#define MIGO_WINDOWS_HOST_KIT_KEYBOARD_FIELD_HPP_

// Internal to the Host Kit, not installed: the text field content's
// showKeyboard opens.

#include <migo/migo.h>
#include <migo/windows/win32_surface_view.hpp>

#include <cstdint>
#include <string>
#include <string_view>

struct HWND__;
struct HFONT__;

namespace migo::windows_host {

/// The desktop's answer to content's showKeyboard -- the Linux Host Kit's, for
/// Win32: a real text field along the bottom of the game, whose height content
/// is told the keyboard has. A PC has a physical keyboard and no soft one, and
/// content has no field of its own to type into: its text is typed here, with
/// the input method the player uses, and the touch keyboard a tablet opens
/// for a focused field. The game is drawn into a child window a sibling cannot
/// reliably stand above, so the field is a window of its own over it, owned by
/// the App's window, that takes the keyboard while it shows: the system's
/// edit control, one line or several, digits only for a number keyboard.
///
/// What it reports is the Session's soft-keyboard events: the field's whole
/// text at each edit the player makes (not content's own correction), the text
/// at a confirm (Enter, in a one-line field), and, when it goes -- content's
/// hideKeyboard, a confirm content did not ask to hold, the player pressing Esc
/// or turning to the game or another window -- the height 0 and the text it was
/// left with. A length limit counts UTF-16 code units, as Android's does.
/// GUI thread only.
class KeyboardField final {
public:
    KeyboardField(MigoSession *session, Win32SurfaceView &view);
    /// Puts the field away without a word to the Session, which is going.
    ~KeyboardField();
    KeyboardField(const KeyboardField &) = delete;
    KeyboardField &operator=(const KeyboardField &) = delete;

    /// Content's showKeyboard. A field already showing takes the new request.
    void show(const MigoKeyboardShowOptions &options);
    /// Content's hideKeyboard. Nothing when the field is not showing.
    void hide();
    /// Content's updateKeyboard: its correction of the whole text, which is not
    /// reported back to it.
    void replaceText(std::string_view utf8);
    [[nodiscard]] bool isShowing() const noexcept { return showing_; }
    /// The field's window, for the App's tests and its accessibility.
    [[nodiscard]] HWND__ *hwnd() const noexcept { return window_; }

private:
    friend struct KeyboardFieldProcedures;

    void makeEditor(bool multiline, bool number);
    void place();
    std::wstring text() const;
    void setText(const std::wstring &text);
    /// The field goes; `report` says whether the Session is told.
    void finish(bool report);
    void edited();
    void send(MigoKeyboardEventType type, const std::wstring &value, double height = 0.0);

    MigoSession *const session_;
    Win32SurfaceView &view_;
    HWND__ *root_ = nullptr;
    HWND__ *window_ = nullptr;
    HWND__ *editor_ = nullptr;
    HFONT__ *font_ = nullptr;
    unsigned font_dpi_ = 0;
    bool following_view_ = false;
    bool following_root_ = false;

    bool showing_ = false;
    bool multiline_ = false;
    bool number_ = false;
    bool hold_ = false;
    std::uint32_t max_length_ = 0;
    int height_ = 0;
    // Set while the field's text is content's, not the player's.
    bool quiet_ = false;
};

}  // namespace migo::windows_host

#endif  // MIGO_WINDOWS_HOST_KIT_KEYBOARD_FIELD_HPP_
