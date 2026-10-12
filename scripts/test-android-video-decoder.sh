#!/usr/bin/env bash
# createVideoDecoder decodes on an Android device's own codec.
#
# The decoder is the NDK's extractor and codec rendering into an image reader
# (engine/crates/platform/src/android/video_decoder.rs), and nothing short of a
# device or an emulator runs those: so this deploys
# scripts/fixtures/video-decoder-probe -- ten 64 x 48 H.264 frames of
# known colour, BT.709 said in the stream -- into a demo host built against
# this tree's AAR, launches it, and reads the probe's verdict from logcat: every
# frame in order with its presentation time and colour, then a seek landing on
# the frame shown there.
#
# Needs one device or emulator on adb with the demo host installed
# (com.minigame.androiddemo, the examples repository's android-java app, built
# with this tree's AAR: scripts/build-android-so.sh then scripts/build-aar.sh
# for the device's ABI). The clip is regenerated, on a Mac, with
#   swiftc -O scripts/fixtures/video-decoder-probe/make-clip.swift -o /tmp/make-clip
#   /tmp/make-clip scripts/fixtures/video-decoder-probe/clip.mp4
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
FIXTURE="$ROOT/scripts/fixtures/video-decoder-probe"
PKG="com.minigame.androiddemo"
GAME="video-decoder-probe"
STAGE="/data/local/tmp/$GAME"
ADB="${ADB:-adb}"

fail() {
  echo "FAIL: $*" >&2
  exit 1
}

command -v "$ADB" > /dev/null || fail "no adb on PATH (or set ADB)"
[[ "$("$ADB" get-state 2>/dev/null)" == "device" ]] || fail "no device on adb"
"$ADB" shell pm path "$PKG" > /dev/null || fail "$PKG is not installed on the device"

echo "[1/3] deploying the probe as game '$GAME'"
"$ADB" shell "rm -rf $STAGE && mkdir -p $STAGE"
"$ADB" push "$FIXTURE/." "$STAGE/" > /dev/null
"$ADB" shell "run-as $PKG sh -c 'rm -rf files/migo/games/$GAME/code && mkdir -p files/migo/games/$GAME/code && cp -R $STAGE/. files/migo/games/$GAME/code/'"
"$ADB" shell "rm -rf $STAGE"

echo "[2/3] running it"
"$ADB" shell am force-stop "$PKG"
"$ADB" logcat -c
"$ADB" shell am start -n "$PKG/.MainActivity" --es gameId "$GAME" > /dev/null

echo "[3/3] waiting for the verdict"
verdict=""
for _ in $(seq 1 60); do
  verdict="$("$ADB" logcat -d | grep -o 'migo-video-probe: \(PASS\|FAIL\).*' | head -1 || true)"
  [[ -n "$verdict" ]] && break
  sleep 1
done
"$ADB" shell am force-stop "$PKG"
[[ -n "$verdict" ]] || fail "the probe said nothing within a minute"
[[ "$verdict" == *PASS* ]] || fail "$verdict"
echo "PASS: ${verdict#migo-video-probe: PASS }"
