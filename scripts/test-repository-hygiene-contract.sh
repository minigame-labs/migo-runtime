#!/usr/bin/env bash
# Mutation contract for the tracked-text publication boundary.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CHECKER="$ROOT/scripts/check-repository-hygiene.py"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
failures=0

pass() { printf '\033[0;32m[ok]\033[0m %s\n' "$*"; }
fail() { printf '\033[0;31m[FAIL]\033[0m %s\n' "$*" >&2; failures=$((failures + 1)); }

run_gate() { python3 "$CHECKER" --root "$1" >/dev/null 2>&1; }
expect_pass() {
    if run_gate "$2"; then pass "$1"; else fail "$1"; fi
}
expect_fail() {
    if run_gate "$2"; then fail "$1"; else pass "$1"; fi
}

if [[ ! -f "$CHECKER" ]]; then
    fail "tracked-text hygiene checker exists"
else
    fixture="$WORK/repo"
    mkdir -p "$fixture"
    git -C "$fixture" init -q
    printf 'neutral public text\n' > "$fixture/README.md"
    git -C "$fixture" add README.md
    expect_pass "neutral tracked text is accepted" "$fixture"

    # Write forbidden fixture bytes from hex so the gate source and this test do
    # not themselves publish the token they are meant to detect.
    python3 - "$fixture/brand.md" <<'PY'
import pathlib, sys
pathlib.Path(sys.argv[1]).write_bytes(bytes.fromhex("63 6f 6d 70 61 74 20 77 78 20 6e 61 6d 65 73 70 61 63 65 0a"))
PY
    git -C "$fixture" add brand.md
    expect_fail "a tracked legacy brand token is rejected" "$fixture"
    git -C "$fixture" rm -q --cached brand.md

    python3 - "$fixture/brand-prefix.md" <<'PY'
import pathlib, sys
pathlib.Path(sys.argv[1]).write_bytes(bytes.fromhex(
    "73 63 6f 70 65 2e 57 78 46 72 69 65 6e 64 49 6e 74 65 72 61 63 74 69 6f 6e 0a"
))
PY
    git -C "$fixture" add brand-prefix.md
    expect_fail "a tracked prefixed legacy identifier is rejected" "$fixture"
    git -C "$fixture" rm -q --cached brand-prefix.md

    # Two shapes carry the short brand's letters without being the brand: a
    # Subresource Integrity digest in a lockfile, and the exclusive-create flag
    # of the engine's file open op. Both must pass -- and the same two letters as
    # a quoted global name must still fail, or neutralising them has widened the
    # rule instead of sharpening it.
    python3 - "$fixture/lock.json" "$fixture/open.js" "$fixture/global.js" <<'PY'
import pathlib, sys
w, x = chr(0x77), chr(0x78)
digest = "sha512-" + w + x.upper() + "R/dYpcqKmfWpEdZjiKJOwCNFndD0DMnrW/cYjVGttEkBfVgcLFHoNrlj47mjOVic9yyNu65alsgF4NQyTa2g=="
pathlib.Path(sys.argv[1]).write_text('{"integrity": "%s"}\n' % digest)
pathlib.Path(sys.argv[2]).write_text('fd = await op_open_file(tmpPath, "%s%s");\n' % (w, x))
pathlib.Path(sys.argv[3]).write_text('const api = globalThis["%s%s"];\n' % (w, x))
PY
    git -C "$fixture" add lock.json open.js
    expect_pass "an integrity digest and the exclusive open flag are not the brand" "$fixture"
    git -C "$fixture" add global.js
    expect_fail "the brand as a quoted global name is still rejected" "$fixture"
    git -C "$fixture" rm -q --cached lock.json open.js global.js
    python3 - "$fixture/path.md" <<'PY'
import pathlib, sys
pathlib.Path(sys.argv[1]).write_bytes(bytes.fromhex(
    "62 75 69 6c 64 3d 2f 68 6f 6d 65 2f 61 6c 69 63 65 2f 77 6f 72 6b 0a"
))
PY
    git -C "$fixture" add path.md
    expect_fail "a tracked user-home path is rejected" "$fixture"
    git -C "$fixture" rm -q --cached path.md

    python3 - "$fixture/secret.pem" <<'PY'
import pathlib, sys
pathlib.Path(sys.argv[1]).write_bytes(bytes.fromhex(
    "2d 2d 2d 2d 2d 42 45 47 49 4e 20 50 52 49 56 41 54 45 20 4b 45 59 2d 2d 2d 2d 2d 0a"
))
PY
    git -C "$fixture" add secret.pem
    expect_fail "tracked private signing material is rejected" "$fixture"
    git -C "$fixture" rm -q --cached secret.pem

    mkdir -p "$fixture/.agents"
    printf 'local state\n' > "$fixture/.agents/state.json"
    git -C "$fixture" add -f .agents/state.json
    expect_fail "tracked local agent state is rejected" "$fixture"
    git -C "$fixture" rm -qr --cached .agents

    # The publication boundary is the index. Local untracked notes are ignored,
    # while the identical bytes become a failure the moment they are staged.
    expect_pass "untracked local files are outside the publication boundary" "$fixture"
fi

if (( failures > 0 )); then
    printf 'Repository hygiene contract: FAIL (%d assertion(s))\n' "$failures" >&2
    exit 1
fi
echo "Repository hygiene contract: PASS"
