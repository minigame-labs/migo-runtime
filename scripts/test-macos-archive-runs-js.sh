#!/usr/bin/env bash
# The shipping macOS archive evaluates JavaScript, not just links.
#
# Every other check on an assembled Migo archive is a link check or a symbol
# audit. `nm` says the engine is in there; `xcodebuild` says a consumer resolves
# against it; the V8 lane's own step runs `cargo test -p migo-runtime-v8`, which
# proves the *V8* archive evaluates script and says nothing about ours. All of
# them pass on an archive that cannot run a game, and the first person to notice
# would be whoever integrated it.
#
# So this links tests/c_host/macos-headless/main.m against the archive the build
# produced and runs a game through it. The content sums 1..1000 -- a value that
# has to be executed to exist -- and calls migo.exitMiniProgram(), so a pass
# means V8 ran and the migo capability surface was installed. The host includes
# nothing but the public headers.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ARCHIVE=""
BUILD_ROOT="${MIGO_APPLE_BUILD_ROOT:-$ROOT/build/apple}"
PRODUCT="macos-v8"
CONFIGURATION="Debug"
ANGLE_DIR=""
DRAWABLES=""
HARDEN="off"
PROBE_STATUS=0
PROBE_REPORT="the JIT entitlement probe did not run"
CONTENT="headless-js-probe"
KEEP=0

fail() {
  echo "FAIL: $*" >&2
  exit 1
}

usage() {
  cat <<'USAGE'
usage: test-macos-archive-runs-js.sh [--archive <libmigo.a>] [options]

  --archive <path>       The static library to link. Defaults to the macos-v8
                         product under the Apple build root.
  --product <name>       Which product's archive to default to (macos-v8).
  --configuration <cfg>  Debug or Release. Default Debug.
  --angle <dir>          Where libEGL.dylib and libGLESv2.dylib live. Defaults
                         to the pinned runtime dependency that
                         scripts/fetch-apple-angle.sh macos installs.
  --content <name>       Which fixture under scripts/fixtures to run. Default
                         headless-js-probe, which proves the archive runs. Use
                         headless-perf-probe for a throughput measurement.
  --harden <mode>        How to sign the host before running it.
                           off          unsigned (the default)
                           with-jit     hardened runtime + allow-jit, which is
                                        the configuration macos-v8 ships in
                           without-jit  hardened runtime, entitlement withheld
                         The last one is a negative control: the README says a
                         missing entitlement selects a WebKit lane and must NOT
                         silently become a jitless V8, and that promise is
                         checkable only by withholding it.
  --drawables <2|3>      maximumDrawableCount for the layer. Omitted leaves the
                         layer's own default, which is not the same as asking for
                         it.
  --keep                 Leave the staged game and the built host in place.

The archive is a required input rather than something this script builds: the
build is measured in hours and belongs to whoever ran it, and a test that
silently rebuilds cannot tell "the archive is broken" from "the archive I made
is broken".
USAGE
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --archive) [[ $# -ge 2 ]] || fail "--archive needs a path"; ARCHIVE="$2"; shift 2 ;;
    --product) [[ $# -ge 2 ]] || fail "--product needs a name"; PRODUCT="$2"; shift 2 ;;
    --configuration) [[ $# -ge 2 ]] || fail "--configuration needs a value"; CONFIGURATION="$2"; shift 2 ;;
    --angle) [[ $# -ge 2 ]] || fail "--angle needs a directory"; ANGLE_DIR="$2"; shift 2 ;;
    --content) [[ $# -ge 2 ]] || fail "--content needs a fixture name"; CONTENT="$2"; shift 2 ;;
    --harden) [[ $# -ge 2 ]] || fail "--harden needs off, with-jit or without-jit"; HARDEN="$2"; shift 2 ;;
    --drawables) [[ $# -ge 2 ]] || fail "--drawables needs 2 or 3"; DRAWABLES="$2"; shift 2 ;;
    --keep) KEEP=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *) fail "unknown argument: $1" ;;
  esac
done

# Argument problems are named before anything expensive happens, and before the
# platform check, because they are true on any machine. A bad --drawables would
# otherwise be found by the host after a link, and a misspelled --content by a
# `cp` reporting a path.
case "$HARDEN" in
  off|with-jit|without-jit) ;;
  *) fail "--harden takes off, with-jit or without-jit; got '$HARDEN'" ;;
esac
if [[ -n "$DRAWABLES" && "$DRAWABLES" != "2" && "$DRAWABLES" != "3" ]]; then
  fail "--drawables takes 2 or 3 (CAMetalLayer allows 2..3); got '$DRAWABLES'"
fi
if [[ ! -d "$ROOT/scripts/fixtures/$CONTENT" ]]; then
  fail "no fixture named '$CONTENT' under scripts/fixtures. The ones this test uses are: $(cd "$ROOT/scripts/fixtures" && ls -d headless-* 2>/dev/null | tr '\n' ' ')"
fi

[[ "$(uname -s)" == "Darwin" ]] || fail "this test links a macOS archive and needs macOS"

if [[ -z "$ARCHIVE" ]]; then
  ARCHIVE="$BUILD_ROOT/$PRODUCT/$CONFIGURATION/macos/libmigo.a"
fi
[[ -f "$ARCHIVE" ]] || fail "no archive at $ARCHIVE. Build one with scripts/build-apple-sdk.sh --platform macos --product $PRODUCT, or point --archive at yours"

# external-frames products carry no engine on purpose, so linking one here would
# fail at the first undefined symbol with a message about migo_engine_create
# rather than about the mistake. Say it before the linker does.
case "$ARCHIVE" in
  *external-frames*|*performance-plus*)
    fail "$ARCHIVE is an external-frames product: it carries no engine by design, so it cannot run JavaScript and this test is not about it" ;;
esac

# The archive is static and the renderer is not: macos-v8 links Migo's engine in
# and loads ANGLE's Metal backend at runtime, exactly as an app consumer does
# through the shipped `Frameworks/Scripts/embed-apple-angle.sh`. A CLI host has
# no embedding phase, so its loader directory is explicit -- the same shape the
# diagnostic package's own test step uses. Without it the run gets as far as
# attach and then fails with `CanvasManager init failed`, which reads like an
# engine defect and is a missing dylib.
if [[ -z "$ANGLE_DIR" ]]; then
  ANGLE_DIR="$ROOT/engine/third_party/angle-apple-macos"
fi
for lib in libEGL.dylib libGLESv2.dylib; do
  [[ -f "$ANGLE_DIR/$lib" ]] || fail "no $lib in $ANGLE_DIR. Install the pinned runtime with scripts/fetch-apple-angle.sh macos, or point --angle at a directory holding both"
done

WORK="$(mktemp -d "${TMPDIR:-/tmp}/migo-macos-js.XXXXXX")"
cleanup() {
  if ((KEEP == 0)); then
    rm -rf "$WORK"
  else
    echo "kept: $WORK"
  fi
  return 0
}
trap cleanup EXIT

CONTENT_ID="$CONTENT"
CODE_DIR="$WORK/files/migo/games/$CONTENT_ID/code"
mkdir -p "$CODE_DIR" "$WORK/cache" "$WORK/code-cache"
for name in game.json game.js; do
  cp "$ROOT/scripts/fixtures/$CONTENT_ID/$name" "$CODE_DIR/$name"
done

HOST="$WORK/macos-headless-host"
echo "[1/2] linking the host against $ARCHIVE"
# The frameworks are the archive's own dependencies, not this host's: it uses
# QuartzCore for the layer and nothing else. They are listed here because a
# static archive carries no link-time dependency record.
clang -fobjc-arc -fmodules -O0 -g \
  -I "$ROOT/include" \
  -o "$HOST" \
  -Wl,-rpath,@executable_path \
  "$ROOT/tests/c_host/macos-headless/main.m" \
  "$ARCHIVE" \
  -framework QuartzCore -framework Metal -framework Foundation \
  -framework CoreFoundation -framework CoreGraphics -framework IOKit \
  -framework IOSurface -framework AppKit -framework Security \
  -framework CoreText -framework CoreServices -framework AudioToolbox \
  -framework AVFoundation -framework CoreMedia -framework CoreAudio \
  -framework SystemConfiguration -framework GameController \
  -lc++ -lz \
  || fail "the host did not link against $ARCHIVE"

# Beside the binary, reached through @executable_path, and not through
# DYLD_LIBRARY_PATH. Two reasons, and the second is the one that matters:
# ANGLE's dylibs carry `@rpath/libEGL.dylib` install names, so this is what a
# consumer's embed phase arranges; and a HARDENED RUNTIME IGNORES EVERY DYLD_*
# VARIABLE, so a test that relied on one could never run the configuration
# macos-v8 actually ships in.
cp "$ANGLE_DIR/libEGL.dylib" "$ANGLE_DIR/libGLESv2.dylib" "$(dirname "$HOST")/"

if [[ "$HARDEN" != "off" ]]; then
  # Ad-hoc, because the entitlement is what is being tested and a Developer ID
  # would add nothing to it: the kernel reads entitlements out of the signature
  # whoever made it. Distribution is the integrator's -- they notarize their app
  # with their own identity -- so this asks the only question that is ours,
  # which is whether the engine works in the shape they will ship it in.
  ENTITLEMENTS="$WORK/host.entitlements"
  {
    echo '<?xml version="1.0" encoding="UTF-8"?>'
    echo '<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">'
    echo '<plist version="1.0"><dict>'
    if [[ "$HARDEN" == "with-jit" ]]; then
      echo '  <key>com.apple.security.cs.allow-jit</key><true/>'
    fi
    echo '</dict></plist>'
  } > "$ENTITLEMENTS"
  # The dylibs are signed too: a hardened-runtime process refuses to load an
  # unsigned library, so signing only the executable would fail at ANGLE rather
  # than at the entitlement, and the failure would be read as the wrong finding.
  for binary in "$(dirname "$HOST")/libEGL.dylib" "$(dirname "$HOST")/libGLESv2.dylib"; do
    codesign --force --options runtime --sign - "$binary" >/dev/null 2>&1 \
      || fail "could not ad-hoc sign $binary"
  done
  codesign --force --options runtime --entitlements "$ENTITLEMENTS" --sign - "$HOST" \
    || fail "could not ad-hoc sign the host with a hardened runtime"
  echo "  signed: hardened runtime, allow-jit=$([[ "$HARDEN" == "with-jit" ]] && echo yes || echo no)"
  codesign -d --entitlements - "$HOST" 2>&1 | sed 's/^/    /'

  # Does withholding the entitlement actually deny executable memory on this
  # machine? Asked with no V8 in the way, and signed the same way the host was,
  # so the control mirrors the measurement rather than approximating it. The
  # answer decides how the run below may be read: see the without-jit branch.
  JIT_PROBE="$WORK/jit-entitlement-probe"
  clang -O0 -o "$JIT_PROBE" "$ROOT/tests/c_host/jit-entitlement-probe/main.c" \
    || fail "could not build the JIT entitlement probe"
  codesign --force --options runtime --entitlements "$ENTITLEMENTS" --sign - "$JIT_PROBE" \
    || fail "could not ad-hoc sign the JIT entitlement probe"
  set +e
  PROBE_REPORT="$("$JIT_PROBE" 2>&1)"
  PROBE_STATUS=$?
  set -e
  echo "  $PROBE_REPORT"
fi

echo "[2/2] running a game through it, with ANGLE beside it (harden=$HARDEN)"
HOST_LOG="$WORK/host.log"
set +e
"$HOST" "$WORK/files" "$CONTENT_ID" ${DRAWABLES:+"$DRAWABLES"} > "$HOST_LOG" 2>&1
HOST_STATUS=$?
set -e
cat "$HOST_LOG"

# What the content reported about the V8 it was actually running on. The gate
# used to infer this from the exit status; see the fixture for why that was
# wrong in both directions.
JIT_FLOOR_MIPS=100
report_line() {
  grep -o 'migo-headless-probe: v8 wasm=[a-z]* mips=[0-9.]* acc=[0-9-]*' "$HOST_LOG" | tail -1
}

assert_v8_has_jit() {
  local report wasm mips
  report="$(report_line || true)"
  # An absent report is a failure, not a pass. A measurement that quietly stops
  # being taken is the failure mode this repository has already been bitten by:
  # a suite that reported "0 assertions" instead of going red.
  [[ -n "$report" ]] || fail "the content never reported which V8 it was running on. The fixture prints that line through console.error and it is missing from the host's output, so this run measured nothing -- fix the reporting before reading anything else here"
  wasm="${report#*wasm=}"; wasm="${wasm%% *}"
  mips="${report#*mips=}"; mips="${mips%% *}"
  echo "  V8 reports: WebAssembly=$wasm, warm loop $mips M it/s (floor $JIT_FLOOR_MIPS)"
  # WebAssembly is the uncalibrated half of the answer: a jitless V8 does not
  # slow it down, it deletes it. CLAUDE.md records the same finding from the
  # HarmonyOS NEXT measurement, where `typeof WebAssembly` came back undefined.
  [[ "$wasm" == "object" ]] || fail "V8 ran without WebAssembly (typeof WebAssembly = $wasm). That is the jitless signature: the .wasm.br bundles every Cocos and Unity export ships would not load at all"
  awk -v m="$mips" -v f="$JIT_FLOOR_MIPS" 'BEGIN { exit !(m >= f) }' \
    || fail "V8 turned the warm loop at $mips M it/s, under the $JIT_FLOOR_MIPS floor that separates a JIT from an interpreter. The archive links and runs, and runs interpreted"
}

if [[ "$HARDEN" == "without-jit" ]]; then
  # Read the instrument before reading the measurement.
  #
  # This step withholds `com.apple.security.cs.allow-jit` and used to assert
  # that no working V8 came out the other side. That assertion has a load-
  # bearing premise -- that withholding the entitlement denies executable
  # memory -- and the premise is false for the strongest signature this project
  # can produce. Measured 2026-09-11 on macOS 26.6: an ad-hoc signature with
  # `--options runtime` really does carry the runtime flag (`flags=0x10002
  # (adhoc,runtime)`) and MAP_JIT and mprotect(PROT_EXEC) are both GRANTED
  # anyway, with the entitlement and without it. Enforcement wants a real
  # signing identity, which wants a paid account, which is not ours to hold:
  # the integrator notarises their app.
  #
  # So a completed run here never meant what the old failure text said it did.
  # It read a run that completed as "V8 silently fell back to jitless", and that
  # fallback does not exist -- jitless is a build-time V8 flag, and a V8 denied
  # executable memory dies rather than degrading.
  if ((PROBE_STATUS == 0)); then
    echo "CONTROL NOT ESTABLISHED: withholding allow-jit did not deny executable memory on this machine."
    echo "  $PROBE_REPORT"
    echo "  The hardened runtime flag is set and the entitlement is absent, and the kernel granted JIT"
    echo "  memory regardless, so this run cannot say anything about what V8 does without it. The check"
    echo "  below is the one that still holds and is the one worth having: it measures whether the V8"
    echo "  that ran had a JIT, which catches a jitless engine shipping no matter what caused it."
    echo "  This turns into a real negative control the moment the probe above reports 'denied' -- on a"
    echo "  stricter OS, on arm64, or under a Developer ID signature."
  else
    echo "  the entitlement is enforced here: $PROBE_REPORT"
    if ((HOST_STATUS == 0)); then
      assert_v8_has_jit
      fail "executable memory was denied and V8 ran a full-speed JIT anyway, which cannot both be true. Read the probe line above before the V8 line: one of the two is measuring something other than what it names"
    fi
    if ((HOST_STATUS > 128)); then
      echo "PASS (partial): nothing silently degraded -- but the host died on signal $((HOST_STATUS - 128)) rather than declining."
      echo "  A crash is not the behaviour Sources/MigoMacV8/README.md promises: a missing entitlement is"
      echo "  supposed to make the profile resolver select a WebKit lane, and that resolver is still"
      echo "  Placeholder.swift. What this run establishes is the negative half: no silent jitless V8."
    else
      echo "PASS: without the entitlement the host declined with exit $HOST_STATUS rather than running a jitless V8"
    fi
    exit 0
  fi
fi

((HOST_STATUS == 0)) || fail "the shipping archive did not run the content to completion (exit $HOST_STATUS)"
assert_v8_has_jit

echo "PASS: the shipping macOS archive evaluated JavaScript on a V8 with a working JIT, turned frames on ANGLE/Metal against a windowless CAMetalLayer, and installed the migo surface"
