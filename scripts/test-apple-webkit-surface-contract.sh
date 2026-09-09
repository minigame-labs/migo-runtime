#!/usr/bin/env bash
# The WebKit Full lane's compliance surface says one thing in two files.
#
# contracts/apple/webkit-full-surface.json is what the lane claims to expose, and
# MigoWebKitSurface.swift is what it exposes. The claim is the one a submission
# rests on: App Review 4.7 permits mini games and 4.7.2 requires Apple's prior
# permission before an app exposes native APIs to them, so "which of these does
# content reach, and by what route" is a reviewable fact about the product.
#
# A fact stated twice drifts, and this pair drifts in the direction that does not
# announce itself: the file a reader checks is the contract, and the file content
# actually meets is the Swift. A bridge method added to the table without a row in
# the contract is a native API reachable from content and absent from the document
# describing the lane -- which is the exact shape of the finding 4.7.2 exists to
# produce.
#
# THE ASYMMETRY THIS ALSO CHECKS was missing from the design it implements. The
# plan's D18 described the tier as a list of switches including canvas, WebGL,
# audio, storage and network. In this lane WebKit runs the JavaScript, so those
# arrive with the web platform and no switch exists; a contract that marked one of
# them off would state a control the runtime does not have. So provenance is
# checked, not just membership: what WebKit grants may never be marked
# default-off, and what has no mechanism may never be marked on.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

python3 - "$ROOT" <<'PYTHON'
from __future__ import annotations

import json
import pathlib
import re
import sys

root = pathlib.Path(sys.argv[1])
contract_path = root / "contracts/apple/webkit-full-surface.json"
swift_path = root / "platforms/apple/core/Sources/MigoAppleCore/MigoWebKitSurface.swift"

problems: list[str] = []
notes: list[str] = []

for path in (contract_path, swift_path):
    if not path.is_file():
        print(f"FAIL: {path.relative_to(root)} is missing", file=sys.stderr)
        raise SystemExit(1)

contract = json.loads(contract_path.read_text(encoding="utf-8"))
swift = swift_path.read_text(encoding="utf-8")


def enum_body(source: str, name: str) -> str:
    """The braces of `enum <name>`, matched rather than regexed.

    An enum here holds nested declarations and comments containing braces, so a
    regex that stopped at the first `}` would read half a table and report the
    other half as missing -- a gate that fails for its own reason.
    """
    match = re.search(r"enum\s+" + re.escape(name) + r"\s*:[^\{]*\{", source)
    if not match:
        return ""
    depth = 0
    for index in range(match.end() - 1, len(source)):
        if source[index] == "{":
            depth += 1
        elif source[index] == "}":
            depth -= 1
            if depth == 0:
                return source[match.end() : index]
    return ""


def raw_values(body: str) -> dict[str, str]:
    """Swift case name -> raw value. A bare `case x` has the raw value "x"."""
    found: dict[str, str] = {}
    for line in body.splitlines():
        stripped = line.strip()
        if not stripped.startswith("case "):
            continue
        match = re.match(r'case\s+(\w+)\s*(?:=\s*"([^"]+)")?\s*$', stripped)
        if match:
            found[match.group(1)] = match.group(2) or match.group(1)
    return found


capability_raw = raw_values(enum_body(swift, "Capability"))
provenance_raw = raw_values(enum_body(swift, "Provenance"))

if not capability_raw or not provenance_raw:
    print(
        "FAIL: the Swift enums could not be read, so every agreement reported below would be "
        "an agreement between the contract and nothing",
        file=sys.stderr)
    raise SystemExit(1)
notes.append(
    f"read {len(capability_raw)} capability case(s) and {len(provenance_raw)} provenance case(s) "
    "out of the Swift")

# --- the Swift table ---------------------------------------------------------

table_match = re.search(r"static let entries: \[Entry\] = \[(.*?)\n    \]", swift, re.S)
if not table_match:
    print("FAIL: the Swift entries table could not be located", file=sys.stderr)
    raise SystemExit(1)

row = re.compile(
    r"Entry\(\s*\.(\w+),\s*\.(\w+),\s*defaultEnabled:\s*(true|false)"
    r"(?:,\s*bridgeMethod:\s*\"([^\"]+)\")?\s*\)")
swift_rows: dict[str, dict] = {}
for match in row.finditer(table_match.group(1)):
    case, provenance, default, method = match.groups()
    if case not in capability_raw:
        problems.append(f"the table names .{case}, which the Capability enum does not declare")
        continue
    if provenance not in provenance_raw:
        problems.append(f"the table names .{provenance}, which the Provenance enum does not declare")
        continue
    name = capability_raw[case]
    if name in swift_rows:
        problems.append(f"{name} has two rows in the Swift table")
    swift_rows[name] = {
        "provenance": provenance_raw[provenance],
        "default_enabled": default == "true",
        "bridge_method": method,
    }

if len(swift_rows) < 2:
    print(
        f"FAIL: only {len(swift_rows)} row(s) parsed out of the Swift table; the comparison "
        "below would be vacuous",
        file=sys.stderr)
    raise SystemExit(1)
notes.append(f"parsed {len(swift_rows)} row(s) out of the Swift table")

# Every case the enum can name must have a row. A case without one is a
# capability `entry(for:)` traps on at runtime, on a device, in front of whoever
# is holding it.
missing_rows = sorted(set(capability_raw.values()) - set(swift_rows))
if missing_rows:
    problems.append(
        f"the Capability enum declares {', '.join(missing_rows)} with no row in the table")

# --- the contract ------------------------------------------------------------

declared = {k: v for k, v in contract["capabilities"].items() if not k.startswith("_")}
notes.append(f"read {len(declared)} capability row(s) out of the contract")

if set(contract["provenances"]) != set(provenance_raw.values()):
    problems.append(
        "the contract's provenances and the Swift Provenance enum are different sets: "
        f"contract has {', '.join(sorted(contract['provenances']))}, Swift has "
        f"{', '.join(sorted(provenance_raw.values()))}")

only_contract = sorted(set(declared) - set(swift_rows))
only_swift = sorted(set(swift_rows) - set(declared))
if only_contract:
    problems.append(
        f"the contract declares {', '.join(only_contract)} and the runtime has no row: content "
        "cannot reach it and the document says it can")
if only_swift:
    problems.append(
        f"the runtime has a row for {', '.join(only_swift)} and the contract does not declare it: "
        "a capability content can reach that the lane's compliance surface does not mention")

for name in sorted(set(declared) & set(swift_rows)):
    want, have = declared[name], swift_rows[name]
    if want["provenance"] != have["provenance"]:
        problems.append(
            f"{name}: the contract says {want['provenance']} and the runtime says "
            f"{have['provenance']}. Provenance decides whether 4.7.2 applies to it at all")
    if bool(want["default_enabled"]) != have["default_enabled"]:
        problems.append(
            f"{name}: the contract has default_enabled={want['default_enabled']} and the runtime "
            f"has {have['default_enabled']}")
    if want.get("bridge_method") != have["bridge_method"]:
        problems.append(
            f"{name}: the contract's bridge method is {want.get('bridge_method')!r} and the "
            f"runtime's is {have['bridge_method']!r}. A method name only one side knows is a "
            "message that is never served or a method never documented")

# --- properties the contract has to hold whatever it says --------------------

for name, entry in sorted(declared.items()):
    provenance = entry.get("provenance")
    if provenance not in contract["provenances"]:
        problems.append(f"{name}: provenance {provenance!r} is not one the contract defines")
    if not entry.get("reason"):
        problems.append(
            f"{name}: no reason. A capability list without reasons is a list nobody can review, "
            "and the review is the point")
    has_method = bool(entry.get("bridge_method"))
    if provenance == "host_bridge" and not has_method:
        problems.append(f"{name}: a native bridge with no method name cannot be called or refused")
    if provenance != "host_bridge" and has_method:
        problems.append(
            f"{name}: {provenance} and it names a bridge method. A native call wearing a web "
            "capability's name is a native API outside the compliance surface")
    if provenance == "web_platform" and not entry.get("default_enabled"):
        problems.append(
            f"{name}: WebKit grants it to any page and the contract marks it off. That states a "
            "control the runtime does not have")
    if provenance == "absent" and entry.get("default_enabled"):
        problems.append(f"{name}: nothing reaches it and the contract marks it on")
    if provenance == "host_granted" and not entry.get("granted_by"):
        problems.append(
            f"{name}: host_granted with no granted_by. The whole content of that provenance is "
            "which callback the host answers")
    if provenance == "host_granted" and entry.get("default_enabled"):
        problems.append(
            f"{name}: a capability WebKit asks the host about is on by default, so the lane's "
            "review position -- no permission in the default surface -- is not what it ships")

methods = [e["bridge_method"] for e in declared.values() if e.get("bridge_method")]
if len(methods) != len(set(methods)):
    problems.append("two capabilities in the contract answer one bridge method name")

if problems:
    for problem in problems:
        print(f"FAIL: {problem}", file=sys.stderr)
    raise SystemExit(1)

for note in notes:
    print(f"  {note}")
print(
    f"PASS: the WebKit Full surface agrees across both files ({len(declared)} capabilities, "
    f"{len(methods)} bridge method(s))")
PYTHON
