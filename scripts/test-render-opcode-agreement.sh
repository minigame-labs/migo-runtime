#!/usr/bin/env bash
# Three implementations name the same opcodes, or a frame silently does not draw.
#
# A render command stream record is twelve bits of opcode and twenty of word
# count.
# Nothing on either side of the process boundary is typed: the producer writes a
# number and the reader switches on it. So an opcode added to one table and not
# another is not a type error, not a link error, and not a runtime error --
# it is a record the reader rejects, on a device, with the frame not drawing and
# nothing in the log that names the opcode.
#
# THE DRIFT THIS EXISTS TO CATCH has a specific history. There was already a
# JS/Rust agreement test, and it was a HAND-WRITTEN LIST of sixty-nine name and
# value pairs inside the runtime crate. Two things were wrong with it: a list
# someone has to extend is a list that falls behind, and it could only see the
# in-process encoder, because the WebContent producer lives outside that crate
# and a test there would have had to reach across the tree to find it.
#
# So the tables are parsed, not restated, and all three are parsed by the same
# gate:
#
#   engine/crates/frame-wire/src/gl.rs                   (the source)
#   engine/crates/runtime-v8/src/rendering/webgl/00_render_command_stream.js
#   platforms/apple/WebContent/PerformancePlus/src/render-opcodes.mjs
#
# Two numbers beside the opcodes are held the same way, because a record carries
# them just as silently: STAGED_PAYLOAD, the byte_length that says an upload's
# bytes were staged ahead of it, and MAX_WEBGL_UPLOAD_BYTES, the producer's copy
# of the one-upload ceiling it refuses above rather than staging for nothing.
#
# And the prefix of every payload record, which the producer's decode-budget
# estimate reads its length at (see below).
#
# Host-only: reads the tables and those constants.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

python3 - <<'PY'
import re
import sys
from pathlib import Path

# Which file carries which block, stated rather than inferred.
#
# Every encoder now carries both blocks. The declaration stays per source
# because that is what makes a gap visible here instead of silently unchecked:
# a source that claims a block must agree on all of it, and one that drops a
# block has to say so on this line rather than by quietly failing to match.
SOURCES = {
    # The envelope, which owns the magic and the version and neither block.
    "rust envelope": (Path("engine/crates/frame-wire/src/stream.rs"), set()),
    "rust": (Path("engine/crates/frame-wire/src/gl.rs"), {"gl"}),
    "rust 2d": (Path("engine/crates/frame-wire/src/canvas2d.rs"), {"2d", "2d flags"}),
    # The resource block has two encoders, not three: the in-process runtime
    # makes these calls as ops, so its encoder declares only gl and 2d below.
    "rust resource": (Path("engine/crates/frame-wire/src/gl_resource.rs"), {"res"}),
    "in-process js": (
        Path("engine/crates/runtime-v8/src/rendering/webgl/00_render_command_stream.js"),
        {"gl", "2d"},
    ),
    "webcontent js": (
        Path("platforms/apple/WebContent/PerformancePlus/src/render-opcodes.mjs"),
        {"gl", "2d", "res", "2d flags"},
    ),
}

PATTERNS = {
    "gl": {
        "rust": re.compile(r"^pub const (OP_[A-Z0-9_]+): u32 = (\d+);", re.M),
        "in-process js": re.compile(r"^\s*const (OP_[A-Z0-9_]+) = (\d+);", re.M),
        "webcontent js": re.compile(r"^export const (OP_[A-Z0-9_]+) = (\d+);", re.M),
    },
    "res": {
        "rust resource": re.compile(r"^pub const (OPR_[A-Z0-9_]+): u32 = (\d+);", re.M),
        "webcontent js": re.compile(r"^export const (OPR_[A-Z0-9_]+) = (\d+);", re.M),
    },
    "2d": {
        "rust 2d": re.compile(r"^pub const (OP2D_[A-Z0-9_]+): u32 = (\d+);", re.M),
        "in-process js": re.compile(r"^\s*const (OP2D_[A-Z0-9_]+) = (\d+);", re.M),
        "webcontent js": re.compile(r"^export const (OP2D_[A-Z0-9_]+) = (\d+);", re.M),
    },
    # Not opcodes: the bits inside one record's word. A resize names width,
    # height or both, because content assigns them separately -- so the meaning
    # of that word is as much a cross-process agreement as the opcode that
    # introduces it, and it is the half a reader cannot detect getting wrong. A
    # producer that swapped the two bits would resize the other dimension, in
    # silence, on a device.
    "2d flags": {
        "rust 2d": re.compile(r"^pub const (RESIZE_CANVAS_[A-Z0-9_]+): u32 = (\d+);", re.M),
        "webcontent js": re.compile(r"^export const (RESIZE_CANVAS_[A-Z0-9_]+) = (\d+);", re.M),
    },
}
# Range markers, not opcodes: they name where the block starts and ends.
BLOCK_MARKERS = {"OP2D_BASE", "OP2D_END", "OPR_BASE", "OPR_PAYLOAD_BASE", "OPR_END"}

HEADER = {
    "rust envelope": re.compile(
        r"^pub const (MAGIC|STREAM_VERSION): u32 = (0x[0-9A-Fa-f_]+|\d+);", re.M
    ),
    "in-process js": re.compile(r"^\s*const (MAGIC|STREAM_VERSION) = (0x[0-9A-Fa-f]+|\d+);", re.M),
    "webcontent js": re.compile(r"^export const (MAGIC|STREAM_VERSION) = (0x[0-9A-Fa-f]+|\d+);", re.M),
}

problems = []
headers = {}
text_of = {}

for name, (path, blocks) in SOURCES.items():
    if not blocks and name not in HEADER:
        # A source that declares no block and no header is a path this gate
        # opens and never reads -- the shape a check takes when it has quietly
        # stopped being one.
        problems.append(f"{name}: declares no opcode block and no header, so nothing checks it")
    if not path.is_file():
        problems.append(f"{name}: {path} is missing")
        continue
    text_of[name] = path.read_text(encoding="utf-8")
    if name in HEADER:
        headers[name] = {
            key: int(value.replace("_", ""), 0)
            for key, value in HEADER[name].findall(text_of[name])
        }

if problems:
    print("FAIL: the opcode tables could not be read.", file=sys.stderr)
    for problem in problems:
        print(f"  * {problem}", file=sys.stderr)
    raise SystemExit(1)

print()

for block, patterns in PATTERNS.items():
    tables = {}
    for name, pattern in patterns.items():
        if name not in text_of:
            continue
        table = {
            op: int(value)
            for op, value in pattern.findall(text_of[name])
            if op not in BLOCK_MARKERS
        }
        if not table:
            problems.append(
                f"{block}: no opcodes parsed out of {name}; the pattern no longer matches"
            )
        tables[name] = table

    if not tables:
        continue
    # The Rust table is the source for its block.
    source_name = next(name for name in tables if name.startswith("rust"))
    source = tables[source_name]
    print(f"  - {block}: {source_name} declares {len(source)} opcodes")

    for name, table in tables.items():
        if name == source_name:
            continue
        missing = sorted(set(source) - set(table))
        extra = sorted(set(table) - set(source))
        mismatched = sorted(
            (op, source[op], table[op])
            for op in set(source) & set(table)
            if source[op] != table[op]
        )
        if missing:
            problems.append(f"{block}: {name} is missing {len(missing)}: {', '.join(missing[:8])}")
        if extra:
            problems.append(
                f"{block}: {name} has {len(extra)} the Rust table does not: {', '.join(extra[:8])}"
            )
        for op, expected, found in mismatched:
            problems.append(f"{block}: {name}: {op} is {found}, the Rust table says {expected}")
        if not (missing or extra or mismatched):
            print(f"    {name} agrees on all {len(table)}")

    # Contiguity within the block, and no overlap with the other blocks. The
    # boundaries are load-bearing: a reader classifies a record by its opcode
    # alone, so an opcode in the wrong range is a record read with the wrong
    # shape rather than one that is rejected.
    values = sorted(source.values())
    if len(set(values)) != len(values):
        duplicates = sorted({v for v in values if values.count(v) > 1})
        problems.append(f"{block}: opcode numbers are reused: {duplicates}")
    if block == "gl":
        FIXED_TOP = 255
        fixed = sorted(v for v in values if v <= FIXED_TOP)
        variable = sorted(v for v in values if v > FIXED_TOP)
        if fixed and fixed != list(range(1, len(fixed) + 1)):
            gaps = [n for n in range(1, fixed[-1] + 1) if n not in set(fixed)]
            problems.append(f"gl: the fixed-length opcodes are not contiguous from 1; missing {gaps[:8]}")
        if variable and variable != list(range(256, 256 + len(variable))):
            gaps = [n for n in range(256, variable[-1] + 1) if n not in set(variable)]
            problems.append(
                f"gl: the variable-length opcodes are not contiguous from 256; missing {gaps[:8]}"
            )
        if variable and max(variable) >= 512:
            problems.append("gl: an opcode has reached the 2D block at 512")
        print(f"    {len(fixed)} fixed-length (1..={fixed[-1] if fixed else 0}), "
              f"{len(variable)} variable-length (256..={variable[-1] if variable else 0})")
    elif block == "res":
        # Two contiguous runs: fixed records from 128, payload records from 192,
        # and nothing of this block outside 128..=255.
        PAYLOAD_BASE = 192
        fixed = sorted(v for v in values if v < PAYLOAD_BASE)
        payload = sorted(v for v in values if v >= PAYLOAD_BASE)
        if fixed != list(range(128, 128 + len(fixed))):
            gaps = [n for n in range(128, (fixed or [128])[-1] + 1) if n not in set(fixed)]
            problems.append(f"res: the fixed records are not contiguous from 128; missing {gaps[:8]}")
        if payload != list(range(PAYLOAD_BASE, PAYLOAD_BASE + len(payload))):
            gaps = [n for n in range(PAYLOAD_BASE, (payload or [PAYLOAD_BASE])[-1] + 1) if n not in set(payload)]
            problems.append(f"res: the payload records are not contiguous from 192; missing {gaps[:8]}")
        if values and (values[0] < 128 or values[-1] > 255):
            problems.append("res: an opcode is outside the block's 128..=255")
        print(f"    {len(fixed)} fixed (128..={fixed[-1] if fixed else 0}), "
              f"{len(payload)} payload (192..={payload[-1] if payload else 0})")
    elif block == "2d flags":
        # Distinct single bits, so a record can name any combination and the
        # reader can refuse the combinations that mean nothing.
        if any(v == 0 or v & (v - 1) for v in values):
            problems.append(f"2d flags: {values} are not all single bits")
        if len(set(values)) != len(values):
            problems.append(f"2d flags: two flags share a bit: {values}")
        print(f"    {len(values)} flag bits ({', '.join(hex(v) for v in values)})")
    else:
        if values != list(range(512, 512 + len(values))):
            gaps = [n for n in range(512, values[-1] + 1) if n not in set(values)]
            problems.append(f"2d: the block is not contiguous from 512; missing {gaps[:8]}")
        print(f"    {len(values)} opcodes (512..={values[-1] if values else 0})")

# The stream header, which is checked before any opcode is read. A producer with
# the right opcodes and the wrong magic emits a stream the reader refuses whole.
for key in ("MAGIC", "STREAM_VERSION"):
    values = {name: header.get(key) for name, header in headers.items()}
    if any(value is None for value in values.values()):
        problems.append(
            f"{key} is not declared by: {', '.join(n for n, v in values.items() if v is None)}"
        )
        continue
    if len(set(values.values())) != 1:
        problems.append(f"{key} disagrees: {values}")
    else:
        print(f"  - {key} agrees: {hex(values['rust envelope'])}")

# The byte_length that says an upload's bytes were staged ahead of it. Only the
# resource block's two encoders write one, and a producer that wrote any other
# value there would have its upload read as a 4 GiB inline payload and the
# packet refused.
STAGED = {
    "rust envelope": re.compile(r"^pub const STAGED_PAYLOAD: u32 = (0x[0-9A-Fa-f_]+);", re.M),
    "webcontent js": re.compile(r"^export const STAGED_PAYLOAD = (0x[0-9A-Fa-f_]+);", re.M),
}
staged = {}
for name, pattern in STAGED.items():
    found = pattern.findall(text_of[name])
    if len(found) != 1:
        problems.append(f"STAGED_PAYLOAD is declared {len(found)} times by {name}")
    else:
        staged[name] = int(found[0].replace("_", ""), 16)
if len(staged) == len(STAGED):
    if len(set(staged.values())) != 1:
        problems.append(f"STAGED_PAYLOAD disagrees: {staged}")
    else:
        print(f"  - STAGED_PAYLOAD agrees: {hex(staged['rust envelope'])}")

# The most one upload may carry. The producer refuses above it rather than
# staging bytes the host would refuse; one lower than the host's would refuse,
# on iOS alone, uploads every other lane accepts.
def ceiling(path, pattern):
    found = re.findall(pattern, Path(path).read_text(encoding="utf-8"), re.M)
    if len(found) != 1 or not re.fullmatch(r"[0-9_ *]+", found[0]):
        problems.append(f"MAX_WEBGL_UPLOAD_BYTES is not one product of integers in {path}: {found}")
        return None
    value = 1
    for factor in found[0].replace("_", "").split("*"):
        value *= int(factor)
    return value
upload_ceilings = {
    "rust": ceiling(
        "engine/crates/shared/src/protocol/render_cmd.rs",
        r"^pub const MAX_WEBGL_UPLOAD_BYTES: usize = ([^;]+);",
    ),
    "webcontent js": ceiling(
        "platforms/apple/WebContent/PerformancePlus/src/lane-stream.mjs",
        r"^export const MAX_WEBGL_UPLOAD_BYTES = ([^;]+);",
    ),
}
if None not in upload_ceilings.values():
    if len(set(upload_ceilings.values())) != 1:
        problems.append(f"MAX_WEBGL_UPLOAD_BYTES disagrees: {upload_ceilings}")
    else:
        print(f"  - MAX_WEBGL_UPLOAD_BYTES agrees: {upload_ceilings['rust']}")

# The payload records' prefixes: the words before a byte payload's length or a word list's count. The producer's
# decode-budget estimate (decode-budget.mjs) reads that length at the prefix it has for the opcode, so a prefix that
# disagrees with the Rust spec reads another word as the length and misestimates that record alone. The interop run
# checks only the records its calls happen to emit: an OPR_INVALIDATE_SUB_FRAMEBUFFER prefix of 6 for 7 walked past
# every gate (2026-10-02). So the tables are compared whole, opcode by opcode, in both directions.
def rust_shapes(path):
    text = Path(path).read_text(encoding="utf-8")
    shapes = {"Bytes": {}, "Words": {}}
    for names, kind, prefix in re.findall(
        r"((?:OP2?D?R?_[A-Z0-9_]+(?:\s*\|\s*)?)+)\s*=>\s*(?:\{\s*return Some\()?RecordSpec::(Bytes|Words)\s*\{\s*prefix_words:\s*(\d+)",
        text,
    ):
        for name in re.split(r"\s*\|\s*", names.strip()):
            shapes[kind][name] = int(prefix)
    for name, prefix in re.findall(r"(OPR_[A-Z0-9_]+)\s*=>\s*(?:bytes|upload)\((\d+)", text):
        shapes["Bytes"][name] = int(prefix)
    return shapes
def js_map(text, name):
    found = re.search(r"export const " + name + r" = new Map\(\[(.*?)\]\);", text, re.S)
    if not found:
        problems.append(f"decode-budget.mjs declares no {name}")
        return {}
    return {op: int(v) for op, v in re.findall(r"\[(OP[A-Z0-9_]+), (\d+)\]", found.group(1))}
budget_text = Path("platforms/apple/WebContent/PerformancePlus/src/decode-budget.mjs").read_text(encoding="utf-8")
for rust_path, label, bytes_map, words_map in (
    ("engine/crates/frame-wire/src/gl_resource.rs", "res", "PAYLOAD_PREFIX_WORDS", "WORD_LIST_PREFIX_WORDS"),
    ("engine/crates/frame-wire/src/canvas2d.rs", "2d", "CANVAS2D_PAYLOAD_PREFIX_WORDS", "CANVAS2D_WORD_LIST_PREFIX_WORDS"),
):
    shapes = rust_shapes(rust_path)
    for kind, js_name in (("Bytes", bytes_map), ("Words", words_map)):
        rust, js = shapes[kind], js_map(budget_text, js_name)
        if not rust:
            problems.append(f"{label}: no {kind} record parsed out of {rust_path}; the pattern no longer matches")
            continue
        for op in sorted(set(rust) | set(js)):
            if rust.get(op) != js.get(op):
                problems.append(
                    f"{label}: {op}'s {kind.lower()} prefix is {rust.get(op)} in {rust_path} "
                    f"and {js.get(op)} in decode-budget.mjs {js_name}"
                )
        if all(rust.get(op) == js.get(op) for op in set(rust) | set(js)):
            print(f"  - {label} {kind.lower()} prefixes agree with decode-budget.mjs on all {len(rust)}")

print()
if problems:
    print("FAIL: the opcode tables disagree.", file=sys.stderr)
    for problem in problems:
        print(f"  * {problem}", file=sys.stderr)
    print(
        "\n  A record header is twelve bits of opcode and twenty of word count.\n"
        "  Nothing is typed across that boundary: a producer writes a number and\n"
        "  the reader switches on it, so a disagreement here is a frame that does\n"
        "  not draw rather than anything that fails to build.\n",
        file=sys.stderr,
    )
    raise SystemExit(1)

print("PASS: every opcode table agrees, block by block.")
PY
