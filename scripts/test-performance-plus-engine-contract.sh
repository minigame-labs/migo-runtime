#!/usr/bin/env bash
# The engine's own JavaScript API layer runs on the Performance+ producer, and
# the frames it draws are ones the host admits.
#
# WHY THIS GATE EXISTS. On iOS Performance+ content runs in WebKit's WebContent
# process, so the engine's WebGL, Canvas2D and `migo.*` layer has to run there
# too -- as the same modules the embedded runtime evaluates, not a second
# implementation of them. scripts/gen-performance-plus-engine.py stages those
# modules for a Worker and answers their ops per contracts/runtime/op-boundary.json.
# Three things can silently go wrong, and each is checked:
#
#   1. The staging. A module the engine adds, an `ext:` specifier left
#      unrewritten, a primordials name or `core` member the producer cannot
#      supply, a lane module implementing an op on the wrong lane: the generator
#      refuses each, and the staged modules are then actually loaded.
#   2. The frames. Content calls `migo.createCanvas().getContext("webgl")` and
#      draws two frames through a fake host; the packets that leave must carry the
#      session's identity and exactly the command words the facade encodes.
#   3. The host's side. Those packets go through the Rust ingress and stream
#      validator the external session uses, in the order they were sent.
#
# Host-only: python3, node, cargo. No Apple toolchain.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

fail() { echo "performance-plus engine contract FAILED: $*" >&2; exit 1; }

command -v node >/dev/null 2>&1 || fail "node is not available, so the staged engine was not run"
command -v cargo >/dev/null 2>&1 || fail "cargo is not available, so the host's side was not checked"

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
STAGED="$WORK/resources"
FRAMES="$WORK/frames"

python3 scripts/gen-performance-plus-engine.py --out "$STAGED" --with-producer \
    || fail "the engine could not be staged for the producer"

output="$(node platforms/apple/WebContent/PerformancePlus/test/engine-bundle.test.mjs "$STAGED" "$FRAMES" 2>&1)" \
    || { printf '%s\n' "$output" >&2; fail "the staged engine did not draw the frames it was asked for"; }
printf '%s\n' "$output" | grep -E "wrote [0-9]+ engine frames|PASS" || true

frames="$(find "$FRAMES" -name 'engine-frame-*.bin' | wc -l)"
(( frames >= 2 )) || fail "the node test wrote $frames frames; it writes two"

status=0
rust="$(cd engine && MIGO_ENGINE_FRAME_DIR="$FRAMES" cargo test -p migo-frame-wire \
    --test engine_frames_js_interop -- --ignored --nocapture 2>&1)" || status=$?
if (( status != 0 )); then
    printf '%s\n' "$rust" >&2
    fail "the host's ingress refused frames the engine's facade produced"
fi
printf '%s\n' "$rust" | grep -qE "admitted $frames frames from the engine's WebGL facade" \
    || { printf '%s\n' "$rust" >&2; fail "the Rust check did not report admitting $frames frames; it may not have run"; }

python3 - "$STAGED/engine/manifest.json" <<'PY'
import json, sys
manifest = json.load(open(sys.argv[1]))
parts = [
    f"{lane} {entry['implemented']}/{entry['classified']}"
    for lane, entry in sorted(manifest["ops"].items())
]
print(
    f"performance-plus engine contract: PASS -- {manifest['modules']} engine modules staged and "
    f"run; ops answered by lane: {', '.join(parts)}"
)
PY
