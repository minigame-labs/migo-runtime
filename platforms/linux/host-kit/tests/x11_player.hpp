#ifndef MIGO_LINUX_HOST_KIT_TESTS_X11_PLAYER_HPP_
#define MIGO_LINUX_HOST_KIT_TESTS_X11_PLAYER_HPP_

// The player's hands on the X server a test runs against, through XTest: input
// the server makes, not events sent to a widget, for what only the server's own
// input path shows -- raw motion, a grab, the keyboard focus. Its own unit,
// because Xlib's macros (None, KeyPress, FocusOut, ...) break Qt's names
// wherever both are seen.

namespace x11_player {

/// While one lives the mouse moves the pointer exactly as far as it is moved:
/// the server's acceleration is off, and is put back as it was after.
class Hands {
public:
    Hands();
    ~Hands();
    Hands(const Hands &) = delete;
    Hands &operator=(const Hands &) = delete;

    /// Moves the mouse by (dx, dy) of the screen's pixels.
    void move(int dx, int dy);
    /// Presses and releases mouse button `button`, 1 being the primary.
    void click(unsigned button);
    /// Presses and releases Esc.
    void escape();

private:
    int numerator_ = 0;
    int denominator_ = 0;
    int threshold_ = 0;
};

}  // namespace x11_player

#endif  // MIGO_LINUX_HOST_KIT_TESTS_X11_PLAYER_HPP_
