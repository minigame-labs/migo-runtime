#!/usr/bin/env bash
# =============================================================================
# Stage migo into the OpenHarmony host project and build the HAP.
#
# The host in platforms/openharmony is an ordinary DevEco project that consumes
# the C SDK: it sees only the public headers and links libmigo_capi.a into its
# own libmigohost.so. That is the same relationship the Android NativeActivity
# host has with the Android C SDK, and keeping it that way is what makes the
# host a test of the SDK rather than an extension of it.
#
# Usage:
#   scripts/build-ohos-host.sh [--arch x86_64|aarch64] [--no-hap]
#
# Env:
#   OHOS_NDK_HOME    OpenHarmony SDK (probed via dev-setup-ohos.sh)
#   DEVECO_HOME      DevEco Studio install (default: the standard Windows path
#                    reached through /mnt/c, since the emulator and hvigor live
#                    on the Windows side while the engine is built in WSL)
# =============================================================================
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/v8-materialise.sh
source "$SCRIPT_DIR/lib/v8-materialise.sh"
# shellcheck source=scripts/lib/cargo-target-dir.sh
source "$SCRIPT_DIR/lib/cargo-target-dir.sh"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
HOST_DIR="$REPO_ROOT/platforms/openharmony"

info() { echo -e "\033[0;36m[ohos-host] $*\033[0m"; }
err()  { echo -e "\033[0;31m[ohos-host] $*\033[0m" >&2; }
ok()   { echo -e "\033[0;32m[ohos-host] $*\033[0m"; }

ARCH="x86_64"
BUILD_HAP=1
while [[ $# -gt 0 ]]; do
    case "$1" in
        --arch) ARCH="$2"; shift 2 ;;
        --no-hap) BUILD_HAP=0; shift ;;
        *) err "unknown argument: $1"; exit 2 ;;
    esac
done
TRIPLE="$ARCH-unknown-linux-ohos"

# ---- build the static library the host links --------------------------------
V8_DIR="$REPO_ROOT/engine/third_party/rusty_v8/$ARCH-linux-ohos"
# Verified against its component manifest, then used from a content-addressed path.
v8_materialise "$V8_DIR" "$REPO_ROOT/engine/target/v8-materialised" || exit 1
if [[ ! -f "$V8_DIR/librusty_v8.a" ]]; then
    err "missing $V8_DIR/librusty_v8.a"
    err "build it first: scripts/build-v8-ohos.sh $ARCH"
    exit 1
fi

eval "$(bash "$SCRIPT_DIR/dev-setup-ohos.sh" | grep '^export ')"
[[ -n "${OHOS_SDK_NATIVE:-}" ]] || { err "OpenHarmony SDK not usable"; exit 1; }

CARGO_TARGET="$(cargo_target_dir "$REPO_ROOT/engine")" || exit 1
STATIC_LIB="$CARGO_TARGET/$TRIPLE/release/libmigo_capi.a"
info "building migo-capi for $TRIPLE"
(
    cd "$REPO_ROOT/engine"
    # From engine/ so rust-toolchain.toml applies; profile-slim because the
    # full profile needs ALSA, which OpenHarmony does not have.
    RUSTY_V8_ARCHIVE="$V8_MATERIALISED_ARCHIVE" \
    RUSTY_V8_SRC_BINDING_PATH="$V8_MATERIALISED_BINDING" \
        cargo build -p migo-capi --release \
            --no-default-features --features profile-slim \
            --target "$TRIPLE"
)
[[ -f "$STATIC_LIB" ]] || { err "no static library produced"; exit 1; }

# ---- stage it where CMakeLists.txt expects ----------------------------------
CPP_DIR="$HOST_DIR/entry/src/main/cpp"
mkdir -p "$CPP_DIR/libs/$ARCH" "$CPP_DIR/migo-include"
cp "$STATIC_LIB" "$CPP_DIR/libs/$ARCH/"
rm -rf "$CPP_DIR/migo-include/migo"
cp -r "$REPO_ROOT/include/migo" "$CPP_DIR/migo-include/"
ok "staged $(stat -c %s "$STATIC_LIB") bytes + public headers"

if [[ $BUILD_HAP -eq 0 ]]; then
    info "--no-hap given; stopping before the DevEco build"
    exit 0
fi

# ---- build the HAP ----------------------------------------------------------
# hvigor and the emulator live on the Windows side while the engine is built in
# WSL. Two constraints shape everything below, both established by hitting them:
#
#   - hvigor rejects a UNC project path outright ("Invalid project path"), so
#     the project cannot be built in place on \\wsl.localhost. It is copied to a
#     real Windows directory first.
#   - cmd.exe refuses a UNC working directory and silently falls back to
#     C:\Windows, so every cmd invocation runs from a local directory.
#
# An earlier revision of this script printed advice to run hvigor by hand
# against the UNC path -- advice that cannot work -- and then exited 0, so it
# reported success while producing nothing. This does the build.
DEVECO_HOME="${DEVECO_HOME:-/mnt/c/Program Files/Huawei/DevEco Studio}"
HVIGOR="$DEVECO_HOME/tools/hvigor/bin/hvigorw.js"
if [[ ! -f "$HVIGOR" ]]; then
    err "hvigor not found at $HVIGOR"
    err "set DEVECO_HOME to the DevEco Studio installation"
    exit 1
fi
command -v wslpath >/dev/null 2>&1 || {
    err "wslpath not found: this path builds the HAP through a Windows-side"
    err "DevEco install and needs WSL. On a Linux DevEco install, run hvigor"
    err "directly against $HOST_DIR after this script has staged it (--no-hap)."
    exit 1
}

WIN_DIR="${MIGO_OHOS_WIN_DIR:-C:\\migo-ohos-host}"
WIN_DIR_WSL="$(wslpath -u "$WIN_DIR")"
info "syncing the project to $WIN_DIR"
mkdir -p "$WIN_DIR_WSL"
# --update so the 350MB archive is not recopied when it has not changed; the
# staged copy is compared by timestamp, and build-ohos-sdk.sh always rebuilds it
# when sources change, so a stale archive cannot survive here either.
if command -v rsync >/dev/null 2>&1; then
    rsync -a --delete --exclude 'build/' --exclude '.hvigor/' --exclude 'oh_modules/' \
        "$HOST_DIR/" "$WIN_DIR_WSL/"
else
    cp -ru "$HOST_DIR/." "$WIN_DIR_WSL/"
fi

# The batch file is generated here rather than kept on the Windows side so the
# recipe lives in the repository. Two details in it are load-bearing:
#   set "ERRORLEVEL=" -- an inherited variable of that name makes %errorlevel%
#     always read as it, hiding every failure.
#   HVIGOR_EXIT captured on the line immediately after node -- any command in
#     between, including echo, replaces %errorlevel% with its own.
DEVECO_WIN="$(wslpath -w "$DEVECO_HOME")"
cat > "$WIN_DIR_WSL/build-hap.bat" <<BAT
@echo off
rem Generated by scripts/build-ohos-host.sh -- do not edit here; edit the script.
setlocal
set "ERRORLEVEL="
set "DEVECO=$DEVECO_WIN"
set "DEVECO_SDK_HOME=%DEVECO%\\sdk"
set "NODE_HOME=%DEVECO%\\tools\\node"
set "JAVA_HOME=%DEVECO%\\jbr"
set "PATH=%NODE_HOME%;%JAVA_HOME%\\bin;%PATH%"
cd /d $WIN_DIR
"%NODE_HOME%\\node.exe" "%DEVECO%\\tools\\hvigor\\bin\\hvigorw.js" --mode module -p product=default assembleHap --no-daemon
set HVIGOR_EXIT=%errorlevel%
echo BUILD_EXIT=%HVIGOR_EXIT%
exit /b %HVIGOR_EXIT%
BAT

info "building the HAP with hvigor"
# Run from a local directory: cmd.exe started from a UNC cwd prints a warning,
# falls back to C:\Windows, and the failure reads like a compile error.
( cd /tmp && cmd.exe /c "$(wslpath -w "$WIN_DIR_WSL/build-hap.bat")" ) || {
    err "hvigor failed"
    exit 1
}

HAP="$WIN_DIR_WSL/entry/build/default/outputs/default/entry-default-unsigned.hap"
[[ -f "$HAP" ]] || { err "hvigor reported success but produced no HAP at $HAP"; exit 1; }
ok "HAP: $HAP ($(stat -c %s "$HAP") bytes)"
info "install it with:  scripts/run-ohos-host.sh"
