#!/usr/bin/env bash
# ============================================================
# Shared plumbing for a device-side fixture probe.
# Location: scripts/lib/device-fixture-runner.sh
#
# There is no scripted device fixture runner today: scripts/verify-*.sh read a
# pixel from the host player via MIGO_PLAYER_PNG, which does not exist on
# Android. This gives a device caller the same three assertions -- frame count,
# dominant colour, distinct colour count -- against the demo app's own
# DebugMigoGameActivity, deployed
# and driven the way scripts/prescreen-run.sh already does it (run-as tar into
# files/migo/games/<id>/code, then `am start` with the real SDK extras).
#
# Sourced, not run directly. A caller sets FIXTURE/SECS/DEVICE (DEVICE
# optional) and calls `run_fixture_on_device`, which leaves:
#   RF_FRAMES    highest "painted N frames" logged (0 if none)
#   RF_COLOUR    dominant sampled colour of the last screencap, "r,g,b,a"
#   RF_DISTINCT  distinct sampled colours in that screencap
#   RF_LOG       path to the captured logcat (pid-scoped when possible)
#   RF_PNG       path to the screencap PNG
# ============================================================

RF_SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
RF_PKG="com.minigame.androiddemo"
# DebugMigoGameActivity (the one that actually puts a game on screen) is
# exported="false". Stock AOSP lets `adb shell am start` (uid 2000, shell)
# through anyway; this device's Huawei/HarmonyOS-hardened build does not --
# "SecurityException: not exported from uid 10262" -- so we go through
# MainActivity's own headless entry instead (exported="true", reads a `gameId`
# extra and forwards internally via startActivity(), which is not subject to
# the exported check because it is the app calling itself).
RF_ACTIVITY="$RF_PKG/.MainActivity"

rf_c_info() { echo -e "\033[0;36m[device-fixture] $*\033[0m" >&2; }
rf_c_err() { echo -e "\033[0;31m[device-fixture] $*\033[0m" >&2; }

# rf_adb_for DEVICE -- prints an array-safe adb prefix; callers do
#   local -a ADB=($(rf_adb_for "$device"))
# only when device is guaranteed to have no spaces (true here: adb serials).
rf_resolve_adb() {
    local device="${1:-}"
    local adb_bin="${ADB_BIN_OVERRIDE:-${ADB:-$HOME/Android/Sdk/platform-tools/adb}}"
    [[ -x "$adb_bin" ]] || adb_bin="$(command -v adb)"
    RF_ADB=("$adb_bin")
    [[ -n "$device" ]] && RF_ADB+=(-s "$device")
}

# Deploy a fixture's bundle into the demo app's private games/<fixture>/code
# slot. Sets RF_ADB (array) as a side effect for the caller to reuse.
rf_deploy_fixture() {
    local fixture="$1" device="${2:-}"
    local bundle="$RF_SCRIPT_DIR/fixtures/$fixture"
    [[ -f "$bundle/game.js" ]] || { rf_c_err "no such fixture: $bundle"; return 2; }

    rf_resolve_adb "$device"
    local work
    work="$(mktemp -d)"

    rf_c_info "deploying $fixture to $RF_PKG (slot: $fixture)"
    local remote_stage="/data/local/tmp/migo-fixture-$$"
    "${RF_ADB[@]}" shell "rm -rf $remote_stage && mkdir -p $remote_stage" >/dev/null
    tar -C "$bundle" -cf "$work/bundle.tar" .
    "${RF_ADB[@]}" push "$work/bundle.tar" "$remote_stage/bundle.tar" >/dev/null 2>&1 \
        || { rf_c_err "adb push failed"; rm -rf "$work"; return 1; }
    local code_dir="files/migo/games/$fixture/code"
    "${RF_ADB[@]}" shell "run-as $RF_PKG sh -c 'rm -rf $code_dir && mkdir -p $code_dir && cd $code_dir && tar -xf $remote_stage/bundle.tar'" >/dev/null 2>&1 || true
    local deployed
    deployed="$("${RF_ADB[@]}" shell "run-as $RF_PKG sh -c 'ls $code_dir/game.js 2>/dev/null'" | tr -d '\r')"
    "${RF_ADB[@]}" shell "rm -rf $remote_stage" >/dev/null 2>&1 || true
    rm -rf "$work"
    [[ -n "$deployed" ]] || { rf_c_err "deploy failed: game.js not staged in the app sandbox"; return 1; }
    return 0
}

# Launch an already-deployed fixture via MainActivity's headless entry. Sets
# RF_PID to the launched process's pid (empty if it could not be read).
rf_launch_fixture() {
    local fixture="$1" device="${2:-}"
    rf_resolve_adb "$device"

    "${RF_ADB[@]}" shell input keyevent KEYCODE_WAKEUP >/dev/null 2>&1 || true
    "${RF_ADB[@]}" shell wm dismiss-keyguard >/dev/null 2>&1 || true

    "${RF_ADB[@]}" logcat -c >/dev/null 2>&1 || true
    "${RF_ADB[@]}" shell "am force-stop $RF_PKG" >/dev/null 2>&1 || true
    rf_c_info "launching $fixture"
    local am_out
    am_out="$("${RF_ADB[@]}" shell "am start -n $RF_ACTIVITY \
        --es gameId $fixture --es entry game.js \
        --es MIGO_CAPI_LOG info" 2>&1 | tr -d '\r')" || true
    if grep -qiE "exception|error|not exported|permission denial" <<<"$am_out"; then
        rf_c_err "am start failed: $am_out"
        return 1
    fi

    sleep 1
    RF_PID="$("${RF_ADB[@]}" shell "pidof $RF_PKG" 2>/dev/null | tr -d '\r' | awk '{print $1}')"
    return 0
}

run_fixture_on_device() {
    local fixture="$1" secs="$2" device="${3:-}"

    rf_deploy_fixture "$fixture" "$device" || return $?
    rf_launch_fixture "$fixture" "$device" || return $?
    local pid="$RF_PID"
    local -a ADB=("${RF_ADB[@]}")
    local work
    work="$(mktemp -d)"

    sleep "$secs"
    "${ADB[@]}" exec-out screencap -p > "$work/frame.png" 2>/dev/null || true

    if [[ -n "$pid" ]]; then
        "${ADB[@]}" logcat -d --pid="$pid" > "$work/logcat.txt" 2>/dev/null || true
    fi
    if [[ ! -s "$work/logcat.txt" ]]; then
        "${ADB[@]}" logcat -d > "$work/logcat.txt" 2>/dev/null || true
    fi

    "${ADB[@]}" shell "am force-stop $RF_PKG" >/dev/null 2>&1 || true

    RF_FRAMES="$(grep -oE "painted [0-9]+ frames?" "$work/logcat.txt" | grep -oE "[0-9]+" | sort -n | tail -1)"
    RF_FRAMES="${RF_FRAMES:-0}"
    read -r RF_COLOUR RF_DISTINCT <<<"$(python3 "$RF_SCRIPT_DIR/lib/dominant_pixel.py" "$work/frame.png" 2>/dev/null)"
    RF_COLOUR="${RF_COLOUR:-no-capture}"
    RF_DISTINCT="${RF_DISTINCT:-0}"
    RF_LOG="$work/logcat.txt"
    RF_PNG="$work/frame.png"
    return 0
}
