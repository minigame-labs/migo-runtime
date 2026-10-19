#!/usr/bin/env bash
# ============================================================
# Measure how much draw-call batching headroom a workload actually has.
# Location: scripts/measure-draw-batching.sh
#
# `GlBatch` executes its commands in order and merges nothing. Whether that
# costs anything had never been measured — and it is not answerable in the
# abstract, because merging is largely the application's job in WebGL: state can
# change between draws, and whether it does is up to the game.
#
# What makes it answerable is one counter. `adjacent_draws` counts draws issued
# with zero *post-dedup* state changes since the previous draw of the same frame:
# the upper bound on what a batching pass could merge. Post-dedup is the load
# bearing part — a state command the shadow swallowed never reached the driver,
# so draws separated only by those are adjacent as far as the GPU is concerned,
# and it is the GPU's view that decides whether a merge is possible.
#
# Two fixtures bracket the answer rather than guessing at a typical frame:
#
#   draw-batching-sprite    64 draws of the *same* range   -> adjacent, not mergeable
#   draw-batching-walk      64 draws walking one buffer     -> adjacent and mergeable
#   draw-batching-material  a state change before each draw -> neither
#
# The first two differ only in whether the ranges advance, and that is the point:
# adjacency says nothing reached the driver in between, mergeability says the two
# draws could become one *and paint the same pixels*. A workload can be 98% the
# first and 0% the second.
#
# A real game's ratio lands between them. Run it against the game catalogue to
# find out where; that is what the bracket is for.
#
# The counter is validated independently by
# `render_frame_state::adjacency::*` in migo-graphics, which replays
# hand-written draw/state interleavings and checks the score. This script is the
# end-to-end path, which also proves the counter is wired from the render thread
# through to the stats a host can read.
#
# Each fixture also paints flat rgba(51,204,102,255), so a run that stopped
# drawing is caught rather than reported as a superb ratio over zero draws.
# ============================================================
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ENGINE_DIR="$SCRIPT_DIR/../engine"
SECS="${SECS:-3}"
PASS="51,204,102,255"

c_info() { printf '\033[0;36m%s\033[0m\n' "$*"; }
c_err() { printf '\033[0;31m%s\033[0m\n' "$*" >&2; }
c_ok() { printf '\033[0;32m%s\033[0m\n' "$*"; }

source "$SCRIPT_DIR/lib/cargo-target-dir.sh"
CARGO_TARGET="$(cargo_target_dir "$ENGINE_DIR")" || exit 1
PLAYER="$CARGO_TARGET/release/migo-player"
if [[ ! -x "$PLAYER" ]]; then
  c_info "building migo-player"
  bash "$SCRIPT_DIR/dev-test-host.sh" build --release -p migo-player || exit 1
fi

export LD_LIBRARY_PATH="$HOME/.local/lib:/usr/lib/wsl/lib:/usr/lib/x86_64-linux-gnu${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
export RUST_LOG="${RUST_LOG:-info}"

printf '\nDRAW-CALL BATCHING HEADROOM  %ss per fixture\n' "$SECS"

status=0
run_fixture() {
  local fixture="$1" expect_hint="$2"
  local log="/tmp/migo-batching-$fixture.log"
  local png="/tmp/migo-batching-$fixture.png"

  MIGO_PLAYER_PNG="$png" "$PLAYER" "$SCRIPT_DIR/fixtures/$fixture" "$SECS" \
    >"$log" 2>&1 || true

  local line colour distinct
  line="$(grep -oE '\[draw-batching\].*' "$log" | tail -1)"
  read -r colour distinct <<<"$(python3 "$SCRIPT_DIR/lib/dominant_pixel.py" "$png" 2>/dev/null)"
  colour="${colour:-no-capture}"

  printf '  %-24s %s\n' "$fixture" "${line:-no measurement in $log}"
  printf '  %-24s pixel=%s (%s colour(s))  expect %s\n' '' "$colour" "${distinct:-0}" "$expect_hint"

  if [[ -z "$line" ]]; then
    c_err "$fixture produced no [draw-batching] line; the counter is not wired through to stats. See $log"
    status=1
    return
  fi
  if [[ "$colour" != "$PASS" ]]; then
    c_err "$fixture presented $colour, wanted $PASS: it stopped drawing, so its ratio is over the wrong number of draws. See $png"
    status=1
  fi
}

# Adjacent but not mergeable: same range redrawn.
run_fixture draw-batching-sprite 'high adjacent, mergeable=0'
# Adjacent and mergeable: ranges advance through one buffer.
run_fixture draw-batching-walk 'high adjacent, high mergeable'
# Neither: a real state change before every draw.
run_fixture draw-batching-material 'adjacent=0 mergeable=0'

printf '\n'
if [[ "$status" -ne 0 ]]; then
  c_err "[draw-batching] a fixture failed; the numbers above cannot be trusted"
  exit 1
fi
c_ok "[draw-batching] both bounds measured; a real workload's ratio lands between them"
