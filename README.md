<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset=".github/assets/lockup-on-dark.svg">
    <img src=".github/assets/lockup-on-light.svg" alt="Migo" width="240">
  </picture>
</p>

<h3 align="center">Run HTML5 games and mini-games inside your native app.</h3>

<p align="center">
  One embeddable engine for Android, iOS, HarmonyOS, Windows, Linux and macOS.
</p>

<p align="center">
  <a href="https://minigame-labs.com/en/"><b>Website</b></a> ·
  <a href="https://minigame-labs.com/docs/en/"><b>Docs</b></a> ·
  <a href="https://github.com/minigame-labs/migo-examples"><b>Examples</b></a> ·
  <a href="https://github.com/minigame-labs/migo-bench"><b>Benchmarks</b></a>
</p>

<p align="center">
  <a href="https://github.com/minigame-labs/migo-runtime/releases"><img src="https://img.shields.io/github/v/release/minigame-labs/migo-runtime?include_prereleases" alt="Release"></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-Migo%20Binary%20License-blue.svg" alt="License: Migo Binary License"></a>
</p>

<p align="center">
  English | <a href="README.zh-CN.md">中文</a>
</p>

This repository is where Migo is released: SDK packages for every platform,
checksums, SBOMs, and the license, security and data-handling documents. The
runtime's source code is not published here.

| **45–56%** | **2.4–2.9×** | **1–240 fps** | **6** |
|:---:|:---:|:---:|:---:|
| less memory than WebView | less CPU than WebView | set by the game or your app | platforms, one engine |

<p align="center"><sub>Memory and CPU measured on Android against the system WebView: same games, same device, both at ~60 fps. <a href="https://github.com/minigame-labs/migo-bench">Reproduce it yourself →</a></sub></p>

> [!NOTE]
> Migo is a game container, not a browser: no DOM, no CSS, no page layout.

## Why Migo

- **Pinned** — you ship the engine, so it never drifts across phone brands or OS updates.
- **Verifiable** — checksummed releases with an SBOM per package; no telemetry, no update check, no licence activation ([DATA-HANDLING.md](DATA-HANDLING.md)).
- **Fast under constraints** — JavaScript boots from a prebuilt V8 startup snapshot, ASTC/KTX2 compressed textures upload without a CPU decode, and the full Web Audio node family plus ICU-backed Intl are built in.
- **Compatible** — Pixi, Phaser, three.js, Babylon.js, PlayCanvas, Egret, p5.js, EaselJS, Howler, raw Canvas/WebGL, and mini-game content: each one run unmodified, at a pinned version, by the conformance suite through the real macOS host and, for most of them, on an iPhone 12. Cocos Creator and Laya are not in that suite yet, so they are not claimed.
- **Yours to control** — your app decides login, payments, ads and downloads. Nothing is faked.

## Platforms

| Platform | Package | JavaScript |
|---|---|---|
| **Android** 8.0+ | AAR (Java/Kotlin) · C ABI for the NDK | V8, JIT |
| **iOS** 15.2+ | Swift package | WebKit, JIT |
| **macOS** 11+ | Swift package | V8, JIT |
| **HarmonyOS NEXT / OpenHarmony** | C ABI for ArkUI `XComponent` | V8, interpreted on phones¹ |
| **Windows** | `migo.dll` + CMake | V8, JIT |
| **Linux** | `.so` / `.a` + CMake, pkg-config · Qt 6 host kit | V8, JIT |

<sub>arm64 and x86_64 on every platform. ¹ A HarmonyOS NEXT phone does not let an app make memory executable, so V8 runs interpreted there and WebAssembly is unavailable; tablets and 2in1 devices granted `ALLOW_WRITABLE_CODE_MEMORY`, and open-source OpenHarmony, keep the JIT. Migo asks the system at startup.</sub>

`migo.*` is the only API the engine installs; everything else comes from an adapter you choose ([migo-wx-adapter](https://github.com/minigame-labs/migo-wx-adapter), [migo-web-adapter](https://github.com/minigame-labs/migo-web-adapter)). On iOS, game code runs in WebKit's JIT-enabled process and Migo renders each frame in your app.

Every API the mini-game platform documents has an answer on Android, iOS and macOS: implemented by the engine or by that platform's host layer, a hook for your app to fill, or not provided for a stated reason. Linux and Windows still lack a few hardware APIs, and OpenHarmony far more for now. [API platform support](https://minigame-labs.com/docs/en/capabilities/api-support/) has each API on each platform, generated from the contracts; if you make games, start at [running your game on Migo](https://minigame-labs.com/docs/en/getting-started/game-developers/).

## Quick start

Complete, runnable host apps for every platform are in [**migo-examples**](https://github.com/minigame-labs/migo-examples).

<details open>
<summary><b>Android</b></summary>

```groovy
implementation files('libs/migo-<version>-android.aar')
```

```java
MigoGameView gameView = new MigoGameView(activity);
gameView.setConfig(new RuntimeConfig.Builder(activity).build());
container.addView(gameView);
gameView.loadGame("my-game", "game.js");
```

[Android guide →](https://minigame-labs.com/docs/en/getting-started/android/)
</details>

<details>
<summary><b>iOS / macOS</b></summary>

```swift
let config = try MigoGameView.Configuration.standard(contentSigning: .unsigned)
try MigoGameInstaller.install(package: gameURL, id: "my-game",
                              version: buildNumber, into: config.directories)

let gameView = MigoGameView(configuration: config)
view.addSubview(gameView)
gameView.loadGame(id: "my-game")
```

[Apple guide →](https://minigame-labs.com/docs/en/getting-started/apple/)
</details>

<details>
<summary><b>C / C++</b> — Windows, Linux, HarmonyOS, Android NDK</summary>

```c
migo_engine_create(...);                // once per process
migo_session_create(...);               // once per game
migo_session_set_host_callbacks(...);
migo_session_attach_surface(...);       // HWND, X11/Wayland, OHNativeWindow, ANativeWindow
migo_session_load_content(...);
migo_session_notify_vsync(...);         // every display frame
```

Headers and a CMake package ship inside every `capi` archive. [C ABI reference →](https://minigame-labs.com/docs/en/reference/overview/) · [Linux and the Qt 6 host kit →](https://minigame-labs.com/docs/en/getting-started/linux/)
</details>

## Download

- **Releases** — [download](https://github.com/minigame-labs/migo-runtime/releases), then verify with `sha256sum -c SHA256SUMS.txt` ([how to verify](https://minigame-labs.com/docs/en/release/download-verification/)).
- **Will my game run?** — every release carries `migo-<version>-prescreen.zip`: it scans a game package on your own machine (Python 3.8, no network, the game never leaves it) and lists what the game calls that the release does not publish. How to run it is in its `README.txt`.
- **Smaller Android installs** — download the engine on first use with the `android-nojni` AAR ([Android guide](https://minigame-labs.com/docs/en/getting-started/android/)).
- **Upgrading** — [what changes between versions](https://minigame-labs.com/docs/en/release/upgrading/) and the [CHANGELOG](CHANGELOG.md).

## License

[Migo Binary License 1.0](LICENSE). The SDK is free to evaluate at any scale.

- **Develop, test, evaluate, benchmark** — free, at any scale.
- **Ship in your own app** — free up to USD 1M company revenue a year and 3M monthly active users. Running other developers' mini-games in your app is included.
- **Above those thresholds, or to resell as an SDK or hosted service** — needs a [commercial license](COMMERCIAL.md).

Versions up to and including v0.9.19 were published under BSL 1.1 and remain under it. Details: [LEGAL.md](LEGAL.md). "Migo" and its logo are trademarks.

## Contact

[Issues](https://github.com/minigame-labs/migo-runtime/issues) · [Security](SECURITY.md) · licensing@minigame-labs.com

Built on [V8](https://v8.dev/), [deno_core](https://github.com/denoland/deno_core), [Skia](https://skia.org/) and [Tokio](https://tokio.rs/). Third-party notices: [NOTICE](NOTICE).
