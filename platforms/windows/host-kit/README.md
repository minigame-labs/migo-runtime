# Migo Windows Host Kit

The Windows counterpart of the Linux Host Kit (`platforms/linux/host-kit`): what
a Win32 App builds on to run a Migo game -- above the C ABI that `migo.dll`
exports, below the App's own window and services.

It is being built in stages (migo-runtime's schedule item X1):

- `migo::windows-surface-host` (`include/migo/windows/surface_host.hpp`): the
  Surface lifecycle controller -- attach a host-owned child `HWND`, update its
  metrics, retire it and observe its release -- owned on one thread, linking
  nothing of the window system. The same contract as the Linux Host Kit's
  `migo::linux_host::SurfaceHost`: a change to one is a change to both.
- `migo::win32-surface-view` (`include/migo/windows/win32_surface_view.hpp`):
  the game's view, the Linux Host Kit's `MigoQtX11SurfaceView` for Win32 -- a
  child window it creates in the App's, attached once shown at a size and
  updated as it is resized or moves to a screen of another scale (physical
  pixels; the scale is the window's DPI over 96, so the process should be
  per-monitor DPI aware); the mouse as a touch while pressed and as the pointer
  stream, the wheel, touch screens and pens (with pressure), keys named as the
  DOM names them, the IME's composition, focus, a pointer lock through raw
  input, and frames paced by the desktop compositor (DwmFlush on a thread of
  its own). Teardown is the C ABI's: detach, the release observed, then the
  window may go -- `close()` does all three.

- `migo::windows-host-services` (`include/migo/windows/host_services.hpp`,
  `json.hpp`): the host-service channel -- answers and events back to one
  Session, closed before it is destroyed so a late answer reaches nobody, each
  call answered once from any thread, a failure always with its reason -- the
  provider interface an App's own services implement, and the JSON their
  requests and answers are made of (the Host Kit's own: strict RFC 8259 in,
  compact out).
- `migo::windows-managed-session` (`include/migo/windows/managed_session.hpp`):
  the Managed shape -- one Session owned with its callback table, its view and
  its teardown. Engine callbacks run on the GUI thread, posted to a
  message-only window of its own (refused once teardown begins; the tasks
  already handed over run exactly once); content is loaded once the view
  attaches; the App's top-level window being shown and not minimized is the
  game running and showing, reported as it changes; frame requests go to the
  view, setKeepScreenOn holds the display awake while the game runs; the App's
  services answer what it names, the Host Kit's built-in ones the rest; who the
  App is defaults to its process's AppUserModelID, else its executable's name;
  content's setEnableDebug shows the debugging panel -- frames a second and
  content's console over the top of the game, taking no input -- and reports
  the aids on (`debugAids`/`setDebugAids`); content's showKeyboard opens a
  real text field along the bottom of the game (the system's edit control,
  one line or several, digits only for a number keyboard, the limit in UTF-16
  code units), reporting each edit, a confirm and its closing; the battery
  (power-setting notifications to the App's window), the network (the Network
  List Manager's events, the adapter's kind) and memory pressure (the system's
  low-memory notification, as CRITICAL) are reported as they change; the
  gamepads Windows.Gaming.Input sees are announced as the view attaches and
  read while the game runs -- an Xbox-class pad on the W3C standard layout
  with dual and trigger rumble, any other controller as its driver gives it;
  what a
  desktop App is not gated on is reported authorized. The GUI thread is a
  single-threaded COM apartment, joined if the App has not made it one.
- `migo::windows-builtin-host-services`
  (`include/migo/windows/builtin_host_services.hpp`): what Windows already
  offers an App, answered with its own -- toasts and loading indicators (a
  card over the game drawn with Direct2D on a layered window, taking no input,
  `mask` keeping the game's from it); modals and action sheets (dialogs of the
  system's controls, modal to the App's window without holding its message
  loop, the keyboard's dialog navigation through a thread message hook); the
  system clipboard (text); the
  window (setWindowSize sizes the game, its state reported, setCursor from a
  CSS keyword or a .cur/.ani file, the view's pointer lock reported and a
  refusal said); the screen (orientation is the game's shape, reported as it
  crosses and turned by setDeviceOrientation; setVisualEffectOnCapture keeps
  the window out of captures; setEnableDebug); the scopes a desktop decides
  (writePhotosAlbum granted); images and files -- the system's open dialog
  on a thread of its own (modal to the App's window, its message loop
  untouched), what is picked copied and staged, images through WIC (drawable
  formats kept, others written upright), videos described by Media Foundation
  with the shell's thumbnail as cover, compressImage, saveImageToPhotosAlbum
  into the Pictures folder, saveFileToDisk through the save dialog, and the
  previews in a Direct2D viewer, http(s) images downloaded first
  (`FileLocations` moves the staging directory and the album); location --
  Windows' location service (Windows.Devices.Geolocation), as its privacy
  settings let desktop Apps use it, which also decide scope.userLocation; the
  recorder -- WASAPI's shared-mode capture of the default input, the system
  converting to the rate asked, written as PCM, WAV or AAC through Media
  Foundation (.m4a, or an ADTS stream cut into frames), one recording at a
  time on a thread of its own. An App can use it in the Bound shape too.

Next: videos.

Build it with CMake against the Migo Windows SDK package
(`find_package(migo)`), and link the targets it exports
(`find_package(migo-windows-host-kit)`).

Its tests run on Windows against a strict fake of the C ABI
(`tests/host_kit_fake`), through `scripts/test-windows-host-kit.sh` (Git Bash,
an MSVC developer environment), in PR CI's `windows-host-kit` job.
