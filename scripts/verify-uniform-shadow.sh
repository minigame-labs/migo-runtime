#!/usr/bin/env bash
# ============================================================
# Assert that a re-linked program's uniform reaches the driver again.
# Location: scripts/verify-uniform-shadow.sh
#
# The engine dedups `glUniform*` per `(program, location)`. A uniform's value is
# state of the program object (GLES 3.0 §2.11.6), so `glUseProgram` cannot
# disturb it — but a successful `glLinkProgram` gives the program fresh uniform
# storage and initialises it, discarding everything the driver held. Leave the
# shadow in place across that and the content's next upload of an *unchanged*
# value is deduped against a driver holding zero: the uniform silently keeps its
# initial value and the draw paints with it. No GL error, no log line.
#
# Why this needs a presented frame rather than a unit test: the unit tests in
# `state_tracker.rs` assert the dedup *predicate*, which is the thing that was
# wrong — so they were all green while the defect was live. The verdict that
# cannot be satisfied by a wrong predicate is the pixel.
#
# Three assertions, because each alone is satisfiable by something other than
# the property:
#
#   * the frame count — a run that painted nothing leaves whatever the previous
#     frame was on the surface;
#   * the distinct sampled colour count — a frame presented through a partial
#     damage region carries wrong pixels in part of the surface while the stale
#     majority still reads as expected;
#   * the colour itself, distinguishing all four outcomes the fixture can
#     produce, so a failure says which one happened.
#
# Measured against the defect: with `state_tracker::invalidate_program_uniforms`
# stubbed to a no-op, this fixture presents rgba(0,0,0,0) over 180 frames. With
# the invalidation in place it presents rgba(51,204,102,255) over 180 frames.
# Nothing else about the run differs, which is what makes the gate a gate.
#
# Usage:
#   scripts/verify-uniform-shadow.sh [SECS]
#
# Requires the host player and its EGL/V8 environment; builds it every run.
# ============================================================
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
ENGINE_DIR="$REPO_ROOT/engine"
SECS="${1:-3}"
FIXTURE="uniform-shadow-probe"

# The fixture's four outcomes. Named so a failure reports the diagnosis rather
# than a triple of numbers.
PASS="51,204,102,255"    # the upload reached the driver
DEDUPED="0,0,0,0"        # u_color left at its initial value
NO_DRAW="217,26,38,255"  # the clear survived; the draw never landed
FROZEN="26,77,230,255"   # frame one still on the surface

c_info() { echo -e "\033[0;36m[uniform-shadow] $*\033[0m"; }
c_ok() { echo -e "\033[0;32m[uniform-shadow] $*\033[0m"; }
c_err() { echo -e "\033[0;31m[uniform-shadow] $*\033[0m" >&2; }

source "$SCRIPT_DIR/lib/cargo-target-dir.sh"
CARGO_TARGET="$(cargo_target_dir "$ENGINE_DIR")" || exit 1
PLAYER="$CARGO_TARGET/release/migo-player"

# Always build, never "build if missing": a mutation run leaves a binary
# compiled from the mutant sitting next to a restored tree, and WSL2 preserves
# mtime, so an if-missing gate happily scores the mutant as the fix.
#
# Through dev-test-host.sh rather than a bare cargo, matching
# verify-bypass-present.sh: it is where this repository establishes the host V8
# archive, the system clang and the Khronos headers the Skia-linked crates need,
# so the gate runs for a caller who has exported nothing.
c_info "building the host player"
bash "$SCRIPT_DIR/dev-test-host.sh" build --release -p migo-player

export LD_LIBRARY_PATH="$HOME/.local/lib:/usr/lib/wsl/lib:/usr/lib/x86_64-linux-gnu${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
export RUST_LOG="${RUST_LOG:-info}"

log="/tmp/migo-uniform-shadow.log"
png="/tmp/migo-uniform-shadow.png"

c_info "running $FIXTURE for ${SECS}s"
MIGO_PLAYER_PNG="$png" "$PLAYER" "$SCRIPT_DIR/fixtures/$FIXTURE" "$SECS" >"$log" 2>&1 || true

painted="$(grep -oE "painted [0-9]+ frames" "$log" | tail -1 | grep -oE "[0-9]+" || echo 0)"
read -r colour distinct <<<"$(python3 "$SCRIPT_DIR/lib/dominant_pixel.py" "$png" 2>/dev/null)"
colour="${colour:-no-capture}"
distinct="${distinct:-0}"

printf '  frames=%-5s pixel=%s (%s colour(s))\n' "$painted" "$colour" "$distinct"

# The fixture skips its draw on frame one, so a run that painted only that frame
# says nothing about the uniform at all.
if [[ "$painted" -lt 2 ]]; then
  c_err "painted $painted frame(s); the fixture needs at least two to re-link once, so its pixel says nothing. See $log"
  exit 1
fi

case "$colour" in
  "$PASS") ;;
  "$DEDUPED")
    c_err "the re-linked program's uniform never reached the driver: the upload of an unchanged value was deduped against a driver that had just reset it, so the draw painted with zero. See state_tracker::invalidate_program_uniforms."
    exit 1
    ;;
  "$NO_DRAW")
    c_err "the draw never landed, so this run says nothing about the uniform shadow. See $log"
    exit 1
    ;;
  "$FROZEN")
    c_err "frame one is still on the surface: presentation stopped, and a green verdict here would have been vacuous. See $log"
    exit 1
    ;;
  *)
    c_err "unexpected dominant pixel $colour; expected one of $PASS / $DEDUPED / $NO_DRAW / $FROZEN. See $log and $png"
    exit 1
    ;;
esac

if [[ "$distinct" -ne 1 ]]; then
  c_err "the surface carries $distinct distinct colours; the fixture paints one flat colour, so anything else means part of the frame did not come from this draw. See $png"
  exit 1
fi

c_ok "a re-linked program's uniform reaches the driver ($painted frames, flat $colour)"
