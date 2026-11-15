#ifndef MIGO_LINUX_QT6_OVERLAY_HPP_
#define MIGO_LINUX_QT6_OVERLAY_HPP_

// Internal to the Host Kit, not installed: what stands over the game.

#include <QWidget>
#include <QWindow>

namespace migo::linux_host::qt6 {

/// Show `overlay` -- a window of its own over the game, which is drawn in a
/// native window a sibling widget cannot stand above -- taking none of the
/// player's input: a click on it is the game's.
inline void showOverTheGame(QWidget &overlay) {
    overlay.setAttribute(Qt::WA_ShowWithoutActivating);
    overlay.show();
    // Qt gives an X window an empty input shape only when the flag changes on a
    // window that exists; one created with the flag takes input (Qt 6.4, xcb).
    overlay.windowHandle()->setFlag(Qt::WindowTransparentForInput, true);
}

}  // namespace migo::linux_host::qt6

#endif  // MIGO_LINUX_QT6_OVERLAY_HPP_
