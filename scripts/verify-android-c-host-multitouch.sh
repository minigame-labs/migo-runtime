#!/usr/bin/env bash
# ============================================================
# Deliver a real two-finger gesture to the Android C host on a device, and read
# the pointer count that reached JS back as pixels.
# Location: scripts/verify-android-c-host-multitouch.sh
#
# Multi-pointer delivery through the C ABI was the one input path never run on
# a device, because a shell cannot synthesize a second finger there. The
# instrumentation in tests/c_host/android-multitouch injects through
# UiAutomation, which reaches a NativeActivity window like a finger does; see
# its header for what each step asserts.
#
# Usage:
#   scripts/verify-android-c-host-multitouch.sh [ABI]
#
# ABI defaults to arm64-v8a. Builds the host (scripts/build-android-c-host.sh)
# and the instrumentation APK every run, installs both on the device adb
# selects (set ANDROID_SERIAL to choose), and exits 0 only on a PASS.
# ============================================================
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
ABI="${1:-arm64-v8a}"
PACKAGE="com.migo.chost"
RUNNER="com.migo.chost.multitouch/.MultiTouchInstrumentation"
CONTENT_ID="touchprobe"

info() { echo -e "\033[0;36m[multitouch] $*\033[0m"; }
ok() { echo -e "\033[0;32m[multitouch] $*\033[0m"; }
err() { echo -e "\033[0;31m[multitouch] $*\033[0m" >&2; }

adb get-state >/dev/null 2>&1 || { err "no device (adb get-state failed; set ANDROID_SERIAL)"; exit 2; }

# Always build: an APK left from another tree would be scored as this one.
info "building the C host ($ABI)"
bash "$SCRIPT_DIR/build-android-c-host.sh" "$ABI"
info "building the instrumentation APK"
(cd "$REPO_ROOT/platforms/android" && ./gradlew --no-daemon :c-host-multitouch:assembleDebug)

HOST_APK="$REPO_ROOT/tests/c_host/android/build/outputs/apk/debug/c-host-example-debug.apk"
TEST_APK="$REPO_ROOT/tests/c_host/android-multitouch/build/outputs/apk/debug/c-host-multitouch-debug.apk"
[[ -f "$HOST_APK" && -f "$TEST_APK" ]] || { err "APKs missing: $HOST_APK / $TEST_APK"; exit 1; }

info "installing"
adb install -r -t "$HOST_APK" >/dev/null
adb install -r -t "$TEST_APK" >/dev/null

info "staging touch-probe as the host's content"
CODE="files/migo/games/$CONTENT_ID/code"
adb shell "run-as $PACKAGE sh -c 'mkdir -p $CODE'"
for f in game.js game.json; do
  adb shell "run-as $PACKAGE sh -c 'cat > $CODE/$f'" < "$REPO_ROOT/tests/c_host/touch-probe/$f"
done
echo "$CONTENT_ID" | adb shell "run-as $PACKAGE sh -c 'cat > files/content-id'"

# A locked or dozing screen swallows injected input and screenshots show the
# lock screen, which would read as a delivery failure.
adb shell input keyevent KEYCODE_WAKEUP
adb shell wm dismiss-keyguard >/dev/null 2>&1 || true

info "running the instrumentation"
# -r: without it, `am instrument` prints a status bundle's stream text in place
# of INSTRUMENTATION_CODE, and the code is the verdict.
OUT="$(adb shell am instrument -r -w "$RUNNER" 2>&1 | tr -d '\r' || true)"
echo "$OUT" | grep -oE '\[multitouch\].*' || true
# `|| true`: a run that dies before reporting has no code line, and that must
# reach the failure report below rather than end the script under pipefail.
CODE_LINE="$(echo "$OUT" | grep -E '^INSTRUMENTATION_CODE:' | tail -1 || true)"

# Stop the host and reset its content, so whoever runs it next gets its default.
adb shell am force-stop "$PACKAGE" >/dev/null 2>&1 || true
adb shell "run-as $PACKAGE rm -f files/content-id" >/dev/null 2>&1 || true

# Activity.RESULT_OK is -1.
if [[ "$CODE_LINE" == "INSTRUMENTATION_CODE: -1" ]]; then
  ok "two pointers reached JS through the C ABI, and each lifted on its own"
  exit 0
fi
err "instrumentation did not pass (${CODE_LINE:-no result line})"
echo "$OUT" | tail -20 >&2
exit 1
