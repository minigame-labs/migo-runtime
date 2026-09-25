#!/usr/bin/env bash
# Every op the engine's JavaScript calls has a decided place to be answered when
# that JavaScript does not run beside the ops.
#
# WHY THIS GATE EXISTS. On iOS Performance+ the engine's JavaScript API layer runs
# in WebKit's WebContent process and its ops are implemented in the host. There is
# no deno_core there to dispatch `ext:core/ops`, so each op needs a lane -- local,
# frame stream, command, synchronous barrier, awaited reply, or unsupported -- and
# the op shim and the host decoders are built from that decision (plan D15). An op
# added to the engine without one is a call that, on that lane, does nothing and
# says nothing: the failure mode this repository keeps finding, arriving through
# the most-used surface it has.
#
# WHAT IS COMPARED. Three sets that are written in three places:
#   registered  deno_core::extension! op lists     (scripts/lib/runtime_ops.py)
#   imported    `ext:core/ops` imports in engine JS (scripts/lib/runtime_ops.py)
#   classified  contracts/runtime/op-boundary.json
# imported must be a subset of registered, and classified must equal imported,
# op for op and extension for extension. A registered op nothing imports needs no
# lane -- no engine JavaScript can call it -- and is reported rather than failed.
#
# AND THE CORE MEMBERS. `core.read`, `core.close` and `core.tryClose` are calls the
# engine's JavaScript makes on deno's `core` object rather than ops, so they are in
# neither set above and are classified in the contract's `core_members` table. Each
# one there must be a member the engine's JavaScript actually calls, and a member on
# a crossing lane must carry a number in service-ops.json (a `local` one must not,
# because nothing crosses for it).
#
# AND THE SHAPE. A lane is a claim about who answers, and the op's Rust signature
# says whether an answer exists:
#   - an async op (or one returning a future) is awaited, so its lane is `async`
#     or `unsupported`, and only an async op may be `async`;
#   - a `stream` or `command` op sends nothing back, so if its signature returns
#     a value or writes into a caller's buffer the entry must say, in
#     `local_answer`, what the producer answers with instead;
#   - `local_answer` on an op that returns nothing, or on another lane, is a
#     claim about nothing and is refused.
#
# Host-only: python3, no toolchain.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

python3 - "$ROOT" <<'PY'
import importlib.util
import json
import pathlib
import sys

root = pathlib.Path(sys.argv[1])
spec = importlib.util.spec_from_file_location("runtime_ops", root / "scripts/lib/runtime_ops.py")
runtime_ops = importlib.util.module_from_spec(spec)
# Registered before it runs: its dataclasses resolve their annotations through
# sys.modules, and an unregistered module has no entry there.
sys.modules[spec.name] = runtime_ops
spec.loader.exec_module(runtime_ops)

CONTRACT = root / "contracts/runtime/op-boundary.json"
LANES = ("local", "stream", "command", "sync", "async", "unsupported")
ANSWERLESS = ("stream", "command")

errors: list[str] = []


def error(message: str) -> None:
    errors.append(message)


ops, imports = runtime_ops.surface(root)
# A reader that found nothing reports a clean comparison of two empty sets.
if len(ops) < 100 or len(imports) < 100:
    print(
        f"op-boundary FAIL: the source reader found {len(ops)} registered and "
        f"{len(imports)} imported ops; it is not reading the engine",
        file=sys.stderr,
    )
    sys.exit(1)

for name in sorted(set(imports) - set(ops)):
    error(f"{name} is imported by {', '.join(imports[name])} and registered by no extension")

try:
    contract = json.loads(CONTRACT.read_text(encoding="utf-8"))
except (OSError, json.JSONDecodeError) as exc:
    print(f"op-boundary FAIL: {CONTRACT} cannot be read: {exc}", file=sys.stderr)
    sys.exit(1)

lanes = contract.get("lanes") or {}
if tuple(sorted(lanes)) != tuple(sorted(LANES)):
    error(f"the contract defines lanes {sorted(lanes)}; this gate knows {sorted(LANES)}")
for lane, meaning in lanes.items():
    if not isinstance(meaning, str) or not meaning.strip():
        error(f"lane {lane} has no stated meaning")

classified: dict[str, tuple[str, object]] = {}
for extension, entry in sorted((contract.get("extensions") or {}).items()):
    if not isinstance(entry.get("reason"), str) or not entry["reason"].strip():
        error(f"{extension} gives no reason for its lanes")
    entries = entry.get("ops") or {}
    if not entries:
        error(f"{extension} classifies no ops")
    for name, value in entries.items():
        if name in classified:
            error(f"{name} is classified under both {classified[name][0]} and {extension}")
        classified[name] = (extension, value)

imported = {name: op for name, op in ops.items() if op.imported_by}
for name in sorted(set(imported) - set(classified)):
    op = imported[name]
    error(
        f"{name} ({op.extension}) is imported by {', '.join(op.imported_by)} and has no lane in "
        f"{CONTRACT.relative_to(root)}"
    )
for name in sorted(set(classified) - set(imported)):
    reason = "registered but imported by no engine JavaScript" if name in ops else "not an op"
    error(f"{name} is classified and is {reason}; remove its entry")


def returns_value(op) -> bool:
    returns = op.returns.replace(" ", "")
    carries = returns not in ("", "()") and not returns.startswith("Result<(),")
    return carries or op.writes_buffer


for name in sorted(set(classified) & set(imported)):
    extension, value = classified[name]
    op = imported[name]
    if extension != op.extension:
        error(f"{name} is classified under {extension} and registered by {op.extension}")
    if isinstance(value, str):
        lane, local_answer = value, None
    elif isinstance(value, dict):
        lane, local_answer = value.get("lane"), value.get("local_answer")
        unknown = sorted(set(value) - {"lane", "local_answer"})
        if unknown:
            error(f"{name} carries fields this gate does not know: {unknown}")
        if not isinstance(local_answer, str) or not local_answer.strip():
            error(f"{name} is written as an object without a local_answer; write the lane alone")
    else:
        error(f"{name}: an entry is a lane name or an object, not {type(value).__name__}")
        continue
    if lane not in LANES:
        error(f"{name} has lane {lane!r}, which is not one of {list(LANES)}")
        continue
    if op.is_async and lane not in ("async", "unsupported"):
        error(f"{name} is awaited by its callers (async in Rust) and cannot be {lane}")
    if lane == "async" and not op.is_async:
        error(f"{name} is async on the lane but returns synchronously in Rust")
    if lane in ANSWERLESS and returns_value(op) and local_answer is None:
        shape = "writes into a caller's buffer" if op.writes_buffer else f"returns {op.returns}"
        error(
            f"{name} is {lane}, which sends nothing back, and {shape}; say in local_answer what "
            f"the producer answers with"
        )
    if local_answer is not None and not (lane in ANSWERLESS and returns_value(op)):
        error(f"{name} has a local_answer, but a {lane} op that returns nothing needs none")

# The core members: classified against what the engine's JavaScript calls, and
# against the numbers the service stream carries them under.
SERVICE_OPS = root / "contracts/runtime/service-ops.json"
try:
    numbered = json.loads(SERVICE_OPS.read_text(encoding="utf-8"))["ops"]
except (OSError, KeyError, json.JSONDecodeError) as exc:
    numbered = {}
    error(f"{SERVICE_OPS.relative_to(root)} cannot be read: {exc}")

core = contract.get("core_members") or {}
if not isinstance(core.get("$comment"), (str, list)) or not core.get("$comment"):
    error("core_members gives no reason for its lanes")
members = core.get("members") or {}
if not members:
    error("core_members classifies no members")
used = runtime_ops.core_members(root)
for name, lane in sorted(members.items()):
    if name not in used:
        error(f"core.{name} is classified and no engine JavaScript calls it; remove its entry")
    if lane not in LANES:
        error(f"core.{name} has lane {lane!r}, which is not one of {list(LANES)}")
        continue
    # `core_<snake case of the member>` is the name its number is filed under.
    wire = "core_" + "".join(f"_{c.lower()}" if c.isupper() else c for c in name)
    if lane in ("sync", "async", "command"):
        if wire not in numbered:
            error(f"core.{name} is {lane} and has no number in {SERVICE_OPS.name} (expected {wire})")
    elif wire in numbered:
        error(f"core.{name} is {lane} and needs no service number, but {wire} has one")

if errors:
    print("op-boundary contract FAILED:", file=sys.stderr)
    for message in errors:
        print(f"  - {message}", file=sys.stderr)
    sys.exit(1)

counts: dict[str, int] = {}
for extension, value in classified.values():
    lane = value if isinstance(value, str) else value["lane"]
    counts[lane] = counts.get(lane, 0) + 1
unimported = sorted(name for name, op in ops.items() if not op.imported_by)
print(
    f"op-boundary contract: PASS -- {len(classified)} imported ops classified "
    f"({', '.join(f'{lane} {counts.get(lane, 0)}' for lane in LANES)}); "
    f"{len(unimported)} registered ops are imported by no engine JavaScript; "
    f"{len(members)} core members classified"
)
PY
