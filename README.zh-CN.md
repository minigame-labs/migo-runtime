<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset=".github/assets/lockup-on-dark.svg">
    <img src=".github/assets/lockup-on-light.svg" alt="Migo" width="240">
  </picture>
</p>

<h3 align="center">在原生 App 中运行 H5 游戏与小游戏</h3>

<p align="center">
  一套可嵌入的引擎，覆盖 Android、iOS、HarmonyOS、Windows、Linux 和 macOS。
</p>

<p align="center">
  <a href="https://minigame-labs.com/"><b>官网</b></a> ·
  <a href="https://minigame-labs.com/docs/"><b>文档</b></a> ·
  <a href="https://github.com/minigame-labs/migo-examples"><b>示例</b></a> ·
  <a href="https://github.com/minigame-labs/migo-bench"><b>性能测试</b></a>
</p>

<p align="center">
  <a href="https://github.com/minigame-labs/migo-runtime/releases"><img src="https://img.shields.io/github/v/release/minigame-labs/migo-runtime?include_prereleases" alt="Release"></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-Migo%20Binary%20License-blue.svg" alt="License: Migo Binary License"></a>
</p>

<p align="center">
  <a href="README.md">English</a> | 中文
</p>

本仓库是 Migo 的发布仓库，提供各平台的 SDK 包、校验和、SBOM，以及许可、安全与数据处理说明。运行时源码不在本仓库公开。

| **47–58%** | **2.3–3.0×** | **1–240 fps** | **6 端** |
|:---:|:---:|:---:|:---:|
| 内存比 WebView 低 | CPU 比 WebView 低 | 帧率由游戏或 App 设定 | 一套引擎 |

<p align="center"><sub>内存与 CPU 为 Android 实测，对比系统 WebView：同游戏、同设备，双方均在约 60 fps 下运行。<a href="https://github.com/minigame-labs/migo-bench">自行复现 →</a></sub></p>

> [!NOTE]
> Migo 是游戏容器，不是浏览器：没有 DOM、没有 CSS、没有页面排版。

## 为什么选择 Migo

- **版本可控**：引擎随 App 打包，不随手机品牌和系统更新而变化。
- **可核验**：发布包附校验和，每个 SDK 包附 SBOM。运行时没有遥测、没有更新检查、没有许可激活，见 [DATA-HANDLING.md](DATA-HANDLING.md)。
- **受限机型上同样快**：JavaScript 从预构建的 V8 启动快照启动，ASTC/KTX2 压缩纹理直接上传、不经 CPU 解码，内置完整的 Web Audio 节点与 ICU 版 Intl。
- **兼容存量内容**：Pixi、Phaser、three.js、Babylon.js、PlayCanvas、Egret、p5.js、EaselJS、Howler、原生 Canvas/WebGL 以及小游戏内容，均以固定版本、不作修改地在 conformance 套件的真实 macOS 宿主上逐一运行，其中多数还在 iPhone 12 上运行过。Cocos Creator 和 Laya 尚未进入该套件，不作兼容性声明。
- **主导权在宿主**：登录、支付、广告、下载由 App 决定，未实现的能力不会伪装成功。

## 平台支持

| 平台 | 交付形式 | JavaScript |
|---|---|---|
| **Android** 8.0+ | AAR（Java/Kotlin）· 面向 NDK 的 C ABI | V8，JIT |
| **iOS** 15.2+ | Swift 包 | WebKit，JIT |
| **macOS** 11+ | Swift 包 | V8，JIT |
| **HarmonyOS NEXT / OpenHarmony** | 面向 ArkUI `XComponent` 的 C ABI | V8，解释执行¹ |
| **Windows** | `migo.dll` + CMake | V8，JIT |
| **Linux** | `.so` / `.a` + CMake、pkg-config · Qt 6 宿主套件 | V8，JIT |

<sub>各平台均支持 arm64 与 x86_64。¹ HarmonyOS NEXT 只允许系统引擎使用 JIT。</sub>

引擎只安装 `migo.*` 这一套 API，其余接口由接入方选用的适配层提供（[migo-wx-adapter](https://github.com/minigame-labs/migo-wx-adapter)、[migo-web-adapter](https://github.com/minigame-labs/migo-web-adapter)）。iOS 上游戏代码运行在 WebKit 带 JIT 的系统进程中，每一帧由 Migo 在 App 内渲染。

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

[Android 接入指南 →](https://minigame-labs.com/docs/getting-started/android/)
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

[Apple 接入指南 →](https://minigame-labs.com/docs/getting-started/apple/)
</details>

<details>
<summary><b>C / C++</b>：Windows、Linux、HarmonyOS、Android NDK</summary>

```c
migo_engine_create(...);                // 每个进程一次
migo_session_create(...);               // 每个游戏一次
migo_session_set_host_callbacks(...);
migo_session_attach_surface(...);       // HWND、X11/Wayland、OHNativeWindow、ANativeWindow
migo_session_load_content(...);
migo_session_notify_vsync(...);         // 每个显示帧
```

头文件与 CMake 包随每个 `capi` 压缩包提供。[C ABI 参考 →](https://minigame-labs.com/docs/reference/overview/) · [Linux 与 Qt 6 宿主套件 →](https://minigame-labs.com/docs/getting-started/linux/)
</details>

## 下载

- **Release**：[下载](https://github.com/minigame-labs/migo-runtime/releases)后用 `sha256sum -c SHA256SUMS.txt` 校验（[校验方法](https://minigame-labs.com/docs/release/download-verification/)）。
- **缩小 Android 安装包**：采用 `android-nojni` AAR，引擎在首次使用时下载（见 [Android 接入指南](https://minigame-labs.com/docs/getting-started/android/)）。
- **版本升级**：[各版本之间的变化](https://minigame-labs.com/docs/release/upgrading/)与 [CHANGELOG](CHANGELOG.md)。

## 许可证

[Migo Binary License 1.0](LICENSE)。SDK 可免费评估，不限规模。

- **开发、测试、评估、性能测试**：免费，不限规模。
- **嵌入自己的 App 上线**：公司年营收不超过 100 万美元、且月活不超过 300 万时免费。在自己的 App 中运行其他开发者的小游戏也包含在内。
- **超出上述阈值，或作为 SDK、托管服务转售**：需要[商业许可](COMMERCIAL.md)。

v0.9.19 及更早版本以 BSL 1.1 发布，仍适用 BSL 1.1。完整说明见 [LEGAL.md](LEGAL.md)。「Migo」名称与 logo 是商标。

## 联系方式

[Issues](https://github.com/minigame-labs/migo-runtime/issues) · [安全](SECURITY.zh-CN.md) · licensing@minigame-labs.com

Migo 构建于 [V8](https://v8.dev/)、[deno_core](https://github.com/denoland/deno_core)、[Skia](https://skia.org/) 与 [Tokio](https://tokio.rs/) 之上。第三方声明见 [NOTICE](NOTICE)。
