#!/usr/bin/env bash
# The WebGL surface content can name is the surface Khronos's specification defines, and what is not there yet is
# written down.
#
# THE DRIFT THIS EXISTS TO CATCH: `webgl-constant-reach` checks that every constant the ENGINE names is nameable by
# content. That is the engine's view of the boundary, and it cannot see what the engine never thought of. Running
# three.js on the runtime found 160 WebGL 2 constants nobody had declared -- `getParameter(gl.MAX_SAMPLES)` passed
# `undefined` to a native op, threw "expected i32", and three.js could not create a WebGLRenderer at all -- and 66
# methods nobody had written, among them `uniform2i`, `vertexAttrib4f`, `vertexAttribIPointer`, `clearBufferfv`,
# `getUniform` and `getVertexAttrib`. A context that claims `webgl2` and answers `undefined` for 16 percent of
# the specification is a context a framework discovers the hard way, one call at a time.
#
# So the authority here is the specification itself: Khronos's IDL, vendored verbatim under
# `contracts/runtime/webgl-idl/` (MIT licence, kept in the file headers; refreshed from
# https://registry.khronos.org/webgl/specs/latest/ -- the date is in `webgl-spec-surface.json`).
#
#   * Every constant in the IDL must be declared in `01_constants.js`, with the IDL's value. No exceptions list: a
#     missing constant is `undefined` passed on as an argument, which is never the right answer.
#   * Every method in the IDL must be defined by the facade, or listed in `webgl-spec-surface.json` with the reason
#     it is not (yet). A list is a plan that can shrink and cannot grow silently: an entry for a method that IS
#     defined fails, so does a method that is neither, and so does a reasonless entry.
#
# Methods are found by reading the facade's class bodies, not by running it, so the gate needs no engine build.
# `webgl-spec-surface` conformance suite checks the same by running it on a device.
#
# Host-only: reads the contract, the vendored IDL and the facade's source.
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

python3 - <<'PY'
import json
import re
import sys
from pathlib import Path

TAG = "[webgl-spec-surface]"
CONTRACT = Path("contracts/runtime/webgl-spec-surface.json")
IDLS = [Path("contracts/runtime/webgl-idl/webgl.idl"), Path("contracts/runtime/webgl-idl/webgl2.idl")]
CONSTANTS = Path("engine/crates/runtime-v8/src/rendering/webgl/01_constants.js")
FACADE = Path("engine/crates/runtime-v8/src/rendering/webgl/02_webgl_context.js")

failures: list[str] = []


def fail(message: str) -> None:
    failures.append(message)


def strip_line_comments(text: str) -> str:
    return re.sub(r"//[^\n]*", "", text)


def idl_surface(path: Path) -> tuple[dict[str, int], set[str]]:
    constants: dict[str, int] = {}
    methods: set[str] = set()
    text = path.read_text(encoding="utf-8")
    for match in re.finditer(
        r"const\s+(?:GLenum|GLint|GLuint|GLint64|GLuint64|GLbitfield|GLsizei|GLboolean)\s+(\w+)\s*=\s*(-?0x[0-9A-Fa-f]+|-?\d+)\s*;",
        text,
    ):
        constants[match.group(1)] = int(match.group(2), 0)
    # `[Attr] ReturnType name(args);` -- one per line in Khronos's generated IDL.
    for match in re.finditer(
        r"^\s*(?:\[[^\]]*\]\s*)?[\w<>?() ,.\[\]]+?\s+(\w+)\s*\([^;{]*\)\s*;", text, re.M
    ):
        if match.group(1) != "constructor":
            methods.add(match.group(1))
    return constants, methods


try:
    contract = json.loads(CONTRACT.read_text(encoding="utf-8"))
except (OSError, ValueError) as error:
    print(f"{TAG} FAIL: cannot read {CONTRACT}: {error}", file=sys.stderr)
    sys.exit(1)

spec_constants: dict[str, int] = {}
spec_methods: set[str] = set()
for idl in IDLS:
    if not idl.is_file():
        print(f"{TAG} FAIL: {idl} is missing", file=sys.stderr)
        sys.exit(1)
    constants, methods = idl_surface(idl)
    if not constants or not methods:
        print(f"{TAG} FAIL: {idl} parsed to {len(constants)} constants and {len(methods)} methods; the parser is broken", file=sys.stderr)
        sys.exit(1)
    spec_constants.update(constants)
    spec_methods |= methods

# --- constants -------------------------------------------------------------------------------------------------------
declared: dict[str, int] = {
    match.group(1): int(match.group(2))
    for match in re.finditer(
        r"^\s{4}([A-Z][A-Za-z0-9_]*)\s*:\s*(-?\d+)\s*,?\s*$",
        strip_line_comments(CONSTANTS.read_text(encoding="utf-8")),
        re.M,
    )
}
if not declared:
    fail(f"{CONSTANTS}: parsed no constants, so nothing could be compared")
for name, value in sorted(spec_constants.items()):
    if name not in declared:
        fail(f"{CONSTANTS}: the specification defines {name} = {value} (0x{value & 0xFFFFFFFF:X}) and it is not declared -- `gl.{name}` is `undefined`")
    elif declared[name] != value:
        fail(f"{CONSTANTS}: {name} is {declared[name]}, the specification says {value} (0x{value & 0xFFFFFFFF:X})")

# --- methods ---------------------------------------------------------------------------------------------------------
facade = FACADE.read_text(encoding="utf-8")
start = facade.find("class WebGLRenderingContext {")
if start < 0:
    print(f"{TAG} FAIL: {FACADE} has no `class WebGLRenderingContext`; the method scan has nothing to read", file=sys.stderr)
    sys.exit(1)
defined = set(re.findall(r"^    (?:static\s+)?(?:async\s+)?([A-Za-z_$][\w$]*)\s*\(", facade[start:], re.M))
defined |= set(re.findall(r"\.prototype\.([A-Za-z_$][\w$]*)\s*=", facade))
defined -= {"constructor", "if", "for", "while", "switch", "catch", "function", "return"}

planned: dict[str, str] = {}
for group in contract["unimplemented_methods"]:
    reason = group.get("reason", "")
    if not reason.strip():
        fail(f"{CONTRACT}: the group beginning {group.get('methods', ['?'])[:2]} has no reason")
    for name in group["methods"]:
        if name in planned:
            fail(f"{CONTRACT}: {name} is listed twice")
        planned[name] = reason

for name in sorted(spec_methods):
    if name in defined:
        if name in planned:
            fail(f"{CONTRACT}: {name} is listed as not implemented, but {FACADE} defines it -- delete the entry, a stale exemption is how this file starts lying")
    elif name not in planned:
        fail(f"{FACADE}: the specification defines {name}() and the facade does not, and {CONTRACT} does not say why -- `gl.{name}` is `undefined` and calling it is a TypeError. Implement it, or list it with the reason")
for name in sorted(planned):
    if name not in spec_methods:
        fail(f"{CONTRACT}: {name} is listed but the specification does not define it -- delete the entry")

print()
print(f"  {len(spec_constants)} specification constants, all declared with the specification's values" if not any("constants" in f or "CONSTANTS" in f for f in failures) else "")
print(f"  {len(spec_methods)} specification methods: {len(spec_methods & defined)} defined, {len(planned)} planned ({len(spec_methods) - len(spec_methods & defined) - len(planned)} unaccounted for)")

if failures:
    print()
    print(f"{TAG} FAIL: {len(failures)} problem(s)", file=sys.stderr)
    for message in failures:
        print(f"  * {message}", file=sys.stderr)
    sys.exit(1)
print(f"{TAG} PASS: the facade names all of the specification's constants and accounts for every method")
PY
