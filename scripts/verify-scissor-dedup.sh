#!/usr/bin/env bash
# ============================================================
# Assert that deduping `glScissor` never clips a draw the game wanted.
# Location: scripts/verify-scissor-dedup.sh
#
# `glScissor` went undeduped for a long time on a stated reason: the engine
# re-points the driver's scissor box behind the state shadow's back.
# `dirty_region::apply_scissor` borrows the box for every partial-damage
# Canvas2D batch, and the DrawingBuffer blit toggles the enable bit around a
# present. A shadow blind to either would report a hit for a call the driver
# needed, leaving every later draw clipped to a box the game never asked for —
# with no GL error and nothing in a log.
#
# The dedup landed once both engine paths were routed through the shadow:
# `ScissorBorrow` carries the pre-borrow state and writes `last_scissor_rect`
# from the same computation that feeds the driver, and the blit only touches the
# enable bit, restoring it from what it read.
#
# Unit tests pin the predicate (`state_tracker::scissor_*`) and the restore
# mapping (`dirty_region::the_reported_box_is_always_the_box_the_driver_holds`,
# including the arm where a disable leaves GL holding the engine's box). What
# they cannot pin is that a *stale* shadow stays impossible end to end, because
# that needs a driver. This does.
#
# The fixture re-asserts an identical scissor box — the exact call a dedup
# swallows — and then widens to the full surface and paints over everything. A
# driver still holding the narrower box leaves red behind, so the failure shows
# up as two colours rather than requiring the gate to know which one is wrong.
# ============================================================
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ENGINE_DIR="$SCRIPT_DIR/../engine"
FIXTURE="scissor-dedup-probe"
SECS="${SECS:-3}"

PASS="51,204,102,255"
CLIPPED="217,26,38,255"

c_info() { printf '\033[0;36m[scissor-dedup] %s\033[0m\n' "$*"; }
c_err() { printf '\033[0;31m%s\033[0m\n' "$*" >&2; }
c_ok() { printf '\033[0;32m[scissor-dedup] %s\033[0m\n' "$*"; }

source "$SCRIPT_DIR/lib/cargo-target-dir.sh"
CARGO_TARGET="$(cargo_target_dir "$ENGINE_DIR")" || exit 1
PLAYER="$CARGO_TARGET/release/migo-player"
if [[ ! -x "$PLAYER" ]]; then
  c_info "building migo-player"
  bash "$SCRIPT_DIR/dev-test-host.sh" build --release -p migo-player || exit 1
fi

export LD_LIBRARY_PATH="$HOME/.local/lib:/usr/lib/wsl/lib:/usr/lib/x86_64-linux-gnu${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
export RUST_LOG="${RUST_LOG:-info}"

log="/tmp/migo-scissor-dedup.log"
png="/tmp/migo-scissor-dedup.png"

c_info "running $FIXTURE for ${SECS}s"
MIGO_PLAYER_PNG="$png" "$PLAYER" "$SCRIPT_DIR/fixtures/$FIXTURE" "$SECS" >"$log" 2>&1 || true

painted="$(grep -oE "painted [0-9]+ frames" "$log" | tail -1 | grep -oE "[0-9]+" || echo 0)"
read -r colour distinct <<<"$(python3 "$SCRIPT_DIR/lib/dominant_pixel.py" "$png" 2>/dev/null)"
colour="${colour:-no-capture}"
distinct="${distinct:-0}"

printf '  frames=%-5s pixel=%s (%s colour(s))\n' "$painted" "$colour" "$distinct"

# Three assertions, because each alone is satisfiable by something other than
# the property: the frame count (a run that painted nothing leaves a blank
# capture), the colour, and the colour *count* — a surface that is half red and
# half green can still report green as dominant.
if [[ "$painted" -lt 2 ]]; then
  c_err "painted $painted frame(s); the fixture needs at least two for its scissor sequence to have run twice. See $log"
  exit 1
fi

if [[ "$distinct" -ne 1 ]]; then
  c_err "the surface holds $distinct colours, not 1: part of it kept the red from the clipped clear, so the re-asserted scissor box did not reach the driver. See $png"
  exit 1
fi

case "$colour" in
  "$PASS")
    c_ok "a re-asserted scissor box reaches the driver ($painted frames, flat $PASS)"
    ;;
  "$CLIPPED")
    c_err "the whole surface is the clipped-clear red: the widened scissor box never reached the driver, so every clear stayed inside the engine's box. See update_scissor in backend/gl/state_tracker.rs."
    exit 1
    ;;
  *)
    c_err "unexpected $colour (wanted $PASS): the fixture did not paint what it intends, so its verdict says nothing. See $png and $log"
    exit 1
    ;;
esac
