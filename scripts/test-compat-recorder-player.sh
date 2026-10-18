#!/usr/bin/env bash
# The compatibility recorder on the engine: what it reports, and a game that still runs.
#
# scripts/test-compat-recorder.sh holds the recorder to its promise in a Node realm
# shaped like the engine's. Three things a shape cannot answer are answered here:
#
#   * The real `migo` -- some 450 functions mirrored from the engine's globals -- and
#     the real WebGL facade, proxied and still working. The fixture draws, and the
#     player fails a run that presents nothing, so a recorder that broke the game
#     fails this gate rather than reporting on a game that never ran.
#   * When "ready" is. The recorder reads it as a timer set before the entry runs,
#     which is only right if the host does not turn the event loop until the entry's
#     synchronous evaluation is over. That is the host's ordering, not the recorder's
#     (runtime-v8 keeps polling the event loop while a module settles), so it is read
#     off the engine: "ready" must follow the entry's last line and its 150 ms of work.
#     The obvious alternative is wrong, measured: a microtask queued in the prelude
#     runs before the entry does, and reported "ready" at 4.8 ms into 150 ms of work.
#   * That off is nothing. The same fixture without the prelude writes no marker line:
#     the recorder exists only where a driver injects it.
#
# Host-only: builds and runs the Linux player headless (EGL), no device.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
FIXTURE="$ROOT/tools/compat-recorder/fixture"
RECORDER="$ROOT/tools/compat-recorder/recorder.js"
PLAYER="$ROOT/engine/target/debug/migo-player"
SECS="${1:-3}"

bash "$ROOT/scripts/dev-test-host.sh" build -p migo-player --offline

export LD_LIBRARY_PATH="$HOME/.local/lib:/usr/lib/wsl/lib:/usr/lib/x86_64-linux-gnu${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
export RUST_LOG="${RUST_LOG:-info}"

run() {
    local log="$1"
    shift
    rm -f "$log"
    # No window server, as in scripts/verify-bypass-present.sh: an inherited DISPLAY
    # makes the offscreen display resolve to X11.
    if ! env -u DISPLAY -u WAYLAND_DISPLAY "$PLAYER" "$FIXTURE" "$SECS" "$@" >"$log" 2>&1; then
        echo "FAIL: the fixture did not run to the end and present (player $*); the log ends:" >&2
        tail -n 40 "$log" | sed 's/^/    | /' >&2
        exit 1
    fi
}

recorded=/tmp/migo-compat-recorded.log
plain=/tmp/migo-compat-plain.log
run "$recorded" --prelude "$RECORDER"
run "$plain"

python3 - "$recorded" "$plain" <<'PY'
import json
import re
import sys

MARK = "__MIGO_COMPAT__"
ANSI = re.compile(r"\x1b\[[0-9;]*m")


def read(path):
    with open(path, encoding="utf-8", errors="replace") as handle:
        return [ANSI.sub("", line.rstrip("\n")) for line in handle]


recorded, plain = read(sys.argv[1]), read(sys.argv[2])
order = []
for line in recorded:
    if MARK in line:
        order.append(json.loads(line[line.index(MARK) + len(MARK):]))
    elif "FIXTURE entry evaluated" in line:
        order.append({"kind": "fixture-entry-end"})
events = [e for e in order if e["kind"] != "fixture-entry-end"]

problems = []


def expect(condition, message):
    if not condition:
        problems.append(message)


def having(kind, **fields):
    return [e for e in events if e["kind"] == kind and all(e.get(k) == v for k, v in fields.items())]


expect(events[:1] == [{"kind": "start", "version": 1}], "the first marker is not the recorder's start")
expect(len(having("call", surface="migo", api="getSystemInfoSync")) == 1,
       "migo.getSystemInfoSync, called twice, is not reported exactly once")
expect(having("call", surface="migo", api="createCanvas"), "migo.createCanvas is not reported")
expect(having("call", surface="migo", api="requestAnimationFrame"), "migo.requestAnimationFrame is not reported")
for name in ("createVKSession", "getGPU"):
    expect(having("absent", surface="migo", api=name), f"the engine lacks migo.{name}, and it is not reported absent")
expect(having("installed", surface="migo", api="shim"), "the fixture's own migo.shim is not reported installed")
expect(not having("call", surface="migo", api="shim"), "the fixture's own migo.shim is reported as a call")
expect(not having("absent", surface="migo", api="getSystemInfoSync"), "a name the engine has is reported absent")
expect(having("gl-extension", context="webgl", name="OES_vertex_array_object", supported=True),
       "OES_vertex_array_object is not reported supported")
expect(having("gl-extension", context="webgl", name="MIGO_no_such_extension", supported=False),
       "an extension nobody has is not reported unsupported")
expect(having("gl-error", context="webgl", code=1280), "the INVALID_ENUM the fixture caused is not reported")
expect(any("FIXTURE identity true" in line for line in recorded),
       "a function read twice from migo was not the same function")
expect(any("FIXTURE getError 1280" in line for line in recorded), "getError did not hand the fixture its code")

kinds = [e["kind"] for e in order]
ready = having("ready")
expect(len(ready) == 1, f"ready is reported {len(ready)} times")
if ready and "fixture-entry-end" in kinds:
    expect(kinds.index("fixture-entry-end") < kinds.index("ready"),
           "ready came before the entry's last line, so it does not mark the end of the entry's evaluation")
    expect(ready[0]["ms"] >= 150, f"ready at {ready[0]['ms']} ms, inside the entry's 150 ms of work")
else:
    expect(False, "the entry's last line or ready is missing")
first = having("first-frame")
expect(len(first) == 1, f"first-frame is reported {len(first)} times")
if ready and first:
    expect(kinds.index("ready") < kinds.index("first-frame") and first[0]["ms"] >= ready[0]["ms"],
           "the first frame came before ready")

expect(not [line for line in plain if MARK in line], "the run without the recorder wrote marker lines")
expect(any("FIXTURE entry evaluated" in line for line in plain), "the run without the recorder did not run the fixture")

for event in events:
    print("  ", json.dumps(event))
for problem in problems:
    print("FAIL:", problem)
if problems:
    sys.exit(1)
print("PASS: the recorder reported the fixture's reach on the engine, and the fixture ran and presented")
PY
