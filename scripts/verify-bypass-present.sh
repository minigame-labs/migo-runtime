#!/usr/bin/env bash
# ============================================================
# Assert that what the content drew reaches the window, and nothing else does.
# Location: scripts/verify-bypass-present.sh
#
# Specification Section 7.3 requires no redundant presentation copy, and Section
# 6.4's GL-state contract requires the engine not to change bindings behind the
# content's back. Both were unmet, and neither showed up in any test, because
# "the blit needs a live GL context" was read as "this is device-blocked". The
# blit is unobservable here; the *presented frame* is not.
#
# Every fixture clears flat, so the captured frame is a verdict. Each run asserts
# three things, because each alone is satisfiable by something other than the
# property:
#
#   * the frame count — 240 frames landing in the wrong buffer is the same count
#     as 240 frames reaching the window;
#   * the distinct sampled colour count — a frame presented through a partial
#     damage region carries wrong pixels in part of the surface while the stale
#     majority still reads as expected;
#   * a first frame in a different colour — "the screen is the expected colour"
#     is an absence claim that an engine which presented once and then stopped
#     satisfies forever, with the JS loop still running at 60 fps. Blue means
#     frozen, red means something the content asked for did not happen.
#
# Every one of those three was added because a mutant walked past the gate
# without it.
#
# Usage:
#   scripts/verify-bypass-present.sh [SECS]
#
# Requires the host player and its EGL/V8 environment; builds it every run.
# ============================================================
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
ENGINE_DIR="$REPO_ROOT/engine"
SECS="${1:-3}"

# The colour both fixtures clear to: 0.2/0.8/0.4 opaque, distinct per channel so
# a wrong pixel says which channel survived rather than only that one did not.
EXPECT="51,204,102,255"

c_info() { echo -e "\033[0;36m[present] $*\033[0m"; }
c_ok() { echo -e "\033[0;32m[present] $*\033[0m"; }
c_err() { echo -e "\033[0;31m[present] $*\033[0m" >&2; }

PLAYER="$ENGINE_DIR/target/debug/migo-player"
# Always build, never "build if missing": a mutation run leaves a binary compiled
# from the mutant sitting next to a restored tree, and WSL2 preserves mtime, so
# an if-missing gate happily scores the mutant as the fix.
#
# Through dev-test-host.sh rather than a bare cargo: it is where this repository
# establishes the host V8 archive, the system clang and the Khronos headers the
# Skia-linked crates need, so the gate runs for a caller who has exported nothing.
c_info "building the host player"
bash "$SCRIPT_DIR/dev-test-host.sh" build -p migo-player --offline

export LD_LIBRARY_PATH="$HOME/.local/lib:/usr/lib/wsl/lib:/usr/lib/x86_64-linux-gnu${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
export RUST_LOG="${RUST_LOG:-info}"

failures=0

run_probe() {
  local name="$1" expect_bypass="$2"
  local log="/tmp/migo-present-$name.log"
  local png="/tmp/migo-present-$name.png"

  MIGO_PLAYER_PNG="$png" "$PLAYER" "$SCRIPT_DIR/fixtures/$name" "$SECS" >"$log" 2>&1 || true

  local painted
  painted="$(grep -oE "painted [0-9]+ frames" "$log" | tail -1 | grep -oE "[0-9]+" || echo 0)"

  # Which path actually ran, from the engine's own transition log rather than
  # from the fixture's intent -- a fixture that failed to create its offscreen
  # canvas would otherwise be scored against the wrong path.
  #
  # The reading is the last transition *before the final painted frame*, not a
  # count of transitions across the whole log. Counting made the verdict depend
  # on how many times the path changed rather than on what it was while the
  # frames were drawn: a canvas destroyed mid-run left equal-and-then-unequal
  # counts and read as bypass, and a transition after the last frame (teardown
  # destroying a canvas) would have decided the verdict outright. Defaults to
  # false because that is where bypass starts.
  #
  # Keyed on the state each line names *first* -- the one it left -- because that
  # keeps the pattern ASCII. The arrow in the engine's message is a three-byte
  # UTF-8 character and `mawk`, which is Ubuntu's default awk, matches `.`
  # against a single byte: a pattern spanning the arrow matches nothing there, so
  # every probe would read as the blit path and `bypass-probe` would fail on a
  # machine whose engine was fine. gawk matches it, which is why the first
  # version of this ran green locally.
  local bypass
  bypass="$(awk '
    /DrawingBuffer bypass: false/ { latest="true" }
    /DrawingBuffer bypass: true/  { latest="false" }
    /painted [0-9]+ frames/ { at_frame=latest }
    END { print (at_frame == "" ? "false" : at_frame) }
  ' "$log")"

  local colour distinct
  read -r colour distinct <<<"$(python3 "$SCRIPT_DIR/lib/dominant_pixel.py" "$png" 2>/dev/null)"
  colour="${colour:-no-capture}"
  distinct="${distinct:-0}"

  printf '  %-19s path=%-19s frames=%-5s pixel=%s (%s colour(s))\n' \
    "$name" "$([[ $bypass == true ]] && echo bypass || echo drawing-buffer-blit)" \
    "$painted" "$colour" "$distinct"

  if [[ "$bypass" != "$expect_bypass" ]]; then
    c_err "$name took the wrong presentation path (bypass=$bypass, wanted $expect_bypass); see $log"
    failures=$((failures + 1))
    return
  fi
  if [[ "$painted" -lt 1 ]]; then
    c_err "$name never painted, so its pixel says nothing about presentation; see $log"
    # The log is on the machine that ran the probe, which on CI is a runner
    # nobody can open afterwards. The reason a player painted nothing is at the
    # end of it -- a failed EGL bring-up, a panic, a content error -- so it is
    # printed here rather than named. Measured 2026-09-17: every probe painted
    # zero frames on the hosted runner and seven on a workstation, and the gate
    # said only "see /tmp/...".
    tail -n 40 "$log" | sed 's/^/    | /' >&2
    failures=$((failures + 1))
    return
  fi
  if [[ "$colour" != "$EXPECT" ]]; then
    c_err "$name painted $painted frames but presented $colour, wanted $EXPECT: the frames went somewhere that is not the window"
    failures=$((failures + 1))
    return
  fi
  # Every fixture here clears to one flat colour, so anything else on the surface
  # is something that reached the window and should not have. Asserting only the
  # dominant colour made this an always-green gate against a partial present: a
  # frame blitted through a partial damage region carries the wrong pixels in part
  # of the surface while the stale majority still reads as expected. A mutant
  # walked past exactly that.
  if [[ "$distinct" != "1" ]]; then
    c_err "$name presented $distinct distinct colours where the fixture draws one: part of the surface came from somewhere else; see $png"
    failures=$((failures + 1))
  fi
}

echo
echo "PRESENTATION PATHS  ${SECS}s per probe, expecting rgba($EXPECT)"
run_probe blit-probe false
run_probe bypass-probe true
# Same expected colour, a different question: not "did the frame reach the window"
# but "did a frame that was never meant for the window stay off it". rtt-probe
# clears its render target red after a canvas switch, so red on screen means the
# content's own framebuffer binding did not survive the engine re-pointing the
# driver, and its re-bind was deduped away. It fits this gate because a wrong
# answer is the same shape: a colour that is not the one the content put on the
# window.
run_probe rtt-probe false
# Its sibling, and the only probe that reaches the post-swap restore. Found by
# mutation: deleting that site's shadow record left rtt-probe green, because
# rtt-probe's first framebuffer call each frame binds `null` and is issued however
# stale the shadow is. This one makes the frame's first call the content's own FBO.
run_probe rtt-boundary-probe false
# A boundary control, kept because the hypothesis it was built to prove turned out
# false. It asks whether an image load disturbs the content's texture binding, and
# the answer is no — because ordinary uploads run on the upload thread, which owns a
# GL context of its own. That is an architectural property worth pinning: an
# "optimisation" that moved uploads onto the render thread to avoid a context switch
# would silently reintroduce the defect 0.60 fixed on the framebuffer binding.
run_probe upload-shadow-probe false
# Two canvases, both drawn to, so the frame really does switch EGL contexts — twice
# per frame, and it *ends* on the offscreen pbuffer so presentation has to bring the
# window back unaided. bypass-probe and blit-probe differ by whether a second canvas
# exists and neither ever draws to one, so until now nothing here switched contexts
# inside a frame at all. Its offscreen clear is red: an empty capture means the
# onscreen clear went somewhere that is not the window, and a red one means an
# offscreen draw arrived there.
run_probe bypass-multi-probe false
# The end-to-end guard over the rewritten GL-object deletion path. An offscreen canvas
# frees a pool of framebuffers while the onscreen canvas keeps a render target of its
# own, which is the shape that used to issue those frees against the onscreen
# context's namespace.
#
# What it cannot see, stated because it bounds the claim: on this host the wrong-context
# delete is invisible. Mesa numbers container objects from one counter for the whole
# share group, so an offscreen framebuffer never holds a name the onscreen context has
# live, and the only consequence here is the leak — the object is not freed and its
# bookkeeping is already gone. A driver that numbers per context, which is what mobile
# GPUs do, collides an offscreen canvas's first framebuffer with the onscreen
# DrawingBuffer. The classification itself is gated by the unit tests in
# `canvas/manager/gl_object.rs`; this probe gates the eleven rewritten call sites.
run_probe fbo-owner-probe false
echo

if [[ "$failures" -gt 0 ]]; then
  c_err "$failures probe(s) did not present what the content drew"
  exit 1
fi
c_ok "every presentation path reaches the window, and nothing else does"
