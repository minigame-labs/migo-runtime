#!/usr/bin/env bash
# The export lists every SDK is linked and checked against come from one place,
# scripts/c-abi-entry-points.py, and it holds two rules.
#
# Before it, four scripts grepped `migo_*(` out of every public header. When
# external_frames.h landed (the Apple external-frames product's entry points,
# compiled only with that feature), the next release's Windows export allowlist
# named 21 functions the embedded DLL does not have and both Windows jobs failed
# at the link; the Linux and Android export checks would have refused the same
# set. The grep also read prose: session.h mentioning
# "migo_session_copy_content_root (external_frames.h)" declared it.
#
# Checked here, without building anything:
#   1. the embedded set is the external-frames set minus exactly what
#      external_frames.h declares, and is not empty;
#   2. a name mentioned only in a comment is not an entry point.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TOOL="$ROOT/scripts/c-abi-entry-points.py"
fail() {
  echo "c-abi entry points contract FAILED: $*" >&2
  exit 1
}

embedded="$(python3 "$TOOL" embedded "$ROOT/include/migo")"
all="$(python3 "$TOOL" external-frames "$ROOT/include/migo")"
[[ -n "$embedded" ]] || fail "the embedded product has no entry points"

WORK="$(mktemp -d "${TMPDIR:-/tmp}/migo-entry-points.XXXXXX")"
trap 'rm -rf "$WORK"' EXIT
mkdir -p "$WORK/only-frames"
cp "$ROOT/include/migo/migo.h" "$ROOT/include/migo/external_frames.h" "$WORK/only-frames/"
frames="$(python3 "$TOOL" external-frames "$WORK/only-frames" | grep -vxF -f <(python3 "$TOOL" embedded "$WORK/only-frames") || true)"
[[ -n "$frames" ]] || fail "external_frames.h declares no entry point this check can see"
expected="$(comm -23 <(echo "$all") <(echo "$frames"))"
[[ "$embedded" == "$expected" ]] \
  || fail "the embedded set is not every entry point minus external_frames.h's: $(diff <(echo "$expected") <(echo "$embedded") | tr '\n' ' ')"

mkdir -p "$WORK/comment"
cat > "$WORK/comment/migo.h" <<'H'
/* See migo_only_in_a_comment (other.h) for that. */
// and migo_only_in_a_line_comment(
int migo_declared(void);
H
got="$(python3 "$TOOL" embedded "$WORK/comment")"
[[ "$got" == "migo_declared" ]] || fail "comments were read as declarations: $(echo "$got" | tr '\n' ' ')"

echo "c-abi entry points contract: PASS ($(echo "$embedded" | wc -l | tr -d ' ') embedded, $(echo "$all" | wc -l | tr -d ' ') with external frames)"
