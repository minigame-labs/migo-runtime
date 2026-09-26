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
  <a href="https://github.com/minigame-labs/migo/actions/workflows/pr-ci.yml"><img src="https://github.com/minigame-labs/migo/actions/workflows/pr-ci.yml/badge.svg" alt="CI"></a>
  <a href="https://github.com/minigame-labs/migo/releases"><img src="https://img.shields.io/github/v/release/minigame-labs/migo?include_prereleases" alt="Release"></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-BSL%201.1-blue.svg" alt="License: BSL 1.1"></a>
</p>

<p align="center">
  English | <a href="README.zh-CN.md">中文</a>
</p>

| **40–44%** | **1.9–2.9×** | **1–240 fps** | **6** |
|:---:|:---:|:---:|:---:|
| less memory than WebView | less CPU than WebView | set by the game or your app | platforms, one engine |

<p align="center"><sub>Memory and CPU measured on Android against the system WebView: same games, same device, both at ~60 fps. <a href="https://github.com/minigame-labs/migo-bench">Reproduce it yourself →</a></sub></p>

> [!NOTE]
> Migo is a game container, not a browser: no DOM, no CSS, no page layout.

## Why Migo

- **Pinned** — you ship the engine, so it never drifts across phone brands or OS updates.
- **Auditable** — full source, checksummed releases, builds reproducible from source.
- **Compatible** — Cocos, Laya, Egret, Pixi, Phaser, raw Canvas/WebGL, and mini-game content.
- **Yours to control** — your app decides login, payments, ads and downloads. Nothing is faked.

## Architecture

```text
┌──────────────────────────────────────────────────────────────────┐
│  YOUR GAME     game.js + assets                                  │
│                Cocos, Laya, Egret, Pixi, Phaser, Canvas/WebGL    │
├──────────────────────────────────────────────────────────────────┤
│  ADAPTERS      migo-wx-adapter, migo-web-adapter                 │
│  (optional)    map wx.* and browser globals onto migo.*          │
└─────────────────────────────────┬────────────────────────────────┘
                                  v  migo.*
┌─────────────────────────────────┴────────────────────────────────┐
│  MIGO RUNTIME                                                    │
│  ┌──────────────────────────┐      ┌──────────────────────────┐  │
│  │ JavaScript engine        │ <--> │ Rust core                │  │
│  │ V8 (WebKit on iOS)       │      │ sessions, frame loop     │  │
│  │ runs your game code      │      │ sandbox, scheduling      │  │
│  └──────────────────────────┘      └──────────────────────────┘  │
│  ┌──────────────────────────┐      ┌──────────────────────────┐  │
│  │ Graphics                 │      │ Services                 │  │
│  │ Canvas 2D, WebGL 1/2     │      │ audio, text, images      │  │
│  │ Skia -> GPU              │      │ files, network           │  │
│  └──────────────────────────┘      └──────────────────────────┘  │
└────────────┬────────────────────────────┬────────────────────────┘
             ^  view, frames, input       v  login, payments, ads
┌────────────┴────────────────────────────┴────────────────────────┐
│  YOUR APP      Migo SDK: Java/Kotlin, Swift, C ABI               │
└──────────────────────────────────────────────────────────────────┘
```

`migo.*` is the only API the engine installs; everything else comes from an adapter you choose. On iOS, game code runs in WebKit's JIT-enabled process and Migo renders each frame in your app.

## Platforms

| Platform | Package | JavaScript |
|---|---|---|
| **Android** 8.0+ | AAR (Java/Kotlin) · C ABI for the NDK | V8, JIT |
| **iOS** 15.2+ | Swift package | WebKit, JIT |
| **macOS** 11+ | Swift package | V8, JIT |
| **HarmonyOS NEXT / OpenHarmony** | C ABI for ArkUI `XComponent` | V8, interpreted¹ |
| **Windows** | `migo.dll` + CMake | V8, JIT |
| **Linux** | `.so` / `.a` + CMake, pkg-config · Qt 6 host kit | V8, JIT |

<sub>arm64 and x86_64 on every platform. ¹ HarmonyOS NEXT reserves JIT for the system engine.</sub>

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

[Android guide →](platforms/android/README.md)
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

[Apple guide →](platforms/apple/README.md)
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

[C ABI guide →](include/migo/README.md) · [Qt 6 host kit →](platforms/linux/host-kit/README.md)
</details>

## Download and build

- **Releases** — [download](https://github.com/minigame-labs/migo/releases), then verify with `sha256sum -c SHA256SUMS.txt`.
- **Smaller Android installs** — [download the engine on first use](platforms/android/README.md#shipping-the-engine-on-demand).
- **From source** — see [BUILD.md](BUILD.md).

## Related projects

- [**migo-examples**](https://github.com/minigame-labs/migo-examples) — runnable host apps for every platform
- [**migo-bench**](https://github.com/minigame-labs/migo-bench) — reproducible Migo vs. WebView benchmarks
- [**migo-web-adapter**](https://github.com/minigame-labs/migo-web-adapter) — browser globals for engines that expect a browser
- [**migo-wx-adapter**](https://github.com/minigame-labs/migo-wx-adapter) — the mini-game `wx.*` API on top of `migo.*`

## License

[BSL 1.1](LICENSE), source-available. Each release becomes Apache 2.0 four years after publication; for the current release, **2030-09-26**.

- **Read, build, test, benchmark, modify, port** — free, at any scale.
- **Ship in your own app** — free up to USD 1M company revenue a year and 3M monthly active users.
- **Resell as an SDK or hosted service** — needs a [commercial license](COMMERCIAL.md).

Details: [LEGAL.md](LEGAL.md). "Migo" and its logo are trademarks: fork the code, not the name.

## Community

[Issues](https://github.com/minigame-labs/migo/issues) · [Contributing](CONTRIBUTING.md) · [Security](SECURITY.md) · licensing@minigame-labs.com

Built on [V8](https://v8.dev/), [deno_core](https://github.com/denoland/deno_core), [Skia](https://skia.org/) and [Tokio](https://tokio.rs/). Third-party notices: [NOTICE](NOTICE).
