#!/usr/bin/env bash
# ============================================================
# Measure resident memory across a long-running workload.
# Location: scripts/measure-steady-state-growth.sh
#
# The runtime's memory contract is that resident memory does not grow across a
# defined long-running workload. The in-process gates
# (`migo_alloc_probe::assert_no_steady_state_growth`) hold individual cycles to
# net zero; this holds the whole process to a flat trend, which is the only
# instrument that can see growth outside the Rust heap — GPU allocations, the V8
# heap, mmap, and allocator fragmentation.
#
# Two-sided by construction, for the same reason the idle-wakeup measurement is:
# a workload that stopped rendering also has flat memory, so the run fails unless
# the content kept producing frames throughout.
#
# Usage:
#   scripts/measure-steady-state-growth.sh [GAME_BUNDLE_DIR] [SECS] [SAMPLE_EVERY]
#
# Requires the host player (scripts/dev-run-player.sh builds it) and its EGL/V8
# environment. Reports the trend; the threshold that turns this into a gate
# belongs in the versioned baseline file, which is Phase 5's.
# ============================================================
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
ENGINE_DIR="$REPO_ROOT/engine"

GAME_DIR="${1:-$REPO_ROOT/../migo-bench/shells/migo-shell/app/src/main/assets/game}"
SECS="${2:-180}"
EVERY="${3:-10}"
# Discarded before the trend is taken: startup, first-frame and cache warm-up are
# not steady state, and counting them would report a rising trend for every run.
SETTLE="${SETTLE:-30}"
LOG=/tmp/migo-growth-player.log

c_info() { echo -e "\033[0;36m[growth] $*\033[0m"; }
c_err() { echo -e "\033[0;31m[growth] $*\033[0m" >&2; }

PLAYER="$ENGINE_DIR/target/debug/migo-player"
[[ -x "$PLAYER" ]] || {
  c_err "player not built: $PLAYER (run scripts/dev-run-player.sh once)"
  exit 1
}
[[ -d "$GAME_DIR" ]] || { c_err "no such bundle: $GAME_DIR"; exit 1; }

export LD_LIBRARY_PATH="$HOME/.local/lib:/usr/lib/wsl/lib:/usr/lib/x86_64-linux-gnu${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
export MIGO_PLAYER_PNG=/tmp/migo-growth-frame.png
export RUST_LOG="${RUST_LOG:-warn}"

c_info "workload ${SECS}s, sampling every ${EVERY}s, first ${SETTLE}s discarded"
"$PLAYER" "$GAME_DIR" "$SECS" >"$LOG" 2>&1 &
player=$!
trap 'kill $player 2>/dev/null || true' EXIT

rss_kb() { awk '/^VmRSS/ {print $2}' "/proc/$player/status" 2>/dev/null; }

sleep "$SETTLE"
first="$(rss_kb)"
[[ -n "$first" ]] || { c_err "the player exited during settle; see $LOG"; exit 1; }

echo
echo "elapsed_s rss_kb"
elapsed="$SETTLE"
last="$first"
peak="$first"
while kill -0 "$player" 2>/dev/null && [[ "$elapsed" -lt "$SECS" ]]; do
  sample="$(rss_kb)"
  [[ -n "$sample" ]] || break
  printf '%9s %s\n' "$elapsed" "$sample"
  last="$sample"
  [[ "$sample" -gt "$peak" ]] && peak="$sample"
  sleep "$EVERY"
  elapsed=$((elapsed + EVERY))
done

# The game's own telemetry is the liveness half: bunnymark prints one line per
# second, so a run that kept rendering has a line near the end of the window.
frames="$(grep -cE "fps=[0-9]+" "$LOG" || true)"

echo
echo "STEADY-STATE GROWTH   window $((elapsed - SETTLE))s after a ${SETTLE}s settle"
echo "first / last / peak   ${first} / ${last} / ${peak} kB"
echo "net change            $((last - first)) kB"
echo "telemetry lines       ${frames}"

if [[ "$frames" -lt 3 ]]; then
  c_err "the workload produced almost no telemetry, so a flat memory trend says nothing: a stalled engine does not grow either"
  exit 1
fi
