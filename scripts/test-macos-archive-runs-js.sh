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

echo "[2/2] running a game through it, with ANGLE from $ANGLE_DIR"
DYLD_LIBRARY_PATH="$ANGLE_DIR${DYLD_LIBRARY_PATH:+:$DYLD_LIBRARY_PATH}" \
  "$HOST" "$WORK/files" "$CONTENT_ID" ${DRAWABLES:+"$DRAWABLES"} \
  || fail "the shipping archive did not run the content to completion"

echo "PASS: the shipping macOS archive evaluated JavaScript, turned frames on ANGLE/Metal against a windowless CAMetalLayer, and installed the migo surface"
