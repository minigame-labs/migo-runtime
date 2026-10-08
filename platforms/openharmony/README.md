# OpenHarmony host

An ordinary DevEco project that consumes migo's C SDK: it sees only the public
headers in `include/migo/` and links `libmigo_capi.a` into its own
`libmigohost.so`. That is the same relationship the Android NativeActivity host
has with the Android C SDK, and keeping it that way is what makes this a test of
the SDK rather than an extension of it.

The surface is an ArkUI `XComponent`. Its `OnSurfaceCreated` callback hands over
an `OHNativeWindow*`, which is exactly what
`MigoOpenHarmonyNativeWindowDescriptor` carries — no translation, only ownership
discipline: the host keeps its reference, the engine takes its own, and the host
must not destroy the window until the release observer reports `RELEASED`.

## Build and run

```sh
bash scripts/build-ohos-host.sh --arch x86_64   # stage the SDK, build the HAP
bash scripts/run-ohos-host.sh --shot /tmp/s.jpeg # install, launch, screenshot
```

`x86_64` is the emulator's architecture. `--arch aarch64` builds for real
hardware; `entry/build-profile.json5` filters ABIs, so building for an
architecture whose archive has not been staged fails the CMake existence check
with a clear message rather than linking something stale.

### When building from WSL2 with DevEco on Windows

The engine is built in WSL and DevEco, hvigor, hdc and the emulator all live on
Windows. Two constraints follow, and both were found by hitting them:

- **hvigor rejects a UNC project path** outright (`Invalid project path`), so
  the project cannot be built in place on `\\wsl.localhost`.
  `build-ohos-host.sh` copies it to `C:\migo-ohos-host` (override with
  `MIGO_OHOS_WIN_DIR`) and builds there.
- **`cmd.exe` refuses a UNC working directory**, prints a warning and falls back
  to `C:\Windows`. Combined with a strict error mode that reads as a compile
  failure when nothing has been compiled yet, so every `cmd` invocation runs
  from a local directory.

`hdc`, not `adb`. They are different protocols with different daemons, so
`adb devices` showing nothing here is the expected result — and an Android phone
attached at the same time appears in `adb` and not in `hdc`.

## Content

The HAP ships `entry/src/main/resources/rawfile/content/` and stages it into the
app's files directory on first run. It cannot be pushed there from outside: the
sandbox path is visible only to this process, and `hdc` runs as the
unprivileged `shell` user. Shipping it inside the package is also what a real
application does.

The bundled content is the **touch probe**: the whole screen is one colour, and
it changes only when input arrives — red before any touch, green while a finger
is down, blue once every finger has lifted. Nothing else in it changes over
time, so any pixel difference between two frames is attributable to input having
crossed the C ABI, the engine, and reached JS.

That matters because a native host has no other window into content-side
behaviour: the library does not hijack a global logger, and an exception in JS
produces no output unless `MIGO_CAPI_LOG` was set before the engine was created.
A screenshot needs nothing at all.

## Verified

On an API 20 emulator (Mate 70 Pro profile, 1316×2598):

- surface attach (generation 1), content load, `content is ready`
- rendering — the probe's red fills the display
- a full touch lifecycle: red before the tap, blue after the finger lifts,
  confirmed by sampling the rendered pixel rather than by reading a log

Not verified: `aarch64` on real hardware, and multi-finger input — `hdc` cannot
synthesise a second pointer, the same limitation `adb` has on Android.
