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
#   4. The globals the engine takes. The engine installs content's mini-game
#      globals over the Worker's, `postMessage` among them, so a producer that
#      reaches the page through the global after the engine loads is talking to
#      the open data context. That happened (2026-09-17: the iOS acceptance test
#      failed asking for an offscreen canvas), so the producer's modules may use
#      the Worker's `postMessage` only through the reference taken before the
#      engine is imported.
#   5. The resource calls. fixtures/webgl-resource-calls.js makes every WebGL
#      resource call the producer answers on its stream lane, through the
#      engine's WebGL 2 facade, on the producer and in the embedded V8 runtime.
#      The producer's records must decode to exactly the commands the in-process
#      ops build, and record the same errors
#      (engine/crates/runtime-v8/src/rendering/webgl/resource_parity.rs).
#
# Host-only: python3, node, cargo (with the host V8 the runtime's own tests use).
# No Apple toolchain.
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

PRODUCER="platforms/apple/WebContent/PerformancePlus/src"
# Worker-side modules only: page-entry.mjs runs in the page, where no engine is.
global_posts="$(grep -nE '(^|[^.A-Za-z_])(self|globalThis)?\.?postMessage\(' "$PRODUCER"/*.mjs \
    | grep -v "^$PRODUCER/page-entry.mjs:" \
    | grep -vE '^[^:]+:[0-9]+:\s*(//|\*)' || true)"
if [[ -n "$global_posts" ]]; then
    printf '%s\n' "$global_posts" >&2
    fail "the producer calls the global postMessage, which is the engine's once it is loaded; use the reference producer-worker.mjs takes first"
fi
grep -q 'const postToPage = self.postMessage.bind(self);' "$PRODUCER/producer-worker.mjs" \
    || fail "producer-worker.mjs no longer takes the Worker's postMessage before the engine can replace it"
awk '/const postToPage = self.postMessage.bind\(self\);/ { taken = NR } /import\("\.\/engine\/boot\.mjs"\)/ { boot = NR } END { exit !(taken && boot && taken < boot) }' \
    "$PRODUCER/producer-worker.mjs" \
    || fail "producer-worker.mjs imports the engine before it takes the Worker's postMessage"

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

PARITY="$WORK/parity"
node platforms/apple/WebContent/PerformancePlus/test/engine-resource-parity.mjs "$STAGED" "$PARITY" \
    || fail "the resource calls did not run on the producer"
status=0
parity="$(cd engine && MIGO_RESOURCE_PARITY_DIR="$PARITY" cargo test -p migo-runtime-v8 --lib \
    resource_parity -- --ignored --nocapture 2>&1)" || status=$?
if (( status != 0 )); then
    printf '%s\n' "$parity" >&2
    fail "the producer's resource records do not decode to the commands the in-process ops build"
fi
printf '%s\n' "$parity" | grep -qE '[0-9]+ commands and [0-9]+ errors agree' \
    || { printf '%s\n' "$parity" >&2; fail "the resource parity check did not report agreeing; it may not have run"; }
printf '%s\n' "$parity" | grep -E 'commands and [0-9]+ errors agree'

# The same question for the Canvas2D text records, which is where the two
# implementations are most likely to drift: a font shorthand parsed on both
# sides, a `maxWidth` that is usually infinite, and alignment keywords that are
# numbers on the wire.
TEXT_PARITY="$WORK/text-parity"
node platforms/apple/WebContent/PerformancePlus/test/engine-resource-parity.mjs "$STAGED" "$TEXT_PARITY" \
    fixtures/canvas2d-text-calls.js \
    || fail "the Canvas2D text calls did not run on the producer"
status=0
text="$(cd engine && MIGO_CANVAS2D_PARITY_DIR="$TEXT_PARITY" cargo test -p migo-runtime-v8 --lib \
    canvas2d_parity -- --ignored --nocapture 2>&1)" || status=$?
if (( status != 0 )); then
    printf '%s\n' "$text" >&2
    fail "the producer's text records do not decode to the commands the in-process ops build"
fi
printf '%s\n' "$text" | grep -qE '[0-9]+ Canvas2D commands agree' \
    || { printf '%s\n' "$text" >&2; fail "the text parity check did not report agreeing; it may not have run"; }
printf '%s\n' "$text" | grep -E '[0-9]+ Canvas2D commands agree'

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
