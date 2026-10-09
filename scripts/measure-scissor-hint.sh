#!/usr/bin/env bash
# ============================================================
# Does the Canvas2D scissor hint's new fail-safe poisoning (any
# SetCompositeOperation call widens the segment's scissor to the full canvas,
# even one that nets out to a no-op) cost anything on a tiled GPU?
# Location: scripts/measure-scissor-hint.sh
#
# Runs scripts/fixtures/scissor-hint-baseline (tight cluster-sized scissor
# every frame) and scripts/fixtures/scissor-hint-composite (identical, plus
# one set-and-reset-to-source-over composite-mode touch per frame, which
# poisons the hint to full-canvas even though the state at draw time is
# ordinary source-over) and compares render-thread CPU%.
#
# CPU%, not frame time: 300 tiny rects is nowhere near enough to miss 60fps
# either way, so a frame-time/fps reading has no power to show a difference
# here regardless of the true cost (same reasoning as scissor-hint-baseline
# and measure-skia-floor-cpu.sh).
#
# Usage:
#   scripts/measure-scissor-hint.sh [--device SERIAL]
# ============================================================
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/lib/device-fixture-runner.sh"

DEVICE="${ANDROID_SERIAL:-}"
while [[ $# -gt 0 ]]; do
    case "$1" in
        --device) DEVICE="${2:?}"; shift 2 ;;
        --device=*) DEVICE="${1#*=}"; shift ;;
        *) shift ;;
    esac
done

measure_one() {
    local fixture="$1" label="$2"
    rf_deploy_fixture "$fixture" "$DEVICE"
    local -a ADB=("${RF_ADB[@]}")

    rf_launch_fixture "$fixture" "$DEVICE"
    local pid="$RF_PID"
    [[ -n "$pid" ]] || { echo "[$label] could not read a pid" >&2; return 1; }

    "${ADB[@]}" shell input keyevent KEYCODE_WAKEUP >/dev/null 2>&1 || true
    "${ADB[@]}" shell svc power stayon true >/dev/null 2>&1 || true
    sleep 5

    local clk; clk="$("${ADB[@]}" shell getconf CLK_TCK | tr -d '\r')"; clk="${clk:-100}"
    cpu_once() {
        local win="$1" t0 t1 v
        v="$("${ADB[@]}" shell "cat /proc/$pid/stat 2>/dev/null" | awk '{print $14+$15}')"; t0="${v:-0}"
        sleep "$win"
        v="$("${ADB[@]}" shell "cat /proc/$pid/stat 2>/dev/null" | awk '{print $14+$15}')"; t1="${v:-0}"
        awk "BEGIN{printf \"%.1f\", ($t1-$t0)/$clk/$win*100}"
    }
    local a b c median
    a="$(cpu_once 2)"; b="$(cpu_once 2)"; c="$(cpu_once 2)"
    median="$(printf '%s\n%s\n%s\n' "$a" "$b" "$c" | sort -n | sed -n '2p')"

    "${ADB[@]}" shell "am force-stop $RF_PKG" >/dev/null 2>&1 || true
    echo "$label: cpu% samples = [$a, $b, $c], median = $median%"
}

measure_one scissor-hint-baseline  "baseline (no composite touch)"
measure_one scissor-hint-composite "composite (set-and-reset per frame)"
