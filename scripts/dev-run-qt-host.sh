#!/usr/bin/env bash
# scripts/dev-run-qt-host.sh
#
# Build and run tests/qt_host: a Linux host built on the Qt Host Kit's Managed
# shape (MigoManagedSession), written as an App would write one. It is the
# platform's own host layer that migo-conformance's Host Kit lane verifies; the
# C host (scripts/dev-run-c-host.sh) is the other shape, the bare C ABI.
#
# Usage:
#   scripts/dev-run-qt-host.sh [GAME_BUNDLE_DIR] [SECONDS]
#   scripts/dev-run-qt-host.sh --build-only
#
# `--build-only` compiles the host and exits, for the reason dev-run-c-host.sh
# gives for its own: the environment below is a cargo fingerprint input, so a
# caller that wants the host built ahead of a timed run builds it through this
# script. It needs Qt 6 Widgets (qt6-base-dev) and a display (DISPLAY, or a
# virtual one: xvfb-run).
#
# The host's settings and its scripted player are environment variables it
# reads itself (tests/qt_host/linux/main.cpp): MIGO_QT_HOST_NETWORK_POLICY,
# MIGO_QT_HOST_LAUNCH_OPTIONS, MIGO_QT_HOST_FEED.
#
# Env overrides:
#   MIGO_HOST_V8_DIR     gn_out dir holding obj/librusty_v8.a + src_binding.rs
#   MIGO_HOST_V8_ARCHIVE explicit linux-gnu librusty_v8.a path
#   MIGO_HOST_V8_BINDING explicit linux-gnu src_binding.rs path
#   MIGO_QT_HOST_ROOT    where the game is deployed (default /tmp/migo-qt-host)
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
ENGINE_DIR="$REPO_ROOT/engine"

q_info() { echo -e "\033[0;36m[qt-host] $*\033[0m"; }
q_err()  { echo -e "\033[0;31m[qt-host] $*\033[0m" >&2; }

BUILD_ONLY=0
if [[ "${1:-}" == "--build-only" ]]; then
    BUILD_ONLY=1
    shift
fi

GAME_DIR="${1:-$REPO_ROOT/../migo-bench/shells/migo-shell/app/src/main/assets/game}"
SECS="${2:-10}"
CONTENT_ID="qt-host-demo"
RUN_ROOT="${MIGO_QT_HOST_ROOT:-/tmp/migo-qt-host}"

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
        [[ -f "$GAME_DIR/$name" ]] || { q_err "missing $GAME_DIR/$name"; exit 1; }
    done
    rm -rf "$CODE_DIR"
    mkdir -p "$CODE_DIR"
    cp -R "$GAME_DIR"/. "$CODE_DIR"/
    q_info "deployed '$CONTENT_ID' to $CODE_DIR"
fi

# ---- build the Qt host ----
#
# Cargo drives the link, as it does for the C host: `tools/qt-host-example`
# compiles tests/qt_host/linux/main.cpp and the Host Kit (moc included) and
# links Qt, while cargo resolves the engine's native dependencies.
cd "$ENGINE_DIR"
q_info "building the Qt host (cargo drives the link) ..."
cargo build -p migo-qt-host-example --offline

source "$SCRIPT_DIR/lib/cargo-target-dir.sh"
CARGO_TARGET="$(cargo_target_dir "$ENGINE_DIR")" || exit 1
BIN="$CARGO_TARGET/debug/migo-qt-host"
[[ -x "$BIN" ]] || { q_err "Qt host binary not produced: $BIN"; exit 1; }
if (( BUILD_ONLY == 1 )); then
    q_info "built $BIN"
    exit 0
fi

q_info "running: $BIN $RUN_ROOT/files $CONTENT_ID $SECS"
exec "$BIN" "$RUN_ROOT/files" "$CONTENT_ID" "$SECS"
