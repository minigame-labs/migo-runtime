# Migo — 为游戏而生的原生运行时

[English](README.md) | [中文](README.zh-CN.md)

[![CI](https://github.com/minigame-labs/migo/actions/workflows/pr-ci.yml/badge.svg)](https://github.com/minigame-labs/migo/actions/workflows/pr-ci.yml)
[![License](https://img.shields.io/badge/license-BSL%201.1-blue.svg)](LICENSE)

**为游戏而生的 WebView 替代方案。** 把 Migo 嵌进你的 App，就能原生运行 HTML5 与小游戏内容——没有浏览器、DOM、CSS 与合成层。启动更快、内存更低，运行时版本由你自己钉死，不随 OEM 与系统版本漂移。

`migo.*` 是这个引擎唯一安装的原生能力面——不多不少，任何规模都一样。这个仓库就保持这一点：一个纯粹的运行时，不掺任何适配层代码。现有内容期望的其他一切，都应由独立发布、可组合的适配层包提供：

- **[migo-web-adapter](https://github.com/minigame-labs/migo-web-adapter)** —— 浏览器风格的 BOM/DOM 层(`window`、`document`、`Image`、`XMLHttpRequest` 等)，给假设自己跑在浏览器环境里的引擎用(Cocos、Egret、Laya、Pixi、原生 Canvas/WebGL)。
- **[migo-wx-adapter](https://github.com/minigame-labs/migo-wx-adapter)** —— 把 `globalThis.wx` 别名到运行时的 `migo.*` 能力上，让主流小游戏形态的内容不改一行就能跑。

适配层只安装各自文档声明的全局对象，可以自由组合——按内容实际需要选用即可。以后再来一个平台（比如快游戏联盟成员），仍是同一个配方：增加一个新的适配层包，引擎不用改。

**想知道你自己那几百款能不能跑？** 别信我们说的，也别把包发给任何人——[PRESCREEN.md](PRESCREEN.md) 就是我们会跑的那套工具，交给你自己跑：包引用了哪些接口、对上本构建实际发布的接口面求差集，以及它在真机上到底出不出帧。**内容一步不出你的机器。**

## 为什么用 Migo

| | Migo | Android 系统 WebView |
|---|---|---|
| **版本一致性** | 运行时由你打包并钉死版本，跨 OEM 与系统版本完全一致 | 随用户系统自动更新，不受你控制 |
| **可审计性** | 源码可获取，沙箱边界可逐行审计 | 闭源 |
| **启动 / 内存** | 无 DOM 与 layout，V8 快照预热——体积小、启动快 | 打包整个 Chromium，常驻开销重 |
| **跨引擎** | 一套 API 覆盖多引擎 | — |

与系统 WebView 的可复现对比测试（同游戏、同设备、同 session）见 [migo-bench](https://github.com/minigame-labs/migo-bench)。

## 平台支持

| 平台 | 状态 | 已发布产物 |
|---|---|---|
| **Android**（arm64-v8a、x86_64） | 已发布 | 含 Java/Kotlin SDK 的 AAR;按 ABI 发布的 C ABI 包(头文件、静态库、CMake 包) |
| **Linux**（x86_64、aarch64） | 已发布 | 静态库与共享库、pkg-config 与 CMake 包;Qt 6 / X11 host kit 在仓库内 |
| **Windows**（x86_64、aarch64） | 已发布 | `migo.dll` 及其导入库、头文件、CMake 包,以及它按名加载的 ANGLE 与 V8 运行时 DLL |
| **OpenHarmony / HarmonyOS NEXT**（aarch64、x86_64） | 已发布 | 按架构产出的 C ABI 包(头文件、静态库、CMake 包、manifest) |
| **iOS**（arm64；模拟器 arm64、x86_64）、**macOS**（arm64、x86_64） | 已发布 | `migo-<version>-apple-sdk.zip`：每个平台一个视图 `MigoGameView` 的 Swift 包——iOS 上内容 JavaScript 跑在 WebKit 的进程里，由引擎在 App 内绘制其帧；macOS 上引擎在进程内运行 V8 |

已发布产物见 [releases 页面](https://github.com/minigame-labs/migo/releases)。每个 release 都带一份
`SHA256SUMS.txt`（用 `sha256sum -c SHA256SUMS.txt` 校验下载），每个归档还带 `.attestation.json`,
记录其名称、大小与 sha256,其中 `package_sha256` 可从源码复现（见 [BUILD.md](BUILD.md)）。

[`include/migo/`](include/migo/) 中的 C ABI 目前是 **candidate**——在上表每个平台上都已有可用运行时，但尚未冻结。冻结前还剩哪些事项，以该目录下的 README 为准。

## 快速开始

**把 Migo 集成进 App** —— 各平台宿主的完整示例（含构建与运行步骤）在 [migo-examples](https://github.com/minigame-labs/migo-examples)。请从那里开始而不是本仓库：它自带可运行的游戏，并会替你解析运行时产物。

**从源码构建运行时** —— 前置条件、各平台配置与构建流程见 [BUILD.md](BUILD.md)。

```bash
# Android AAR（在 Linux/macOS 上构建）
./scripts/build-aar.sh release arm64-v8a
```

预构建的 V8 归档不入库，而是下载后对其 component manifest 校验：

```bash
bash scripts/fetch-v8-archives.sh          # Android 目标（构建默认）
bash scripts/fetch-v8-archives.sh --all    # 所有带 manifest 的目标
```

### 让引擎不进首包（Android）

`libmigo.so` 单 ABI 约占 17 MB 商店下载、45 MB 安装体积。如果小游戏只是你 app 的次要功能，
可以发一个不含它的 APK，等用户第一次打开小游戏时再取——从不打开的用户就永远不用为它付费。

依赖 `migo-<version>-android-nojni.aar` 而不是 `migo-<version>-android.aar`，
引擎从 `migo-<version>-jni-android-<arch>.tar.gz` 取，然后交给 SDK：

```java
MigoNativeLoader.setProvider(context, abi -> {
    File engine = new File(context.getNoBackupFilesDir(), abi + "/libmigo.so");
    return engine.isFile() ? engine : null;   // null 表示"还没下载好"
});
```

文件在加载前会对 AAR 内嵌的 artifact manifest 做校验，所以下载不完整、或镜像还在发上一个版本，
都会以可读的原因失败，而不是在引擎内部崩溃。`MigoNativeLoader.requiredArtifact(context)`
返回需要比对的摘要；`MigoNativeLoader.prepare(context, file)` 则在你调用它的线程上当场做这次校验——
这样坏包在下载线程上就被发现，而不是等用户点开游戏时才表现为启动失败。

从哪里取取决于你上哪个商店：Google Play 上唯一合规的来源是
[Play Feature Delivery](https://developer.android.com/guide/playcore/feature-delivery)
（从 Play 之外获取可执行代码违反 Device and Network Abuse 政策）；没有 Feature Delivery 的商店
则预期你自己托管该文件，[LEGAL.md](LEGAL.md) 已明确这是被许可的。Migo 自己不下载任何东西——
内置一个下载器必然对其中一种商店是错的。

## 架构

```text
+------------------------------------------------------------------------------------+
|                                      Your App                                      |
+------------------------------------------------------------------------------------+
|                                      Migo SDK                                      |
+---------------------+--------------------+--------------------+--------------------+
|       Graphics      |       Audio        |        I/O         |     JS Runtime     |
|     (Skia / GL)     |     (WebAudio)     |     (File/Net)     |   (deno_core/V8)   |
+---------------------+--------------------+--------------------+--------------------+
|                                  Rust Core Engine                                  |
+------------------------------------------------------------------------------------+
|              Platform Layer (Android | Linux | Windows | OpenHarmony)              |
+------------------------------------------------------------------------------------+
```

## 仓库结构

```text
migo/
├── engine/                 # Rust 核心引擎
│   ├── crates/
│   │   ├── core/           # 核心运行时与会话生命周期
│   │   ├── graphics/       # 渲染（Canvas2D, WebGL）
│   │   ├── audio/          # 音频处理
│   │   ├── io/             # 文件与网络 I/O
│   │   ├── runtime-v8/     # JavaScript 运行时（V8，经 deno_core）
│   │   ├── shared/         # 共享类型与协议
│   │   ├── platform/       # 平台相关代码
│   │   ├── capi/           # C ABI 实现
│   │   ├── capi-abi/       # C ABI 布局与版本契约
│   │   └── android-jni/    # JNI 入口（libmigo.so）
│   ├── tools/              # 快照生成、headless player、C 宿主示例
│   └── Cargo.toml
├── include/migo/           # 公开 C 头文件
├── platforms/
│   ├── android/            # Android SDK（AAR）
│   ├── linux/              # Linux host kit（Qt 6 / X11）
│   ├── openharmony/        # OpenHarmony 宿主（ArkUI XComponent）
│   └── windows/            # Windows
├── tests/                  # 一致性资产（C ABI lane、C 宿主、探针内容）
├── contracts/              # 产物 manifest schema
├── scripts/                # 构建与契约门禁脚本
├── BUILD.md                # 从源码构建指南
├── LICENSE                 # 许可证（BSL 1.1）
├── LEGAL.md                # 法律声明（许可/商标/测试内容）
├── COMMERCIAL.md           # 商业许可：谁需要、谁不需要、怎么谈
└── NOTICE                  # 第三方声明
```

## 相关仓库

| 仓库 | 用途 |
|---|---|
| [migo-examples](https://github.com/minigame-labs/migo-examples) | 各平台宿主集成示例，一个平台一个目录 |
| [migo-bench](https://github.com/minigame-labs/migo-bench) | Migo 与 WebView 的可复现对比测试 |
| [migo-web-adapter](https://github.com/minigame-labs/migo-web-adapter) | 浏览器风格 BOM/DOM 兼容适配层 |
| [migo-wx-adapter](https://github.com/minigame-labs/migo-wx-adapter) | `wx.*` 兼容适配层，面向主流小游戏内容 |

## 许可证

Migo 采用 **source-available** 的 [Business Source License 1.1](LICENSE)（BSL 1.1）。**每个发布版本在其发布满四年时转为 Apache 2.0** —— 具体日期就写在该版本随附的 `LICENSE` 里（当前为 2030-09-25）。

- **阅读、审计、构建、测试、评测、修改、移植** —— 任何规模、任何主体，无条件授予。
- **把 Migo 嵌进你自己的 App 上线** —— 年营收 ≤ USD 1,000,000 且月活 ≤ 3,000,000 时免费。
- **把 Migo 作为独立 SDK 转售，或提供托管运行时服务** —— 需要商业许可。

完整声明见 [LEGAL.md](LEGAL.md)，商业许可见 [COMMERCIAL.md](COMMERCIAL.md)。

"Migo" 及 Migo logo 是 Migo Authors 的商标。许可证授予的是**软件**上的权利，不含名称：你可以 fork 代码，但不能沿用名称。

## 参与贡献

欢迎贡献 —— 见 [CONTRIBUTING.md](CONTRIBUTING.md)。提交 Pull Request 前，请确认 `scripts/test-*-contract.sh` 下的契约门禁仍然通过；它们编码了普通测试覆盖不到的不变量。

## 致谢

Migo 构建于 [Deno Core](https://github.com/denoland/deno_core)、[V8](https://v8.dev/)、[Tokio](https://tokio.rs/) 与 [Skia](https://skia.org/)（Ganesh GL 后端 + SkParagraph 文本排版）之上。完整依赖与许可清单见 [NOTICE](NOTICE)。

## 支持

- Issues: https://github.com/minigame-labs/migo/issues
- 集成指南：[migo-examples](https://github.com/minigame-labs/migo-examples) —— 可运行的游戏与各宿主的构建/运行说明
- 从源码构建：[BUILD.md](BUILD.md)
- 商业许可：licensing@minigame-labs.com
