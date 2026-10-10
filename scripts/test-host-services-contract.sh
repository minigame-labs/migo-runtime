#!/usr/bin/env bash
# The host-service channel's numbers mean the same thing on both sides of the C
# ABI, and every name the channel routes to still exists.
#
# WHY THIS GATE EXISTS. A C host calls the engine's ads, payment, sign-in,
# sharing and navigation by three integers -- a service, a method, an event --
# and the engine turns a completion into a call on one of content's host-bridge
# hooks. Each of those facts is written in more than one place:
#
#   contracts/runtime/host-services.json       the definition
#   include/migo/host_services.h               what a C, C++ or Swift host compiles
#   engine/crates/capi-abi/src/host_services.rs the library's constants
#   engine/crates/runtime-v8/src               the ops content's calls go through,
#                                               and the hooks results land on
#
# Two halves that disagree about one number call a different method with the
# right-looking payload, and nothing at either end can see it: the library
# answers MIGO_OK, the host's handler runs, content waits forever. A hook that
# was renamed, or an op that was removed, fails the same silent way -- the
# result is delivered to a name nobody listens on. So the header and the Rust
# constants are compared to the contract in both directions (a stray constant
# is a number the contract does not reserve), and every op and hook the
# contract names must still be registered.
#
# The Rust routing table (engine/crates/capi/src/host_services.rs) is held to
# the same contract by its own unit test, `the_table_is_the_contract`, which
# needs a compiler; this gate needs only python3.
#
# Usage: test-host-services-contract.sh [repository-root]
set -euo pipefail

ROOT="${1:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}"
SCRIPTS="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

python3 - "$ROOT" "$SCRIPTS/lib" <<'PY'
import json
import pathlib
import re
import sys

root = pathlib.Path(sys.argv[1])
sys.path.insert(0, sys.argv[2])
import runtime_ops  # noqa: E402

errors: list[str] = []


def fail(message: str) -> None:
    errors.append(message)


contract_path = root / "contracts/runtime/host-services.json"
contract = json.loads(contract_path.read_text(encoding="utf-8"))
services = contract.get("services")
if not isinstance(services, dict) or not services:
    print(f"FAIL: {contract_path} defines no services", file=sys.stderr)
    sys.exit(1)

# ---- the contract is well formed ------------------------------------------------

expected: dict[str, int] = {}
ops: dict[str, str] = {}
hooks: dict[str, str] = {}
service_ids: dict[int, str] = {}
CODE_FIELDS = {"errCode", "errno", None}
# A result field naming a handed-over file: `a`, `a[]`, `a.b`, `a[].b`, ...
FILE_FIELD = re.compile(r"[A-Za-z][A-Za-z0-9]*(\[\])?(\.[A-Za-z][A-Za-z0-9]*(\[\])?)*")

for name, service in services.items():
    if not re.fullmatch(r"[a-z][a-z_]*", name):
        fail(f"service name {name!r} is not lower_snake_case")
    sid = service.get("id")
    if not isinstance(sid, int) or not 0 <= sid < 64:
        fail(f"{name}: id {sid!r} is not a bit index in host_services (0..63)")
        continue
    if sid in service_ids:
        fail(f"{name}: id {sid} is already {service_ids[sid]}'s")
    service_ids[sid] = name
    upper = name.upper()
    expected[f"MIGO_HOST_SERVICE_{upper}"] = sid

    method_ids: dict[int, str] = {}
    for method_name, method in service.get("methods", {}).items():
        where = f"{name}.{method_name}"
        mid = method.get("id")
        if not isinstance(mid, int) or not 0 <= mid < 65536:
            fail(f"{where}: id {mid!r} is not a method number (0..65535)")
            continue
        if mid in method_ids:
            fail(f"{where}: id {mid} is already {name}.{method_ids[mid]}'s")
        method_ids[mid] = method_name
        expected[f"MIGO_{upper}_{method_name.upper()}"] = mid
        op = method.get("op")
        if not isinstance(op, str) or not op.startswith("op_"):
            fail(f"{where}: no op")
        else:
            ops[op] = where
        kind = method.get("kind")
        if kind == "call":
            if not isinstance(method.get("hook"), str) or not method["hook"]:
                fail(f"{where}: a call needs `hook`")
            if "api" in method:
                fail(f"{where}: content composes errMsg from the API it called, so `api` means nothing")
            if "error_code_field" not in method or method["error_code_field"] not in CODE_FIELDS:
                fail(f"{where}: error_code_field must be one of errCode, errno or null")
            if isinstance(method.get("hook"), str):
                hooks[method["hook"]] = where
            if "progress_hook" in method:
                if not isinstance(method["progress_hook"], str) or not method["progress_hook"]:
                    fail(f"{where}: progress_hook must name a hook")
                else:
                    hooks[method["progress_hook"]] = f"{where} progress"
            withheld = method.get("withheld", [])
            if not isinstance(withheld, list) or not all(isinstance(f, str) and f for f in withheld):
                fail(f"{where}: withheld must list result field names")
            files = method.get("files", [])
            if not isinstance(files, list) or not all(
                isinstance(f, str) and FILE_FIELD.fullmatch(f) for f in files
            ):
                fail(f"{where}: files must list result fields as `a`, `a[]` or `a[].b`")
        elif kind == "command":
            for key in ("api", "hook", "error_code_field", "progress_hook", "withheld", "files"):
                if key in method:
                    fail(f"{where}: a command is answered by nothing, so `{key}` means nothing")
        else:
            fail(f"{where}: kind {kind!r} is neither call nor command")

    event_ids: dict[int, str] = {}
    for event_name, event in service.get("events", {}).items():
        where = f"{name} event {event_name}"
        eid = event.get("id")
        if not isinstance(eid, int) or eid < 0:
            fail(f"{where}: id {eid!r} is not an event number")
            continue
        if eid in event_ids:
            fail(f"{where}: id {eid} is already {event_ids[eid]}'s")
        event_ids[eid] = event_name
        expected[f"MIGO_{upper}_EVENT_{event_name.upper()}"] = eid
        if not isinstance(event.get("hook"), str) or not event["hook"]:
            fail(f"{where}: no hook")
        else:
            hooks[event["hook"]] = where

# Names in the header's MIGO_HOST_SERVICE_ space that are not service numbers.
NOT_NUMBERING = re.compile(r"MIGO_HOST_SERVICE_(STATUS_|RESULT_FLAG_|PAYLOAD_MAX_BYTES$)")
prefixes = tuple(f"MIGO_{name.upper()}_" for name in services)


def numbering(name: str) -> bool:
    if name.startswith("MIGO_HOST_SERVICE_"):
        return not NOT_NUMBERING.match(name)
    return name.startswith(prefixes)


def compare(label: str, defined: dict[str, int]) -> None:
    for name, value in sorted(expected.items()):
        if name not in defined:
            fail(f"{label}: {name} = {value} is in the contract and not defined")
        elif defined[name] != value:
            fail(f"{label}: {name} is {defined[name]}, the contract says {value}")
    for name in sorted(n for n in defined if numbering(n) and n not in expected):
        fail(f"{label}: {name} is a number the contract does not reserve")


# ---- a service's closed list of names -------------------------------------------
#
# The ecosystem service carries content APIs by name; its contract lists every
# name it may carry, and the runtime module that defines them must list the same:
# a name only one side knows is a request no host expects, or one content cannot
# make.

# A name is an API's, or an object member's as `Class.member`.
NAME = r"'([A-Za-z]+(?:\.[A-Za-z]+)?)'"


def js_names(text: str, kind: str) -> list[str]:
    names = []
    if kind == "events":
        block = re.search(r"const ECOSYSTEM_EVENTS = \{(.*?)\};", text, re.S)
        names += re.findall(r"^\s+(on[A-Za-z]+):", block.group(1), re.M) if block else []
    # A request (or an event) is a function's of its own, or an object's.
    constants = {
        "calls": ("ECOSYSTEM_CALLS", "ECOSYSTEM_OBJECT_CALLS"),
        "events": ("ECOSYSTEM_OBJECT_EVENTS",),
        "values": ("ECOSYSTEM_VALUES",),
    }
    for constant in constants[kind]:
        block = re.search(r"const " + constant + r" = \[(.*?)\];", text, re.S)
        names += re.findall(NAME, block.group(1)) if block else []
    return names


for name, service in services.items():
    names = service.get("names")
    if names is None:
        continue
    source = root / names.get("declared_in", "")
    if not source.is_file():
        fail(f"{name}: names.declared_in {names.get('declared_in')!r} is not a file")
        continue
    text = runtime_ops.strip_js_comments(source.read_text(encoding="utf-8"))
    for kind in ("calls", "events", "values"):
        declared = sorted(js_names(text, kind))
        listed = names.get(kind, [])
        if listed != sorted(listed):
            fail(f"{name}: names.{kind} is not sorted")
        for missing in sorted(set(declared) - set(listed)):
            fail(f"{name}: {source.name} declares {kind[:-1]} {missing}, the contract does not")
        for extra in sorted(set(listed) - set(declared)):
            fail(f"{name}: the contract lists {kind[:-1]} {extra}, {source.name} does not declare it")

# ---- the C header ----------------------------------------------------------------

header_path = root / "include/migo/host_services.h"
header = re.sub(r"/\*.*?\*/|//[^\n]*", " ", header_path.read_text(encoding="utf-8"), flags=re.S)
header_defines = {
    m.group(1): int(m.group(2))
    for m in re.finditer(r"#define\s+(MIGO_[A-Z0-9_]+)\s+(\d+)U\b", header)
}
compare(str(header_path.relative_to(root)), header_defines)

# ---- the library's Rust constants ----------------------------------------------

abi_path = root / "engine/crates/capi-abi/src/host_services.rs"
abi_text = abi_path.read_text(encoding="utf-8")
abi_defines = {
    m.group(1): int(m.group(2))
    for m in re.finditer(r"pub const (MIGO_[A-Z0-9_]+): u32 = (\d+);", abi_text)
}
compare(str(abi_path.relative_to(root)), abi_defines)
known = re.search(r"MIGO_HOST_SERVICES_KNOWN: u64 =(.*?);", abi_text, re.S)
if not known:
    fail(f"{abi_path.relative_to(root)}: no MIGO_HOST_SERVICES_KNOWN")
else:
    named = set(re.findall(r"\(1 << (MIGO_HOST_SERVICE_[A-Z_]+)\)", known.group(1)))
    services_expected = {f"MIGO_HOST_SERVICE_{n.upper()}" for n in services}
    if named != services_expected:
        fail(
            "MIGO_HOST_SERVICES_KNOWN names "
            f"{sorted(named)}, the contract's services are {sorted(services_expected)}"
        )

# ---- the engine's ops and hooks --------------------------------------------------

registered = runtime_ops.registered_ops(root)
for op, where in sorted(ops.items()):
    if op not in registered:
        fail(f"{where}: {op} is not registered by any engine extension")

js_hooks: set[str] = set()
for path in (root / runtime_ops.RUNTIME_SRC).rglob("*.js"):
    text = runtime_ops.strip_js_comments(path.read_text(encoding="utf-8"))
    js_hooks.update(re.findall(r"\b(_internal[A-Za-z0-9]+)\s*:\s*core\.propNonEnumerable\(", text))
for hook, where in sorted(hooks.items()):
    if hook not in js_hooks:
        fail(f"{where}: hook {hook} is not installed by the engine's JavaScript")

if errors:
    for message in errors:
        print(f"FAIL: {message}", file=sys.stderr)
    sys.exit(1)

print(
    f"OK: {len(services)} host services, {len(expected)} numbers, "
    f"{len(ops)} ops and {len(hooks)} hooks agree with the contract"
)
PY
