#!/usr/bin/env bash
# The macOS product surface runs a game, signed the way it ships.
#
# `test-macos-archive-runs-js.sh` proves the archive runs a game through the C
# ABI, from a host written against the headers. What an integrator links is not
# that: it is `MigoMacV8.MigoGameView`, which owns the engine session, the layer,
# the display clock, input and teardown. Until this gate nothing had run it, so
# the one piece of the macOS product an app actually touches was the piece with
# no evidence.
#
# So this builds tests/swift_host/macos-game-view -- an app of sixty lines that
# installs a package with MigoGameInstaller, puts a MigoGameView in a window and
# loads it -- signs it with the hardened runtime, and runs it twice:
#
#   with allow-jit     the game must become ready, turn frames on the view's
#                      display clock, draw Canvas2D, report a V8 with a JIT,
#                      and ask to exit.
#   signed / tampered  the package signed with a fresh Ed25519 key and loaded
#                      with contentSigning .verified: it runs; the same package
#                      with one byte of game.js changed after signing is refused.
#                      This is the key travelling MigoEngineConfig's
#                      code_signing_public_key into the engine's verifier.
#   without allow-jit  the view must REFUSE to start and name the entitlement.
#                      This is the view's policy, not the kernel's: an ad-hoc
#                      signature does not get JIT denied (see the other
#                      script's without-jit branch), so the only thing that
#                      keeps a jitless-or-dead V8 from shipping is the view
#                      reading its own signature. This run is what shows it does.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
FIXTURE="$ROOT/scripts/fixtures/headless-js-probe"
HOST_PACKAGE="$ROOT/tests/swift_host/macos-game-view"
PACKAGE="$ROOT/platforms/apple"
KEEP=0
while [[ $# -gt 0 ]]; do
  case "$1" in
    # The Swift package to run against. Defaults to the source tree; the release
    # job points it at the unpacked release asset, so what is proven to run is
    # what is published rather than what it was made from.
    --package) PACKAGE="$(cd "${2:?--package needs a directory}" && pwd)"; shift 2 ;;
    --keep) KEEP=1; shift ;;
    *) echo "usage: test-macos-game-view.sh [--package DIR] [--keep]" >&2; exit 2 ;;
  esac
done

fail() {
  echo "FAIL: $*" >&2
  exit 1
}

[[ "$(uname -s)" == "Darwin" ]] || fail "this runs a macOS app and needs macOS"
[[ -d "$PACKAGE/Frameworks/MigoEngine.xcframework" ]] \
  || fail "no MigoEngine.xcframework in $PACKAGE; build it with scripts/build-apple-sdk.sh --platform macos --product macos-v8"
# A locked console occludes every window, and an occluded view gets no
# display-link ticks -- by design, as for any app -- so the game would become
# ready and never turn a frame. Said here, before a two-minute timeout says it
# less clearly. (Measured on the lab Mac: CGSSessionScreenIsLocked, and the host
# reported visible=false from its first second.)
if ioreg -n Root -d1 -a 2>/dev/null | grep -A1 CGSSessionScreenIsLocked | grep -q '<true/>'; then
  fail "the console session is locked, so every window is occluded and no view gets frames. Unlock the Mac's screen (or run on a CI runner, whose session is unlocked) and rerun"
fi

EMBED="$PACKAGE/Frameworks/Scripts/embed-apple-angle.sh"
[[ -f "$EMBED" ]] || fail "the package has no Frameworks/Scripts/embed-apple-angle.sh; it is part of the SDK"

WORK="$(mktemp -d "${TMPDIR:-/tmp}/migo-game-view.XXXXXX")"
cleanup() {
  if ((KEEP == 0)); then
    # The engine seals a verified package read-only; owner write comes back
    # first, as any installer's trusted uninstall does.
    chmod -R u+w "$WORK" 2>/dev/null
    rm -rf "$WORK"
  else
    echo "kept: $WORK"
  fi
  return 0
}
trap cleanup EXIT

# A copy of the host whose one dependency names $PACKAGE, so the committed
# manifest keeps its relative path and the run can still point anywhere.
cp -R "$HOST_PACKAGE" "$WORK/host"
python3 - "$WORK/host/Package.swift" "$PACKAGE" <<'PY'
import pathlib, sys
manifest = pathlib.Path(sys.argv[1])
text = manifest.read_text()
old = 'path: "../../../platforms/apple"'
if old not in text:
    sys.exit("the host manifest no longer names ../../../platforms/apple; update this script")
manifest.write_text(text.replace(old, f'path: "{sys.argv[2]}"'))
PY
HOST_PACKAGE="$WORK/host"

echo "[1/4] building the host app against $PACKAGE"
swift build --package-path "$HOST_PACKAGE" --scratch-path "$WORK/build" -c debug \
  > "$WORK/build.log" 2>&1 || { tail -40 "$WORK/build.log"; fail "the host did not build"; }
BIN_DIR="$(swift build --package-path "$HOST_PACKAGE" --scratch-path "$WORK/build" -c debug --show-bin-path)"
HOST="$WORK/app/MigoGameViewHost"
mkdir -p "$WORK/app"
cp "$BIN_DIR/MigoGameViewHost" "$HOST"
# ANGLE beside the binary, put there by the helper the SDK ships for exactly
# this -- the build phase an integrator adds -- rather than copied from this
# repository, so a helper that stopped working would fail here first. Beside
# the binary because ANGLE's install names are @rpath and a hardened runtime
# ignores DYLD_* variables.
bash "$EMBED" --frameworks-dir "$PACKAGE/Frameworks" --destination "$WORK/app" \
  --architectures "$(uname -m | sed 's/aarch64/arm64/')" > /dev/null \
  || fail "the SDK's embed-apple-angle.sh could not install ANGLE"
install_name_tool -add_rpath @executable_path "$HOST" 2>/dev/null || true
# The SwiftPM resource bundles the products carry, where Bundle.module looks.
find "$BIN_DIR" -maxdepth 1 -name '*.bundle' -exec cp -R {} "$WORK/app/" \;

sign() {
  local entitlements="$WORK/$1.entitlements"
  {
    echo '<?xml version="1.0" encoding="UTF-8"?>'
    echo '<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">'
    echo '<plist version="1.0"><dict>'
    [[ "$1" == "with-jit" ]] && echo '  <key>com.apple.security.cs.allow-jit</key><true/>'
    echo '</dict></plist>'
  } > "$entitlements"
  for lib in "$WORK/app/libEGL.dylib" "$WORK/app/libGLESv2.dylib"; do
    codesign --force --options runtime --sign - "$lib" >/dev/null 2>&1 || fail "could not sign $lib"
  done
  codesign --force --options runtime --entitlements "$entitlements" --sign - "$HOST" >/dev/null 2>&1 \
    || fail "could not sign the host ($1)"
}

run() {
  local log="$WORK/$1-${2:-unsigned}.log" status
  set +e
  MIGO_CAPI_LOG=info "$HOST" "$FIXTURE" "$WORK/data-$1-${2:-unsigned}" 120 "${2:-unsigned}" > "$log" 2>&1
  status=$?
  set -e
  grep -E '^\[game-view-host\]|migo-headless-probe' "$log" | sed 's/^/    /'
  LAST_LOG="$log"
  return $status
}

echo "[2/4] hardened runtime WITH allow-jit: the game runs"
sign with-jit
status=0
run with-jit || status=$?
((status == 0)) || { tail -60 "$LAST_LOG"; fail "the game did not run to exit through MigoGameView (exit $status)"; }
grep -q '\[game-view-host\] unavailability: none' "$LAST_LOG" \
  || fail "the view thought it could not run in a process signed with allow-jit"
probe="$(grep -o 'migo-headless-probe: v8 wasm=[a-z]* mips=[0-9.]*' "$LAST_LOG" | tail -1)"
[[ -n "$probe" ]] || fail "the content never reported its V8; the engine log did not reach the app's output"
[[ "$probe" == *"wasm=object"* ]] || fail "V8 ran without WebAssembly: $probe"
mips="${probe##*mips=}"
awk -v m="$mips" 'BEGIN { exit !(m >= 100) }' || fail "V8 ran interpreted ($mips M it/s): $probe"
canvas="$(grep -o 'migo-headless-probe: canvas2d .*' "$LAST_LOG" | tail -1)"
[[ "$canvas" == *"rgba=0,128,255,255"* ]] || fail "Canvas2D did not draw what the fixture filled: ${canvas:-no report}"
# The device, through the view: the network NWPathMonitor reported (a runner
# is connected), the display wake lock taken, and the game's log entry handed
# to the app as an event.
device="$(grep -o 'migo-headless-probe: device .*' "$LAST_LOG" | tail -1)"
[[ "$device" == *"/true keep=ok log=ok"* ]] || fail "the device did not answer through MigoGameView: ${device:-no report}"
grep -q '\[game-view-host\] game log: .*"key":"probe"' "$LAST_LOG" \
  || fail "the game's log entry did not reach the app"

echo "[3/4] signed content: verified with its key runs, tampered is refused"
status=0
run with-jit signed || status=$?
((status == 0)) || { tail -40 "$LAST_LOG"; fail "a correctly signed package did not run under contentSigning .verified (exit $status)"; }
status=0
run with-jit tampered || status=$?
((status == 4)) || { tail -40 "$LAST_LOG"; fail "a package changed after signing was not refused (exit $status)"; }
grep -qiE 'hash|integrity|signature' "$LAST_LOG" \
  || fail "the tampered package was refused without saying it failed verification"
grep -q '\[game-view-host\] ready' "$LAST_LOG" && fail "the tampered package became ready before it was refused"

echo "[4/4] hardened runtime WITHOUT allow-jit: the view refuses, and says why"
sign without-jit
status=0
run without-jit || status=$?
((status == 3)) || { tail -40 "$LAST_LOG"; fail "without allow-jit the view should refuse to start (exit 3), got exit $status"; }
grep -q 'allow-jit is not in this process' "$LAST_LOG" \
  || fail "the view refused without naming the missing entitlement"

echo "PASS: MigoGameView ran the game with JIT, verified signed content and refused tampered content, and refused to start without allow-jit"
