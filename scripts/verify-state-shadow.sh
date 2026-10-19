#!/usr/bin/env bash
# ============================================================
# Assert that the GL state shadows dedup only what is actually redundant.
# Location: scripts/verify-state-shadow.sh
#
# The engine skips a driver call when its shadow says the state already matches.
# Three of those shadows were containers whose key space the spec fixes, and were
# re-shaped to match it: TEXTURE_2D bindings (hash map keyed by the
# `GL_TEXTURE0 + i` enum -> array indexed by `i`), `glEnable`/`glDisable` (two
# hash sets -> two bitmasks), and vertex-attribute pointers/enables/divisors
# (three containers keyed `(vao, index)` -> one record per VAO, attributes
# indexed directly).
#
# The unit tests pin the dedup predicates. What they cannot pin is that a wrong
# predicate stays invisible: a shadow that over-dedups skips a call the driver
# needed, and the result is wrong pixels with no GL error and nothing in a log.
# So the fixture gives each of the three a step whose outcome is the colour.
#
# Both failure modes were reproduced before this gate was written, by breaking
# the implementation and capturing the frame:
#
#   * `TextureUnitShadow::index` collapsed onto slot 0 -> rgba(0,0,0,255)
#   * `VertexArrayShadow::enable` collapsed onto bit 0 -> rgba(0,0,0,255)
#
# and the intact tree presents rgba(51,204,102,255) over 240 frames. Nothing else
# about those runs differed.
#
# Three assertions, because each alone is satisfiable by something other than the
# property: the frame count (a run that painted nothing leaves the previous
# frame), the colour (distinguishing all four outcomes the fixture can produce),
# and the distinct sampled colour count (a frame presented through a partial
# damage region carries wrong pixels in only part of the surface).
#
# Usage:
#   scripts/verify-state-shadow.sh [SECS]
#
# Requires the host player and its EGL/V8 environment; builds it every run.
# ============================================================
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
ENGINE_DIR="$REPO_ROOT/engine"
SECS="${1:-3}"
FIXTURE="state-shadow-probe"

PASS="51,204,102,255"     # every shadow deduped only what was redundant
LOST_BINDING="0,0,0,255"  # unit 3 unbound, or attribute 2 left disabled
NO_WRITE="217,26,38,255"  # the draw wrote nothing; the blend disable was deduped
FROZEN="26,77,230,255"    # frame one still on the surface

c_info() { echo -e "\033[0;36m[state-shadow] $*\033[0m"; }
c_ok() { echo -e "\033[0;32m[state-shadow] $*\033[0m"; }
c_err() { echo -e "\033[0;31m[state-shadow] $*\033[0m" >&2; }

source "$SCRIPT_DIR/lib/cargo-target-dir.sh"
CARGO_TARGET="$(cargo_target_dir "$ENGINE_DIR")" || exit 1
PLAYER="$CARGO_TARGET/release/migo-player"

# Always build, never "build if missing": a mutation run leaves a binary compiled
# from the mutant sitting next to a restored tree, and WSL2 preserves mtime, so
# an if-missing gate happily scores the mutant as the fix.
#
# Through dev-test-host.sh rather than a bare cargo, matching
# verify-bypass-present.sh: it is where this repository establishes the host V8
# archive, the system clang and the Khronos headers the Skia-linked crates need,
# so the gate runs for a caller who has exported nothing.
c_info "building the host player"
bash "$SCRIPT_DIR/dev-test-host.sh" build --release -p migo-player

export LD_LIBRARY_PATH="$HOME/.local/lib:/usr/lib/wsl/lib:/usr/lib/x86_64-linux-gnu${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
export RUST_LOG="${RUST_LOG:-info}"

log="/tmp/migo-state-shadow.log"
png="/tmp/migo-state-shadow.png"

c_info "running $FIXTURE for ${SECS}s"
MIGO_PLAYER_PNG="$png" "$PLAYER" "$SCRIPT_DIR/fixtures/$FIXTURE" "$SECS" >"$log" 2>&1 || true

painted="$(grep -oE "painted [0-9]+ frames" "$log" | tail -1 | grep -oE "[0-9]+" || echo 0)"
read -r colour distinct <<<"$(python3 "$SCRIPT_DIR/lib/dominant_pixel.py" "$png" 2>/dev/null)"
colour="${colour:-no-capture}"
distinct="${distinct:-0}"

printf '  frames=%-5s pixel=%s (%s colour(s))\n' "$painted" "$colour" "$distinct"

# The fixture skips its draw on frame one, so a run that painted only that frame
# exercised none of the shadows.
if [[ "$painted" -lt 2 ]]; then
  c_err "painted $painted frame(s); the fixture needs at least two to draw once, so its pixel says nothing. See $log"
  exit 1
fi

case "$colour" in
  "$PASS") ;;
  "$LOST_BINDING")
    c_err "a shadow over-deduped: either the texture bound to unit 3 was skipped because the shadow forgot which unit it was talking about, or attribute 2's enable was skipped because the shadow mis-indexed attributes. See TextureUnitShadow and VertexArrayShadow in canvas/manager/types.rs."
    exit 1
    ;;
  "$NO_WRITE")
    c_err "the draw wrote nothing: the glDisable(BLEND) was deduped away, so blending stayed on and blendFunc(ZERO, ONE) discarded every fragment. See CapabilityShadow in canvas/manager/types.rs."
    exit 1
    ;;
  "$FROZEN")
    c_err "frame one is still on the surface: presentation stopped, and a green verdict here would have been vacuous. See $log"
    exit 1
    ;;
  *)
    c_err "unexpected dominant pixel $colour; expected one of $PASS / $LOST_BINDING / $NO_WRITE / $FROZEN. See $log and $png"
    exit 1
    ;;
esac

if [[ "$distinct" -ne 1 ]]; then
  c_err "the surface carries $distinct distinct colours; the fixture paints one flat colour, so anything else means part of the frame did not come from this draw. See $png"
  exit 1
fi

c_ok "the GL state shadows dedup only what is redundant ($painted frames, flat $colour)"
