#!/usr/bin/env bash
# A GL constant the executor can hand to content must be a constant content can name.
#
# THE DRIFT THIS EXISTS TO CATCH has shipped four times in one file, and every time
# the symptom was the same: not an error, a silent wrong branch. A constant absent
# from `01_constants.js` makes `gl.THAT_NAME` evaluate to `undefined`, and
# `undefined` is a perfectly good argument to pass to a GL call and a perfectly good
# thing to compare against.
#
#   * `UNIFORM_BUFFER` absent -- a UBO upload targeted `undefined`, the shader never
#     received a projection, and nothing was visible.
#   * `INVALID_INDEX` absent -- the documented `if (index === gl.INVALID_INDEX)`
#     compared against `undefined` and never took that branch.
#   * the whole WebGL 2 sync set absent -- `fenceSync(gl.SYNC_GPU_COMMANDS_COMPLETE, 0)`
#     passed `undefined` as the fence condition, and `clientWaitSync`'s four status
#     branches all compared against `undefined`.
#   * `DRAW_FRAMEBUFFER` absent -- `blitFramebuffer` is implemented and is reachable
#     only by binding that target, so the whole blit path was dead code.
#
# The first three were each found by someone tripping over the consequence. Fixing
# them one at a time did not converge. The fourth was found by running this
# derivation by hand; this gate is that derivation, so there is no fifth.
#
# WHAT IS DERIVED, AND WHY THE INTERSECTION MATTERS: every `glow::NAME` in the
# executor, intersected with glow's own `pub const NAME: u32` set. The intersection
# is not tidiness -- without it `glow::Context` and `glow::Program` arrive as the
# "constants" `C` and `P`, and a gate that reports those has taught its readers to
# ignore it.
#
# WHAT IS CLASSIFIED, AND WHY IT CANNOT BE DERIVED: a constant the executor uses for
# an internal GLES call is not necessarily part of WebGL's surface -- WebGL wraps
# some of them and exposes no enum. `contracts/runtime/webgl-constant-reach.json`
# carries those with a reason each. A derived constant that is neither declared nor
# classified fails, and so does a classification for a constant that is declared or
# no longer derived: a stale exemption is how this kind of file starts lying.
#
# Values are compared, not just names. A constant present with the wrong number is
# the same silent wrong branch wearing the right name.
#
# Sources are LISTED, not globbed, and each carries its reason for being on the
# content boundary. Widening the set from one file to six is what found the fifth
# instance -- 48 sized internal formats, gating six implemented methods -- so a source
# quietly dropped is how a sixth would hide. A listed source that names no glow
# constant fails too: it is either the wrong file or one that stopped touching GL.
#
# Host-only: reads the contract, its sources, and the vendored glow source.
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

python3 - <<'PY'
import json
import re
import sys
from pathlib import Path

TAG = "[webgl-constants]"
CONTRACT = Path("contracts/runtime/webgl-constant-reach.json")

problems: list[str] = []


def fail(message: str) -> None:
    problems.append(message)


contract = json.loads(CONTRACT.read_text(encoding="utf-8"))
# Every source carries its own reason for being on the content boundary. Widening the
# set is what found the fifth instance, and narrowing it silently is how a sixth would
# hide -- so each entry is named here rather than globbed.
sources = {Path(k): v for k, v in contract["sources"].items() if not k.startswith("_")}
shim = Path(contract["sources"]["_shim_constants"])
for path in [*sources, shim]:
    if not path.is_file():
        print(f"{TAG} could not run: {path} is missing.", file=sys.stderr)
        raise SystemExit(1)
for path, reason in sources.items():
    if not reason.strip():
        print(f"{TAG} could not run: {path} has no reason recorded.", file=sys.stderr)
        raise SystemExit(1)


def strip_line_comments(text: str) -> str:
    """Drop `//` comments so a constant named in prose is not read as code.

    The same reason the startup-ordering pins strip them: comments in this repo
    deliberately name the thing they explain, so a comment saying a constant is
    deliberately absent would otherwise satisfy a search for it.
    """
    out = []
    for line in text.splitlines():
        cut = line.find("//")
        out.append(line if cut < 0 else line[:cut])
    return "\n".join(out)


# glow is the authority for both the name set and the values. A missing registry is
# a missing tool, and a gate whose tool is absent must say so rather than report the
# invariant it was unable to check -- this repo has already spent weeks on a gate
# that announced a violation when ripgrep was simply not installed.
glow_dirs = sorted(Path.home().glob(".cargo/registry/src/*/glow-*/src"))
if not glow_dirs:
    print(
        f"{TAG} could not run: no vendored glow source under "
        "~/.cargo/registry/src/*/glow-*/src.\n"
        "This is a missing dependency, NOT a contract violation. "
        "Run `cargo fetch` in engine/ and re-run.",
        file=sys.stderr,
    )
    raise SystemExit(127)

glow_values: dict[str, int] = {}
for source in sorted(glow_dirs[-1].glob("*.rs")):
    for match in re.finditer(
        r"pub const ([A-Z][A-Z0-9_]*)\s*:\s*u32\s*=\s*(0x[0-9A-Fa-f]+|\d+)",
        source.read_text(encoding="utf-8"),
    ):
        glow_values[match.group(1)] = int(match.group(2), 0)
if not glow_values:
    print(f"{TAG} could not run: parsed no constants out of {glow_dirs[-1]}.", file=sys.stderr)
    raise SystemExit(127)

# Per source, then merged. Attributing each name to only its first source made the
# emptiness check below misfire: EXTENSIONS is named by two of them, so the second was
# reported as contributing nothing. What the check must ask is whether a source names
# any glow constant, not whether it was the first to.
exempt_names = set(contract["not_exposed_by_webgl"])
per_source: dict[Path, set[str]] = {}
for path in sources:
    hits = set(re.findall(r"glow::([A-Z][A-Z0-9_]*)", strip_line_comments(path.read_text(encoding="utf-8"))))
    per_source[path] = hits & glow_values.keys()
# Every GL-touching file, boundary or not: the internal-only audit needs the internal ones.
per_source_all: dict[Path, set[str]] = dict(per_source)
named: dict[str, Path] = {}
for path, hits in per_source.items():
    for name in hits:
        named.setdefault(name, path)
derived = sorted(named)
if not derived:
    fail(
        "no `glow::CONST` names resolved against glow in any source, so this gate would "
        "pass without checking anything"
    )
# Every source must name at least one, or it is either the wrong file or one that stopped
# touching GL -- and a source nobody checks is how the set silently narrows.
for path, hits in per_source.items():
    if not hits:
        fail(f"{path}: names no glow constant, so listing it as a source checks nothing")

# No trailing comma required. Requiring one silently dropped the LAST entry in the
# table, which is how the first version of this gate reported `RGBA8` as absent when
# it was declared -- a parser that quietly loses one item is the same disease this
# gate exists to catch, in the gate itself.
declared: dict[str, int] = {
    match.group(1): int(match.group(2))
    for match in re.finditer(
        r"^\s{4}([A-Z][A-Z0-9_]*)\s*:\s*(\d+)\s*,?\s*$",
        strip_line_comments(shim.read_text(encoding="utf-8")),
        re.M,
    )
}
if not declared:
    fail(f"{shim}: parsed no constants, so nothing could be compared")
# The table's last entry has no trailing comma, so a parser that requires one loses
# it silently. Anchor on the count: `WebglConstants` is a flat object literal, so
# every `NAME: <number>` line in it is a constant and the two must agree.
literal_lines = sum(
    1
    for line in strip_line_comments(shim.read_text(encoding="utf-8")).splitlines()
    if re.match(r"^\s{4}[A-Z][A-Z0-9_]*\s*:\s*\d+\s*,?\s*$", line)
)
if literal_lines != len(declared):
    fail(
        f"{shim}: {literal_lines} constant line(s) present but {len(declared)} parsed -- "
        "the parser is dropping entries, so every absence it reports is suspect"
    )

# The source list is derived too. Listing the boundary files and checking each named
# something was not enough: deleting one line took coverage from 132 constants to 77 and
# still passed. So every file under graphics/src that names a glow constant must be
# claimed as a boundary source or as internal, and one in neither fails -- dropping a
# source now requires reclassifying it in writing.
internal: dict[str, str] = contract["internal_only"]
gl_files = sorted(
    path
    for path in Path("engine/crates/graphics/src").rglob("*.rs")
    if set(re.findall(r"glow::([A-Z][A-Z0-9_]*)", strip_line_comments(path.read_text(encoding="utf-8"))))
    & glow_values.keys()
)
if not gl_files:
    fail("no file under engine/crates/graphics/src names a glow constant, which cannot be right")
for path in gl_files:
    per_source_all.setdefault(
        path,
        set(re.findall(r"glow::([A-Z][A-Z0-9_]*)", strip_line_comments(path.read_text(encoding="utf-8"))))
        & glow_values.keys(),
    )
for path in gl_files:
    key = str(path)
    if path in sources and key in internal:
        fail(f"{key}: claimed as both a boundary source and internal-only")
    elif path not in sources and key not in internal:
        fail(
            f"{key}: names GL constants and is claimed neither as a content boundary in "
            f"`sources` nor as internal in `internal_only` of {CONTRACT}. If content can "
            "reach it, its constants must be nameable; if not, say so"
        )
for key, reason in internal.items():
    if Path(key) not in gl_files:
        fail(
            f"{CONTRACT}: {key} is classified internal-only but names no glow constant -- "
            "delete the entry rather than leaving one nothing checks"
        )
    elif not reason.strip():
        fail(f"{CONTRACT}: {key} has no reason recorded for being internal-only")

# The audit that found the sixth instance, run every time instead of by hand.
#
# A constant named ONLY by files classified internal-only is never checked by anything
# below -- that is what internal-only means. So a wrong entry there is not untidy, it is
# a permanent blind spot. Classifying backend/gl/state_tracker.rs as internal hid the
# eight WebGL 2 pixel-store parameters for exactly that reason: it shadows them because
# *content* sets them through pixelStorei, and it was the only file naming them.
#
# The rule, and note what it is NOT. The first version also failed when such a constant
# WAS declared, reasoning that a declaration means someone believed content can reach it.
# That is a false positive for FORWARDED enums, and running it said so immediately:
# `UNSIGNED_SHORT` and `UNSIGNED_INT` are `drawElements` index types, declared correctly
# and reachable by content, while the executor merely passes the value through without
# ever naming it -- the same shape as `bindFramebuffer`'s target and `texParameteri`'s
# `pname`. So "declared" carries no contradiction and that half is gone.
#
# What remains is the half that found the sixth instance: a constant named only by
# internal-only files, not declared, and not classified, is checked by nothing at all.
# Either WebGL does not expose it -- say so -- or the file naming it is a boundary.
internal_paths = {Path(k) for k in internal}
only_internal: dict[str, Path] = {}
for path, hits in per_source_all.items():
    if path not in internal_paths:
        continue
    for name in hits:
        if not any(name in per_source.get(b, set()) for b in sources):
            only_internal.setdefault(name, path)
for name, path in sorted(only_internal.items()):
    if name in declared:
        # Forwarded: content names it, the engine passes the value through. Fine.
        continue
    if name not in exempt_names:
        fail(
            f"{name} = {glow_values[name]} (0x{glow_values[name]:X}) is named only by "
            f"{path}, classified internal-only, and nothing checks it. If WebGL genuinely "
            f"does not expose it, record that in {CONTRACT}'s `not_exposed_by_webgl` so the "
            "claim is written down rather than implied by a classification"
        )

exempt: dict[str, str] = contract["not_exposed_by_webgl"]

for name in derived:
    if name in declared:
        if declared[name] != glow_values[name]:
            fail(
                f"{shim}: {name} is {declared[name]}, but glow says {glow_values[name]} "
                f"(0x{glow_values[name]:X}) -- content would pass the wrong enum"
            )
        if name in exempt:
            fail(
                f"{CONTRACT}: {name} is classified as not exposed by WebGL, but {shim} "
                "declares it. One of the two is wrong, and a stale exemption is how this "
                "file starts lying"
            )
        continue
    if name not in exempt:
        fail(
            f"{shim}: {named[name]} can hand {name} = {glow_values[name]} "
            f"(0x{glow_values[name]:X}) to content, and content cannot name it -- "
            f"`gl.{name}` is `undefined`, which is a silent wrong branch rather than an "
            f"error. Declare it, or classify it in {CONTRACT} with the reason WebGL does "
            "not expose it"
        )

# Validated against every GL constant the engine names, boundary or internal -- not only
# the boundary set. "WebGL does not expose this" is a claim about the constant, not about
# which file happens to name it, and scoping the check to boundary sources made two of
# this gate's own checks contradict each other: the internal-only audit asked for
# PROGRAM_BINARY_LENGTH to be recorded here while this loop demanded its deletion.
engine_named = set(derived) | set(only_internal)
for name in sorted(exempt):
    if name not in glow_values:
        fail(f"{CONTRACT}: {name} is not a glow constant, so the exemption cannot be checked")
    elif name not in engine_named:
        fail(
            f"{CONTRACT}: {name} is classified, but nothing in the engine names it any more "
            "-- delete the exemption rather than leaving one nothing checks"
        )
    elif not exempt[name].strip():
        fail(f"{CONTRACT}: {name} has no reason recorded for being unexposed")

print()
print(f"  {len(derived)} GL constant(s) reachable across {len(sources)} boundary source(s)")
print(f"  {len(internal)} file(s) classified internal-only, {len(gl_files)} touch GL in total")
print(f"    {sum(1 for n in derived if n in declared)} nameable by content")
print(f"    {len(exempt)} classified as not exposed by WebGL")
print(f"  {len(declared)} constant(s) declared for content in total")

if problems:
    print()
    print(f"\033[0;31m{TAG} FAIL: a constant the executor can produce is not one content can name.\033[0m", file=sys.stderr)
    for problem in problems:
        print(f"  * {problem}", file=sys.stderr)
    raise SystemExit(1)

print()
print(f"\033[0;32m{TAG} PASS: every reachable GL constant is nameable or classified.\033[0m")
PY
