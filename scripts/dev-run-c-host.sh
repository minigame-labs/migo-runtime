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
mkdir -p "$CODE_DIR"
for name in game.json game.js; do
    [[ -f "$GAME_DIR/$name" ]] || { c_err "missing $GAME_DIR/$name"; exit 1; }
    cp "$GAME_DIR/$name" "$CODE_DIR/$name"
done
c_info "deployed '$CONTENT_ID' to $CODE_DIR"

# ---- build the C host ----
#
# Cargo drives the link: `tools/c-host-example` compiles tests/c_host/linux/main.c
# through a `#![no_main]` bin crate, so the C code sees nothing but the public
# headers while cargo resolves the native dependencies.
#
# For the packaged path -- the same C file built with plain cc and pkg-config,
# no cargo -- use tests/c_host/build-with-pkgconfig.sh after running
# scripts/build-linux-sdk.sh.
cd "$ENGINE_DIR"
c_info "building the C host (cargo drives the link) ..."
cargo build -p migo-c-host-example --offline

BIN="$ENGINE_DIR/target/debug/migo-c-host"
[[ -x "$BIN" ]] || { c_err "C host binary not produced: $BIN"; exit 1; }

c_info "running: $BIN $RUN_ROOT/files $CONTENT_ID $SECS"
exec "$BIN" "$RUN_ROOT/files" "$CONTENT_ID" "$SECS"
