#!/usr/bin/env bash
# ============================================================
# A cargo note this repository PARSES must be captured with colour pinned off.
# Location: scripts/test-parsed-cargo-note-contract.sh
#
# `cargo rustc -- --print native-static-libs` is how five build scripts learn
# what the C ABI archive needs from the system: the frameworks and libraries
# their dependencies link, written into a module map, a pkg-config file or a
# CMake config so a consumer's link resolves. The list is derived per build
# rather than transcribed, which is right -- and it is derived by matching text.
#
# `dtolnay/rust-toolchain` exports CARGO_TERM_COLOR=always. Under it rustc emits
#
#     \e[1m\e[92mnote\e[0m\e[1m: native-static-libs: -lobjc ... -lm\e[0m
#
# so `note: native-static-libs:` is not a substring of the line, and the
# trailing reset arrives glued to the last flag with no separating space. A
# parser then either sees no note at all or reads one flag named `-lm\e[0m`.
# Neither is visible to a developer, because a pipe turns cargo's colour off
# unless something forces it on: it happens only on CI.
#
# This gate exists because the fix was applied four times and missed once.
# Android found it, Linux and both Windows captures copied the pin, and
# build-apple-sdk.sh -- written later -- captured without it. Every Apple row of
# PR CI then failed with "rustc reported no native link dependencies" against a
# log that contained them, and it read as an Apple toolchain problem for days.
#
# The rule: every `--print native-static-libs` invocation under scripts/ carries
# CARGO_TERM_COLOR=never on the command that runs it. Both spellings count --
# a shell prefix or `env` assignment joined to the command by continuations, and
# the `set CARGO_TERM_COLOR=never` a generated batch file uses -- provided
# nothing separates the pin from the invocation but comment or continuation
# lines. A blank line ends the block: a pin further away than that is a pin on
# something else.
# ============================================================
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

python3 - "$ROOT_DIR" <<'PY'
from __future__ import annotations

import pathlib
import re
import sys

root = pathlib.Path(sys.argv[1])
scripts = root / "scripts"

# The invocation, not the prose: `--print native-static-libs` also appears in
# comments and in --help strings that describe it, and a gate that counted those
# would pass on a repository where every real capture had been deleted.
INVOCATION = re.compile(r"\bcargo\s+rustc\b.*--print[= ]native-static-libs")
PIN = "CARGO_TERM_COLOR=never"


def code(line: str) -> str:
    """The line with its commentary removed.

    Both halves of this gate are text matches, and prose about the pin is not the
    pin: the first draft passed a build script whose pin had been deleted,
    because the comment above the command still named the variable. A batch
    file's `rem` is a comment for the same reason a shell's `#` is.
    """
    without = line.split("#", 1)[0]
    return "" if without.strip().lower().startswith("rem ") else without

failures: list[str] = []
checked: list[str] = []

for path in sorted(scripts.rglob("*")):
    if not path.is_file() or path.suffix not in {".sh", ".bash"}:
        continue
    lines = path.read_text(errors="replace").splitlines()

    # Continuations first: the invocation is one command even when cargo's flags
    # are spread over six lines, and the pin may sit on any of them.
    logical: list[tuple[int, str]] = []
    start, buffer = None, ""
    for number, line in enumerate(lines, start=1):
        if start is None:
            start = number
        buffer += code(line)
        if code(line).rstrip().endswith("\\"):
            buffer = buffer.rstrip()[:-1] + " "
            continue
        logical.append((start, buffer))
        start, buffer = None, ""
    if buffer:
        logical.append((start or len(lines), buffer))

    for number, command in logical:
        stripped = command.strip()
        if stripped.startswith("#") or not INVOCATION.search(command):
            continue
        where = f"{path.relative_to(root)}:{number}"
        checked.append(where)
        if PIN in command:
            continue
        # A generated batch file cannot prefix an assignment onto a command, so
        # it says `set CARGO_TERM_COLOR=never` on its own line. Accept that, and
        # only that: scanning back stops at the first blank line, because a pin
        # in a previous paragraph is a pin on a previous command.
        pinned = False
        for previous in reversed(lines[: number - 1]):
            if not previous.strip():
                break
            if PIN in code(previous):
                pinned = True
                break
        if not pinned:
            failures.append(where)

# A scan that matched nothing is the failure mode this repository has hit most
# often in its own gates: the identifier moved, the gate went quiet, and quiet
# read as pass. Five captures exist today; fewer than two means the pattern, not
# the code, is what changed.
if len(checked) < 2:
    sys.exit(
        "parsed-cargo-note contract: found %d `--print native-static-libs` "
        "invocations under scripts/, which means this gate is matching nothing "
        "rather than passing" % len(checked)
    )

if failures:
    print("parsed-cargo-note contract: FAIL", file=sys.stderr)
    for where in failures:
        print(f"  {where}: captures --print native-static-libs without {PIN}", file=sys.stderr)
    print(
        "\n  Forced colour wraps the note in ANSI codes: `note` and its colon are\n"
        "  separated, and the trailing reset glues onto the last -l flag. Pin the\n"
        "  variable on the command, as the other captures do.",
        file=sys.stderr,
    )
    sys.exit(1)

print(f"parsed-cargo-note contract: PASS ({len(checked)} capture sites)")
for where in checked:
    print(f"  {where}")
PY
