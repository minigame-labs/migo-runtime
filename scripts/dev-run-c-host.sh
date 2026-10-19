#!/usr/bin/env bash
# scripts/dev-run-c-host.sh
#
# Build and run tests/c_host: a third-party host written in C that drives the
# engine through the public C ABI only. It links the `capi` staticlib, which is
# why no shared-TLS V8 rebuild is needed yet — a staticlib goes
# into a normal executable.
#
# Usage:
#   scripts/dev-run-c-host.sh [GAME_BUNDLE_DIR] [SECONDS]
#   scripts/dev-run-c-host.sh --build-only
#
# `--build-only` compiles the host and exits, for the reason dev-run-player.sh
# gives for its own: the environment below is a cargo fingerprint input, so a
# caller that wants the host built ahead of a timed run (the conformance
# suite's C ABI runner) builds it through this script, not with a `cargo build`
# of its own.
#
# The whole bundle directory is deployed -- the files a game loads beside its
# game.js (fonts, images) are content too.
#
# Env overrides:
#   MIGO_HOST_V8_DIR     gn_out dir holding obj/librusty_v8.a + src_binding.rs
#   MIGO_HOST_V8_ARCHIVE explicit linux-gnu librusty_v8.a path
#   MIGO_HOST_V8_BINDING explicit linux-gnu src_binding.rs path

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
ENGINE_DIR="$REPO_ROOT/engine"

c_info() { echo -e "\033[0;36m[c-host] $*\033[0m"; }
c_err()  { echo -e "\033[0;31m[c-host] $*\033[0m" >&2; }

BUILD_ONLY=0
if [[ "${1:-}" == "--build-only" ]]; then
    BUILD_ONLY=1
    shift
fi

GAME_DIR="${1:-$REPO_ROOT/../migo-bench/shells/migo-shell/app/src/main/assets/game}"
SECS="${2:-10}"
CONTENT_ID="c-host-demo"
RUN_ROOT="${MIGO_C_HOST_ROOT:-/tmp/migo-c-host}"

# ---- host V8 + Skia toolchain env (same as scripts/dev-test-host.sh) ----
# shellcheck source=scripts/lib/host-v8.sh
source "$SCRIPT_DIR/lib/host-v8.sh"
host_v8_resolve "$REPO_ROOT" || exit 1
V8_ARCHIVE="$HOST_V8_ARCHIVE"
V8_BINDING="$HOST_V8_BINDING"
bash "$SCRIPT_DIR/dev-setup-skia.sh" >/dev/null

export RUSTY_V8_ARCHIVE="$V8_ARCHIVE"
export RUSTY_V8_SRC_BINDING_PATH="$V8_BINDING"
export CC="${CC_HOST:-/usr/bin/clang}"
export CXX="${CXX_HOST:-/usr/bin/clang++}"
export CPATH="$HOME/.local/skia-headers${CPATH:+:$CPATH}"
export LIBRARY_PATH="$HOME/.local/lib${LIBRARY_PATH:+:$LIBRARY_PATH}"
export LD_LIBRARY_PATH="$HOME/.local/lib:/usr/lib/wsl/lib:/usr/lib/x86_64-linux-gnu${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
unset ANDROID_NDK ANDROID_NDK_HOME || true

# ---- deploy the game bundle where the ABI expects installed content ----
CODE_DIR="$RUN_ROOT/files/migo/games/$CONTENT_ID/code"
if (( BUILD_ONLY == 0 )); then
    for name in game.json game.js; do
        [[ -f "$GAME_DIR/$name" ]] || { c_err "missing $GAME_DIR/$name"; exit 1; }
    done
    rm -rf "$CODE_DIR"
    mkdir -p "$CODE_DIR"
    cp -R "$GAME_DIR"/. "$CODE_DIR"/
    c_info "deployed '$CONTENT_ID' to $CODE_DIR"
fi

# ---- build the C host ----
#
# Cargo drives the link: `tools/c-host-example` compiles tests/c_host/main.c
# through a `#![no_main]` bin crate, so the C code sees nothing but the public
# headers while cargo resolves the native dependencies.
#
# For the packaged path -- the same C file built with plain cc and pkg-config,
# no cargo -- use tests/c_host/build-with-pkgconfig.sh after running
# scripts/build-linux-sdk.sh.
cd "$ENGINE_DIR"
c_info "building the C host (cargo drives the link) ..."
cargo build -p migo-c-host-example --offline

source "$SCRIPT_DIR/lib/cargo-target-dir.sh"
CARGO_TARGET="$(cargo_target_dir "$ENGINE_DIR")" || exit 1
BIN="$CARGO_TARGET/debug/migo-c-host"
[[ -x "$BIN" ]] || { c_err "C host binary not produced: $BIN"; exit 1; }
if (( BUILD_ONLY == 1 )); then
    c_info "built $BIN"
    exit 0
fi

c_info "running: $BIN $RUN_ROOT/files $CONTENT_ID $SECS"
exec "$BIN" "$RUN_ROOT/files" "$CONTENT_ID" "$SECS"
