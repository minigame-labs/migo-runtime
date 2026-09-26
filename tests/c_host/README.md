# Embedding migo from C

One example, two platforms. Both hosts do the same thing — create an engine and
a session, attach the window they own, load content, and forward input — and
both see nothing but the headers under `include/migo`. If either ever needs
something outside those headers, the ABI is incomplete, and finding that out is
the reason these exist.

| | |
|---|---|
| `linux/` | An X11 host. Built with plain `cc` and `pkg-config`, or with CMake through `find_package(migo)`. |
| `android/` | A NativeActivity host. No Java at all: the manifest declares `android:hasCode="false"` and names this library's `ANativeActivity_onCreate`. |
| `touch-probe/` | Content shared by both, for verifying that input arrives. |
| `android-multitouch/` | An instrumentation APK that injects a real two-finger gesture into `android/` running `touch-probe` and reads the pointer count back as pixels. Run it with `scripts/verify-android-c-host-multitouch.sh`; it needs a device, and nothing else here can produce a second finger. |
| `keyboard-probe/` | Content shared by both, for verifying the soft-keyboard round trip. |
| `surface-recreate-probe/` | Content shared by both, for verifying that the main canvas still describes the surface after the window was destroyed and recreated at a different size. |
| `lifecycle-probe/` | Content shared by both, for verifying that the engine stops painting while the app is away and that content is told it went away. |

The Android module lives here rather than under `platforms/android/` because it
is a *consumer* of what that tree ships, not part of the product. Gradle picks
it up through `projectDir` in `platforms/android/settings.gradle`.

## Linux

```sh
bash scripts/build-linux-sdk.sh              # stages dist/migo-linux-x86_64
bash tests/c_host/linux/build-with-pkgconfig.sh
./tests/c_host/linux/c-host <files-dir> <content-id> <seconds>
```

## Android

```sh
bash scripts/build-android-c-host.sh         # arm64-v8a: cross-compiles capi, builds the APK
bash scripts/build-android-c-host.sh x86_64  # the ABI an emulator runs at usable speed
adb install -r tests/c_host/android/build/outputs/apk/debug/*.apk
```

Content goes where the engine resolves it, under the app's own files directory:
`<files>/migo/games/<content-id>/code/{game.json,game.js}`. The host reads
`<files>/content-id` to decide which bundle to load, so switching probes needs no
rebuild:

```sh
adb shell "cat game.js | run-as com.migo.chost sh -c 'cat > files/migo/games/<id>/code/game.js'"
adb shell "echo <id> | run-as com.migo.chost sh -c 'cat > files/content-id'"
```

Backgrounding the app destroys the window and resuming creates a new one, which
is the lifecycle no desktop has. Rotating **while backgrounded** is the version of
it that hands the engine a window of a *different* size, rather than a resize of
the same one — a distinction worth keeping in mind, because the two take different
paths through the engine and only the second one destroys the surface:

```sh
adb shell input keyevent KEYCODE_HOME
adb shell settings put system accelerometer_rotation 0
adb shell settings put system user_rotation 1
adb shell am start -n com.migo.chost/android.app.NativeActivity
```

## What each platform proves that the other cannot

The Linux host maps one mouse to one touch point, so it cannot exercise the
multi-pointer path at all. The Android host carries every pointer from
`AMotionEvent_getPointerCount`, which is the only place `MIGO_TOUCH_MAX_POINTS`
and the per-pointer `CHANGED`/`REMOVED` flags are tested.

Android is also the only place the lifecycle contract runs for real: nothing on
a desktop takes the application away, so `migo_session_set_visibility` and the
detach/attach cycle only meet genuine pause, resume and window loss here.

The soft keyboard splits the same way. On Linux the host has no keyboard to
raise, so `on_show_keyboard` only proves the callback arrives with the right
options. On Android it drives the real system IME through
`ANativeActivity_showSoftInput`, which is the only place content's
`migo.showKeyboard` raises an actual keyboard — and the only place the rule that
a host-supplied keyboard beats the platform's is load-bearing, since Android's
own accessor claims a keyboard it reaches over JNI to a Java SDK that a pure
native host does not have.

Neither host reads text back from a real IME. Recovering Unicode text from a
NativeActivity means `KeyEvent.getUnicodeChar` over JNI, which is the Java
dependency this example exists to avoid, so both play the same fixed script.
That covers the ABI; it does not cover a real IME's text, which is what the
still-open IME-composition blocker is about.
