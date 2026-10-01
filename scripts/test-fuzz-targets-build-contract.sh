#!/usr/bin/env bash
# The fuzz targets compile, are declared, and are described -- because nothing else builds them.
#
# THE DRIFT THIS EXISTS TO CATCH: `engine/fuzz/*` are not workspace members (cargo-fuzz brings its
# own profile and sanitizer flags, and a member would push them onto every other crate), so
# `cargo build --workspace` never compiles them. A rename in `frame-wire` or `frame-decode` then
# breaks a fuzz target and nothing says so: the property it checked stops being checked, and the
# first anyone learns of it is the day someone wants to run the fuzzer and finds a target that no
# longer builds. A fuzz target that does not build is worse than none -- the README still lists it
# as a guarantee.
#
# Three checks, each of which has to be able to fail:
#   * every fuzz crate under `engine/fuzz/*` type-checks (stable `cargo check`; the sanitizer
#     flags are only needed to RUN a target, not to build it),
#   * every `fuzz_targets/*.rs` is declared as a `[[bin]]` in its crate's Cargo.toml (cargo-fuzz
#     does not see an undeclared file) and every declared one exists,
#   * every target has a row in `engine/fuzz/README.md`, so the list of what is fuzzed is the
#     list of what builds.
#
# It does not run a fuzzer: that is minutes to hours, and the README's run history records what
# was run, when, and what it found.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

python3 - <<'PY'
import re
import sys
from pathlib import Path

root = Path("engine/fuzz")
readme = (root / "README.md").read_text()
failures = []
crates = sorted(p.parent for p in root.glob("*/Cargo.toml"))
print(f"  {len(crates)} fuzz crate(s) under {root}: {', '.join(c.name for c in crates)}")
if len(crates) < 2:
    failures.append("control: the scan found fewer than the two crates this repo has, so it is not looking where the crates are")

targets = 0
for crate in crates:
    manifest = (crate / "Cargo.toml").read_text()
    declared = {}
    for block in re.findall(r"\[\[bin\]\](.*?)(?=\n\[|\Z)", manifest, re.S):
        name = re.search(r'name\s*=\s*"([^"]+)"', block)
        path = re.search(r'path\s*=\s*"([^"]+)"', block)
        if name and path:
            declared[name.group(1)] = path.group(1)
    on_disk = {p.relative_to(crate).as_posix() for p in (crate / "fuzz_targets").glob("*.rs")}
    for name, path in declared.items():
        targets += 1
        if path not in on_disk:
            failures.append(f"{crate.name}: [[bin]] `{name}` points at `{path}`, which does not exist")
        if f"`{crate.name}/{name}`" not in readme:
            failures.append(f"{crate.name}/{name}: no row in engine/fuzz/README.md -- the list of what is fuzzed must be the list of what builds")
    for path in sorted(on_disk - set(declared.values())):
        failures.append(f"{crate.name}: `{path}` is not declared as a [[bin]], so cargo-fuzz will never run it")
print(f"  {targets} fuzz target(s) declared")
if targets < 2:
    failures.append("control: fewer than the two targets this repo has were found")

if failures:
    print("FAIL: the fuzz targets have drifted from what builds:", file=sys.stderr)
    for f in failures:
        print(f"  * {f}", file=sys.stderr)
    sys.exit(1)
PY

TARGET_DIR="$(mktemp -d "${TMPDIR:-/tmp}/migo-fuzz-check.XXXXXX")"
trap 'rm -rf "$TARGET_DIR"' EXIT

status=0
for manifest in engine/fuzz/*/Cargo.toml; do
    crate="$(dirname "$manifest")"
    echo "  cargo check: $crate"
    if ! CARGO_TARGET_DIR="$TARGET_DIR/$(basename "$crate")" cargo check --quiet --manifest-path "$manifest" 2>"$TARGET_DIR/err.log"; then
        echo "FAIL: $crate does not build; the property it checks is no longer being checked:" >&2
        sed 's/^/    /' "$TARGET_DIR/err.log" | tail -30 >&2
        status=1
    fi
done
[ "$status" -eq 0 ] || exit 1

echo
echo "PASS: every fuzz target is declared, described and builds."
