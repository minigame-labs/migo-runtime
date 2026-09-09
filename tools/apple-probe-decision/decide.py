#!/usr/bin/env python3
"""Turn raw Performance+ probe measurements into a decision, or into a refusal.

The four G0 choices -- JavaScript agent, transport, frame clock, WebView host
shape -- are deliberately unresolved in the design. This tool is what resolves
them, and more importantly it is what refuses to resolve them from data that
cannot carry the weight.

The refusals are the point. "No data", "the simulator worked", and "we ran the
interesting half of the matrix" are the three ways a preference gets recorded as
a measurement, and each of them produces a `rejected` verdict here with the
reason attached. A tool that always names a winner is a tool that has replaced
the evidence rule rather than implemented it.

Rules come from contracts/apple/performance-probe.schema.json, so they are
changed in one place and the change is visible in a diff.

Usage:
    python3 tools/apple-probe-decision/decide.py \\
        --input  docs/performance/apple/p00/raw \\
        --output docs/performance/apple/p00/decision.json

Exit status is 0 when the tool ran, whatever it decided: a refusal is a result,
not a crash. It is non-zero only when it could not run at all -- unreadable
input, a malformed schema, no records. `--require-decision` turns a refusal into
a non-zero exit for callers that want the stricter contract.
"""

from __future__ import annotations

import argparse
import json
import math
import random
import sys
from pathlib import Path
from typing import Any

REPO_ROOT = Path(__file__).resolve().parents[2]
SCHEMA_PATH = REPO_ROOT / "contracts" / "apple" / "performance-probe.schema.json"


class RunRejected(Exception):
    """The raw data cannot support a decision. Carries every reason, not the first."""

    def __init__(self, reasons: list[str]) -> None:
        super().__init__("; ".join(reasons))
        self.reasons = reasons


def load_schema(path: Path) -> dict[str, Any]:
    with path.open(encoding="utf-8") as handle:
        schema = json.load(handle)
    for key in ("decision_rules", "primary_metric", "record", "variables"):
        if key not in schema:
            raise SystemExit(f"{path} has no {key!r} section; it is not the probe schema")
    return schema


def load_records(directory: Path) -> list[dict[str, Any]]:
    """Every *.json under `directory`, each holding one record or a list of them."""
    if not directory.is_dir():
        raise SystemExit(f"--input {directory} is not a directory")
    records: list[dict[str, Any]] = []
    for path in sorted(directory.rglob("*.json")):
        with path.open(encoding="utf-8") as handle:
            try:
                loaded = json.load(handle)
            except json.JSONDecodeError as error:
                raise SystemExit(f"{path}: {error}") from error
        items = loaded if isinstance(loaded, list) else [loaded]
        for item in items:
            if not isinstance(item, dict):
                raise SystemExit(f"{path}: a record must be an object, found {type(item).__name__}")
            item["_source"] = str(path.relative_to(directory))
            records.append(item)
    if not records:
        raise SystemExit(f"--input {directory} contains no *.json records")
    return records


def validate(records: list[dict[str, Any]], schema: dict[str, Any]) -> list[str]:
    """Field and enum problems, as a list. A single one rejects the whole run."""
    required = schema["record"]["required"]
    enums = schema["record"]["enums"]
    problems: list[str] = []

    for record in records:
        source = record.get("_source", "?")
        missing = [field for field in required if field not in record]
        if missing:
            # Named individually: "some fields are missing" sends whoever reads
            # this back to the schema to work out which.
            problems.append(f"{source}: missing {', '.join(missing)}")
        for field, allowed in enums.items():
            value = record.get(field)
            if value is not None and value not in allowed:
                problems.append(
                    f"{source}: {field}={value!r} is not one of {', '.join(allowed)}"
                )
        for field in (
            "input_to_present_p50_ms",
            "input_to_present_p95_ms",
            "input_to_present_p99_ms",
            "missed_vsync_ratio",
            "cpu_seconds",
        ):
            value = record.get(field)
            if value is not None and not isinstance(value, (int, float)):
                problems.append(f"{source}: {field} must be a number, found {type(value).__name__}")

        # A combination the product cannot have is a harness that mislabelled a
        # run, and a mislabelled run is worse than a missing one: it is counted.
        for rule in schema["record"].get("combination_rules", []):
            if all(record.get(k) == v for k, v in rule["when"].items()) and all(
                record.get(k) == v for k, v in rule["forbid"].items()
            ):
                condition = ", ".join(f"{k}={v!r}" for k, v in rule["when"].items())
                forbidden = ", ".join(f"{k}={v!r}" for k, v in rule["forbid"].items())
                problems.append(
                    f"{source}: {condition} cannot occur with {forbidden}: {rule['reason']}"
                )
    return problems


def usable(records: list[dict[str, Any]], schema: dict[str, Any]) -> tuple[list[dict], list[str]]:
    """Drop what cannot be evidence, and say why each one went."""
    rules = schema["decision_rules"]
    notes: list[str] = []
    kept: list[dict[str, Any]] = []

    for record in records:
        source = record.get("_source", "?")
        if record.get("device_class") == "simulator" and not rules["simulator_counts_as_evidence"]:
            # The simulator runs the host's JavaScriptCore on the host's CPU
            # with the host's memory. It can answer an ABI or lifecycle
            # question and it cannot answer any question this decision asks.
            notes.append(f"{source}: excluded, simulator measurements are not device evidence")
            continue
        if rules["correctness_mismatch_disqualifies_arm"]:
            if record.get("correctness_hash") != record.get("correctness_expected_hash"):
                notes.append(
                    f"{source}: excluded, correctness hash {record.get('correctness_hash')!r} "
                    f"does not match {record.get('correctness_expected_hash')!r}"
                )
                continue
        if record.get("terminations", 0):
            notes.append(f"{source}: excluded, the run recorded {record['terminations']} termination(s)")
            continue
        kept.append(record)
    return kept, notes


def condition_fields(schema: dict[str, Any], variable: str) -> list[str]:
    """Everything that must be identical for two records to be one comparison.

    The design dimensions except the one under test, plus the recorded
    conditions the schema lists as held fixed. The second half is the part that
    was missing: `variables` names the four things G0 chooses between, and
    holding only those fixed leaves the device, the OS build, the payload size,
    the refresh rate, the power state and whether JIT was even on free to differ
    inside a single arm.
    """
    dimensions = [name for name in schema["variables"] if name != "_comment"]
    held = schema["record"].get("held_fixed", [])
    return [name for name in dimensions if name != variable] + list(held)


def group_key(record: dict[str, Any], fields: list[str]) -> tuple:
    """The condition a record was measured under, as a hashable key."""
    return tuple(record.get(field) for field in fields)


def bootstrap_interval(values: list[float], rules: dict[str, Any]) -> tuple[float, float]:
    """A percentile bootstrap interval, seeded so a decision is reproducible.

    Nonparametric on purpose: frame-time samples are skewed and bounded below,
    and a normal interval on twenty of them reports a precision that is not
    there. No dependency either -- this tool runs on a lab Mac that should not
    need a package index to answer a question about a build.
    """
    rng = random.Random(rules["bootstrap_seed"])
    size = len(values)
    means = []
    for _ in range(rules["bootstrap_resamples"]):
        means.append(sum(rng.choice(values) for _ in range(size)) / size)
    means.sort()
    tail = (1.0 - rules["confidence"]) / 2.0
    low = means[max(0, int(math.floor(tail * len(means))))]
    high = means[min(len(means) - 1, int(math.ceil((1.0 - tail) * len(means))) - 1)]
    return low, high


def decide_variable(
    records: list[dict[str, Any]], variable: str, schema: dict[str, Any]
) -> dict[str, Any]:
    """Compare the levels of one variable, holding every recorded condition fixed.

    Each condition decides on its own and the conditions must then agree. The
    earlier version pooled every condition into one set of arms with a running
    mean and a min/max interval, which had two consequences worth naming
    because neither announced itself:

      * a transport measured on a fast phone against a rival measured on a slow
        one produced a winner, and

      * a transport that won at 4 KiB and lost at 4 MiB produced a winner too,
        because the two results were averaged before anything compared them.

    A condition that does not separate its arms is reported and does not veto:
    absence of evidence is not contrary evidence. A condition that selects a
    *different* winner is contrary evidence, and rejects.
    """
    rules = schema["decision_rules"]
    metric = schema["primary_metric"]["name"]
    fields = condition_fields(schema, variable)

    groups: dict[tuple, dict[str, list[float]]] = {}
    for record in records:
        key = group_key(record, fields)
        groups.setdefault(key, {}).setdefault(record[variable], []).append(float(record[metric]))

    # Only a group that actually holds the conditions fixed AND has enough
    # levels can decide anything. A group with one level is not a comparison,
    # however many samples it has.
    comparable = {
        key: levels
        for key, levels in groups.items()
        if len(levels) >= rules["min_arms_per_variable"]
        and all(len(samples) >= rules["min_samples_per_arm"] for samples in levels.values())
    }

    if not comparable:
        shortfalls = []
        for key, levels in sorted(groups.items(), key=lambda item: str(item[0])):
            held = dict(zip(fields, key))
            if len(levels) < rules["min_arms_per_variable"]:
                shortfalls.append(
                    f"holding {held}: only {len(levels)} level(s) of {variable} were measured"
                )
            else:
                thin = {
                    level: len(samples)
                    for level, samples in levels.items()
                    if len(samples) < rules["min_samples_per_arm"]
                }
                shortfalls.append(
                    f"holding {held}: {thin} sample(s), below the floor of "
                    f"{rules['min_samples_per_arm']}"
                )
        return {
            "variable": variable,
            "decision": "rejected",
            "reasons": shortfalls or [f"no records carry {variable}"],
        }

    conditions: list[dict[str, Any]] = []
    for key, levels in sorted(comparable.items(), key=lambda item: str(item[0])):
        arms: dict[str, dict[str, Any]] = {}
        for level, samples in sorted(levels.items()):
            low, high = bootstrap_interval(samples, rules)
            arms[level] = {
                "samples": len(samples),
                "mean_ms": sum(samples) / len(samples),
                "ci_low_ms": low,
                "ci_high_ms": high,
            }
        ordered = sorted(arms.items(), key=lambda item: item[1]["mean_ms"])
        best_name, best = ordered[0]
        runner_name, runner = ordered[1]
        entry: dict[str, Any] = {"held": dict(zip(fields, key)), "arms": arms}

        # Non-overlapping intervals, or no winner. Two arms whose intervals
        # overlap have not been told apart by this data, and naming the lower
        # mean anyway is the whole failure mode this tool exists to prevent.
        if best["ci_high_ms"] >= runner["ci_low_ms"]:
            entry["decision"] = "rejected"
            entry["reason"] = (
                f"{best_name} and {runner_name} have overlapping "
                f"{int(rules['confidence'] * 100)}% intervals "
                f"({best['ci_low_ms']:.3f}-{best['ci_high_ms']:.3f} vs "
                f"{runner['ci_low_ms']:.3f}-{runner['ci_high_ms']:.3f} ms): this data does "
                f"not separate them"
            )
        else:
            entry["decision"] = "selected"
            entry["winner"] = best_name
            entry["margin_ms"] = runner["mean_ms"] - best["mean_ms"]
        conditions.append(entry)

    decisive = [entry for entry in conditions if entry["decision"] == "selected"]
    winners = sorted({entry["winner"] for entry in decisive})

    if len(winners) > 1:
        return {
            "variable": variable,
            "decision": "rejected",
            "conditions": conditions,
            "reasons": [
                f"the conditions disagree: "
                + "; ".join(
                    f"{entry['winner']} wins holding {entry['held']}" for entry in decisive
                )
                + ". A choice that depends on the condition is not a choice this matrix made"
            ],
        }

    if not winners:
        return {
            "variable": variable,
            "decision": "rejected",
            "conditions": conditions,
            "reasons": [entry["reason"] for entry in conditions],
        }

    return {
        "variable": variable,
        "decision": "selected",
        "winner": winners[0],
        "conditions": conditions,
        "metric": metric,
        "margin_ms": min(entry["margin_ms"] for entry in decisive),
        "_margin_comment": (
            "The smallest margin across the conditions that separated, not the "
            "average: a claim is only as strong as the condition it holds least in."
        ),
    }


def collapse(reasons: list[str]) -> list[str]:
    """Identical reasons, once each, with how many records said it.

    A matrix is forty records per arm, so one bad enum value produced forty
    copies of one sentence. Nobody reads to the bottom of that, which is where
    a second, different problem would have been.
    """
    counts: dict[str, int] = {}
    for reason in reasons:
        counts[reason] = counts.get(reason, 0) + 1
    return [
        reason if count == 1 else f"{reason} (x{count})" for reason, count in counts.items()
    ]


def build_decision(records: list[dict[str, Any]], schema: dict[str, Any]) -> dict[str, Any]:
    problems = validate(records, schema)
    if problems:
        raise RunRejected(collapse(problems))

    kept, notes = usable(records, schema)
    if not kept:
        raise RunRejected(["no record survived the evidence rules"] + notes)

    variables = [name for name in schema["variables"] if name != "_comment"]
    per_variable = [decide_variable(kept, variable, schema) for variable in variables]
    selected = [entry for entry in per_variable if entry["decision"] == "selected"]

    return {
        "schema_version": schema["schema_version"],
        "decision": "selected" if len(selected) == len(per_variable) else "rejected",
        "records_read": len(records),
        "records_used": len(kept),
        "excluded": notes,
        "variables": per_variable,
        "_note": (
            "A per-variable rejection is a result. It means this matrix did not "
            "separate that choice, and the choice stays open until one does."
        ),
    }


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--input", required=True, type=Path, help="directory of raw *.json records")
    parser.add_argument("--output", type=Path, help="where to write decision.json")
    parser.add_argument("--schema", type=Path, default=SCHEMA_PATH)
    parser.add_argument(
        "--require-decision",
        action="store_true",
        help="exit non-zero when the verdict is a refusal",
    )
    args = parser.parse_args(argv)

    schema = load_schema(args.schema)
    records = load_records(args.input)

    try:
        decision = build_decision(records, schema)
    except RunRejected as rejected:
        decision = {
            "schema_version": schema["schema_version"],
            "decision": "rejected",
            "records_read": len(records),
            "records_used": 0,
            "reasons": rejected.reasons,
        }

    rendered = json.dumps(decision, indent=2, sort_keys=False) + "\n"
    if args.output:
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(rendered, encoding="utf-8")
    else:
        sys.stdout.write(rendered)

    print(
        f"decision: {decision['decision']} "
        f"({decision.get('records_used', 0)}/{decision['records_read']} records used)",
        file=sys.stderr,
    )
    if args.require_decision and decision["decision"] != "selected":
        return 2
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
