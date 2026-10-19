#!/usr/bin/env bash
# Does this bundle actually run? The half a scanner cannot answer.
#
# `scripts/prescreen-game.sh` reports which API names a bundle needs and whether
# this build publishes them. That is a real answer to a narrow question, and on
# its own it is the wrong shape to give a customer: migration also fails on
# things that are not API names -- DOM assumptions, WebGL2, subpackage layout,
# timing, a shader that compiles everywhere except here. None of those appear in
# a name diff. The only way to see them is to run the bundle.
#
# ON A DEVICE, NOT ON THE LINUX PLAYER. This is not a preference. The Linux
# player has no device services, so any content that touches login, storage or
# system info gets "not supported" and the report fills with blockers that do
# not exist on Android -- judging a migratable customer un-migratable, which is
# the exact inverse of what this tool is for.
#
# WHAT IT CAN AND CANNOT SEE:
#
#   * It observes the screen and the log. It does not instrument the content, so
#     it works on a bundle that was never built with this in mind -- which is the
#     whole point, since the bundle belongs to someone else.
#   * "Painted" here means the surface carries a real image: many distinct
#     colours rather than one flat fill. A game that legitimately renders a solid
#     colour for its first seconds reads as not-painted, and the report says the
#     evidence is a screenshot so a human can overrule it. A verdict this tool
#     states without evidence attached would be worse than no verdict.
#   * "Still painting" is two captures a few seconds apart differing. A finished
#     static scene reads as stalled. Same remedy: the PNGs are attached.
#   * A clean run is not a certification. It means nothing blocked in the first
#     N seconds on one device.
#
# Usage:
#   scripts/prescreen-run.sh <bundle-dir> --package PKG --activity NAME
#                            [--device SERIAL] [--game-id ID] [--secs N]
#                            [--out FILE] [--keep]
#
#   --package   an installed host that loads a bundle from its own private
#               directory. The report names it, because "it ran" means nothing
#               without saying what ran it.
#   --activity  the activity that puts the *game* on screen. Required, and not
#               guessed: the first version resolved the launcher activity, landed
#               on a host's menu screen, saw a colourful animated-enough surface
#               and reported "Runs" for a bundle that had never been loaded. A
#               host's own UI is indistinguishable from content by pixels alone,
#               so the caller has to say which window is the game, and this checks
#               that window is the one in front.
#   --game-id   which slot under <files>/migo/games/<id>/code the host reads
#               (default: demo)
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

BUNDLE=""; PKG=""; SERIAL="${ANDROID_SERIAL:-}"; ACTIVITY=""; GAME_ID="demo"; ENTRY="game.js"
SECS=20; OUT=""; KEEP=false
while [[ $# -gt 0 ]]; do
    case "$1" in
        --package) PKG="${2:?--package requires a value}"; shift 2 ;;
        --package=*) PKG="${1#*=}"; shift ;;
        --device) SERIAL="${2:?--device requires a value}"; shift 2 ;;
        --device=*) SERIAL="${1#*=}"; shift ;;
        --activity) ACTIVITY="${2:?--activity requires a value}"; shift 2 ;;
        --activity=*) ACTIVITY="${1#*=}"; shift ;;
        --entry) ENTRY="${2:?--entry requires a value}"; shift 2 ;;
        --entry=*) ENTRY="${1#*=}"; shift ;;
        --game-id) GAME_ID="${2:?--game-id requires a value}"; shift 2 ;;
        --game-id=*) GAME_ID="${1#*=}"; shift ;;
        --secs) SECS="${2:?--secs requires a number}"; shift 2 ;;
        --secs=*) SECS="${1#*=}"; shift ;;
        --out) OUT="${2:?--out requires a path}"; shift 2 ;;
        --out=*) OUT="${1#*=}"; shift ;;
        --keep) KEEP=true; shift ;;
        -h|--help) sed -n '2,45p' "$0"; exit 0 ;;
        *) BUNDLE="$1"; shift ;;
    esac
done

[[ -n "$BUNDLE" ]] || { echo "usage: $0 <bundle-dir> --package PKG" >&2; exit 2; }
[[ -d "$BUNDLE" ]] || { echo "not a directory: $BUNDLE" >&2; exit 2; }
[[ -n "$PKG" ]] || { echo "--package is required: this reports what ran the bundle, and an unnamed host is not evidence" >&2; exit 2; }
[[ -f "$BUNDLE/game.js" ]] || { echo "no game.js in $BUNDLE; the host loads game.js from the bundle root" >&2; exit 2; }

ADB_BIN="${ADB:-$HOME/Android/Sdk/platform-tools/adb}"
[[ -x "$ADB_BIN" ]] || ADB_BIN="$(command -v adb || true)"
[[ -n "$ADB_BIN" ]] || { echo "no adb; set ADB=/path/to/adb" >&2; exit 2; }
ADB=("$ADB_BIN")
[[ -n "$SERIAL" ]] && ADB+=(-s "$SERIAL")

# One device or an explicit serial. Guessing between two attached devices is how
# a report ends up describing hardware nobody asked about.
attached="$("${ADB[@]}" devices | awk 'NR>1 && $2=="device" {n++} END {print n+0}')"
if [[ -z "$SERIAL" && "$attached" -gt 1 ]]; then
    echo "$attached devices attached; pass --device SERIAL" >&2
    exit 2
fi
[[ "$attached" -ge 1 ]] || { echo "no device attached" >&2; exit 2; }

"${ADB[@]}" shell "pm path $PKG" >/dev/null 2>&1 || {
    echo "package not installed on the device: $PKG" >&2; exit 2; }

if [[ -z "$ACTIVITY" ]]; then
    echo "--activity is required: name the activity that puts the game on screen." >&2
    echo "  Resolving the launcher instead lands on a host's menu, where a colourful" >&2
    echo "  screen means the host is fine and says nothing about the bundle." >&2
    exit 2
else
    [[ "$ACTIVITY" == */* ]] || ACTIVITY="$PKG/$ACTIVITY"
fi

WORK="$(mktemp -d)"
cleanup() { [[ "$KEEP" == true ]] || rm -rf "$WORK"; }
trap cleanup EXIT

say() { echo "[prescreen-run] $*" >&2; }

DEVICE_MODEL="$("${ADB[@]}" shell getprop ro.product.model 2>/dev/null | tr -d '\r')"
DEVICE_SDK="$("${ADB[@]}" shell getprop ro.build.version.sdk 2>/dev/null | tr -d '\r')"

# ---- deploy -------------------------------------------------------------
# Through /data/local/tmp and `run-as`, so this needs no root and touches only
# the host's own sandbox. The bundle never leaves the customer's device.
say "deploying $(basename "$BUNDLE") to $PKG (slot: $GAME_ID)"
REMOTE_STAGE="/data/local/tmp/migo-prescreen-$$"
"${ADB[@]}" shell "rm -rf $REMOTE_STAGE && mkdir -p $REMOTE_STAGE" >/dev/null
tar -C "$BUNDLE" -cf "$WORK/bundle.tar" . 2>/dev/null
"${ADB[@]}" push "$WORK/bundle.tar" "$REMOTE_STAGE/bundle.tar" >/dev/null 2>&1 \
    || { echo "adb push failed" >&2; exit 1; }
CODE_DIR="files/migo/games/$GAME_ID/code"
if ! "${ADB[@]}" shell "run-as $PKG sh -c 'rm -rf $CODE_DIR && mkdir -p $CODE_DIR && cd $CODE_DIR && tar -xf $REMOTE_STAGE/bundle.tar'" 2>&1 | grep -qv .; then
    :
fi
deployed="$("${ADB[@]}" shell "run-as $PKG sh -c 'ls $CODE_DIR/game.js 2>/dev/null'" | tr -d '\r')"
"${ADB[@]}" shell "rm -rf $REMOTE_STAGE" >/dev/null 2>&1 || true
[[ -n "$deployed" ]] || {
    echo "deploy failed: game.js is not in the host's private directory." >&2
    echo "  \`run-as\` needs a debuggable build of $PKG. A release host cannot be driven this way." >&2
    exit 1; }

# ---- the screen has to be on, and this is not a nicety ------------------
#
# A launched activity on a sleeping phone resumes and pauses again within a
# couple of hundred milliseconds, never gets a surface, and never starts an
# engine session. The report then says "No engine session ran" -- which reads as
# "your bundle is broken" and is really "the phone was asleep". Condemning
# someone else's content for the tool's own environment is the one failure this
# script is least allowed to have, and it is the same shape as the focus-probe
# blind spot documented further down.
#
# So: wake it, dismiss the keyguard, and if it will not wake, refuse to produce a
# verdict at all. A refusal is recoverable; a false negative published to a
# customer is not.
screen_state() {
    "${ADB[@]}" shell "dumpsys power | grep -m1 mWakefulness=" 2>/dev/null \
        | tr -d '\r' | sed 's/.*mWakefulness=//'
}
if [[ "$(screen_state)" != "Awake" ]]; then
    say "screen is asleep -- waking it"
    "${ADB[@]}" shell input keyevent KEYCODE_WAKEUP >/dev/null 2>&1 || true
    "${ADB[@]}" shell wm dismiss-keyguard >/dev/null 2>&1 || true
    sleep 2
fi
if [[ "$(screen_state)" != "Awake" ]]; then
    echo "the device will not wake, so nothing this run reports would be about the bundle:" >&2
    echo "  a launched activity on a sleeping phone pauses before it gets a surface, and" >&2
    echo "  the verdict would read as a failure of your content. Unlock the phone (a" >&2
    echo "  secured lockscreen cannot be dismissed from adb) and run this again." >&2
    exit 2
fi

# ---- run ----------------------------------------------------------------
"${ADB[@]}" logcat -c >/dev/null 2>&1 || true
"${ADB[@]}" shell "am force-stop $PKG" >/dev/null 2>&1 || true
say "launching $ACTIVITY for ${SECS}s"
# MIGO_CAPI_LOG=info is the only outlet a content-side exception has on a pure
# native host; without it a JS throw is a black screen and a clean log.
# `am start` reports failure in its output, not always in its exit code, and
# swallowing both leaves the caller with "exit 1" and nothing to act on.
# `|| true` is load-bearing, and the reason is the block right below.
#
# `am start` reports a refusal in its OUTPUT while adb exits 255, and this file
# runs under `set -euo pipefail` -- so the assignment itself aborted the script
# and every line of diagnosis below was unreachable. A host whose game activity
# is not exported (the common case: `android:exported="false"` is the correct
# default, and the Android example in migo-examples ships exactly that) produced
# `EXIT=255`, no report, and not one word about why.
#
# The extras are also the SDK's real ones now. `MigoGameActivity` reads
# `migo_game_id` / `migo_entry_point` and calls `onLaunchFailed(ERR_INVALID_GAME_ID)`
# without them; this script had been sending `gameId` / `migoGameId`, which match
# nothing, so a stock Migo-SDK host could not be driven by Migo's own prescreen
# runner. The older spellings stay for third-party hosts that may read them.
am_out="$("${ADB[@]}" shell "am start -n $ACTIVITY \
    --es migo_game_id $GAME_ID --es migo_entry_point $ENTRY \
    --es gameId $GAME_ID --es migoGameId $GAME_ID \
    --es MIGO_CAPI_LOG info" 2>&1 | tr -d '\r')" || true
if grep -qiE "error|does not exist|not exported|permission denial" <<<"$am_out"; then
    echo "could not start $ACTIVITY:" >&2
    sed 's/^/  /' <<<"$am_out" >&2
    echo "  Activities this host exports:" >&2
    "${ADB[@]}" shell "cmd package query-activities --brief -a android.intent.action.MAIN 2>/dev/null | grep $PKG" 2>/dev/null | sed 's/^/    /' >&2 || true
    echo "  A host that cannot be started into its game surface from adb cannot be" >&2
    echo "  prescreened this way; ask its owner for an entry point that takes a game id." >&2
    exit 1
fi

# Which window is in front, across the spellings Android has used. `dumpsys
# window windows` carries mCurrentFocus on older releases and not on API 31,
# where the same line lives under plain `dumpsys window` -- a probe that knew
# only the first spelling came back empty on a modern phone and the report read
# that silence as "the host was never in front", condemning the bundle for the
# tool's own blind spot.
# Every other Migo host on the device writes the same tags. Reading the whole
# buffer meant another app's session lifecycle counted as evidence for this
# bundle -- on a phone with the bench shell installed, "a session reached
# RUNNING" was true and had nothing to do with what was being prescreened. So
# the dump is bound to this launch's pid.
sleep 2
TARGET_PID="$("${ADB[@]}" shell "pidof $PKG" 2>/dev/null | tr -d '\r' | awk '{print $1}')"
[[ -n "$TARGET_PID" ]] && say "host pid $TARGET_PID"

focus_now() {
    local out
    out="$("${ADB[@]}" shell dumpsys window 2>/dev/null \
        | grep -m1 -oE 'mCurrentFocus=Window\{[^}]*\}' | tr -d '\r')"
    if [[ -z "$out" ]]; then
        out="$("${ADB[@]}" shell dumpsys activity activities 2>/dev/null \
            | grep -m1 -oE 'ResumedActivity: ActivityRecord\{[^}]*\}' | tr -d '\r')"
    fi
    printf '%s' "$out"
}

sleep "$(( SECS / 2 ))"
"${ADB[@]}" exec-out screencap -p > "$WORK/first.png" 2>/dev/null || true
focus_now > "$WORK/focus1.txt" 2>/dev/null || true
sleep "$(( SECS - SECS / 2 ))"
"${ADB[@]}" exec-out screencap -p > "$WORK/second.png" 2>/dev/null || true
focus_now > "$WORK/focus2.txt" 2>/dev/null || true
if [[ -n "$TARGET_PID" ]]; then
    "${ADB[@]}" logcat -d --pid="$TARGET_PID" > "$WORK/logcat.txt" 2>/dev/null || true
    # --pid is unsupported on older platform-tools/devices; an empty capture there
    # would read as "the content logged nothing", which is a finding rather than a
    # gap in the tool.
    if [[ ! -s "$WORK/logcat.txt" ]]; then
        "${ADB[@]}" logcat -d > "$WORK/logcat.txt" 2>/dev/null || true
        echo "unfiltered" > "$WORK/log-scope.txt"
    else
        echo "pid $TARGET_PID" > "$WORK/log-scope.txt"
    fi
else
    "${ADB[@]}" logcat -d > "$WORK/logcat.txt" 2>/dev/null || true
    echo "unfiltered (host pid not found)" > "$WORK/log-scope.txt"
fi

alive="$("${ADB[@]}" shell "pidof $PKG" 2>/dev/null | tr -d '\r')"
"${ADB[@]}" shell "am force-stop $PKG" >/dev/null 2>&1 || true

REPORT_DIR="${OUT:+$(dirname "$OUT")}"
REPORT_DIR="${REPORT_DIR:-$PWD}"
mkdir -p "$REPORT_DIR"
cp "$WORK/first.png" "$REPORT_DIR/prescreen-frame-1.png" 2>/dev/null || true
cp "$WORK/second.png" "$REPORT_DIR/prescreen-frame-2.png" 2>/dev/null || true

# The reading is scripts/lib/prescreen_run_report.py's, so it can be tested on a capture made by hand.
python3 "$ROOT_DIR/scripts/lib/prescreen_run_report.py" "$BUNDLE" "$WORK" "$PKG" "$ACTIVITY" "$DEVICE_MODEL" \
        "$DEVICE_SDK" "$SECS" "$alive" "$REPORT_DIR" > "${OUT:-/dev/stdout}"

if [[ -n "$OUT" ]]; then
    say "report -> $OUT"
fi
