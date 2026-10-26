#!/usr/bin/env bash
# Cargo outputs are read from where cargo wrote them.
#
# Scripts built with cargo and then named engine/target/... themselves. Cargo writes wherever
# CARGO_TARGET_DIR, CARGO_BUILD_TARGET_DIR or build.target-dir says, so with any of them set the
# build succeeded and the script went on with whatever an earlier build had left at the literal
# path. Nothing failed: five player gates and the idle-wakeup instrument ran a player from before
# the change they were checking and passed, and the Android C host packaged a staticlib from
# August. That one was caught only because a symbol had been added since -- a staticlib one commit
# old would have linked, and the APK would have tested the wrong engine.
#
#   1. cargo_target_dir (scripts/lib/cargo-target-dir.sh) answers what cargo answers: the
#      default, CARGO_TARGET_DIR and CARGO_BUILD_TARGET_DIR.
#   2. No script names a profile directory (debug/, release/, <triple>/release/) under a literal
#      target directory -- unless the same file hands cargo that directory itself with
#      --target-dir, so the literal is the directory it chose. Comments are not paths.
#
# Two kinds of file keep a literal, each only while the reason holds:
#   * a V8 recipe whose bytes are still sealed into a component manifest as the provenance of the
#     archive it built (provenance.build_recipe_sha256). Changing a line voids that provenance,
#     so it changes only with a rebuild -- and the first edit makes this gate name its literal.
#   * a file that runs no cargo and looks for what an earlier build in a sibling checkout left at
#     its default place. The environment it sees says nothing about where that build wrote.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=scripts/lib/cargo-target-dir.sh
source "$ROOT/scripts/lib/cargo-target-dir.sh"
failures=0
fail() { echo "FAIL: $*" >&2; failures=$((failures + 1)); }

# --- 1. the helper ------------------------------------------------------------
resolved() { # <env assignments...> -> what the helper says for engine/
    env -u CARGO_TARGET_DIR -u CARGO_BUILD_TARGET_DIR "$@" \
        bash -c 'source "$0/scripts/lib/cargo-target-dir.sh" && cargo_target_dir "$0/engine"' "$ROOT"
}
[[ "$(resolved)" == "$ROOT/engine/target" ]] \
    || fail "with nothing set the helper says $(resolved), not $ROOT/engine/target"
elsewhere="$(mktemp -d)"
trap 'rm -rf "$elsewhere"' EXIT
[[ "$(resolved CARGO_TARGET_DIR="$elsewhere/a")" == "$elsewhere/a" ]] \
    || fail "the helper does not follow CARGO_TARGET_DIR"
[[ "$(resolved CARGO_BUILD_TARGET_DIR="$elsewhere/b")" == "$elsewhere/b" ]] \
    || fail "the helper does not follow CARGO_BUILD_TARGET_DIR"

# --- 2. no literal profile directory ------------------------------------------
python3 "$ROOT/scripts/testing/test_snapshot_target_dir.py"
literals="$(python3 - "$ROOT" <<'PY'
import hashlib, json, pathlib, re, sys
root = pathlib.Path(sys.argv[1])
# A directory named exactly `target`, then an optional triple, then a profile directory.
literal = re.compile(r"""(?:(?<=/)|(?<=^)|(?<=["'\s=]))target/(?:[^/\s"'()]+/)?(?:debug|release)/""")
runs_cargo = re.compile(r"\bcargo\s")
DISCOVERY = {"scripts/lib/host-v8.sh", "scripts/lib/linux-sysroot.sh"}

sealed = set()
for manifest in root.glob("engine/third_party/rusty_v8/*/component-manifest.json"):
    provenance = json.loads(manifest.read_text()).get("provenance", {})
    if provenance.get("build_recipe") and provenance.get("build_recipe_sha256"):
        sealed.add((provenance["build_recipe"], provenance["build_recipe_sha256"]))

files = sorted(
    p for p in [*root.glob("scripts/**/*.sh"), *root.glob("scripts/*.py")]
    if "scripts/ci/tests" not in p.as_posix()
)
for path in files:
    name = path.relative_to(root).as_posix()
    raw = path.read_bytes()
    text = raw.decode("utf-8", errors="replace")
    if "--target-dir" in text:
        continue
    if (name, hashlib.sha256(raw).hexdigest()) in sealed:
        continue
    code = [(n, line) for n, line in enumerate(text.splitlines(), 1) if not line.lstrip().startswith("#")]
    if name in DISCOVERY and not any(runs_cargo.search(line) for _, line in code):
        continue
    for number, line in code:
        if literal.search(line):
            print(f"{name}:{number}: {line.strip()}")
PY
)"
if [[ -n "$literals" ]]; then
    fail "a cargo output named by a literal target directory; resolve it with cargo_target_dir:"
    echo "$literals" >&2
fi

if [[ $failures -eq 0 ]]; then
    echo "PASS: every cargo output a script reads is read from where cargo wrote it"
else
    exit 1
fi
