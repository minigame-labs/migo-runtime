#!/usr/bin/env python3
"""The engine's op surface, read out of its sources.

Three facts about every op are written in three places, and nothing but a
reader like this one can put them side by side:

  registered  the `ops = [...]` list of a `deno_core::extension!` in
              engine/crates/runtime-v8/src -- what the embedded runtime can call
  defined     the `#[op2]` function itself -- its shape (fast, async, what it
              returns)
  imported    the `import { ... } from "ext:core/ops"` lists of the engine's
              JavaScript -- what the API layer actually calls

`contracts/runtime/op-boundary.json` classifies each registered op for the
lanes that run the engine's JavaScript somewhere other than beside the ops. It
supplies only the classification, which cannot be derived; the op list is
derived here, so a new op that nobody classified, or a classification for an op
that no longer exists, is a failure rather than a gap. The gate is
scripts/test-runtime-op-boundary-contract.sh.

Prints JSON with --json, or a table otherwise. Standard library only: this runs
on every host that runs the gate.
"""

from __future__ import annotations

import argparse
import json
import pathlib
import re
import sys
from dataclasses import asdict, dataclass, field

RUNTIME_SRC = pathlib.Path("engine/crates/runtime-v8/src")

# A block is `extension!(` or `deno_core::extension!(` up to its matching
# parenthesis. Test fixtures register throwaway extensions; those live in files
# under tests/ or named *_tests.rs, or inside #[cfg(test)] modules, and are not
# the engine's surface.
EXTENSION_OPEN = re.compile(r"(?:deno_core::)?extension!\s*\(")
IDENT = re.compile(r"[A-Za-z_][A-Za-z0-9_]*")
OP_DEF = re.compile(
    r"#\[(?:deno_core::)?op2(?P<attr>\([^\]]*\))?\]\s*"
    r"(?:#\[[^\]]*\]\s*)*"
    r"(?:pub(?:\([^)]*\))?\s+)?(?P<async>async\s+)?(?:unsafe\s+)?fn\s+(?P<name>op_[A-Za-z0-9_]+)\b"
)
# An op stamped out by a local macro: `canvas_state_op!(op_save, ...)`.
MACRO_OP = re.compile(r"\b(?P<macro>[a-z_][a-z0-9_]*)!\s*\(\s*(?P<name>op_[A-Za-z0-9_]+)\b")
IMPORT_OPS = re.compile(r"import\s*\{(?P<names>[^}]*)\}\s*from\s*[\"']ext:core/ops[\"']", re.S)
CORE_OPS_REF = re.compile(r"\b(?:core\.)?ops\.(op_[A-Za-z0-9_]+)")


@dataclass
class Op:
    name: str
    extension: str
    source: str = ""
    attributes: list[str] = field(default_factory=list)
    is_async: bool = False
    # What the op hands back to JavaScript, as written after `->`; empty for
    # none. Macro-stamped ops leave it empty and say so in `attributes`.
    returns: str = ""
    # Whether a parameter is a mutable slice the op writes its answer into --
    # an answer JavaScript waits for as surely as a return value.
    writes_buffer: bool = False
    imported_by: list[str] = field(default_factory=list)


def mask_rust(text: str) -> str:
    """The source with comments and the contents of string and char literals
    blanked, length preserved.

    One pass, because the two cannot be removed separately: `"http://"` is a
    string that contains a comment marker, and `// "` is a comment that contains
    a quote. Structure -- brackets, attributes, identifiers -- is then read from
    what remains, where neither can masquerade as it.
    """
    out = list(text)
    length = len(text)
    index = 0

    def blank(start: int, stop: int) -> None:
        for position in range(start, min(stop, length)):
            if out[position] != "\n":
                out[position] = " "

    while index < length:
        char = text[index]
        if text.startswith("//", index):
            stop = text.find("\n", index)
            stop = length if stop < 0 else stop
            blank(index, stop)
            index = stop
            continue
        if text.startswith("/*", index):
            depth, cursor = 1, index + 2
            while cursor < length and depth:
                if text.startswith("/*", cursor):
                    depth, cursor = depth + 1, cursor + 2
                elif text.startswith("*/", cursor):
                    depth, cursor = depth - 1, cursor + 2
                else:
                    cursor += 1
            blank(index, cursor)
            index = cursor
            continue
        raw = re.match(r'b?r(#*)"', text[index : index + 260])
        if raw and (index == 0 or not (text[index - 1].isalnum() or text[index - 1] == "_")):
            opening = raw.end()
            closing = '"' + raw.group(1)
            stop = text.find(closing, index + opening)
            stop = length if stop < 0 else stop + len(closing)
            blank(index + opening, stop - len(closing))
            index = stop
            continue
        if char == '"':
            cursor = index + 1
            while cursor < length and text[cursor] != '"':
                cursor += 2 if text[cursor] == "\\" else 1
            blank(index + 1, cursor)
            index = cursor + 1
            continue
        if char == "'":
            if index + 1 < length and text[index + 1] == "\\":
                stop = text.find("'", index + 2)
                if stop > 0:
                    blank(index + 1, stop)
                    index = stop + 1
                    continue
            elif index + 2 < length and text[index + 2] == "'":
                blank(index + 1, index + 2)
                index += 3
                continue
        index += 1
    return "".join(out)


def strip_js_comments(text: str) -> str:
    text = re.sub(r"/\*.*?\*/", " ", text, flags=re.S)
    return re.sub(r"(?m)^\s*//[^\n]*", " ", text)


def balanced(text: str, open_index: int) -> str:
    """The text between the bracket at open_index and its match. Call it on
    masked source, where brackets in comments and literals are gone."""
    depth = 0
    for index in range(open_index, len(text)):
        char = text[index]
        if char in "([{":
            depth += 1
        elif char in ")]}":
            depth -= 1
            if depth == 0:
                return text[open_index + 1 : index]
    raise ValueError("unbalanced brackets")


def is_test_source(path: pathlib.Path) -> bool:
    parts = path.parts
    return "tests" in parts or path.name.endswith("_tests.rs") or path.name == "tests.rs"


def cfg_test_spans(text: str) -> list[tuple[int, int]]:
    spans = []
    for match in re.finditer(r"#\[cfg\(test\)\]\s*mod\s+\w+\s*\{", text):
        brace = text.index("{", match.start())
        body = balanced(text, brace)
        spans.append((match.start(), brace + len(body) + 2))
    return spans


def registered_ops(root: pathlib.Path) -> dict[str, Op]:
    ops: dict[str, Op] = {}
    for path in sorted((root / RUNTIME_SRC).rglob("*.rs")):
        if is_test_source(path.relative_to(root / RUNTIME_SRC)):
            continue
        text = mask_rust(path.read_text(encoding="utf-8"))
        excluded = cfg_test_spans(text)
        for match in EXTENSION_OPEN.finditer(text):
            if any(start <= match.start() < end for start, end in excluded):
                continue
            body = balanced(text, match.end() - 1)
            name_match = IDENT.search(body)
            if not name_match:
                continue
            extension = name_match.group(0)
            ops_match = re.search(r"\bops\s*=\s*\[", body)
            if not ops_match:
                continue
            listed = balanced(body, ops_match.end() - 1)
            for entry in listed.split(","):
                entry = entry.strip()
                if not entry:
                    continue
                # `op_x`, `module::op_x`, `op_x<T>` and `op_x::<T>` all name op_x.
                path_only = re.sub(r"<[^<>]*>", "", entry).replace(" ", "")
                segments = [part for part in path_only.split("::") if part.startswith("op_")]
                if not segments or not IDENT.fullmatch(segments[-1]):
                    raise ValueError(f"{path}: unreadable op entry {entry!r} in {extension}")
                name = segments[-1]
                if name in ops and ops[name].extension != extension:
                    raise ValueError(
                        f"{name} is registered by both {ops[name].extension} and {extension}"
                    )
                ops[name] = Op(name=name, extension=extension)
    return ops


def attach_definitions(root: pathlib.Path, ops: dict[str, Op]) -> None:
    for path in sorted((root / RUNTIME_SRC).rglob("*.rs")):
        text = mask_rust(path.read_text(encoding="utf-8"))
        for match in OP_DEF.finditer(text):
            op = ops.get(match.group("name"))
            if op is None or op.source:
                continue
            attr = (match.group("attr") or "").strip("()")
            op.source = str(path.relative_to(root))
            op.attributes = [part.strip() for part in attr.split(",") if part.strip()]
            op.is_async = bool(match.group("async")) or "async" in op.attributes
            paren = text.index("(", match.end())
            params = balanced(text, paren)
            op.writes_buffer = bool(re.search(r"&\s*mut\s*\[", params))
            after = text[paren + len(params) + 2 :]
            arrow = re.match(r"\s*->\s*", after)
            if arrow:
                body = after[arrow.end() :]
                depth, end = 0, 0
                for end, char in enumerate(body):
                    if char in "<([":
                        depth += 1
                    elif char in ">)]":
                        depth -= 1
                    elif char in "{;" and depth == 0:
                        break
                    elif body.startswith("where", end) and depth == 0:
                        break
                op.returns = " ".join(body[:end].split())
                # A `fast` op that hands back a future is awaited by its caller
                # exactly like an `async` one; the attribute does not say so.
                if "Future" in op.returns:
                    op.is_async = True
        for match in MACRO_OP.finditer(text):
            op = ops.get(match.group("name"))
            if op is None or op.source:
                continue
            op.source = str(path.relative_to(root))
            op.attributes = [f"macro:{match.group('macro')}"]


def imported_ops(root: pathlib.Path) -> dict[str, list[str]]:
    imports: dict[str, list[str]] = {}
    for path in sorted((root / RUNTIME_SRC).rglob("*.js")):
        text = strip_js_comments(path.read_text(encoding="utf-8"))
        relative = str(path.relative_to(root))
        names: set[str] = set()
        for match in IMPORT_OPS.finditer(text):
            for item in match.group("names").split(","):
                item = item.strip()
                if not item:
                    continue
                names.add(item.split(" as ")[0].strip())
        names.update(CORE_OPS_REF.findall(text))
        for name in names:
            imports.setdefault(name, []).append(relative)
    return imports


def surface(root: pathlib.Path) -> tuple[dict[str, Op], dict[str, list[str]]]:
    ops = registered_ops(root)
    attach_definitions(root, ops)
    imports = imported_ops(root)
    for name, files in imports.items():
        if name in ops:
            ops[name].imported_by = sorted(files)
    return ops, imports


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--root", default=".", help="repository root")
    parser.add_argument("--json", action="store_true", help="print JSON")
    args = parser.parse_args(argv)
    root = pathlib.Path(args.root).resolve()
    ops, imports = surface(root)
    unregistered = sorted(set(imports) - set(ops))
    if args.json:
        json.dump(
            {
                "ops": {name: asdict(op) for name, op in sorted(ops.items())},
                "imported_but_unregistered": {name: imports[name] for name in unregistered},
            },
            sys.stdout,
            indent=2,
        )
        print()
        return 0
    for name, op in sorted(ops.items(), key=lambda item: (item[1].extension, item[0])):
        shape = "async" if op.is_async else ("fast" if "fast" in op.attributes else "plain")
        print(f"{op.extension:24} {name:48} {shape:6} {len(op.imported_by)}")
    for name in unregistered:
        print(f"{'<unregistered>':24} {name:48} imported by {', '.join(imports[name])}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
