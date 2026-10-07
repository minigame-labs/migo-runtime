#!/usr/bin/env bash
# Reproduce, in ~60 seconds and without a device: does a GPU context loss take
# the Canvas2D drawing state with it?
#
# The probe sets `fillStyle` exactly once and fills the canvas every frame. The
# JS setter de-duplicates against its own shadow, so the value is never
# re-sent -- and Canvas2D, unlike WebGL, has no context-loss event for content
# to react to, because browsers restore 2D contexts transparently and no engine
# listens for one. If the render side rebuilds its context at spec defaults,
# every later fill paints opaque black and nothing anywhere reports an error.
#
# Measured on this repo: master before the fix captured (0, 0, 0); with the
# state carried through recovery, (255, 0, 255).
#
# DEV-ONLY, and deliberately not a CI gate: the Linux player needs a host
# (linux-gnu) V8 archive and the Skia host setup, neither of which a stock
# runner has -- see the note in .github/workflows/pr-ci.yml. The CI-visible half
# of this invariant is `recovery_gives_every_rebuilt_2d_context_its_drawing_state_back`
# plus the `plan_share_group_restore` unit tests, which run anywhere.
#
# Prerequisites: bash scripts/dev-setup-skia.sh (once), and MIGO_HOST_V8_DIR or
# ../rusty_v8_src, as resolved by scripts/lib/host-v8.sh.
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PROBE="$ROOT_DIR/engine/tools/player/testdata/canvas2d-context-loss"
OUT="${MIGO_CTXLOSS_PNG:-$(mktemp -d)/after.png}"
EXPECTED="255,0,255"

mkdir -p "$(dirname "$OUT")"
MIGO_PLAYER_PNG="$OUT" bash "$ROOT_DIR/scripts/dev-run-player.sh" "$PROBE" 10 > /tmp/ctxloss-player.log 2>&1 \
    || { echo "ERROR: player run failed; see /tmp/ctxloss-player.log" >&2; exit 1; }

# The run must actually have lost the context, or a pass means nothing.
grep -q "triggering context loss" /tmp/ctxloss-player.log || {
    echo "ERROR: the probe never triggered a context loss -- this run proves nothing" >&2
    exit 1
}
grep -q "EGL recovery re-registered" /tmp/ctxloss-player.log || {
    echo "ERROR: no EGL recovery ran -- the loss was not handled, so the state" >&2
    echo "       question was never asked" >&2
    exit 1
}

actual="$(python3 - "$OUT" <<'PY'
import struct, sys, zlib

path = sys.argv[1]
data = open(path, "rb").read()
assert data[:8] == b"\x89PNG\r\n\x1a\n", f"{path} is not a PNG"

pos, idat = 8, b""
while pos < len(data):
    length = struct.unpack(">I", data[pos:pos + 4])[0]
    kind = data[pos + 4:pos + 8]
    body = data[pos + 8:pos + 8 + length]
    if kind == b"IHDR":
        width, height, _depth, colour = struct.unpack(">IIBB", body[:10])
    elif kind == b"IDAT":
        idat += body
    elif kind == b"IEND":
        break
    pos += 12 + length

channels = {0: 1, 2: 3, 4: 2, 6: 4}[colour]
raw = zlib.decompress(idat)
stride = width * channels
previous = bytearray(stride)
middle = None
for y in range(height):
    filt = raw[y * (stride + 1)]
    line = bytearray(raw[y * (stride + 1) + 1:(y + 1) * (stride + 1)])
    for x in range(stride):
        a = line[x - channels] if x >= channels else 0
        b = previous[x]
        c = previous[x - channels] if x >= channels else 0
        v = line[x]
        if filt == 1:
            v += a
        elif filt == 2:
            v += b
        elif filt == 3:
            v += (a + b) // 2
        elif filt == 4:
            p = a + b - c
            pa, pb, pc = abs(p - a), abs(p - b), abs(p - c)
            v += a if (pa <= pb and pa <= pc) else (b if pb <= pc else c)
        line[x] = v & 255
    previous = line
    if y == height // 2:
        middle = bytes(line)

off = (width // 2) * channels
print(",".join(str(component) for component in middle[off:off + 3]))
PY
)"

if [[ "$actual" != "$EXPECTED" ]]; then
    echo "FAIL: the Canvas2D drawing state did not survive the context loss" >&2
    echo "      centre pixel: got ($actual), want ($EXPECTED)" >&2
    echo "      (0,0,0) is the spec default fill: the context was rebuilt but its" >&2
    echo "      state was not, and the content will never re-send it." >&2
    exit 1
fi

echo "PASS: the 2D drawing state survived a GPU context loss (centre pixel $actual)"
