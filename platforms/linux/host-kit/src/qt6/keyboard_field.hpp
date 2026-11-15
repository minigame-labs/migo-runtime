#ifndef MIGO_LINUX_QT6_KEYBOARD_FIELD_HPP_
#define MIGO_LINUX_QT6_KEYBOARD_FIELD_HPP_

// Internal to the Host Kit, not installed: the text field content's
// showKeyboard opens.

#include <migo/linux/qt6/x11_surface_view.hpp>
#include <migo/migo.h>

#include <QObject>
#include <QPointer>
#include <QString>
#include <QWidget>

#include <cstdint>

class QPlainTextEdit;
class QValidator;

namespace migo::linux_host::qt6 {

/// The desktop's answer to content's showKeyboard: a real text field along the
/// bottom of the game, whose height content is told the keyboard has. A
/// desktop has a physical keyboard and no soft one, and content has no field of
/// its own to type into: its text is typed here, and an on-screen keyboard that
/// follows the focused field -- a touch device's -- opens for it, with the
/// digits and the confirm key content asked for. The game is drawn in a native
/// window a sibling widget cannot stand above, so the field is a window of its
/// own over it, as the other overlays are, but one that takes the keyboard's
/// focus while it shows.
///
/// What it reports is the Session's soft-keyboard events: the field's whole
/// text at each edit the player makes (not the keystroke, and not content's own
/// correction), the text at a confirm (Enter, in a one-line field), and, when
/// it goes -- content's hideKeyboard, a confirm content did not ask to hold,
/// the player pressing Esc or turning to the game or another window -- the
/// height 0 and the text it was left with. A length limit counts UTF-16 code
/// units, as Android's does. GUI thread only.
class KeyboardField final : public QObject {
public:
    KeyboardField(MigoSession *session, MigoQtX11SurfaceView &view);
    /// Puts the field away without a word to the Session, which is going.
    ~KeyboardField() override;
    KeyboardField(const KeyboardField &) = delete;
    KeyboardField &operator=(const KeyboardField &) = delete;

    /// Content's showKeyboard. A field already showing takes the new request.
    void show(const MigoKeyboardShowOptions &options);
    /// Content's hideKeyboard. Nothing when the field is not showing.
    void hide();
    /// Content's updateKeyboard: its correction of the whole text, which is
    /// not reported back to it.
    void replaceText(const QString &text);
    [[nodiscard]] bool isShowing() const noexcept { return showing_; }

protected:
    bool eventFilter(QObject *watched, QEvent *event) override;

private:
    class Line;

    QWidget *editor() const;
    QString text() const;
    void place();
    /// The field goes; `report` says whether the Session is told.
    void finish(bool report);
    void send(MigoKeyboardEventType type, const QString &value, double height = 0.0);
    void linesEdited();

    MigoSession *const session_;
    QPointer<MigoQtX11SurfaceView> view_;
    QPointer<QWidget> window_;
    Line *line_ = nullptr;
    QPlainTextEdit *lines_ = nullptr;
    /// What a number keyboard can type: digits.
    QValidator *digits_ = nullptr;
    bool showing_ = false;
    bool multiline_ = false;
    bool hold_ = false;
    /// The editor has had the focus since the field opened: losing it now is
    /// the player turning away, and before it is the field's own coming up.
    bool focused_ = false;
    std::uint32_t max_length_ = 0;
    int height_ = 0;
};

}  // namespace migo::linux_host::qt6

#endif  // MIGO_LINUX_QT6_KEYBOARD_FIELD_HPP_
