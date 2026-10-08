#!/usr/bin/env bash
# ============================================================
# Measure the engine's wakeups per second while idle.
# Location: scripts/measure-idle-wakeups.sh
#
# The runtime's idle-quiescence contract requires no polling loop and no
# fixed-interval wakeup when idle, measured as wakeups per second at idle. This
# is the instrument for that metric on the engine-paced platforms (Linux,
# Windows, HarmonyOS, and any C host that does not drive frames itself), where
# the frame clock is the engine's own.
#
# A wakeup from a channel wait or a timed wait is a voluntary context switch, so
# /proc/<tid>/status:voluntary_ctxt_switches counts them directly, with no
# instrumentation in the engine to be wrong about.
#
# Both numbers matter and neither is sufficient alone. An engine that never
# renders also reports zero wakeups, so the run asserts that the probe content
# actually painted before it believes the silence. That is not a hypothetical:
# deleting the engine-paced arm produces zero wakeups and zero painted frames.
#
# Usage:
#   scripts/measure-idle-wakeups.sh [GAME_BUNDLE_DIR] [SETTLE_SECS] [WINDOW_SECS]
#
# The default bundle paints two frames and then stops requesting them, so
# everything after the settle period is genuine idle. Requires the host player
# (scripts/dev-run-player.sh builds it) and its EGL/V8 environment.
# ============================================================
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
ENGINE_DIR="$REPO_ROOT/engine"

GAME_DIR="${1:-$SCRIPT_DIR/fixtures/idle-probe}"
SETTLE="${2:-5}"
WINDOW="${3:-5}"
LOG=/tmp/migo-idle-wakeups.log

c_info() { echo -e "\033[0;36m[idle] $*\033[0m"; }
c_err() { echo -e "\033[0;31m[idle] $*\033[0m" >&2; }

PLAYER="$ENGINE_DIR/target/debug/migo-player"
[[ -x "$PLAYER" ]] || {
  c_err "player not built: $PLAYER (run scripts/dev-run-player.sh once)"
  exit 1
}

export LD_LIBRARY_PATH="$HOME/.local/lib:/usr/lib/wsl/lib:/usr/lib/x86_64-linux-gnu${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
export MIGO_PLAYER_PNG=/tmp/migo-idle-frame.png
export RUST_LOG="${RUST_LOG:-warn}"

"$PLAYER" "$GAME_DIR" $((SETTLE + WINDOW + 4)) >"$LOG" 2>&1 &
player=$!
trap 'kill $player 2>/dev/null || true' EXIT

# The render thread owns the frame clock, so it is the thread the requirement is
# about; the process total catches anything else that polls.
render_tid=""
for _ in $(seq 1 100); do
  for tid_dir in /proc/$player/task/*; do
    [[ -r "$tid_dir/comm" ]] || continue
    if grep -qi "render" "$tid_dir/comm" 2>/dev/null; then
      render_tid="$(basename "$tid_dir")"
      break
    fi
  done
  [[ -n "$render_tid" ]] && break
  sleep 0.1
done
[[ -n "$render_tid" ]] || { c_err "render thread never appeared; see $LOG"; exit 1; }
c_info "render thread tid=$render_tid comm=$(cat "/proc/$player/task/$render_tid/comm")"

# `^` anchored: nonvoluntary_ctxt_switches contains the same substring.
thread_wakeups() {
  awk '/^voluntary_ctxt_switches/ {print $2}' "/proc/$player/task/$render_tid/status"
}
process_wakeups() {
  awk '/^voluntary_ctxt_switches/ {total += $2} END {print total}' /proc/$player/task/*/status
}

sleep "$SETTLE"
thread_before="$(thread_wakeups)"
process_before="$(process_wakeups)"
threads="$(ls /proc/$player/task | wc -l)"
sleep "$WINDOW"
thread_after="$(thread_wakeups)"
process_after="$(process_wakeups)"

painted="$(grep -c "idle-probe] painted frame" "$LOG" || true)"

echo
echo "IDLE WAKEUPS      window ${WINDOW}s after a ${SETTLE}s settle"
echo "render thread     $(((thread_after - thread_before) / WINDOW))/s   (${thread_before} -> ${thread_after})"
echo "whole process     $(((process_after - process_before) / WINDOW))/s   (${process_before} -> ${process_after}, ${threads} threads)"
echo "frames painted    ${painted}"

if [[ "$painted" -lt 1 ]]; then
  c_err "the probe content never painted, so the wakeup counts say nothing: a dead engine is also quiet"
  exit 1
fi
