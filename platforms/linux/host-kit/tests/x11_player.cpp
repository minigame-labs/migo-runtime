#include "x11_player.hpp"

#include <QGuiApplication>
#include <QtGui/qguiapplication_platform.h>

#include <X11/Xlib.h>
#include <X11/extensions/XTest.h>
#include <X11/keysym.h>

namespace x11_player {

namespace {

/// Qt's own connection: what the server makes reaches Qt in order with what
/// Qt asked of it.
Display *display() {
    return qGuiApp->nativeInterface<QNativeInterface::QX11Application>()->display();
}

}  // namespace

Hands::Hands() {
    XGetPointerControl(display(), &numerator_, &denominator_, &threshold_);
    XChangePointerControl(display(), True, True, 1, 1, 1);
    XSync(display(), False);
}

Hands::~Hands() {
    XChangePointerControl(display(), True, True, numerator_, denominator_, threshold_);
    XSync(display(), False);
}

void Hands::move(int dx, int dy) {
    XTestFakeRelativeMotionEvent(display(), dx, dy, CurrentTime);
    XFlush(display());
}

void Hands::click(unsigned button) {
    XTestFakeButtonEvent(display(), button, True, CurrentTime);
    XTestFakeButtonEvent(display(), button, False, CurrentTime);
    XFlush(display());
}

void Hands::escape() {
    const KeyCode escape = XKeysymToKeycode(display(), XK_Escape);
    XTestFakeKeyEvent(display(), escape, True, CurrentTime);
    XTestFakeKeyEvent(display(), escape, False, CurrentTime);
    XFlush(display());
}

}  // namespace x11_player
