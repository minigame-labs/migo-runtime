<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset=".github/assets/lockup-on-dark.svg">
    <img src=".github/assets/lockup-on-light.svg" alt="Migo" width="240">
  </picture>
</p>

<h3 align="center">在你的原生 App 里运行 H5 游戏与小游戏</h3>

<p align="center">
  一个可嵌入的引擎，覆盖 Android、iOS、HarmonyOS、Windows、Linux 和 macOS。
</p>

<p align="center">
  <a href="https://minigame-labs.com/"><b>官网</b></a> ·
  <a href="https://minigame-labs.com/docs/"><b>文档</b></a> ·
  <a href="https://github.com/minigame-labs/migo-examples"><b>示例</b></a> ·
  <a href="https://github.com/minigame-labs/migo-bench"><b>性能测试</b></a>
</p>

<p align="center">
  <a href="https://github.com/minigame-labs/migo/actions/workflows/pr-ci.yml"><img src="https://github.com/minigame-labs/migo/actions/workflows/pr-ci.yml/badge.svg" alt="CI"></a>
  <a href="https://github.com/minigame-labs/migo/releases"><img src="https://img.shields.io/github/v/release/minigame-labs/migo?include_prereleases" alt="Release"></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-BSL%201.1-blue.svg" alt="License: BSL 1.1"></a>
</p>

<p align="center">
  <a href="README.md">English</a> | 中文
</p>

| **40–44%** | **1.9–2.9×** | **1–240 fps** | **6 端** |
|:---:|:---:|:---:|:---:|
| 内存比 WebView 低 | CPU 比 WebView 低 | 帧率由游戏或 App 设定 | 一套引擎 |

<p align="center"><sub>内存与 CPU 为 Android 实测，对比系统 WebView：同游戏、同设备，双方都在约 60 fps 下运行。<a href="https://github.com/minigame-labs/migo-bench">自己复现 →</a></sub></p>

> [!NOTE]
> Migo 是游戏容器，不是浏览器：没有 DOM、没有 CSS、没有页面排版。

## 为什么选 Migo

- **版本可控** —— 引擎随 App 打包，不随手机品牌和系统更新漂移。
- **可审计** —— 源码公开，发布包带校验和，可从源码复现。
- **兼容现有内容** —— Cocos、Laya、Egret、Pixi、Phaser、原生 Canvas/WebGL，以及小游戏内容。
- **主导权在你** —— 登录、支付、广告、下载由你的 App 决定，没实现的能力不会假装成功。

## 架构

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

引擎只安装 `migo.*` 这一套 API，其余接口都由你选择的适配层提供。iOS 上游戏代码跑在 WebKit 带 JIT 的系统进程里，每一帧由 Migo 在你的 App 内渲染。

## 平台支持

| 平台 | 交付形式 | JavaScript |
|---|---|---|
| **Android** 8.0+ | AAR（Java/Kotlin）· 面向 NDK 的 C ABI | V8，JIT |
| **iOS** 15.2+ | Swift 包 | WebKit，JIT |
| **macOS** 11+ | Swift 包 | V8，JIT |
| **HarmonyOS NEXT / OpenHarmony** | 面向 ArkUI `XComponent` 的 C ABI | V8，解释执行¹ |
| **Windows** | `migo.dll` + CMake | V8，JIT |
| **Linux** | `.so` / `.a` + CMake、pkg-config · Qt 6 host kit | V8，JIT |

<sub>各平台均支持 arm64 与 x86_64。¹ HarmonyOS NEXT 只允许系统引擎使用 JIT。</sub>

## 快速开始

各平台完整可运行的宿主 App 见 [**migo-examples**](https://github.com/minigame-labs/migo-examples)。

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

[Android 指南 →](platforms/android/README.zh-CN.md)
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

[Apple 指南 →](platforms/apple/README.md)
</details>

<details>
<summary><b>C / C++</b> —— Windows、Linux、HarmonyOS、Android NDK</summary>

```c
migo_engine_create(...);                // 每个进程一次
migo_session_create(...);               // 每个游戏一次
migo_session_set_host_callbacks(...);
migo_session_attach_surface(...);       // HWND、X11/Wayland、OHNativeWindow、ANativeWindow
migo_session_load_content(...);
migo_session_notify_vsync(...);         // 每个显示帧
```

[C ABI 指南 →](include/migo/README.md) · [Qt 6 host kit →](platforms/linux/host-kit/README.md)
</details>

## 下载与构建

- **Release** —— [下载](https://github.com/minigame-labs/migo/releases)后用 `sha256sum -c SHA256SUMS.txt` 校验。
- **缩小 Android 安装包** —— [引擎首次使用时再下载](platforms/android/README.zh-CN.md#按需下发引擎)。
- **从源码构建** —— 见 [BUILD.md](BUILD.md)。

## 相关项目

- [**migo-examples**](https://github.com/minigame-labs/migo-examples) —— 各平台可运行的宿主 App
- [**migo-bench**](https://github.com/minigame-labs/migo-bench) —— Migo 与 WebView 的可复现对比测试
- [**migo-web-adapter**](https://github.com/minigame-labs/migo-web-adapter) —— 为依赖浏览器环境的引擎提供浏览器全局对象
- [**migo-wx-adapter**](https://github.com/minigame-labs/migo-wx-adapter) —— 在 `migo.*` 之上提供小游戏 `wx.*` API

## 许可证

[BSL 1.1](LICENSE)，源码公开。每个版本发布满四年后转为 Apache 2.0；当前版本的转换日期是 **2030-09-26**。

- **阅读、构建、测试、评测、修改、移植** —— 免费，不限规模。
- **嵌入你自己的 App 上线** —— 公司年营收不超过 100 万美元、且月活不超过 300 万时免费。
- **作为 SDK 转售或提供托管服务** —— 需要[商业许可](COMMERCIAL.md)。

完整条款见 [LEGAL.md](LEGAL.md)。"Migo" 名称与 logo 是商标：代码可以 fork，名字不能沿用。

## 社区

[Issues](https://github.com/minigame-labs/migo/issues) · [贡献指南](CONTRIBUTING.zh-CN.md) · [安全](SECURITY.zh-CN.md) · licensing@minigame-labs.com

Migo 构建于 [V8](https://v8.dev/)、[deno_core](https://github.com/denoland/deno_core)、[Skia](https://skia.org/) 与 [Tokio](https://tokio.rs/) 之上。第三方声明见 [NOTICE](NOTICE)。
