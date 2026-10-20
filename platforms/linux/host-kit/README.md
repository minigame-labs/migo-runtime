# Migo Linux Host Kit

This directory contains source-level adapters for embedding Migo in an
application-owned Linux UI. It does not create an application, a top-level
window, or an event loop.

What is claimed:

- `migo::linux-surface-host` is a toolkit-neutral C++17 lifecycle controller
  for X11 and Wayland handles supplied by a host.
- `migo::qt6-x11-surface-view` is a Qt 6.4+ Widgets adapter for the `xcb`
  platform plugin, using a native child `QWidget`. It carries the Surface
  lifecycle, input, focus, IME composition and the frame request.
- `migo::qt6-host-services` is the host-service channel for Qt: a reply that
  answers a call exactly once, from any thread, and reaches nobody once its
  Session's channel is closed; the provider interface an App answers its own
  services through; and `MigoQtBuiltInHostServices`, which answers with Qt's own
  what a desktop already offers -- the clipboard; the game's toasts, loading
  indicators, modals and action sheets; and the window's size, state and cursor
  (a pointer lock is refused until the Host Kit reads XInput 2's raw motion).
- `migo::qt6-managed-session` is the Managed ownership shape: `MigoManagedSession`
  owns the Session, its callback table and the view, and installs the built-in
  services. Its `Options` carry the network policy, the launch options and the
  App's own services; what the App names is its, the built-in services answer the
  rest. The Bound shape is the view driven by an App-owned `SurfaceHost`. They are
  separate targets, so an App that owns its own Session never links a type that
  creates and destroys Sessions.
- Qt Wayland, Qt Quick and GTK 4 are not claimed, and the last two are blocked
  on the same thing -- see "Render-path limits".

## Managed or Bound

| | Bound | Managed |
|---|---|---|
| Session owner | the App | `MigoManagedSession` |
| Callback table | the App installs it | the wrapper installs it |
| `MigoEngine` | the App | the App -- the wrapper never owns one |
| View disappears | only the attachment retires; the Session can stay paused and bind a new view | Surface retires, then the Session is destroyed |

They are two types rather than one type with a flag, because what differs is who
is responsible for an asynchronous teardown, and a boolean cannot answer that.

`MigoManagedSession` deliberately declines the soft-keyboard capability, and
declines it whole: the three callbacks install together or not at all, and the
mini-game input model migo mirrors requires the host to own a text field and
report its whole current value. A desktop host has a physical keyboard whose
input already reaches content as key and composition events, so claiming the
capability and not maintaining a field would be worse than not having it --
content's `migo.showKeyboard` (or the same call made through a platform-compat
adapter) correctly reports failure instead.

Teardown is the C ABI's three steps and cannot be shortened: `close()` begins
the detach and returns immediately, the view's release observer reports
`RELEASED`, and only then is the Session destroyed. Destroying the wrapper while
it is still running terminates, for the same reason the view does.

A whole App on the Managed shape -- an engine, a window, a game with its network
policy and launch options, closing on the game's exit or the window's -- is
`tests/qt_host/linux/main.cpp`; `scripts/dev-run-qt-host.sh` builds and runs it
against a game directory. It is what migo-conformance's Host Kit lane runs, so
what that lane verifies is this shape as an App uses it.

## Input, focus and frames

Translation happens in the view's own event handlers on the GUI thread. There is
no event filter of any kind, no private Qt API, no polling and no thread hop: a
call from another thread returns `MIGO_ERROR_WRONG_THREAD` before touching Qt
state or entering Migo.

- **Coordinates are CSS pixels, which need no conversion.** Qt's logical
  position is physical pixels divided by the device pixel ratio, and that is the
  same ratio the view reports as `scale_factor` at attach. Multiplying by it
  here -- the conversion that looks missing -- puts every tap in the wrong place
  on a HiDPI screen.
- **A mouse drives both streams by default.** Mini-game content written for a phone
  listens for touch; content written for a PC mini-game platform listens for the mouse.
  Neither is synthesized from the other by the engine, so the view sends both
  and `setPointerDelivery()` narrows it. Content that listens for both would
  otherwise act on one press twice. Hover reaches the mouse stream only:
  mini-game content on a phone has no hover concept.
- **`code` comes from the hardware scan code, `key` from the layout.** The key
  that produces "a" on a French keyboard is still `KeyQ`, so WASD movement works
  for every layout. A key this build cannot name is reported as
  `"Unidentified"`, never as an empty `code`, which the C ABI rejects.
- **Losing focus retracts what is still in progress**: a held press becomes a
  touch cancel and an open preedit is ended. Both are states with no later event
  that would correct them.
- **Frames follow Qt's clock.** The App calls `requestFrame()` from its own
  `on_request_frame` callback -- the view never installs the callback table --
  and the view arms `QWindow::requestUpdate()`. A repaint Qt performs for its
  own reasons is not reported as a frame boundary. There is no interval timer
  driving frames anywhere in this adapter.
- **The delivery path allocates nothing per event**, verified by counting
  `malloc` (Qt's containers do not use `operator new`) across a delivered burst
  against an identical undelivered one.

Deliberately not delivered, so they are not mistaken for gaps to be filled
later:

- **The preedit's cursor and selection.** `QInputMethodEvent` carries them, but
  the DOM `CompositionEvent` does not, so a browser does not give them to canvas
  content either. Matching the Web contract is the point; adding them would be
  Migo-specific API that no existing HTML5 game reads.
- **A synthetic key release on focus loss.** A browser does not synthesize one,
  and inventing an "up" for a key the user may still be holding is its own wrong
  answer. Unlike a press or a preedit, the engine makes no promise about held
  keys across a focus change.
- **Hover as touch.** mini-game content has no hover concept; a free motion stream
  would be events no game reads.

The public Migo C ABI is still marked `MIGO_C_ABI_CANDIDATE`. Treat this Host
Kit as an integration preview until that ABI is frozen.

## Ownership contract

The application owns the `MigoSession`, `SurfaceHost`, parent widget and native
window. Keep both dependency chains valid:

```text
QApplication > parent QWidget > MigoQtX11SurfaceView
MigoSession > SurfaceHost ───────> MigoQtX11SurfaceView
```

Create exactly one `SurfaceHost` for a Session and keep it at a stable address
for that Session's surface lifetime. Replacement views borrow the same
controller so surface generations stay strictly increasing. A Session has at
most one Attached or Retiring view; attach a replacement only after the prior
release reaches RELEASED. A replacement may be constructed earlier, but it
remains passive: its resize, close and destruction cannot act on the current
owner's generation. The controller
never owns the Session, X11 `Display`, XID, `wl_display`, or `wl_surface`.
`QApplication` must already exist before the Qt view and must outlive its final
release, because Qt owns the X11 connection and GUI event loop. The
toolkit-neutral `SurfaceHost` itself has no such Qt dependency.
After a view successfully attaches, use that view's resize/detach/poll path for
its generation; calling the borrowed controller directly would bypass the
native-window lifetime guards.

Before deleting the view or any ancestor that owns its native child window,
call `close()` or `beginDetach()`, then keep the widget and GUI event loop alive
until `surfaceReleased` is emitted. A pending release is a real driver lease;
destroying the window early is a use-after-free. The adapter fails fast if Qt
tries to destroy the native surface before release instead of hiding this bug.
Polling is fast only during the expected short retirement window, then backs
off; after two seconds `surfaceReleaseStalled` fires once for diagnostics while
the authoritative observer remains alive and continues to be checked.
If the ABI query/destroy operation itself returns an error, automatic polling
pauses to avoid an error storm but the view stays Retiring and keeps ownership;
the host must diagnose the error and explicitly call `pollDetach()` to retry.
Reparenting may recreate the XID and therefore is allowed only after the view is
Detached; attach it again after the new parent/native child is established.
`hide()` alone does not retire the native target or change Session lifecycle;
the Bound host decides whether a hidden pane stays warm, pauses the Session, or
calls `beginDetach()` to release presentation resources.

Construct `SurfaceHost` on the Qt GUI thread that owns the Session. All methods
must stay on that thread and be serialized with other calls through the same
Session; foreign-thread calls return `MIGO_ERROR_WRONG_THREAD` before entering
the C ABI. The QWidget adapter also rejects a foreign thread before reading or
mutating any Qt state. The Bound Session host remains responsible for
callbacks, lifecycle/focus, content loading, input and audio-focus policy. Migo
continues to own its default audio renderer; this surface-only adapter neither
opens an audio device nor provides a host-mixer API.

## Build and consume from source

On Ubuntu 24.04, the exact dependency set for the current Qt Widgets/X11
adapter and its contract tests is:

```bash
sudo apt-get update
sudo apt-get install -y \
  cmake ninja-build g++ ripgrep xvfb xauth \
  qt6-base-dev qt6-base-dev-tools qt6-qpa-plugins \
  libx11-dev libxkbcommon-x11-dev libxcb-cursor0
```

The adapter's compile-time API uses Qt Core/Gui/Widgets and Xlib. The remaining
packages supply the xcb platform plugin's runtime dependencies and the isolated
Xvfb positive-path test. A native Wayland host that consumes only the
toolkit-neutral controller also needs its distribution's Wayland development
package (on Ubuntu: `libwayland-dev`), but that does not turn the Qt adapter
into a Wayland implementation. Do not install Qt private-header packages for
this Host Kit.

The effective deployment floor is the stricter of the core SDK manifest
(`glibc`, GLIBCXX, arch and CPU baseline) and the Qt distribution selected by
the application. “Qt 6.4+” is an adapter API floor, not a claim that every Qt
6.4 package runs on every glibc 2.31 distribution. Official prebuilt Host Kit
artifacts, if introduced later, must record both identities in their own
artifact manifest plus the exact compatible core SDK manifest hash (which pins
V8 revision, GN arguments and snapshot policy/inputs). The current source build
inherits the application's toolchain and Qt ABI instead.

After installing or staging the Migo Linux SDK so `find_package(migo)` works:

```cmake
find_package(migo CONFIG REQUIRED)
add_subdirectory(path/to/migo/platforms/linux/host-kit migo-host-kit)

target_link_libraries(my_app PRIVATE migo::qt6-x11-surface-view)
```

Applications that provide their own native integration can link only
`migo::linux-surface-host`; this keeps Qt out of their dependency graph. The
Host Kit can also be installed and consumed. Install rules default to ON
for a standalone Host Kit configure and OFF when it is an App subdirectory, so
embedding it cannot silently change the App's install manifest:

```bash
cmake -S platforms/linux/host-kit -B build/host-kit \
  -DCMAKE_PREFIX_PATH=/path/to/migo-sdk \
  -DCMAKE_INSTALL_PREFIX=/path/to/prefix \
  -DMIGO_LINUX_HOST_KIT_ENABLE_INSTALL=ON
cmake --build build/host-kit
cmake --install build/host-kit
```

The installed consumer then uses:

```cmake
find_package(migo-linux-host-kit CONFIG REQUIRED)
target_link_libraries(my_app PRIVATE migo::qt6-x11-surface-view)
```

Minimal construction after the application has created a Session:

```cpp
class GamePane final : public QWidget {
public:
    GamePane(MigoSession *session, QWidget &parent)
        : QWidget(&parent), surface_host_(session), game_view_(surface_host_, *this) {
        layout_.addWidget(&game_view_);
        setLayout(&layout_);
    }

private:
    QVBoxLayout layout_;
    migo::linux_host::SurfaceHost surface_host_;
    migo::linux_host::qt6::MigoQtX11SurfaceView game_view_;
};
```

Production shutdown must wait for `surfaceReleased` before destroying
`GamePane`. A real application should connect that signal to its asynchronous
window/session shutdown coordinator rather than blocking the GUI thread.

## Render-path limits

The X11 widget is a direct native-surface path: Migo presents to the child XID
without CPU copies, `QPainter`, framebuffer readback or an intermediate Qt
texture. It keeps the host's ancestor widgets non-native, so embedding one game
surface does not impose native-window painting and resize costs on the whole UI
tree. Qt Quick uses a compositor-owned scene graph, so a child-window
overlay would break clipping, transforms and frame scheduling. It remains
unsupported until Migo exposes an explicit zero-copy texture plus synchronization
contract. Qt 6.4 also does not expose the Wayland display/surface pair needed by
this adapter through a supported public API; private Qt headers are forbidden.

GTK 4 is blocked by the same missing contract, which is not obvious from the
roadmap's ordering: it gives a `GdkSurface` only to widgets implementing
`GtkNative` -- the toplevel and popovers -- and removed `GtkSocket`/`GtkPlug`, so
there is no public way to place a native target inside an App's layout for Migo
to present into. The only thing on offer is the toplevel's own surface, and
presenting there ignores layout, clipping and z-order, which is precisely the
child-window overlay this project refuses as a fallback.
`scripts/test-gtk4-surface-capability.sh` runs a probe that establishes this, so
the claim is evidence rather than a documentation reading -- and so that the day
GTK 4 changes its answer, the gate fails and somebody re-reads the roadmap.

Run the isolated contract without building V8:

```bash
bash scripts/test-linux-qt-host-kit.sh
```
