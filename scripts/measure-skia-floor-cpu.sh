#!/usr/bin/env bash
# ============================================================
# Does the Skia per-context resource-cache floor earn its keep, i.e. does a
# lower cap make Skia thrash?
# Location: scripts/measure-skia-floor-cpu.sh
#
# Frame time is the wrong instrument here: at 60 vsyncs/s a fixture that never
# asks for more than 60 draws/s reads as flat regardless of how much
# render-thread work each frame costs, because vsync is the ceiling, not the
# workload. So this measures render-thread CPU% instead --
# /proc/<pid>/stat (utime+stime) delta, median of three 2s windows -- the same
# instrument used by the other render measurements, for the same reason (a
# single window occasionally lands on a stalled moment).
#
# Run this once against the shipped build (MIN_PER_CTX_BYTES = 4 MiB) and
# once against a build with it forced to 0 (aggregate/n honoured exactly, no
# floor -- 1.2 MiB/context at 80 contexts on TierA's 96 MiB budget), same
# fixture, device cooled to the same starting temperature both times. If Skia
# is not thrashing at 1.2 MiB/context the two CPU% figures should agree
# within noise; if it is, the floor build should read visibly lower.
#
# Usage:
#   scripts/measure-skia-floor-cpu.sh <fixture> [--device SERIAL] [--label L]
# ============================================================
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/lib/device-fixture-runner.sh"

FIXTURE="${1:?usage: $0 <fixture> [--device SERIAL] [--label L]}"
shift
DEVICE="${ANDROID_SERIAL:-}"
LABEL="$FIXTURE"
while [[ $# -gt 0 ]]; do
    case "$1" in
        --device) DEVICE="${2:?}"; shift 2 ;;
        --device=*) DEVICE="${1#*=}"; shift ;;
        --label) LABEL="${2:?}"; shift 2 ;;
        --label=*) LABEL="${1#*=}"; shift ;;
        *) shift ;;
    esac
done

rf_deploy_fixture "$FIXTURE" "$DEVICE"
ADB=("${RF_ADB[@]}")

temp_c() { echo $(( $("${ADB[@]}" shell cat /sys/class/thermal/thermal_zone0/temp 2>/dev/null | tr -d '\r') / 1000 )); }
echo "[$LABEL] device temp before launch: $(temp_c)C" >&2

rf_launch_fixture "$FIXTURE" "$DEVICE"
PID="$RF_PID"
[[ -n "$PID" ]] || { echo "could not read a pid for $RF_PKG after launch" >&2; exit 1; }

"${ADB[@]}" shell input keyevent KEYCODE_WAKEUP >/dev/null 2>&1 || true
"${ADB[@]}" shell svc power stayon true >/dev/null 2>&1 || true

# Let the scene reach steady state (all contexts created, caches warmed)
# before sampling -- matches the "settled" window measure-skia-floor-pss.sh
# uses.
sleep 10

clk="$("${ADB[@]}" shell getconf CLK_TCK | tr -d '\r')"; clk="${clk:-100}"

cpu_once() {
    local win="$1" t0 t1 v
    v="$("${ADB[@]}" shell "cat /proc/$PID/stat 2>/dev/null" | awk '{print $14+$15}')"
    t0="${v:-0}"
    sleep "$win"
    v="$("${ADB[@]}" shell "cat /proc/$PID/stat 2>/dev/null" | awk '{print $14+$15}')"
    t1="${v:-0}"
    awk "BEGIN{printf \"%.1f\", ($t1-$t0)/$clk/$win*100}"
}

a="$(cpu_once 2)"
b="$(cpu_once 2)"
c="$(cpu_once 2)"
median="$(printf '%s\n%s\n%s\n' "$a" "$b" "$c" | sort -n | sed -n '2p')"

echo "[$LABEL] device temp after: $(temp_c)C" >&2
"${ADB[@]}" shell "am force-stop $RF_PKG" >/dev/null 2>&1 || true

echo "$LABEL: cpu% samples = [$a, $b, $c], median = $median%"
