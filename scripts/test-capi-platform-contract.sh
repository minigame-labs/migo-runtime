#!/usr/bin/env bash
# The C ABI must compile for every platform it claims to support.
#
# capi was desktop-only for two slices and nothing noticed, because no gate ever
# built it for Android. A cross `check` is cheap and catches the whole class of
# regression: naming a platform type directly from an entry point, or adding a
# call only one backend can satisfy.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
ENGINE_DIR="$REPO_ROOT/engine"
TARGET="${MIGO_ANDROID_TARGET:-aarch64-linux-android}"

info() { echo -e "\033[0;36m[capi-platform] $*\033[0m"; }
err() { echo -e "\033[0;31m[capi-platform] $*\033[0m" >&2; }

# shellcheck source=scripts/lib/android-ndk.sh
source "$SCRIPT_DIR/lib/android-ndk.sh"
android_ndk_read_pin "$REPO_ROOT/contracts/artifact-manifest/android-v8.lock.json" || exit 1
android_ndk_resolve || { err "cannot resolve the pinned Android NDK"; exit 1; }
NDK_BIN="$(echo "$ANDROID_NDK_HOME"/toolchains/llvm/prebuilt/*/bin)"
export PATH="$NDK_BIN:$PATH"

# engine/.cargo/config.toml names aarch64-linux-android-ar without a path, and
# the NDK ships it as llvm-ar. Provide the expected name here rather than
# editing that shared config, which the AAR build also relies on.
SHIM_DIR="$(mktemp -d)"
trap 'rm -rf "$SHIM_DIR"' EXIT
ln -sf "$NDK_BIN/llvm-ar" "$SHIM_DIR/aarch64-linux-android-ar"
export PATH="$SHIM_DIR:$PATH"

V8_DIR="$ENGINE_DIR/third_party/rusty_v8/$TARGET"
if [[ ! -f "$V8_DIR/librusty_v8.a" ]]; then
    err "missing $V8_DIR/librusty_v8.a"
    exit 1
fi

# Set inline, never exported: leaking these into a host build makes it link the
# Android archive and fail confusingly.
# Run from inside engine/ rather than passing --manifest-path from the repo
# root. rust-toolchain.toml is resolved from the working directory, so the
# manifest-path form silently used the machine's default toolchain instead of
# the pinned one -- a gate checking a different compiler than the product ships
# with. Observed: the repo root resolved to stable, which lacks the aarch64
# OpenHarmony std that engine's pinned 1.95.0 has.
#
# ⚠ Moving into engine/ also makes engine/.cargo/config.toml's `[env]` block
# apply, and that block sets a bare `CC = "clang-18"` for every target. cc-rs
# then compiles zstd for aarch64-linux-android with the host clang against
# /usr/include, which fails on `bits/libc-header-start.h`. It does not fail on a
# developer machine that already exports a `CC` pointing at the NDK -- an
# exported variable beats the non-forcing `[env]` -- so this is invisible
# locally and only appears on a clean CI runner. Pinning the target-scoped
# names is the fix, because cc-rs resolves CC_<target> before bare CC; that is
# the same lever engine/.cargo/config.toml already uses for the Windows target.
TARGET_U="${TARGET//-/_}"
NDK_API="${MIGO_ANDROID_API:-26}"
NDK_CC="$NDK_BIN/aarch64-linux-android${NDK_API}-clang"
if [[ ! -x "$NDK_CC" ]]; then
    err "NDK clang driver not found: $NDK_CC"
    err "the contract would otherwise compile Android code with the host compiler"
    exit 1
fi

info "cross-checking capi for $TARGET"
(
    cd "$ENGINE_DIR"
    env \
        "CC_${TARGET_U}=$NDK_CC" \
        "CXX_${TARGET_U}=${NDK_CC}++" \
        "AR_${TARGET_U}=$NDK_BIN/llvm-ar" \
        RUSTY_V8_ARCHIVE="$V8_DIR/librusty_v8.a" \
        RUSTY_V8_SRC_BINDING_PATH="$V8_DIR/src_binding.rs" \
        cargo check -p migo-capi --target "$TARGET"
)

info "OK: the C ABI compiles for $TARGET"

# -----------------------------------------------------------------------------
# OpenHarmony round.
#
# This is not "one more platform for completeness". OpenHarmony targets report
# `target_os = "linux"` with `target_env = "ohos"`, so every bare
# `cfg(target_os = "linux")` in the tree matches them too -- and the Android
# round above is structurally incapable of catching that class, because Android
# reports its own target_os. platform/src/lib.rs and capi/src/platform/mod.rs
# spell `not(target_env = "ohos")` today; this round is what keeps them that
# way.
#
# profile-slim is required rather than preferred: the full profile pulls
# audio -> cpal -> alsa-sys -> pkg-config, and OpenHarmony has no ALSA.
# -----------------------------------------------------------------------------
OHOS_TARGET="${MIGO_OHOS_TARGET:-aarch64-unknown-linux-ohos}"
OHOS_ARCH="${OHOS_TARGET%%-*}"
OHOS_V8_DIR="$ENGINE_DIR/third_party/rusty_v8/$OHOS_ARCH-linux-ohos"

ohos_skip() {
    # Loud on stderr, never silent. A quietly skipped lane is exactly how the
    # ILP32 layout assertions stayed broken for two releases.
    echo -e "\033[0;33m[capi-platform] SKIPPED OpenHarmony round: $1\033[0m" >&2
    if [[ "${MIGO_CAPI_REQUIRE_OHOS:-0}" == "1" ]]; then
        err "MIGO_CAPI_REQUIRE_OHOS=1 makes that skip an error"
        exit 1
    fi
    exit 0
}

# Every export, not just the OHOS_* ones: skia-bindings resolves its compiler
# through CLANGCC (then plain CC, then the literal "clang"), so filtering to
# OHOS_* leaves it building Skia with whatever compiler the machine happens to
# have -- which on a machine with an Android NDK is the NDK's clang, against
# bionic headers, for a musl target.
if ! OHOS_EXPORTS="$(bash "$SCRIPT_DIR/dev-setup-ohos.sh" 2>/dev/null | grep '^export ')"; then
    ohos_skip "no usable OpenHarmony SDK (see scripts/dev-setup-ohos.sh)"
fi
eval "$OHOS_EXPORTS"
[[ -n "${OHOS_SDK_NATIVE:-}" ]] || ohos_skip "dev-setup-ohos.sh produced no OHOS_SDK_NATIVE"
[[ -f "$OHOS_V8_DIR/librusty_v8.a" ]] || \
    ohos_skip "missing $OHOS_V8_DIR/librusty_v8.a (build it with scripts/build-v8-ohos.sh $OHOS_ARCH)"

info "cross-checking capi for $OHOS_TARGET"
# `env` rather than an assignment prefix: cc-rs wants CC_<target with
# underscores>, and bash does not expand a variable to form the NAME of an
# assignment prefix -- `CC_${x}=v cmd` is parsed as a command named
# "CC_...=v", which exits 127 and reads like a missing compiler.
OHOS_TARGET_U="${OHOS_TARGET//-/_}"
(
    cd "$ENGINE_DIR"
    env \
        PATH="$OHOS_SDK_NATIVE/llvm/bin:$PATH" \
        "CC_${OHOS_TARGET_U}=$OHOS_SDK_NATIVE/llvm/bin/$OHOS_TARGET-clang" \
        "CXX_${OHOS_TARGET_U}=$OHOS_SDK_NATIVE/llvm/bin/$OHOS_TARGET-clang++" \
        "AR_${OHOS_TARGET_U}=$OHOS_SDK_NATIVE/llvm/bin/llvm-ar" \
        RUSTY_V8_ARCHIVE="$OHOS_V8_DIR/librusty_v8.a" \
        RUSTY_V8_SRC_BINDING_PATH="$OHOS_V8_DIR/src_binding.rs" \
        cargo check -p migo-capi --target "$OHOS_TARGET" \
            --no-default-features --features profile-slim
)

info "OK: the C ABI compiles for $OHOS_TARGET"
