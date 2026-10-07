#!/usr/bin/env bash
# ============================================================
# Does the Skia per-context resource-cache floor's overshoot on many-canvas
# scenes actually materialise as held PSS?
# Location: scripts/measure-skia-floor-pss.sh
#
# backend/gl/surface.rs caps each Canvas2DContext's Ganesh cache at
# max(aggregate / live_contexts, MIN_PER_CTX_BYTES). Past a context count the
# 4 MiB floor outranks the aggregate share, so the *ceiling* the process would
# grant N contexts is N * 4 MiB -- 320 MiB at 80 contexts on TierA. Skia's cap
# is a ceiling, not a reservation, so whether that overshoot is real is a
# question only a device can answer: this samples TOTAL PSS / Native Heap /
# Graphics from `dumpsys meminfo` once a second while a many-offscreen-canvas
# fixture runs, and reports the settled (second-half-of-run) figures.
#
# Usage:
#   scripts/measure-skia-floor-pss.sh <fixture> [SECS] [--device SERIAL]
#
# Intended fixtures: skia-floor-probe-30, skia-floor-probe-80,
# skia-floor-probe-80-dynamic (scripts/fixtures/).
# ============================================================
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/lib/device-fixture-runner.sh"

FIXTURE="${1:?usage: $0 <fixture> [SECS] [--device SERIAL]}"
shift
SECS=30
DEVICE="${ANDROID_SERIAL:-}"
while [[ $# -gt 0 ]]; do
    case "$1" in
        --device) DEVICE="${2:?}"; shift 2 ;;
        --device=*) DEVICE="${1#*=}"; shift ;;
        *) SECS="$1"; shift ;;
    esac
done

rf_deploy_fixture "$FIXTURE" "$DEVICE"
ADB=("${RF_ADB[@]}")

temp_millideg() {
    "${ADB[@]}" shell "cat /sys/class/thermal/thermal_zone0/temp" 2>/dev/null | tr -d '\r'
}
echo "device temp before launch: $(( $(temp_millideg) / 1000 ))C" >&2

rf_launch_fixture "$FIXTURE" "$DEVICE"
PID="$RF_PID"
[[ -n "$PID" ]] || { echo "could not read a pid for $RF_PKG after launch" >&2; exit 1; }
echo "pid=$PID, sampling for ${SECS}s" >&2

sample_meminfo() {
    "${ADB[@]}" shell "dumpsys meminfo $RF_PKG" 2>/dev/null | tr -d '\r'
}

WORK="$(mktemp -d)"
SAMPLES="$WORK/samples.tsv"
printf 'ts\tpss_kb\tnative_kb\tgraphics_kb\n' > "$SAMPLES"

t=0
while [[ "$t" -lt "$SECS" ]]; do
    out="$(sample_meminfo)"
    pss="$(grep -oE 'TOTAL PSS:\s*[0-9]+' <<<"$out" | grep -oE '[0-9]+' | head -1)"
    native="$(grep -oE 'Native Heap:\s*[0-9]+' <<<"$out" | grep -oE '[0-9]+' | head -1)"
    graphics="$(grep -oE 'Graphics:\s*[0-9]+' <<<"$out" | grep -oE '[0-9]+' | head -1)"
    printf '%d\t%s\t%s\t%s\n' "$t" "${pss:-0}" "${native:-0}" "${graphics:-0}" >> "$SAMPLES"
    sleep 1
    t=$((t + 1))
done

"${ADB[@]}" exec-out screencap -p > "$WORK/frame.png" 2>/dev/null || true
# `read` returns non-zero at EOF, so under `set -e` an empty screencap (a
# blanked screen is enough) killed the script *after* the samples were already
# collected -- throwing away a completed measurement because the liveness
# screenshot failed. Liveness is a cross-check, not the measurement.
COLOUR="unavailable"; DISTINCT="0"
read -r COLOUR DISTINCT < <(python3 "$SCRIPT_DIR/lib/dominant_pixel.py" "$WORK/frame.png" 2>/dev/null) \
    || { COLOUR="unavailable"; DISTINCT="0"; }

"${ADB[@]}" logcat -d --pid="$PID" > "$WORK/logcat.txt" 2>/dev/null || true
# Same reason the screencap read is guarded: with `pipefail`, a `grep` that
# matches nothing fails the whole pipeline, and `set -e` then discarded a
# measurement whose samples were already on disk. A fixture that logs no frame
# counter is a liveness gap to report, not a reason to throw the numbers away.
FRAMES="$(grep -oE "frame [0-9]+" "$WORK/logcat.txt" | grep -oE "[0-9]+" | sort -n | tail -1 || true)"
FRAMES="${FRAMES:-0}"

echo "device temp after run: $(( $(temp_millideg) / 1000 ))C" >&2
"${ADB[@]}" shell "am force-stop $RF_PKG" >/dev/null 2>&1 || true

echo
echo "SKIA CACHE FLOOR PSS  fixture=$FIXTURE  ${SECS}s"
echo
printf '%-6s %10s %12s %12s\n' "t(s)" "PSS(KB)" "Native(KB)" "Graphics(KB)"
tail -n +2 "$SAMPLES" | awk -F'\t' '{printf "%-6s %10s %12s %12s\n", $1, $2, $3, $4}'

# Settled figures: mean over the second half, past whatever warm-up allocation
# the first canvases still cause.
half=$(( SECS / 2 ))
settled="$(tail -n +2 "$SAMPLES" | awk -F'\t' -v h="$half" '$1>=h {pss+=$2; nat+=$3; gfx+=$4; n++} END {if (n>0) printf "%.0f\t%.0f\t%.0f", pss/n, nat/n, gfx/n}')"
IFS=$'\t' read -r settled_pss settled_native settled_graphics <<<"$settled"

echo
echo "settled (mean of t>=${half}s): PSS=${settled_pss}KB  Native=${settled_native}KB  Graphics=${settled_graphics}KB"
echo "liveness: $FRAMES frames logged, screen dominant colour $COLOUR ($DISTINCT distinct)"
echo "samples: $SAMPLES"
