#!/usr/bin/env bash
# The probe harness must write records the decision tools accept.
#
# THE DRIFT THIS EXISTS TO CATCH is one this project has already paid for in a
# different arm: a Swift file that agrees with a JSON contract to the character
# and has never been compiled, and a lane that passed while checking nothing
# about the artifact it named. The probe harness has a sharper version of the
# same shape, because its failure is not caught by a build at all.
#
# G0 is a lab session on borrowed devices. The decision tool refuses the WHOLE
# RUN when one required field is missing -- deliberately, because a sample
# nobody can characterise cannot be averaged in. So a harness that omits one
# field produces a folder of JSON that looks like a successful day and decides
# nothing, and it says so hours after the phones have gone back.
#
# This gate makes the schema and the encoder answer to each other before the
# devices are booked:
#
#   * every field the contract requires is a coding key the encoder writes,
#   * every closed enum has the same members on both sides, and
#   * every capability the contract declares is a case the record can answer.
#
# The derivation runs in the direction that catches the real failure. Reading
# the schema and looking for each field in the Swift is the check; listing the
# fields here would be a third copy, and a third copy agrees with whichever of
# the other two it was pasted from.
#
# Host-only: python3, no Apple toolchain. The compile-and-run half is
# platforms/apple/core/Tests/MigoProbeCoreTests, which apple-ci.yml runs on the
# macOS runner. This half runs on every Linux PR, months before a lab day.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

PERF_SCHEMA="contracts/apple/performance-probe.schema.json"
CAP_SCHEMA="contracts/apple/capability-probe.schema.json"
PERF_SWIFT="platforms/apple/core/Sources/MigoProbeCore/MigoProbeRecord.swift"
CAP_SWIFT="platforms/apple/core/Sources/MigoProbeCore/MigoCapabilityRecord.swift"

for required in "$PERF_SCHEMA" "$CAP_SCHEMA" "$PERF_SWIFT" "$CAP_SWIFT"; do
    if [[ ! -f "$required" ]]; then
        echo "FAIL: $required is missing; the probe harness cannot be checked against its contract." >&2
        exit 1
    fi
done

if ! command -v python3 >/dev/null 2>&1; then
    echo "FAIL: python3 is not available, so the probe record contract is unverified." >&2
    exit 1
fi

python3 - "$PERF_SCHEMA" "$CAP_SCHEMA" "$PERF_SWIFT" "$CAP_SWIFT" <<'PY'
import json
import re
import sys

perf_schema_path, cap_schema_path, perf_swift_path, cap_swift_path = sys.argv[1:5]

problems: list[str] = []
notes: list[str] = []


def load(path):
    with open(path, encoding="utf-8") as handle:
        return json.load(handle)


def read(path):
    with open(path, encoding="utf-8") as handle:
        return handle.read()


perf_schema = load(perf_schema_path)
cap_schema = load(cap_schema_path)
perf_swift = read(perf_swift_path)
cap_swift = read(cap_swift_path)
swift = perf_swift + "\n" + cap_swift


# --- parsing the Swift side ---------------------------------------------------
#
# One parser, used for every enum and every CodingKeys block. `case foo = "bar"`
# has the wire name written out; a bare `case foo` has "foo" as its raw value,
# which is how `agent`, `origin` and `errors` are spelled. Getting that wrong in
# the direction that DROPS a case would make this gate report a missing field
# that is present, so both forms are exercised by the controls below.
CASE = re.compile(r'^\s*case\s+([A-Za-z_][A-Za-z0-9_]*)\s*(?:=\s*"([^"]*)")?\s*$', re.M)


def members(source: str, declaration: str) -> list[str]:
    """The raw values of the cases inside one enum or CodingKeys declaration."""
    start = source.find(declaration)
    if start < 0:
        return []
    # The declaration's body ends at the closing brace in column zero of the
    # type, or the next declaration at the same nesting -- taking everything up
    # to the next `public enum`/`public struct` is enough and cannot swallow a
    # later declaration's cases.
    rest = source[start + len(declaration):]
    stop = len(rest)
    for terminator in ("\npublic enum ", "\npublic struct ", "\npublic extension ", "\nextension "):
        found = rest.find(terminator)
        if found >= 0:
            stop = min(stop, found)
    body = rest[:stop]
    return [(explicit or name) for name, explicit in CASE.findall(body)]


# The control. Without one, a parser that matched nothing -- a renamed type, a
# changed indentation -- would report "every field is present" for a file it
# never read.
control = members(perf_swift, "public enum MigoProbeClock: String, Codable, Sendable, CaseIterable {")
if sorted(control) != ["display_link_relay", "window_raf_relay", "worker_raf"]:
    problems.append(
        "the control failed: parsing MigoProbeClock returned "
        f"{control!r}, and that enum has three cases with written-out raw values. "
        "The parser is not reading these files, so every clean result below is vacuous."
    )
else:
    notes.append("control: the Swift parser reads MigoProbeClock's three cases")

bare = members(perf_swift, "public enum MigoProbeDeviceClass: String, Codable, Sendable, CaseIterable {")
if sorted(bare) != ["device", "simulator"]:
    problems.append(
        "the second control failed: MigoProbeDeviceClass has two cases with NO written raw "
        f"value, and the parser returned {bare!r}. A parser that only sees `case x = \"y\"` "
        "would silently report every bare case as absent."
    )
else:
    notes.append("control: the parser resolves a bare `case` to its own name")


# --- 1. every required field is a coding key ---------------------------------
def check_required(schema, swift_source, coding_keys_declaration, label):
    required = schema["record"]["required"]
    optional = schema["record"].get("optional", [])
    keys = members(swift_source, coding_keys_declaration)
    if not keys:
        problems.append(f"{label}: no CodingKeys parsed from {coding_keys_declaration!r}")
        return
    missing = [field for field in required if field not in keys]
    if missing:
        problems.append(
            f"{label}: the encoder has no coding key for {', '.join(missing)}. "
            "The decision tool refuses the whole run when one required field is absent, "
            "so every record this harness writes would be refused."
        )
    unknown = [key for key in keys if key not in required and key not in optional]
    if unknown:
        problems.append(
            f"{label}: the encoder writes {', '.join(unknown)}, which no contract names. "
            "The decision tool ignores unknown fields, so a measurement placed in one is "
            "dropped silently rather than refused."
        )
    if not missing and not unknown:
        notes.append(f"{label}: {len(required)} required field(s) present, nothing invented")


check_required(
    perf_schema, perf_swift,
    "public enum CodingKeys: String, CodingKey, CaseIterable {",
    "MigoProbeRecord")
check_required(
    cap_schema, cap_swift,
    "public enum CodingKeys: String, CodingKey, CaseIterable {",
    "MigoCapabilityRecord")


# --- 2. every closed enum agrees, in both directions -------------------------
#
# The mapping from a contract field to the Swift type that carries it is written
# here because the names do not follow one rule -- `lockdown_mode` is
# MigoLockdownMode and `transport_owner` is MigoProbeTransportOwner. What the
# gate will not allow is for the mapping to go stale quietly: a contract enum
# with no entry here is a failure that names itself, so adding an enum to the
# schema forces a decision about how it is checked.
ENUM_TYPES = {
    ("performance", "device_class"): "MigoProbeDeviceClass",
    ("performance", "agent"): "MigoProbeAgent",
    ("performance", "layout"): "MigoProbeLayout",
    ("performance", "origin"): "MigoProbeOrigin",
    ("performance", "transport"): "MigoProbeTransport",
    ("performance", "transport_owner"): "MigoProbeTransportOwner",
    ("performance", "clock"): "MigoProbeClock",
    ("performance", "thermal_state"): "MigoProbeThermalState",
    ("performance", "power_state"): "MigoProbePowerState",
    ("capability", "device_class"): "MigoProbeDeviceClass",
    ("capability", "lockdown_mode"): "MigoLockdownMode",
    # The same Swift type as the performance record's `origin`, on purpose: a
    # capability answered for one origin and a measurement taken at that origin
    # have to name it identically or nothing can join them.
    ("capability", "origin"): "MigoProbeOrigin",
    ("capability", "state"): "MigoCapabilityState",
}

declared_enums = [("performance", name, values)
                  for name, values in perf_schema["record"]["enums"].items()]
declared_enums += [("capability", name, values)
                   for name, values in cap_schema["record"]["enums"].items()]
declared_enums += [("capability", name, values)
                   for name, values in cap_schema["answer"]["enums"].items()]

for origin, field, allowed in declared_enums:
    type_name = ENUM_TYPES.get((origin, field))
    if type_name is None:
        problems.append(
            f"the {origin} contract declares the closed enum {field!r} and this gate has no "
            "Swift type mapped to it, so nothing checks that the harness can record its "
            "values. Add it to ENUM_TYPES."
        )
        continue
    declaration = f"public enum {type_name}: String, "
    start = swift.find(declaration)
    if start < 0:
        problems.append(f"{type_name} is not declared in MigoProbeCore, but {field!r} needs it")
        continue
    line_end = swift.find("{\n", start)
    cases = members(swift, swift[start:line_end + 1])
    if sorted(cases) != sorted(allowed):
        problems.append(
            f"{field!r} differs: the contract allows {sorted(allowed)} and {type_name} "
            f"carries {sorted(cases)}. A value the contract allows and Swift cannot spell "
            "is an arm the harness cannot record; the reverse is an arm the tool rejects."
        )
notes.append(f"{len(declared_enums)} closed enum(s) compared against Swift")


# --- 3. every declared capability is answerable -------------------------------
declared_capabilities = sorted(
    name for name in cap_schema["capabilities"] if not name.startswith("_"))
capability_cases = members(
    cap_swift, "public enum MigoProbeCapability: String, CodingKey, Codable, Sendable, CaseIterable {")
if sorted(capability_cases) != declared_capabilities:
    problems.append(
        f"the capability set differs: the contract declares {declared_capabilities} and "
        f"MigoProbeCapability carries {sorted(capability_cases)}. A declared capability with "
        "no case cannot be answered, and the record writes every case, so a case with no "
        "contract entry writes a key nothing reads."
    )
else:
    notes.append(f"{len(declared_capabilities)} capability question(s) answerable")


# --- 4. the answer type keeps evidence mandatory ------------------------------
#
# The contract's reason for requiring evidence on the yeses is that "true" with
# no evidence is indistinguishable from a probe that returned a default. That
# only holds while the Swift makes it non-optional.
answer_required = cap_schema["answer"]["required"]
if "evidence" in answer_required:
    if re.search(r"public var evidence:\s*String\?", cap_swift):
        problems.append(
            "MigoCapabilityAnswer.evidence is optional while the contract requires it. "
            "An answer with no evidence is indistinguishable from a probe that returned a "
            "default, which is the case this field exists to rule out."
        )
    elif not re.search(r"public var evidence:\s*String\b", cap_swift):
        problems.append("MigoCapabilityAnswer has no `evidence` property at all")
    else:
        notes.append("capability answers cannot be written without evidence")

print()
for note in notes:
    print(f"  - {note}")
print()

if problems:
    print("FAIL: the probe harness and its contracts disagree.", file=sys.stderr)
    print(file=sys.stderr)
    for problem in problems:
        print(f"  * {problem}", file=sys.stderr)
    print(file=sys.stderr)
    print(
        "  Why this matters: G0 is a lab session on borrowed devices, and the decision "
        "tool\n  refuses the whole run over one missing field. A disagreement found here "
        "costs a\n  commit; the same disagreement found on lab day costs the devices.",
        file=sys.stderr)
    raise SystemExit(1)

print("PASS: the probe records carry every field, enum value and capability their contracts declare.")
PY
