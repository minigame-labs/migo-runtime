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

Next: a Managed session owning the Session and its callbacks, the host-service
channel, and the built-in services the platform's API asks of a Windows PC.

Build it with CMake against the Migo Windows SDK package
(`find_package(migo)`), and link the targets it exports
(`find_package(migo-windows-host-kit)`).

Its tests run on Windows against a strict fake of the C ABI
(`tests/host_kit_fake`), through `scripts/test-windows-host-kit.sh` (Git Bash,
an MSVC developer environment), in PR CI's `windows-host-kit` job.
