#!/usr/bin/env bash
# ============================================================
# Assert that the size of the onscreen canvas answers to one rule.
# Location: scripts/verify-canvas-follows-surface.sh
#
# The rule: a backing store the content never sized is the engine's, derived from
# the surface, and has to be re-derived when the surface changes; a size the
# content chose with `canvas.width` is the content's and must never move. Three
# installs have to agree about it -- a fresh create, a resize of the same native
# surface, and a destroy-and-recreate -- and the third one disagreed: it kept the
# preserved DrawingBuffer at the size derived from the surface the app was
# suspended on, so a canvas nobody sized came back from a rotation still
# describing a portrait window while `getSystemInfoSync()` reported the real one.
#
# An offscreen resize is the destroy-and-recreate path: a pbuffer of the new size
# *is* a new native surface, so it always takes the preserved-buffer branch. That
# makes this reachable with no device and no window server -- which is what the
# defect's own note said was impossible, having read "its trigger is Android
# rotation" as "only Android can show it".
#
# Two fixtures, because either half alone is satisfiable by the wrong engine:
#
#   * canvas-follow-probe never sizes its canvas and paints its verdict, so it
#     catches a backing store that did not follow;
#   * canvas-owned-probe fixes its own resolution and fills that, so a backing
#     store the engine moved anyway leaves part of the surface unpainted. It
#     cannot ask the question in JS -- `canvas.width` is the number JS holds --
#     so geometry asks it instead.
#
# Both paint grey until the window has actually changed extent: green is also
# what a correct pre-resize frame would be, so without that an engine that
# presented once and stopped would pass forever. The player refusing a capture
# that is not the resized extent is the second half of the same liveness reading.
#
# Usage:
#   scripts/verify-canvas-follows-surface.sh [SECS]
#
# Requires the host player and its EGL/V8 environment; builds it every run.
# ============================================================
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
ENGINE_DIR="$REPO_ROOT/engine"
SECS="${1:-6}"

# The verdict colour both fixtures paint once the surface has moved: 51/204/102
# opaque, distinct per channel so a wrong pixel says which channel survived.
EXPECT="51,204,102,255"
# The extent the run resizes to. Neither dimension matches the startup surface
# and neither matches canvas-owned-probe's own resolution, so no coincidence can
# make a stale or moved buffer read as correct.
RESIZE="1000x700"

c_info() { echo -e "\033[0;36m[canvas-size] $*\033[0m"; }
c_ok() { echo -e "\033[0;32m[canvas-size] $*\033[0m"; }
c_err() { echo -e "\033[0;31m[canvas-size] $*\033[0m" >&2; }

source "$SCRIPT_DIR/lib/cargo-target-dir.sh"
CARGO_TARGET="$(cargo_target_dir "$ENGINE_DIR")" || exit 1
PLAYER="$CARGO_TARGET/debug/migo-player"
# Always build, never "build if missing": a mutation run leaves a binary compiled
# from the mutant next to a restored tree, and WSL2 preserves mtime, so an
# if-missing gate happily scores the mutant as the fix.
#
# Through dev-test-host.sh rather than a bare cargo: it is where this repository
# establishes the host V8 archive, the system clang and the Khronos headers that
# the Skia-linked crates need, so a gate that reached for cargo directly would
# either need its own copy of all three or refuse to run for anyone who had not
# exported them by hand.
c_info "building the host player"
bash "$SCRIPT_DIR/dev-test-host.sh" build -p migo-player --offline

export LD_LIBRARY_PATH="$HOME/.local/lib:/usr/lib/wsl/lib:/usr/lib/x86_64-linux-gnu${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
export RUST_LOG="${RUST_LOG:-info}"

failures=0

run_probe() {
  local name="$1"
  local log="/tmp/migo-canvas-size-$name.log"
  local png="/tmp/migo-canvas-size-$name.png"

  local status=0
  MIGO_PLAYER_PNG="$png" "$PLAYER" "$SCRIPT_DIR/fixtures/$name" "$SECS" \
    --offscreen --resize "$RESIZE" >"$log" 2>&1 || status=$?

  local colour distinct
  read -r colour distinct <<<"$(python3 "$SCRIPT_DIR/lib/dominant_pixel.py" "$png" 2>/dev/null)"
  colour="${colour:-no-capture}"
  distinct="${distinct:-0}"

  printf '  %-20s exit=%-3s pixel=%s (%s colour(s))\n' "$name" "$status" "$colour" "$distinct"

  # The player already refuses to write a capture that is not the resized extent,
  # or to exit 0 having presented nothing after the transition. Reading its status
  # first keeps "the resize never happened" from being reported as a colour.
  if [[ "$status" -ne 0 ]]; then
    c_err "$name: the player did not prove a resized frame was presented; see $log"
    failures=$((failures + 1))
    return
  fi
  if [[ "$colour" != "$EXPECT" ]]; then
    c_err "$name presented $colour, wanted $EXPECT: the canvas size did not answer to the rule; see $png"
    failures=$((failures + 1))
    return
  fi
  # Each fixture fills its whole canvas with one colour, so a second colour on the
  # surface is buffer the content's fill never reached -- which is what a backing
  # store of the wrong size looks like when the content, not the engine, chose it.
  if [[ "$distinct" != "1" ]]; then
    c_err "$name presented $distinct distinct colours where the fixture paints one: part of the surface is outside what the content filled; see $png"
    failures=$((failures + 1))
  fi
}

echo
echo "CANVAS SIZE ACROSS A SURFACE RECREATE  ${SECS}s per probe, resize to $RESIZE, expecting rgba($EXPECT)"
run_probe canvas-follow-probe
run_probe canvas-owned-probe
echo

if [[ "$failures" -gt 0 ]]; then
  c_err "$failures probe(s) disagreed about who owns the canvas size"
  exit 1
fi
c_ok "the canvas follows a recreated surface, and a size the content chose survives it"
