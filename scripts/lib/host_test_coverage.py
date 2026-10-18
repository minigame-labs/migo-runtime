#!/usr/bin/env python3
"""Which of the workspace's test binaries no host step runs.

`scripts/verify-change.sh` declares its host suites as a list of cargo step
strings. Every one of them said `--lib`, on both sides of the gate, and nothing
compared that list against the targets cargo would actually build -- so thirteen
integration-test binaries holding 95 tests were run by no local step, and 35 of
them by no job anywhere. `migo-capi-abi`'s nine binaries are the sharpest case:
its lib has no tests at all, so `--lib` there is a step that cannot fail.

The audit exists because the crate-name comparison in
`scripts/test-local-verification-contract.sh` could not see this. Two lists that
name the same crates can still run different binaries, and the difference is one
word in the middle of a step.

A test binary is what cargo says it is: every lib, bin and integration-test
target whose `test` flag is set, which a lib and a bin have unless their
manifest says `test = false`. This audit used to count only `kind: ["test"]`,
the `tests/` half, and the other half held more: `migo-services` keeps all 272
of its tests -- the network gate and every socket and HTTP client -- in its lib,
and `migo-player` its three teardown-ordering tests in its bin. Neither crate was
named by any step, local or CI, and this audit reported nothing missing.

Reads the steps on stdin, one per line, and prints `package::target (kind)` for
every test binary no step runs.

Exit codes:
    0  coverage determined; uncovered targets, if any, are on stdout
    1  cargo could not describe the workspace, so coverage is unknown
    3  no `engine/Cargo.toml` in this tree, so there is nothing to audit
"""

from __future__ import annotations

import argparse
import json
import pathlib
import shlex
import subprocess
import sys

# Cargo's own wording for `--tests`: "all targets that have the test = true
# manifest flag set", which is every test binary there is.
_SELECTS_ALL = {"--tests", "--all-targets"}

# Flags selecting a kind this audit does not count, so a step carrying only
# these runs none of the binaries below.
_SELECTS_NONE = {"--doc", "--examples", "--benches"}

# Flags that consume the following word, so it is never mistaken for a flag.
_TAKES_VALUE = {"--example", "--bench", "--features", "--target",
                "--manifest-path", "-j", "--jobs"}

_LIB_KINDS = {"lib", "rlib", "dylib", "cdylib", "staticlib", "proc-macro"}

ALL = object()
"""Sentinel: this step runs every test binary its packages have."""


def target_kind(kinds: list[str]) -> str | None:
    """`lib`, `bin` or `test` for a target cargo builds a test binary from."""
    if _LIB_KINDS.intersection(kinds):
        return "lib"
    if kinds == ["bin"]:
        return "bin"
    if kinds == ["test"]:
        return "test"
    return None


def step_coverage(step: str) -> tuple[list[str], object | set[tuple[str, str]]] | None:
    """The packages a step names and the targets it runs, or None if it is not a
    `cargo test` step at all.

    A selection is a set of `(kind, name)` pairs, where a name of `*` stands for
    every target of that kind: `--lib` is `("lib", "*")` because a package has at
    most one, `--bins` is `("bin", "*")`.

    `build --workspace --all-targets` is deliberately None: it compiles every
    test binary and runs none of them, and treating a compile as coverage is how
    a binary comes to exist in a green tree without ever executing.
    """
    words = shlex.split(step)
    if not words or words[0] != "test":
        return None

    packages: list[str] = []
    selected: set[tuple[str, str]] = set()
    selects_all = False
    selects_any = False

    index = 1
    while index < len(words):
        word = words[index]
        value = words[index + 1] if index + 1 < len(words) else None
        if word in ("-p", "--package"):
            if value is not None:
                packages.append(value)
            index += 2
            continue
        if word in ("--test", "--bin"):
            if value is not None:
                selected.add((word[2:], value))
            selects_any = True
            index += 2
            continue
        if word in _SELECTS_ALL:
            selects_all = True
        elif word == "--lib":
            selected.add(("lib", "*"))
            selects_any = True
        elif word == "--bins":
            selected.add(("bin", "*"))
            selects_any = True
        elif word in _SELECTS_NONE:
            selects_any = True
        elif word in _TAKES_VALUE:
            index += 1
        index += 1

    # No target selection at all: cargo runs every test binary and the doc tests.
    if selects_all or not selects_any:
        return packages, ALL
    return packages, selected


def coverage_by_package(steps) -> dict[str, object | set[tuple[str, str]]]:
    merged: dict[str, object | set[tuple[str, str]]] = {}
    for step in steps:
        parsed = step_coverage(step)
        if parsed is None:
            continue
        packages, covered = parsed
        for package in packages:
            existing = merged.get(package)
            if existing is ALL or covered is ALL:
                merged[package] = ALL
            else:
                merged[package] = set(existing or set()) | set(covered)
    return merged


def test_targets(engine: pathlib.Path) -> dict[str, list[tuple[str, str]]]:
    """Every workspace member's test binaries as `(kind, name)`, from cargo itself.

    Globbing `tests/*.rs` would be a second implementation of cargo's target
    discovery: it counts `tests/common/mod.rs` as a binary when it is a module
    directory, it cannot see an explicit `[[test]]`, and it cannot see a lib's
    or a bin's `test = false`.
    """
    result = subprocess.run(
        ["cargo", "metadata", "--no-deps", "--offline", "--format-version", "1"],
        cwd=engine,
        capture_output=True,
        text=True,
    )
    if result.returncode != 0:
        raise RuntimeError(result.stderr.strip() or "cargo metadata failed")
    metadata = json.loads(result.stdout)

    targets: dict[str, list[tuple[str, str]]] = {}
    for package in metadata.get("packages", []):
        found = []
        for target in package.get("targets", []):
            kind = target_kind(target.get("kind", []))
            if kind is not None and target.get("test", False):
                found.append((kind, target["name"]))
        if found:
            targets[package["name"]] = sorted(found)
    return targets


def is_covered(reach, kind: str, name: str) -> bool:
    if reach is ALL:
        return True
    if reach is None:
        return False
    return (kind, name) in reach or (kind, "*") in reach


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", required=True, help="repository root")
    args = parser.parse_args()

    engine = pathlib.Path(args.root) / "engine"
    if not (engine / "Cargo.toml").is_file():
        return 3

    try:
        targets = test_targets(engine)
    except (RuntimeError, json.JSONDecodeError, OSError) as error:
        print(f"cargo could not describe the workspace: {error}", file=sys.stderr)
        return 1

    covered = coverage_by_package(
        line.strip() for line in sys.stdin if line.strip()
    )

    for package in sorted(targets):
        reach = covered.get(package)
        for kind, name in targets[package]:
            if not is_covered(reach, kind, name):
                print(f"{package}::{name} ({kind})")
    return 0


if __name__ == "__main__":
    sys.exit(main())
