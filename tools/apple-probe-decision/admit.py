#!/usr/bin/env python3
"""Turn capability-gate records into the candidate set later gates may measure.

Measurement gate 1 answers what a device can host. The plan says that answer
cuts the candidates every later gate draws from, and that sentence is only true
if something performs the cut: left in prose it becomes a paragraph nobody runs,
and the first person to build a performance matrix builds the full cross product
-- including arms no device can host, whose numbers then belong to whatever ran
instead of them.

This is the counterpart to decide.py and it refuses in the same spirit. "We only
had the simulator", "we only measured one OS" and "nothing was eliminated" are
three ways a candidate set gets waved through, and each of them produces a
refusal here with the reason attached.

WHAT IT WILL NOT DO is pick a winner. Admission is not selection: a candidate
that survives gate 1 has only been shown to be runnable, and which of the
survivors is fastest is decide.py's question, on data this tool's output says is
worth collecting.

Rules come from contracts/apple/capability-probe.schema.json, and the candidate
space from contracts/apple/performance-probe.schema.json, so neither is written
twice.

Usage:
    python3 tools/apple-probe-decision/admit.py \\
        --input  docs/performance/apple/g0/capability \\
        --output docs/performance/apple/g0/admission.json

Exit status is 0 when the tool ran, whatever it decided. `--require-admission`
turns a refusal into a non-zero exit.
"""

from __future__ import annotations

import argparse
import itertools
import json
import sys
from pathlib import Path
from typing import Any

REPO_ROOT = Path(__file__).resolve().parents[2]
CAPABILITY_SCHEMA = REPO_ROOT / "contracts" / "apple" / "capability-probe.schema.json"
PERFORMANCE_SCHEMA = REPO_ROOT / "contracts" / "apple" / "performance-probe.schema.json"


class RunRejected(Exception):
    """The records cannot support an admission. Carries every reason, not the first."""

    def __init__(self, reasons: list[str]) -> None:
        super().__init__("; ".join(reasons))
        self.reasons = reasons


def load_json(path: Path) -> dict[str, Any]:
    with path.open(encoding="utf-8") as handle:
        return json.load(handle)


def load_records(directory: Path) -> list[dict[str, Any]]:
    if not directory.is_dir():
        raise SystemExit(f"--input {directory} is not a directory")
    records: list[dict[str, Any]] = []
    for path in sorted(directory.rglob("*.json")):
        loaded = load_json(path)
        items = loaded if isinstance(loaded, list) else [loaded]
        for item in items:
            if not isinstance(item, dict):
                raise SystemExit(f"{path}: a record must be an object")
            item["_source"] = str(path.relative_to(directory))
            records.append(item)
    if not records:
        raise SystemExit(f"--input {directory} contains no *.json records")
    return records


def validate(records: list[dict[str, Any]], schema: dict[str, Any]) -> list[str]:
    """Field, enum and completeness problems, as a list."""
    required = schema["record"]["required"]
    enums = schema["record"]["enums"]
    declared = sorted(name for name in schema["capabilities"] if not name.startswith("_"))
    answer_states = schema["answer"]["enums"]["state"]
    answer_required = schema["answer"]["required"]
    problems: list[str] = []

    for record in records:
        source = record.get("_source", "?")
        missing = [field for field in required if field not in record]
        if missing:
            problems.append(f"{source}: missing {', '.join(missing)}")
        for field, allowed in enums.items():
            value = record.get(field)
            if value is not None and value not in allowed:
                problems.append(
                    f"{source}: {field}={value!r} is not one of {', '.join(allowed)}")

        answers = record.get("capabilities")
        if not isinstance(answers, dict):
            problems.append(f"{source}: capabilities is not an object")
            continue

        # An absent capability and a `false` read the same to a person and
        # differently to this tool, so absence is refused rather than defaulted.
        absent = [name for name in declared if name not in answers]
        if absent:
            problems.append(f"{source}: no answer for {', '.join(absent)}")
        unknown = [name for name in answers if name not in declared]
        if unknown:
            problems.append(
                f"{source}: answers {', '.join(sorted(unknown))}, which the contract does not "
                "declare; a capability nothing consumes is a probe reporting into the void")
        for name, answer in answers.items():
            if not isinstance(answer, dict):
                problems.append(f"{source}: the answer for {name} is not an object")
                continue
            for field in answer_required:
                if field not in answer or answer[field] in (None, ""):
                    problems.append(
                        f"{source}: {name} has no {field}; an answer with no evidence is "
                        "indistinguishable from a probe that returned a default")
            state = answer.get("state")
            if state is not None and state not in answer_states:
                problems.append(f"{source}: {name} has state {state!r}")
    return problems


def usable(records: list[dict[str, Any]], schema: dict[str, Any]) -> tuple[list[dict], list[str]]:
    rules = schema["probe_rules"]
    notes: list[str] = []
    kept: list[dict[str, Any]] = []
    for record in records:
        source = record.get("_source", "?")
        if record.get("device_class") == "simulator" and not rules["simulator_counts_as_evidence"]:
            # The simulator runs the host's JavaScriptCore on the host's CPU, so
            # it answers "is JIT on" with the host's answer -- yes on every Mac,
            # and nothing about an iPhone in Lockdown Mode. Some answers here
            # would in fact be the same, and admitting the schema-shaped half
            # while rejecting the device-shaped half is the split nobody
            # maintains correctly.
            notes.append(f"{source}: excluded, simulator answers are not device evidence")
            continue
        kept.append(record)
    return kept, notes


def candidate_space(performance_schema: dict[str, Any], capability_schema: dict[str, Any]):
    """Every (variable, origin) assignment the performance matrix could contain.

    Derived from the performance schema's own enums rather than listed here: a
    list would need the same maintenance as the thing it describes, and would
    quietly stop covering a level somebody added.
    """
    enums = performance_schema["record"]["enums"]
    dimensions = [name for name in performance_schema["variables"] if name != "_comment"]
    dimensions.append("origin")
    levels = [enums[name] for name in dimensions]
    combination_rules = performance_schema["record"].get("combination_rules", [])

    for values in itertools.product(*levels):
        candidate = dict(zip(dimensions, values))
        impossible = False
        for rule in combination_rules:
            if all(candidate.get(k) == v for k, v in rule["when"].items()) and all(
                candidate.get(k) == v for k, v in rule["forbid"].items()
            ):
                impossible = True
                break
        if not impossible:
            yield candidate


def blockers(
    candidate: dict[str, str], answers: dict[str, Any], rules: list[dict[str, Any]]
) -> list[str]:
    """Why this candidate cannot run here, or an empty list."""
    reasons: list[str] = []
    for rule in rules:
        if not all(candidate.get(key) == value for key, value in rule["when"].items()):
            continue
        for name in rule.get("requires", []):
            state = (answers.get(name) or {}).get("state")
            if state != "available":
                reasons.append(f"{name} is {state}: {rule['reason']}")
        alternatives = rule.get("requires_any", [])
        if alternatives:
            states = {name: (answers.get(name) or {}).get("state") for name in alternatives}
            if not any(state == "available" for state in states.values()):
                listed = ", ".join(f"{name} is {state}" for name, state in states.items())
                reasons.append(f"none of {listed}: {rule['reason']}")
    return reasons


def build_admission(
    records: list[dict[str, Any]],
    capability_schema: dict[str, Any],
    performance_schema: dict[str, Any],
) -> dict[str, Any]:
    problems = validate(records, capability_schema)
    if problems:
        raise RunRejected(problems)

    kept, notes = usable(records, capability_schema)
    if not kept:
        raise RunRejected(["no record survived the evidence rules"] + notes)

    rules = capability_schema["admission"]["rules"]
    if not rules:
        # An empty rule set admits the whole cross product, which looks like a
        # very successful gate and is the absence of one.
        raise RunRejected(
            ["the contract declares no admission rules, so every candidate would be admitted "
             "without anything having been checked"])

    # One condition per (device, os_build, origin). Two OS builds under one
    # marketing version are two conditions, and a capability admitted from one
    # is a capability admitted for one.
    conditions: dict[tuple, dict[str, Any]] = {}
    for record in kept:
        key = (
            record["hardware_identifier"],
            record["os_version"],
            record["os_build"],
            record["origin"],
        )
        if key in conditions:
            raise RunRejected(
                [f"two records describe {key}; a second answer for one condition is a "
                 "disagreement this tool cannot resolve, not a larger sample"])
        conditions[key] = record["capabilities"]

    admitted: list[dict[str, Any]] = []
    conditional: list[dict[str, Any]] = []
    eliminated: list[dict[str, Any]] = []

    for candidate in candidate_space(performance_schema, capability_schema):
        runs_on: list[str] = []
        blocked_on: list[dict[str, Any]] = []
        for key, answers in sorted(conditions.items()):
            if key[3] != candidate["origin"]:
                continue
            why = blockers(candidate, answers, rules)
            label = f"{key[0]} {key[1]} ({key[2]})"
            if why:
                blocked_on.append({"condition": label, "reasons": why})
            else:
                runs_on.append(label)

        entry = {"candidate": candidate, "runs_on": runs_on, "blocked_on": blocked_on}
        if not runs_on and not blocked_on:
            # No record for this origin at all. Not eliminated -- unmeasured,
            # and calling it eliminated would let a missing record read as a
            # finding.
            entry["state"] = "unmeasured"
            conditional.append(entry)
        elif not blocked_on:
            entry["state"] = "admitted"
            admitted.append(entry)
        elif not runs_on:
            entry["state"] = "eliminated"
            eliminated.append(entry)
        else:
            entry["state"] = "conditional"
            conditional.append(entry)

    measured_minors = sorted({record["os_version"] for record in kept})
    required_minors = capability_schema["probe_rules"]["required_os_minors"]
    coverage_gaps = [
        minor
        for minor in required_minors
        if minor != "current" and not any(v.startswith(minor) for v in measured_minors)
    ]

    verdict = "admitted"
    refusals: list[str] = []
    if coverage_gaps:
        verdict = "provisional"
        refusals.append(
            f"no device answered on iOS {', '.join(coverage_gaps)}; the version-sensitive "
            "assumptions (A15, A21, A23, A24) are settled per OS and not once")
    if not admitted:
        verdict = "rejected"
        refusals.append(
            "no candidate is admissible on every measured condition, so there is nothing a "
            "later gate could measure everywhere")

    return {
        "schema_version": capability_schema["schema_version"],
        "verdict": verdict,
        "records_read": len(records),
        "records_used": len(kept),
        "excluded": notes,
        "conditions": [
            {"hardware": k[0], "os_version": k[1], "os_build": k[2], "origin": k[3]}
            for k in sorted(conditions)
        ],
        "measured_os_versions": measured_minors,
        "rules_applied": len(rules),
        "candidates_considered": len(admitted) + len(conditional) + len(eliminated),
        "admitted": admitted,
        "conditional": conditional,
        "eliminated": eliminated,
        "refusals": refusals,
        "_note": (
            "Admission is not selection. A candidate here has been shown to be runnable and "
            "nothing more; which of them is fastest is decide.py's question."
        ),
    }


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--input", required=True, type=Path)
    parser.add_argument("--output", type=Path)
    parser.add_argument("--capability-schema", type=Path, default=CAPABILITY_SCHEMA)
    parser.add_argument("--performance-schema", type=Path, default=PERFORMANCE_SCHEMA)
    parser.add_argument(
        "--require-admission",
        action="store_true",
        help="exit non-zero when the verdict is not a clean admission")
    args = parser.parse_args(argv)

    capability_schema = load_json(args.capability_schema)
    performance_schema = load_json(args.performance_schema)
    for key in ("probe_rules", "record", "capabilities", "answer", "admission"):
        if key not in capability_schema:
            raise SystemExit(f"{args.capability_schema} has no {key!r}; it is not the schema")

    records = load_records(args.input)
    try:
        result = build_admission(records, capability_schema, performance_schema)
    except RunRejected as rejected:
        result = {
            "schema_version": capability_schema["schema_version"],
            "verdict": "rejected",
            "records_read": len(records),
            "reasons": rejected.reasons,
        }

    text = json.dumps(result, indent=2, sort_keys=True)
    if args.output:
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(text + "\n", encoding="utf-8")
    else:
        print(text)

    if args.require_admission and result["verdict"] != "admitted":
        return 2
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
