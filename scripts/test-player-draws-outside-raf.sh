#!/usr/bin/env bash
# Content that draws outside requestAnimationFrame is shown.
#
# The frame end that publishes a frame's commands and presents them ran only in the
# requestAnimationFrame loop, and the loop runs only while callbacks are queued. So a game that
# drew at its top level, from a timer or in an input handler drew and was never shown -- on
# Android a game drawing from setInterval stayed black for as long as it ran, and the Linux
# player reported such a game as never having drawn. A draw-once game showed on Android only
# because some unrelated frame happened to follow it. The engine now ends a frame for content
# drawn outside the loop, at the next frame, as a browser presents what a task drew:
#
#   * once   one fill at the top level is on screen, in its colour;
#   * timer  a game drawing every 300 ms keeps being presented: the frame captured at the end is
#            a late draw's, not the first one's (the red channel is the draw count x 16);
#   * text   a timer task that calls only fillText, which reaches the renderer through its own op
#            and not the command stream, puts its text on screen.
#
# Host-only: builds and runs the Linux player headless (EGL), no device.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# Where cargo puts it: a CARGO_TARGET_DIR elsewhere would otherwise build one player and run another.
PLAYER="${CARGO_TARGET_DIR:-$ROOT/engine/target}/debug/migo-player"
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

# The frame the player captured: "centre" prints its centre pixel as "r g b", "white" the number
# of pixels that are near white.
pixels() {
    python3 - "$1" "$2" <<'PY'
import struct, sys, zlib
data = open(sys.argv[1], "rb").read()
pos, idat = 8, b""
while pos < len(data):
    length = struct.unpack(">I", data[pos:pos + 4])[0]
    kind, body = data[pos + 4:pos + 8], data[pos + 8:pos + 8 + length]
    if kind == b"IHDR":
        width, height, _, colour = struct.unpack(">IIBB", body[:10])
    elif kind == b"IDAT":
        idat += body
    pos += 12 + length
raw, bpp = zlib.decompress(idat), (4 if colour == 6 else 3)
stride, previous, rows = width * bpp + 1, bytearray(width * bpp), []
for y in range(height):
    kind, line = raw[y * stride], bytearray(raw[y * stride + 1:(y + 1) * stride])
    for i in range(len(line)):
        a = line[i - bpp] if i >= bpp else 0
        b, c = previous[i], (previous[i - bpp] if i >= bpp else 0)
        if kind == 1: line[i] = (line[i] + a) & 255
        elif kind == 2: line[i] = (line[i] + b) & 255
        elif kind == 3: line[i] = (line[i] + (a + b) // 2) & 255
        elif kind == 4:
            p = a + b - c
            pa, pb, pc = abs(p - a), abs(p - b), abs(p - c)
            line[i] = (line[i] + (a if pa <= pb and pa <= pc else b if pb <= pc else c)) & 255
    rows.append(line)
    previous = line
if sys.argv[2] == "centre":
    x = width // 2
    print(*rows[height // 2][x * bpp:x * bpp + 3])
else:
    print(sum(1 for row in rows for x in range(width)
              if min(row[x * bpp], row[x * bpp + 1], row[x * bpp + 2]) >= 220))
PY
}

play() { # fixture -> exit status; the frame lands in $WORK/<fixture>.png
    local status=0
    MIGO_PLAYER_PNG="$WORK/$1.png" "$PLAYER" "$ROOT/scripts/fixtures/$1" 3 > "$WORK/$1.log" 2>&1 || status=$?
    echo "$status"
}

status="$(play draw-outside-raf-once)"
check "a fill at the top level is presented" '[[ $status -eq 0 ]]'
r= g= b=
[[ $status -eq 0 ]] && read -r r g b < <(pixels "$WORK/draw-outside-raf-once.png" centre)
check "in its colour (#c03080, saw ${r:-?} ${g:-?} ${b:-?})" '[[ "${r:-} ${g:-} ${b:-}" == "192 48 128" ]]'

status="$(play draw-outside-raf-timer)"
check "a game drawing from a timer is presented" '[[ $status -eq 0 ]]'
r= g= b=
[[ $status -eq 0 ]] && read -r r g b < <(pixels "$WORK/draw-outside-raf-timer.png" centre)
check "and keeps being presented: the last frame is a late draw (red ${r:-?}, the first is 16)" \
    '[[ ${r:-0} -ge 128 && ${g:-1} -eq 0 && ${b:-1} -eq 0 ]]'

# A frame is presented as soon as the canvas exists, so a run that ended without one says little;
# what the frame holds is the evidence.
status="$(play draw-outside-raf-text)"
white=0
[[ $status -eq 0 ]] && white="$(pixels "$WORK/draw-outside-raf-text.png" white)"
check "text drawn by a task that writes nothing else, through its own op, is on screen ($white white pixels)" \
    '[[ $white -ge 2000 ]]'

if [[ $failures -eq 0 ]]; then
    echo "PASS: content drawn outside requestAnimationFrame reaches the screen"
else
    echo "FAIL: $failures check(s) failed; logs: $WORK (kept)"
    trap - EXIT
    exit 1
fi
