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
    --keep) KEEP=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *) fail "unknown argument: $1" ;;
  esac
done

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

CONTENT_ID="headless-js-probe"
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
  -lc++ -lobjc -lz \
  || fail "the host did not link against $ARCHIVE"

echo "[2/2] running a game through it"
"$HOST" "$WORK/files" "$CONTENT_ID" || fail "the shipping archive did not run the content to completion"

echo "PASS: the shipping macOS archive evaluated JavaScript and installed the migo surface"
