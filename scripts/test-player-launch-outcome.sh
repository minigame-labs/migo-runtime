#!/usr/bin/env bash
# The headless player ends a run whose game did not start, and says why.
#
# It heard nothing from the engine: the Linux platform reported its host notifications
# nowhere, so a game whose entry threw waited out the 30-second present ceiling and the run
# after it -- 36 seconds -- and ended with "nothing was presented during the run", the reason
# left in the engine's log. The launch's failure now reaches the player as
# `notify_game_failed`, and two runs hold it to that:
#
#   * An entry that throws: the run fails within seconds, saying "the game did not start"
#     and the engine's reason, and the compatibility report blocks on it.
#   * A loading screen, then a rejection: the game presents a frame and its top-level await
#     rejects before the run ends. The frame must not read as a game that ran -- which it did
#     whenever the rejection landed after the player last looked, so the player reads the
#     launch's outcome once the session is joined, when nothing more can be reported.
#
# Host-only: builds and runs the Linux player headless (EGL), no device.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PLAYER="$ROOT/engine/target/debug/migo-player"
bash "$ROOT/scripts/dev-test-host.sh" build -p migo-player --offline

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
failures=0
check() {
    if eval "$2"; then
        echo "  ok   $1"
    else
        failures=$((failures + 1))
        echo "  FAIL $1"
    fi
}

bundle() {
    mkdir -p "$WORK/$1"
    printf '{"deviceOrientation":"portrait"}\n' > "$WORK/$1/game.json"
    cat > "$WORK/$1/game.js"
}

# Seconds the player took, its exit status, and its log without colour.
play() {
    local name="$1" started ended status=0
    started=$(date +%s)
    MIGO_PLAYER_PNG="$WORK/$name.png" "$PLAYER" "$WORK/$name" 4 > "$WORK/$name.log" 2>&1 || status=$?
    ended=$(date +%s)
    sed -i 's/\x1b\[[0-9;]*m//g' "$WORK/$name.log"
    echo "$((ended - started)) $status"
}

bundle throws <<'JS'
throw new Error("the level failed to load");
JS
read -r secs status < <(play throws)
check "an entry that throws fails the run" '[[ $status -ne 0 ]]'
check "within seconds, not after the 30 s present ceiling (took ${secs}s)" '[[ $secs -lt 15 ]]'
check "saying the game did not start, and the engine's reason" \
    'grep -q "player failed: the game did not start: module evaluation failed: Error: the level failed to load" "$WORK/throws.log"'
check "and not that nothing was presented" '! grep -q "nothing was presented during the run" "$WORK/throws.log"'

# A frame on screen first, so the run has a picture to be fooled by; the rejection comes
# inside the run, after the first present.
bundle loads <<'JS'
const canvas = migo.createCanvas();
const gl = canvas.getContext("webgl");
function paint() {
    gl.clearColor(0.2, 0.8, 0.4, 1);
    gl.clear(gl.COLOR_BUFFER_BIT);
    requestAnimationFrame(paint);
}
requestAnimationFrame(paint);
await new Promise((_, reject) => setTimeout(() => reject(new Error("the assets never arrived")), 2500));
JS
read -r secs status < <(play loads)
check "a game that presented a loading screen and then failed to start fails the run" '[[ $status -ne 0 ]]'
check "saying it did not start" \
    'grep -q "player failed: the game did not start: .*the assets never arrived" "$WORK/loads.log"'
# The rejection comes 2.5 s in; only a run that presented first goes on to its 4 s window.
check "after it presented and its run went on (took ${secs}s)" '[[ $secs -ge 4 ]]'

report="$WORK/report.json"
bash "$ROOT/scripts/compat-report.sh" "$WORK/throws" --secs 4 --json "$report" --out "$WORK/report.md" \
    > /dev/null 2>&1 || true
check "the compatibility report blocks on the game that did not start" \
    'python3 -c "import json,sys; r=json.load(open(sys.argv[1])); sys.exit(0 if any(b.get(\"label\") == \"the game did not start\" for b in r[\"blocking\"]) else 1)" "$report"'

if [[ $failures -eq 0 ]]; then
    echo "PASS: a game that did not start ends the run with the engine's reason, loading screen or not"
else
    echo "FAIL: $failures check(s) failed; logs: $WORK (kept)"
    trap - EXIT
    exit 1
fi
