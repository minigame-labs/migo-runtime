# Migo Build Guide

How to set up a development environment and build Migo on **Linux**,
**macOS**, and **Windows**. Covers host-side Rust builds (unit tests,
smoke checks), Android cross-compilation, the Linux C-API SDK,
the Windows C-API SDK, and the OpenHarmony C-API SDK.

> This file describes *how* to build the engine from source.

---

## Contents

- [Overview](#overview)
- [Prerequisites (all platforms)](#prerequisites-all-platforms)
- [Platform setup](#platform-setup)
  - [Linux (Ubuntu/Debian/WSL2)](#linux-ubuntudebian-wsl2)
  - [macOS (Apple Silicon + Intel)](#macos-apple-silicon--intel)
  - [Windows 10/11](#windows-1011)
- [Build workflows](#build-workflows)
  - [1. Host Rust tests (fast)](#1-host-rust-tests-fast)
  - [2. Android shared library (`libmigo.so`)](#2-android-shared-library-libmigoso)
  - [3. Android AAR (for app integration)](#3-android-aar-for-app-integration)
  - [4. Linux x86_64 SDK](#4-linux-x86_64-sdk)
  - [5. Windows x86_64 SDK](#5-windows-x86_64-sdk)
  - [6. OpenHarmony SDK (aarch64 / x86_64)](#6-openharmony-sdk-aarch64--x86_64)
  - [7. Release asset from a staged SDK](#7-release-asset-from-a-staged-sdk)
- [Troubleshooting](#troubleshooting)
- [Project invariants](#project-invariants)

---

## Overview

Migo is a Rust multi-crate engine (`engine/crates/`) that ships as a
native library and C ABI on four platforms. Main outputs:

| Output | Tool | Primary use |
|---|---|---|
| Host binaries + unit tests | `cargo test` / `cargo build` | Develop & test on the dev machine |
| `libmigo.so` (arm64 / x86_64) | `scripts/build-android-so.{sh,ps1}` | Drop-in native lib for Android |
| AAR | `scripts/build-aar.{sh,ps1}` | Gradle dependency for Android |
| Linux SDK (`libmigo.so`, `libmigo.a`) | `scripts/build-linux-sdk.sh` | C-ABI consumer on Linux x86_64 |
| Windows SDK (`migo.dll`, `migo.lib`) | `scripts/build-windows-sdk.sh` | C-ABI consumer on Windows x64 |
| OpenHarmony SDK (`libmigo_capi.a`) | `scripts/build-ohos-sdk.sh` | C-ABI consumer on OpenHarmony |

**Minimum Android API**: `26` (Android 8.0 Oreo). Enforced by
`ANDROID_API=26` in `scripts/build-android-so.sh` and
`minSdk 26` in `platforms/android/library/build.gradle`.

---

## Prerequisites (all platforms)

### Rust toolchain

```bash
# Install rustup from https://rustup.rs/ (or your package manager).
# engine/rust-toolchain.toml pins an exact rustc version (not "stable") and
# rustup installs it on first cargo invocation inside engine/. The workspace is
# edition 2024, which needs rustc >= 1.85 regardless of the pin.
cd engine && rustup show          # verify the pin is respected
```

Add Android targets:

```bash
rustup target add aarch64-linux-android
rustup target add x86_64-linux-android
```

Install `cargo-ndk` (cross-compile helper):

```bash
cargo install cargo-ndk
```

### Android NDK (only if building `libmigo.so`/AAR)

- **Version**: r23 through r27 have been verified. r23b/r25 are the
  two most battle-tested; we regularly build on `r23.2.8568313`.
- **Required env var**: `ANDROID_NDK_HOME` pointing to the NDK root
  (directory containing `toolchains/llvm/prebuilt/`).

### Skia source-build tooling

Migo compiles Skia from source on **every target, the host included** —
not just Android. `skia-safe 0.93` is configured with `binary-cache`,
but migo's feature set carries `embed-freetype`, and none of the
prebuilt archives in the upstream `rust-skia/skia-binaries` releases
were built with that tag. Every clean build therefore:

1. Attempts the binary download → receives `HTTP 404` (expected).
2. Falls back to a **from-source Skia compile** (`STARTING A FULL BUILD`),
   roughly 30–50 min cold. Incremental Rust-only rebuilds reuse the
   compiled Skia and take seconds.

The from-source path needs (host builds included):

| Tool | Minimum version | Why |
|---|---|---|
| `python3` | 3.8+ | Skia's `gn` generator scripts |
| `ninja`   | 1.10+ | Build runner (actually builds .o files) |
| `git`     | any modern | Skia's `gclient sync` pulls submodules |
| disk      | ~10 GB free | Skia source + build intermediates |
| RAM       | ~6 GB peak  | Some C++ translation units are big |

### JDK (only for AAR builds)

JDK 17+ is recommended. Gradle 8.4 (bundled in `platforms/android/`)
has been verified on 17 and 21.

---

## Platform setup

### Linux (Ubuntu/Debian/WSL2)

```bash
# System packages
sudo apt update
sudo apt install -y python3 git curl build-essential pkg-config \
    libfontconfig1-dev libfreetype6-dev libegl1-mesa-dev default-jdk

# ninja — dev-setup-skia.sh installs a pinned prebuilt to ~/.local/bin
source scripts/dev-setup-skia.sh   # NOTE: `source`, not `bash`

# Android NDK — via sdkmanager or Android Studio SDK Manager
# Then:
export ANDROID_NDK_HOME="$HOME/Android/Sdk/ndk/23.2.8568313"
# (persist it in your shell rc file)

# Install cargo-ndk + Rust targets
rustup target add aarch64-linux-android x86_64-linux-android
cargo install cargo-ndk
```

**WSL2 specific — RAM knob**: Skia's from-source build spikes to
~6 GB. WSL2 defaults to 50% host RAM; if your machine has 8 GB you'll
OOM. Create `%USERPROFILE%\.wslconfig` on the Windows side:

```ini
[wsl2]
memory=12GB
swap=8GB
```

Then `wsl --shutdown` and reopen.

### macOS (Apple Silicon + Intel)

```bash
# Via Homebrew
brew install python@3.12 ninja git openjdk@17
export JAVA_HOME=$(/usr/libexec/java_home -v 17)

# Android NDK — via sdkmanager (installed by Android Studio)
# or direct download from https://developer.android.com/ndk/downloads
export ANDROID_NDK_HOME="$HOME/Library/Android/sdk/ndk/23.2.8568313"

# Rust targets + cargo-ndk
rustup target add aarch64-linux-android x86_64-linux-android
cargo install cargo-ndk
```

Apple Silicon note: the NDK you download **must** contain
`toolchains/llvm/prebuilt/darwin-arm64` (recent NDKs do; older r21
bundles only `darwin-x86_64` and require Rosetta to run).

### Windows 10/11

```powershell
# Rust toolchain — download rustup-init.exe from https://rustup.rs/
# After install, restart the terminal and verify:
rustc --version
cargo --version

# Rust targets
rustup target add aarch64-linux-android
rustup target add x86_64-linux-android

# cargo-ndk
cargo install cargo-ndk

# Skia from-source dependencies
winget install Python.Python.3.12         # or installer from python.org
winget install Ninja-build.Ninja
winget install Git.Git

# Android NDK — install via Android Studio SDK Manager
#   SDK Tools → "NDK (Side by side)" v23.x or v25.x
# Default path ends up at %LOCALAPPDATA%\Android\Sdk\ndk\<version>

# Set ANDROID_NDK_HOME at user scope (persists across sessions)
[Environment]::SetEnvironmentVariable(
    "ANDROID_NDK_HOME",
    "$env:LOCALAPPDATA\Android\Sdk\ndk\23.2.8568313",
    "User"
)

# Restart PowerShell, then verify
$env:ANDROID_NDK_HOME
Test-Path "$env:ANDROID_NDK_HOME\toolchains\llvm\prebuilt\windows-x86_64"

# JDK 17 (only needed for AAR build)
winget install EclipseAdoptium.Temurin.17.JDK
```

**PowerShell execution policy** — if scripts refuse to run:

```powershell
# One-off: run a single script bypassing policy
powershell -ExecutionPolicy Bypass -File .\scripts\build-android-so.ps1 arm64-v8a release

# Or relax at user scope (only your account)
Set-ExecutionPolicy -Scope CurrentUser -ExecutionPolicy RemoteSigned
```

**Git long-path support** — Skia checkouts include paths > 260 chars:

```powershell
git config --global core.longpaths true
```

---

## Build workflows

### 1. Host Rust tests

No NDK needed, and the `v8` crate downloads its own prebuilt archive.
The **first** build still compiles Skia from source, though (see *Skia
source-build tooling* above — ~30–50 min cold, needs `python3`, `ninja`,
and the `-dev` headers below); after that, Rust-only rebuilds take
seconds.

```bash
cd engine
cargo test --workspace --lib --doc
cargo check --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
```

Beyond the Skia tooling, the host crates link `fontconfig`, `freetype`,
`EGL`, `GLES` and `asound`. On Debian/Ubuntu:

```bash
sudo apt install -y libgl-dev libegl-dev libgles-dev \
    libfontconfig1-dev libfreetype-dev libasound2-dev
```

On a minimal Ubuntu / WSL2 host, `scripts/dev-test-host.sh <cargo-args>`
wires up those headers, the system clang (the NDK's clang breaks the
host Skia build), and an offline V8 archive — e.g.
`scripts/dev-test-host.sh test --workspace --lib`.

`scripts/ci/run_smoke.sh` runs the four checks above (plus the Android
feature gate) in order and is the canonical pre-merge gate.

### 2. Android shared library (`libmigo.so`)

Produces `engine/jniLibs/<abi>/libmigo.so` and `libc++_shared.so`.

**Linux/macOS**:

```bash
# arm64 only, release
bash scripts/build-android-so.sh arm64-v8a release

# Both ABIs
bash scripts/build-android-so.sh all release

# Debug (faster, unoptimised)
bash scripts/build-android-so.sh arm64-v8a debug
```

**Windows**:

```powershell
.\scripts\build-android-so.ps1 arm64-v8a release
.\scripts\build-android-so.ps1 all release
.\scripts\build-android-so.ps1 arm64-v8a           # debug
```

**Expected timings**:

| Scenario | Time |
|---|---|
| Cold build (no cached Skia) | **10–25 min** (Skia source compile dominates) |
| Incremental (Rust-only change) | 1–2 min |
| Both ABIs cold | 20–40 min |

The script prints `[SUCCESS] All Android builds succeeded` at the
end. Resulting files:

```
engine/jniLibs/arm64-v8a/libmigo.so
engine/jniLibs/arm64-v8a/libc++_shared.so
engine/jniLibs/x86_64/libmigo.so
engine/jniLibs/x86_64/libc++_shared.so
```

### 3. Android AAR (for app integration)

Requires step 2 (which it invokes internally) plus JDK 17+.

**Linux/macOS**:

```bash
bash scripts/build-aar.sh release
bash scripts/build-aar.sh release arm64-v8a           # single ABI
```

**Windows**:

```powershell
.\scripts\build-aar.ps1 release
.\scripts\build-aar.ps1 release arm64-v8a
```

Artifact:

```
platforms/android/dist/library-release.aar
```

Skip the Rust step if the `.so` files are already up-to-date:

```bash
bash scripts/build-aar.sh release --skip-rust
```

### 4. Linux x86_64 SDK

Produces `dist/migo-linux-x86_64/` with `libmigo.so.<version>` (soname
`libmigo.so.1`), `libmigo.a`, public headers, CMake package, and
`pkg-config` `.pc`. Builds on Linux only.

**Prerequisites beyond the common set**:

- `lld` (e.g. `apt-get install lld`). GNU `ld` produces a malformed
  symbol-version table for this link; only `lld` is supported. The
  script looks for `ld.lld` on `PATH` or honours `MIGO_LLD`.
- The Debian bullseye amd64 sysroot (enforces glibc 2.31 /
  GLIBCXX 3.4.28 loader floor). Fetch it first:

  ```bash
  bash scripts/fetch-linux-sysroot.sh
  ```

  This materialises the sysroot at
  `engine/third_party/linux-sysroot/` from Chromium's published
  tarball and verifies it against the sha256 in
  `engine/third_party/linux-sysroot/sysroots.json`. Previous builds
  required a sibling `../rusty_v8_src` checkout; that dependency is
  gone.

- Prebuilt V8 archive for `x86_64-linux-gnu`:

  ```bash
  bash scripts/fetch-v8-archives.sh x86_64-linux-gnu
  ```

**Build**:

```bash
bash scripts/fetch-linux-sysroot.sh
bash scripts/fetch-v8-archives.sh x86_64-linux-gnu
bash scripts/build-linux-sdk.sh
bash scripts/test-linux-sdk-contract.sh
```

The contract gate verifies the loader floor, export surface, soname /
version-symlink chain, manifest consistency, declared dynamic
dependencies, and that the staged headers compile standalone under C11
and C++17.

**Linux Qt 6 host kit** — `platforms/linux/host-kit/` ships a Qt 6
X11 surface view and managed-session helper. Its own gate:

```bash
bash scripts/test-linux-qt-host-kit.sh
```

Requires `cmake`, `ninja`, `c++`, `rg`, and `xvfb-run` in addition to
Qt 6 with xcb support.

### 5. Windows SDK (x86_64 / arm64)

Produces `dist/migo-windows-<arch>/` with `bin/migo.dll`,
`lib/migo.lib` (MSVC import library), runtime DLLs (`rusty_v8.dll`,
ANGLE), public headers, and a CMake package. Built by `release.yml`'s
`release-windows` (x86_64, `windows-latest`) and `release-windows-arm64`
(arm64, `windows-11-arm`) jobs via `scripts/build-windows-sdk-native.sh`
— no by-hand step for a normal release. Neither job cross-compiles
`migo.dll` itself: each runs natively on a runner of its own
architecture, `ilammy/msvc-dev-cmd` puts that runner's native
`cl.exe`/`link.exe` on `PATH`. The cross-compilation this SDK's inputs
*did* need happened once, upstream: `scripts/build-v8-windows.sh` /
`scripts/build-angle-windows.sh aarch64`, run from this project's own
x86_64 dev machine, produced the `aarch64-pc-windows-msvc` V8 archive
and ANGLE runtime this section's arm64 steps below fetch prebuilt.
Building `migo.dll` for arm64 without that cross-compilation step
having already run needs either a native arm64 Windows machine, or the
GitHub-hosted `windows-11-arm` runner `release-windows-arm64` uses.

Two entry points produce the identical package, for two
different environments (see `scripts/lib/windows-sdk-package.sh` for
what they share):

- **`scripts/build-windows-sdk-native.sh`** — runs directly on Windows:
  CI, or any Windows box with Git for Windows and Visual Studio Build
  Tools, no WSL involved. Assumes `link.exe`/`cl.exe` are already on
  `PATH` (`vcvars64.bat` already ran, or `ilammy/msvc-dev-cmd` already
  ran as a prior CI step) rather than locating Visual Studio itself.
- **`scripts/build-windows-sdk.sh`** — this project's WSL2 dev-machine
  path, where the toolchain lives on a native Windows disk reached by
  crossing a WSL/Windows boundary (`wslpath`, a synced Windows-side
  worktree under `/mnt/c/...`, `cmd.exe`-dispatched batch files) that a
  CI runner's checkout, already on native NTFS, does not have.

**Requirements** (native, e.g. CI):

- MSVC toolchain (Visual Studio 2022 or Build Tools) with
  `VC.Tools.x86.x64` (x86_64) or `VC.Tools.ARM64` (arm64), already on
  `PATH`.
- Windows V8 artifacts (`rusty_v8.lib`, `rusty_v8.dll`, `rusty_v8.dll.lib`,
  `src_binding.rs`) in
  `engine/third_party/rusty_v8/<x86_64|aarch64>-pc-windows-msvc/`. Get them
  with:

  ```bash
  bash scripts/fetch-v8-archives.sh x86_64-pc-windows-msvc    # or aarch64-pc-windows-msvc
  ```

  or rebuild from source with `bash scripts/build-v8-windows.sh [aarch64]`
  (needs GN, Ninja and a rusty_v8 checkout on a Windows local disk — see
  that script's own header for the full prerequisite list; it re-seals
  `component-manifest.json` when it finishes).
- ANGLE runtime DLLs (`libEGL.dll`, `libGLESv2.dll`, `d3dcompiler_47.dll`),
  pinned in `contracts/artifact-manifest/windows-angle.lock.json` (ANGLE
  publishes no official prebuilt Windows binaries, so these are self-hosted
  on the same release the V8 archives use — see that lock file's
  `why_self_hosted` note). Get them with:

  ```bash
  bash scripts/fetch-windows-angle.sh          # x64, the default
  bash scripts/fetch-windows-angle.sh arm64    # or arm64
  ```

**Build** (native):

```bash
bash scripts/build-windows-sdk-native.sh              # x86_64, the default
bash scripts/test-windows-sdk-contract.sh --strict

bash scripts/build-windows-sdk-native.sh aarch64       # or arm64
bash scripts/test-windows-sdk-contract.sh aarch64 --strict
```

**Build** (WSL, this project's dev machine — additionally needs a
Windows-side worktree on a local drive, provisioned by
`bash platforms/windows/spike/sync-worktree.sh`; UNC paths are unusable
for `cargo`):

```bash
bash scripts/build-windows-sdk.sh
```

Either script compiles the `migo-capi` staticlib (stage 1), discovers
the Skia / V8 link-search directories from the build output (stage 2),
links `migo.dll` with `/OPT:REF` and a `.def` export allowlist derived
from the headers, stages the package, and writes the package manifest.
`build-windows-sdk.sh` additionally runs the contract gate itself for
local one-command convenience; the CI job runs it as its own explicit
step (above) so a failure shows up as its own named check. Either way
the gate requires MSVC (`dumpbin`, `cl`) to verify the export surface
and load the DLL.

### 6. OpenHarmony SDK (aarch64 / x86_64)

Produces `dist/migo-ohos-<arch>/` with `lib/libmigo_capi.a` (static),
public headers, and a CMake package. Builds on Linux.

**Prerequisites**:

- OpenHarmony SDK (5.1.0-Release or later; ~3.2 GB). Install
  instructions are in the header of `scripts/dev-setup-ohos.sh`.
  Set `OHOS_NDK_HOME` to the directory containing `native/`.
- The OpenHarmony V8 archive, normally **fetched prebuilt** rather than
  built:

  ```bash
  bash scripts/fetch-v8-archives.sh x86_64-linux-ohos aarch64-linux-ohos
  ```

  `build-ohos-sdk.sh` only falls back to building V8 from source
  (`scripts/build-v8-ohos.sh`, which needs a `rusty_v8_src` checkout at
  `../rusty_v8_src` relative to the repo root) when no archive is already
  present at `engine/third_party/rusty_v8/<arch>-linux-ohos/librusty_v8.a` —
  fetch it first and that multi-hour step never runs. This is the same
  archive `release-ohos` fetches in CI.

**Verify the SDK and print required exports**:

```bash
bash scripts/dev-setup-ohos.sh --check
```

**Build** (single command; builds V8 if absent, then packages and
gates):

```bash
# x86_64 (emulator target):
bash scripts/build-ohos-sdk.sh x86_64

# aarch64 (device target):
bash scripts/build-ohos-sdk.sh aarch64

# Both arches:
bash scripts/build-ohos-sdk.sh --all
```

`build-ohos-sdk.sh` runs the package contract and API floor gate
internally. To run the contract gate against an existing staged package:

```bash
bash scripts/test-ohos-sdk-contract.sh dist/migo-ohos-x86_64
```

**Known gaps** (from `dist/migo-ohos-x86_64/share/migo/ohos-x86_64-manifest.json`):

- Only x86_64 has been run on a device (an API 20 emulator). The
  aarch64 package is built and gated but has not run on real
  HarmonyOS NEXT hardware.
- Multi-touch is unverified: `hdc` cannot synthesise a second pointer.

OpenHarmony has published releases since v0.9.2 (`release-ohos` in
`release.yml`, GitHub-hosted `ubuntu-latest`, no device involved — it fetches
the prebuilt V8 archive and SDK, then builds and stages the package). The
device-side gaps above are about what has been *run* and verified on real
hardware, not about whether the package ships.

---

### 7. Release asset from a staged SDK

Every `build-*-sdk.sh` above stops at a staged prefix directory. The
asset a release publishes is one step further on:

```bash
bash scripts/package-sdk.sh dist/migo-linux-x86_64
# -> dist/migo-<version>-capi-linux-x86_64.tar.gz
#    dist/migo-<version>-capi-linux-x86_64.tar.gz.attestation.json
```

The asset name is derived from the staged prefix with the release version
inserted, so the same command serves all four platforms
(`dist/migo-android-arm64` → `migo-<version>-capi-android-arm64.tar.gz`,
`dist/migo-windows-x86_64` → `migo-<version>-capi-windows-x86_64.tar.gz`). The
`capi` segment distinguishes these from the Android AAR, whose `.aar`
extension already says "Android, Java/Kotlin", and the version is in the
name because a file that has been renamed or moved off the release page is
otherwise unidentifiable. Staged prefixes use `arm64`/`x86_64` — the public
vocabulary — while `arm64-v8a` and `aarch64` stay where the NDK and the Rust
target triple need them. `--output-dir` places the pair somewhere else.
`scripts/test-release-asset-naming-contract.sh` holds the scheme.

Windows joined this path in #55: `windows_sdk_write_manifest`
(`scripts/lib/windows-sdk-package.sh`, called by both
`build-windows-sdk.sh` and `build-windows-sdk-native.sh`) writes
`share/migo/windows-<arch>-manifest.json` for `package-sdk.sh` to attest
against, exactly as the other three build scripts do. v0.9.4 shipped
`migo-<version>-capi-windows-{x86_64,arm64}.tar.gz` with attestations
this way.

The archive is **byte-identical for a given commit**: entries sorted,
owner and group normalised to numeric `0`, permissions derived from the
owner bits so the builder's umask cannot leak in, every mtime taken from
`SOURCE_DATE_EPOCH`, and gzip told to store neither a timestamp nor the
source file name. `SOURCE_DATE_EPOCH` defaults to `HEAD`'s commit time,
so the default is already reproducible:

```bash
SOURCE_DATE_EPOCH=1700000000 bash scripts/package-sdk.sh dist/migo-linux-x86_64
```

That property is what makes the `.attestation.json` beside the archive
worth anything — the `package_sha256` it records is a number the
recipient can arrive at independently. It is held by
`scripts/test-sdk-package-reproducibility-contract.sh`, which packages a
synthetic prefix twice and compares.

---

## Troubleshooting

### `curl: (22) The requested URL returned error: 404` on skia-bindings

**Not a failure**. The skia-bindings prebuilt download only supports
host targets; for Android it returns 404 and the build falls back to
`STARTING A FULL BUILD`. If the real failure happens *after* that
line, something in the source-compile chain is missing — check the
next 100 lines of the log:

```bash
# Linux/macOS
bash scripts/build-android-so.sh arm64-v8a release 2>&1 | tee /tmp/build.log
tail -120 /tmp/build.log

# Windows
.\scripts\build-android-so.ps1 arm64-v8a release 2>&1 | Tee-Object build.log
Get-Content build.log -Tail 120
```

Most common root causes, in order:

1. **`ninja: command not found`** — install per
   [platform setup](#platform-setup) or run `source scripts/
   dev-setup-skia.sh` on Linux.
2. **`python3: command not found`** — install Python 3.8+.
3. **OOM during `SkCanvas.cpp` compile** — free up memory or raise
   WSL2 limit.
4. **`gclient sync` timing out** — set a git / network proxy:
   ```bash
   export HTTPS_PROXY=http://127.0.0.1:7890
   export HTTP_PROXY=http://127.0.0.1:7890
   git config --global http.proxy $HTTP_PROXY
   ```

### `error: ANDROID_NDK_HOME is not set`

The env var must be set *and* the NDK directory actually exist:

```bash
# Linux/macOS
echo "$ANDROID_NDK_HOME"
ls "$ANDROID_NDK_HOME/toolchains/llvm/prebuilt"
```

```powershell
# Windows
$env:ANDROID_NDK_HOME
Test-Path "$env:ANDROID_NDK_HOME\toolchains\llvm\prebuilt\windows-x86_64"
```

On Windows the env var must be in **User** (not Process) scope if you
want it to persist across terminal restarts. See the setup section
above.

### `libclang_rt.builtins-aarch64-android.a not found`

Some NDK versions lay out the clang builtins differently. Verify it
exists somewhere under your NDK:

```bash
find "$ANDROID_NDK_HOME" -name 'libclang_rt.builtins-aarch64-android.a'
```

If empty, try a different NDK version (r23b or r25c are known-good).

### `libc++_shared.so not found` after build

The `copy libc++_shared.so` step points at
`toolchains/llvm/prebuilt/<host>/sysroot/usr/lib/<target>/libc++_shared.so`.
If your NDK has a different host directory name, either install a
matching NDK or edit the script's host path.

### Gradle: `Could not find gradle-8.4-all.zip`

The AAR script downloads the Gradle wrapper on first run; requires
network access. If behind a firewall:

```bash
export GRADLE_OPTS='-Dhttps.proxyHost=127.0.0.1 -Dhttps.proxyPort=7890'
```

### `[linux-sdk] no lld found`

The Linux SDK link requires `lld`; GNU `ld` produces a malformed
symbol-version table for this link and cannot be used. Install it:

```bash
sudo apt-get install lld
```

Or point the script at an existing copy:

```bash
MIGO_LLD=/path/to/ld.lld bash scripts/build-linux-sdk.sh
```

### `[linux-sdk] sysroot not found`

The Debian bullseye sysroot must be materialised before the Linux SDK
can be built:

```bash
bash scripts/fetch-linux-sysroot.sh
```

The script downloads the sysroot from Chromium's CDN and verifies its
sha256 against `engine/third_party/linux-sysroot/sysroots.json`.

### `[ohos-setup] no OpenHarmony SDK found`

Set `OHOS_NDK_HOME` to the directory containing `native/`, or place
the SDK at one of the probed locations (`~/ohos-sdk`,
`~/.ohos-sdk`, `/opt/ohos-sdk`, `/usr/local/ohos-sdk`). See the
header of `scripts/dev-setup-ohos.sh` for download instructions.

---

## Project invariants

Things the build guarantees (verified by `scripts/ci/run_smoke.sh`
and the per-platform contract gates):

- **Android feature gate**: `zune-image` and `image` crates **never**
  appear in the Android feature graph (the platform decoder is
  BitmapFactory via JNI). Verified by
  `scripts/ci/check_android_feature_gate.sh`.
- **minSdk = 26**: enforced by `ANDROID_API=26` in both
  `build-android-so.{sh,ps1}` and `platforms/android/library/
  build.gradle`.
- **Zero AndroidX dependencies** in `library/build.gradle` (see the
  declaration comment there).
- **Linux loader floor**: glibc 2.31 / GLIBCXX 3.4.28, enforced by
  the Debian bullseye sysroot and verified by
  `scripts/test-linux-sdk-contract.sh`.
- **C-ABI export surface**: only documented `migo_*` entry points are
  exported from `libmigo.so` (Linux) and `migo.dll` (Windows);
  enforced by a linker version script / `.def` file derived from the
  headers and verified by the contract gates.
- **Single release version**: `release/VERSION` is the sole version
  source; all build consumers derive from it. Enforced by
  `scripts/test-release-version-contract.sh`.

If any of these change, update **this file** and the corresponding CI
check in lock-step.
