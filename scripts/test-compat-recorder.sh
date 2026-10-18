#!/usr/bin/env bash
# The compatibility recorder reports what a game reaches for, and the game cannot tell.
#
# tools/compat-recorder/recorder.js is injected ahead of content nobody here wrote, to
# say which APIs it calls, which names it looks for and does not find, and what it asks
# of WebGL. A recorder that changes the game's behaviour does worse than record
# nothing: it reports on a run that would not have happened, and the report is what a
# customer's migration is judged by.
#
# THE DRIFT THIS EXISTS TO CATCH is the recorder's own, and its first draft had two.
# It listened on onUnhandledRejection to report rejections -- and the engine tells
# onError about a rejection only when nobody listens for rejections, so a game that
# reported its failures through onError went quiet the moment it was recorded. And it
# wrapped each API in a fresh function, which carries none of the original's own
# properties: `migo.WebGLRenderingContext.TEXTURE_2D` read undefined, through a
# namespace that mirrors every engine global. Neither shows in a report; both change
# the game under it.
#
# A second pass found three more. A surface's methods got the proxy as their receiver,
# so a surface whose methods or getters read private fields threw a brand error. Content
# that patched an intrinsic after boot -- Object.prototype.toJSON, JSON.stringify,
# WeakMap.prototype.get, an Object.prototype.get that turns every descriptor the
# recorder handled into an invalid one -- made the recorder throw into the game's next
# call. And its own "ready" timer ran before an entry whose dependency awaited, and took
# one of the game's timers; the engine's log has the right moment, and the report reads it.
#
# So each check asserts both halves: the line the recorder writes, and that the
# game's view -- receivers, arguments, identity, new.target, own properties, absent
# names, descriptors, global names, intrinsics it patched -- is what it would be
# without it. The checks run in a Node realm shaped like the engine's;
# scripts/test-compat-recorder-player.sh runs the recorder on the engine itself.
#
# Host-only: node, no device.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TEST="$ROOT/tools/compat-recorder/recorder.test.mjs"

if ! command -v node >/dev/null 2>&1; then
    # Not a pass: an environment without node cannot answer this question.
    echo "FAIL: node is not available, so the compatibility recorder is unverified." >&2
    exit 1
fi

node "$TEST"
