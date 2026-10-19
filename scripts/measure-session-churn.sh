#!/usr/bin/env bash
# ============================================================
# Measure what a session leaves behind when a host runs one game after another.
# Location: scripts/measure-session-churn.sh
#
# A game centre opens a game, closes it and opens the next, all in one process.
# measure-steady-state-growth.sh watches one long session and cannot see that:
# whatever a session still holds once it is shut down -- an isolate, a thread, a
# texture, a library it loaded -- grows the process once per game. This runs the
# player's `--sessions N` mode, which shuts each session down, joins it, and reads
# the C heap's live bytes and the resident set once glibc has handed back what it
# holds free (`left_behind` in engine/tools/player/src/main.rs), then fits a line
# through each over the sessions after the first SETTLE.
#
# What it was built to find, and found: every session opened the system EGL and
# dropped it at shutdown, so the whole GL driver was unloaded and loaded again
# between games, and each cycle leaked about 4 kB. The resident set could not
# show it -- glibc's arenas grew that by hundreds of MB with nothing alive -- and
# nothing ran sessions back to back.
#
# Two-sided: the player fails the run when a session presents nothing, because a
# session that never started leaves nothing behind either.
#
# Thresholds, in kB a session, from 30-session runs of the debug player on
# llvmpipe (bunnymark / scripts/fixtures/bypass-probe):
#   heap      0.4-1.2 / 0.9 with nothing of Migo's held -- what is left is V8's own
#             region and page bookkeeping; 5.1 / 4.8 with the EGL library dropped
#             at every shutdown; 3,432 with each session's JS runtime held.
#             MAX_HEAP_KB=3.
#   resident  -19 to 73 / 4 to 14 with nothing held: one 5 MB reading, which a
#             driver or the kernel can produce at any session, moves the fit by
#             that much. 20,564 with each JS runtime held. MAX_RESIDENT_KB=256.
#
# Usage:
#   scripts/measure-session-churn.sh [GAME_BUNDLE_DIR] [SESSIONS] [SECS]
#
# Requires the host player (scripts/dev-run-player.sh builds it) and its EGL/V8
# environment.
# ============================================================
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
ENGINE_DIR="$REPO_ROOT/engine"

GAME_DIR="${1:-$REPO_ROOT/../migo-bench/shells/migo-shell/app/src/main/assets/game}"
SESSIONS="${2:-30}"
SECS="${3:-1}"
# Left out of the fit: the first sessions fill pools that later ones reuse -- V8's
# pages, glibc's thread-stack cache, the shader cache -- and are not churn.
SETTLE="${SETTLE:-5}"
MAX_HEAP_KB="${MAX_HEAP_KB:-3}"
MAX_RESIDENT_KB="${MAX_RESIDENT_KB:-256}"
LOG=/tmp/migo-churn-player.log

c_info() { echo -e "\033[0;36m[churn] $*\033[0m"; }
c_err() { echo -e "\033[0;31m[churn] $*\033[0m" >&2; }

source "$SCRIPT_DIR/lib/cargo-target-dir.sh"
CARGO_TARGET="$(cargo_target_dir "$ENGINE_DIR")" || exit 1
PLAYER="$CARGO_TARGET/debug/migo-player"
[[ -x "$PLAYER" ]] || {
  c_err "player not built: $PLAYER (run scripts/dev-run-player.sh once)"
  exit 1
}
[[ -d "$GAME_DIR" ]] || { c_err "no such bundle: $GAME_DIR"; exit 1; }
[[ "$SESSIONS" -ge $((SETTLE + 10)) ]] || {
  c_err "$SESSIONS sessions leave fewer than 10 to fit after a settle of $SETTLE"
  exit 1
}

export LD_LIBRARY_PATH="$HOME/.local/lib:/usr/lib/wsl/lib:/usr/lib/x86_64-linux-gnu${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
export MIGO_PLAYER_PNG=/tmp/migo-churn-frame.png

c_info "$SESSIONS sessions of ${SECS}s each, the first $SETTLE left out of the fit"
if ! "$PLAYER" "$GAME_DIR" "$SECS" --sessions "$SESSIONS" >"$LOG" 2>&1; then
  c_err "the player failed; see $LOG"
  grep -E 'ERROR|presented nothing' "$LOG" | tail -3 >&2 || true
  exit 1
fi

samples="$(sed -nE 's/.*session ([0-9]+)\/[0-9]+: heap ([0-9]+) kB in use, resident ([0-9]+) kB, after shutdown.*/\1 \2 \3/p' "$LOG")"
count="$(wc -l <<<"$samples")"
[[ "$count" -eq "$SESSIONS" ]] || {
  c_err "$count of $SESSIONS sessions reported; see $LOG"
  exit 1
}

echo
echo "session heap_kb resident_kb"
awk '{printf "%7s %7s %11s\n", $1, $2, $3}' <<<"$samples"

# Least-squares slope through the sessions after the settle: robust to one noisy
# reading, which first-to-last is not.
read -r heap_slope resident_slope < <(awk -v settle="$SETTLE" '
  $1 > settle { n++; sx += $1; sxx += $1 * $1; sh += $2; sxh += $1 * $2; sr += $3; sxr += $1 * $3 }
  END {
    d = n * sxx - sx * sx
    printf "%.2f %.2f\n", (n * sxh - sx * sh) / d, (n * sxr - sx * sr) / d
  }' <<<"$samples")

echo
echo "SESSION CHURN   $((SESSIONS - SETTLE)) sessions fitted"
echo "heap            ${heap_slope} kB a session   (at most ${MAX_HEAP_KB})"
echo "resident        ${resident_slope} kB a session   (at most ${MAX_RESIDENT_KB})"

failed=0
awk -v s="$heap_slope" -v m="$MAX_HEAP_KB" 'BEGIN { exit !(s > m) }' && {
  c_err "each session leaves ${heap_slope} kB of C heap behind"
  failed=1
}
awk -v s="$resident_slope" -v m="$MAX_RESIDENT_KB" 'BEGIN { exit !(s > m) }' && {
  c_err "each session leaves ${resident_slope} kB resident behind"
  failed=1
}
exit "$failed"
