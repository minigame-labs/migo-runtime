#!/usr/bin/env python3
"""The decision tool's refusals, which are the part worth testing.

Anyone can write a tool that names a winner. The reason this one exists is that
"no data", "the simulator worked" and "we ran the interesting half of the
matrix" all look like evidence in a report and none of them are, so each of them
gets a case here proving the tool says so.

The fixtures are generated rather than committed. A hand-written matrix of two
hundred JSON records is a fixture nobody reads and everybody trusts; generating
them keeps the *shape* of each case visible in one function, which is the part a
reviewer actually needs to check.

Run:  python3 tools/apple-probe-decision/tests/test_decide.py
Gate: scripts/test-apple-probe-decision.sh
"""

from __future__ import annotations

import json
import random
import subprocess
import sys
import tempfile
from pathlib import Path

HERE = Path(__file__).resolve().parent
TOOL = HERE.parent / "decide.py"
REPO_ROOT = HERE.parents[2]
SCHEMA = REPO_ROOT / "contracts" / "apple" / "performance-probe.schema.json"

failures: list[str] = []
checks = 0


def check(condition: bool, message: str) -> None:
    global checks
    checks += 1
    if not condition:
        failures.append(message)
        print(f"  FAIL  {message}")


def record(**overrides) -> dict:
    """One well-formed measurement. Overrides are how each case says what it is about."""
    base = {
        "schema_version": 1,
        "run_id": "r1",
        "trial_seed": 1,
        "captured_at": "2026-09-04T00:00:00Z",
        "device_class": "device",
        "hardware_identifier": "iPhone14,5",
        "ram_bytes": 4 * 1024**3,
        "os_version": "18.2",
        "os_build": "22C150",
        "webkit_build": "620.1.2",
        "app_build": "1",
        "agent": "dedicated_worker",
        "layout": "attached_visible",
        "origin": "loopback",
        "transport": "loopback_websocket",
        "transport_owner": "same_context",
        "clock": "display_link_relay",
        "payload_class_bytes": 32768,
        "refresh_hz": 60,
        "duration_s": 1800,
        "thermal_state": "nominal",
        "power_state": "plugged",
        "jit_enabled": True,
        "input_to_present_p50_ms": 8.0,
        "input_to_present_p95_ms": 12.0,
        "input_to_present_p99_ms": 16.0,
        "missed_vsync_ratio": 0.001,
        "cpu_seconds": 30.0,
        "wakeups": 1000,
        "app_footprint_bytes": 200 * 1024**2,
        "gpu_bytes": 60 * 1024**2,
        "copies_per_frame": 1,
        "allocations_per_frame": 0,
        "errors": 0,
        "terminations": 0,
        "correctness_hash": "abc",
        "correctness_expected_hash": "abc",
    }
    base.update(overrides)
    return base


def run(records: list[dict], *extra: str) -> dict:
    with tempfile.TemporaryDirectory() as directory:
        raw = Path(directory) / "raw"
        raw.mkdir()
        (raw / "records.json").write_text(json.dumps(records), encoding="utf-8")
        output = Path(directory) / "decision.json"
        result = subprocess.run(
            [sys.executable, str(TOOL), "--input", str(raw), "--output", str(output),
             "--schema", str(SCHEMA), *extra],
            capture_output=True,
            text=True,
        )
        if result.returncode not in (0, 2):
            raise AssertionError(f"the tool crashed: {result.stderr}")
        return json.loads(output.read_text(encoding="utf-8"))


def arm(variable: str, level: str, mean: float, count: int, spread: float, seed: int) -> list[dict]:
    """`count` samples around `mean`, deterministic so a case cannot pass by luck."""
    rng = random.Random(seed)
    return [
        record(**{variable: level, "input_to_present_p95_ms": rng.gauss(mean, spread)})
        for _ in range(count)
    ]


def arm_with(mean: float, count: int, spread: float, seed: int, **overrides) -> list[dict]:
    """Like `arm`, for cases that set more than one field.

    One `random.Random(seed)` for the whole arm, not one per record: a fresh
    generator per sample returns the same number every time, and forty identical
    samples have no interval at all, which separates arms that should overlap.
    """
    rng = random.Random(seed)
    return [
        record(**overrides, input_to_present_p95_ms=rng.gauss(mean, spread))
        for _ in range(count)
    ]


def verdict_for(decision: dict, variable: str) -> dict:
    """The verdict for one variable, or a failure that says why there is none.

    Both ways this can go wrong used to surface as a traceback -- `KeyError:
    'variables'` when the tool refused the whole run, `StopIteration` when the
    schema no longer names the variable. A red that reports neither the run it
    refused nor the variable it does not know teaches its reader to ignore it.
    """
    if "variables" not in decision:
        raise AssertionError(
            "the tool refused the run outright, so there is no per-variable verdict: "
            f"{decision.get('reasons')}"
        )
    for entry in decision["variables"]:
        if entry["variable"] == variable:
            return entry
    known = [entry["variable"] for entry in decision["variables"]]
    raise AssertionError(
        f"the schema does not name {variable!r} as a decidable variable; it decides {known}"
    )


# --- a matrix that does separate the arms ------------------------------------
clean = arm("transport", "loopback_websocket", 12.0, 40, 0.2, 1) + arm(
    "transport", "scheme_request", 9.0, 40, 0.2, 2
)
decision = run(clean)
transport = verdict_for(decision, "transport")
check(transport["decision"] == "selected", f"a separated matrix selects: {transport}")
check(transport.get("winner") == "scheme_request", f"the faster arm wins: {transport.get('winner')}")
check(
    decision["decision"] == "rejected",
    "the run as a whole stays rejected while the other three variables are undecided",
)
check(
    all(
        verdict_for(decision, name)["decision"] == "rejected"
        for name in ("agent", "clock", "layout")
    ),
    "a variable with one level measured is not decided by the transport data",
)

# --- the refusals ------------------------------------------------------------
missing = run([{k: v for k, v in row.items() if k != "thermal_state"} for row in clean])
check(missing["decision"] == "rejected", "a missing field rejects the run")
check(
    any("thermal_state" in reason for reason in missing.get("reasons", [])),
    f"the refusal names the missing field: {missing.get('reasons')}",
)

simulator = run([record(**{**row, "device_class": "simulator"}) for row in clean])
check(simulator["decision"] == "rejected", "simulator-only data is not a selection")
check(
    any("simulator" in reason for reason in simulator.get("reasons", [])),
    f"the refusal says the simulator is why: {simulator.get('reasons')}",
)

thin = arm("transport", "loopback_websocket", 12.0, 5, 0.2, 3) + arm(
    "transport", "scheme_request", 9.0, 5, 0.2, 4
)
thin_decision = verdict_for(run(thin), "transport")
check(thin_decision["decision"] == "rejected", "five samples per arm is below the floor")

one_sided = arm("transport", "loopback_websocket", 12.0, 40, 0.2, 5)
half = verdict_for(run(one_sided), "transport")
check(half["decision"] == "rejected", "one arm is not a comparison")

noisy = arm("transport", "loopback_websocket", 12.0, 40, 4.0, 6) + arm(
    "transport", "scheme_request", 11.6, 40, 4.0, 7
)
noise = verdict_for(run(noisy), "transport")
check(
    noise["decision"] == "rejected",
    f"overlapping intervals do not name a winner: {noise.get('winner')}",
)
check(
    any("overlapping" in reason for reason in noise.get("reasons", [])),
    f"the refusal says the intervals overlap: {noise.get('reasons')}",
)

wrong = [record(**{**row, "correctness_hash": "wrong"}) for row in clean[:40]] + clean[40:]
correctness = run(wrong)
check(
    correctness["records_used"] == 40,
    f"an arm that renders the wrong pixels is excluded, not merely ranked: "
    f"{correctness['records_used']} used",
)

terminated = run([record(**{**row, "terminations": 1}) for row in clean])
check(terminated["decision"] == "rejected", "a run that killed WebContent is not a measurement")

# --- the condition a comparison is made under --------------------------------
# Everything below is one defect: `variables` named the four things G0 chooses
# between, and holding only those fixed left the device, the OS build, the
# payload size, the refresh rate, the power state and whether JIT was on free to
# differ inside a single arm. The schema said all of them were recorded "so that
# one variable at a time can be checked"; nothing checked.

mixed_hardware = arm_with(12.0, 40, 0.2, 11, transport="loopback_websocket",
                          hardware_identifier="iPhone12,1") + arm_with(
    9.0, 40, 0.2, 12, transport="scheme_request", hardware_identifier="iPhone17,1")
mixed = verdict_for(run(mixed_hardware), "transport")
check(
    mixed["decision"] == "rejected",
    f"two arms measured on two different phones are not a comparison: {mixed.get('winner')}",
)

mixed_payload = arm("transport", "loopback_websocket", 12.0, 40, 0.2, 13) + arm_with(
    9.0, 40, 0.2, 14, transport="scheme_request", payload_class_bytes=4 * 1024 * 1024)
payload = verdict_for(run(mixed_payload), "transport")
check(
    payload["decision"] == "rejected",
    f"an arm at 32 KiB against an arm at 4 MiB is not a comparison: {payload.get('winner')}",
)

mixed_jit = arm("transport", "loopback_websocket", 12.0, 40, 0.2, 15) + arm_with(
    9.0, 40, 0.2, 16, transport="scheme_request", jit_enabled=False)
jit = verdict_for(run(mixed_jit), "transport")
check(
    jit["decision"] == "rejected",
    f"a JIT arm against a Lockdown-Mode arm is not a comparison: {jit.get('winner')}",
)

# --- conditions that disagree ------------------------------------------------
# WebSocket wins at 32 KiB, the scheme wins at 4 MiB. Pooling them produced one
# winner from two opposite results.
disagree = (
    arm("transport", "loopback_websocket", 9.0, 40, 0.2, 21)
    + arm("transport", "scheme_request", 12.0, 40, 0.2, 22)
    + arm_with(12.0, 40, 0.2, 23, transport="loopback_websocket",
               payload_class_bytes=4 * 1024 * 1024)
    + arm_with(9.0, 40, 0.2, 24, transport="scheme_request",
               payload_class_bytes=4 * 1024 * 1024)
)
split = verdict_for(run(disagree), "transport")
check(
    split["decision"] == "rejected",
    f"opposite winners under two conditions do not make a choice: {split.get('winner')}",
)
check(
    any("disagree" in reason for reason in split.get("reasons", [])),
    f"the refusal says the conditions disagree: {split.get('reasons')}",
)

# A condition that merely fails to separate is absence of evidence, not contrary
# evidence, so it reports and does not veto.
one_decisive = (
    arm("transport", "loopback_websocket", 12.0, 40, 0.2, 31)
    + arm("transport", "scheme_request", 9.0, 40, 0.2, 32)
    + arm_with(11.0, 40, 4.0, 33, transport="loopback_websocket", refresh_hz=120)
    + arm_with(10.8, 40, 4.0, 34, transport="scheme_request", refresh_hz=120)
)
partial = verdict_for(run(one_decisive), "transport")
check(
    partial["decision"] == "selected" and partial.get("winner") == "scheme_request",
    f"a condition that does not separate does not veto one that does: {partial}",
)
check(
    any(entry["decision"] == "rejected" for entry in partial.get("conditions", [])),
    "the condition that did not separate is still reported",
)

# --- the candidate set the 2026-09-07 review added ---------------------------
# A22: WebSocket is exposed to Workers, so the Window relay is a consequence of
# the blocking primitive, not a requirement. Three topologies record as the same
# agent and transport; only transport_owner tells them apart.
owner = verdict_for(
    run(
        arm("transport_owner", "same_context", 12.0, 40, 0.2, 41)
        + arm("transport_owner", "io_worker", 9.0, 40, 0.2, 42)
    ),
    "transport_owner",
)
check(
    owner["decision"] == "selected" and owner.get("winner") == "io_worker",
    f"the topology dimension is decidable: {owner}",
)

# A21: synchronous XHR is a second public blocking primitive, so it is an arm of
# the transport gate and has to be nameable.
sync_xhr = verdict_for(
    run(
        arm("transport", "loopback_websocket", 12.0, 40, 0.2, 43)
        + arm("transport", "sync_xhr_rpc", 9.0, 40, 0.2, 44)
    ),
    "transport",
)
check(
    sync_xhr["decision"] == "selected" and sync_xhr.get("winner") == "sync_xhr_rpc",
    f"sync_xhr_rpc is a recordable transport: {sync_xhr}",
)

impossible = run(
    [record(agent="window", transport_owner="window_relay") for _ in range(40)]
    + arm("transport", "scheme_request", 9.0, 40, 0.2, 45)
)
check(
    impossible["decision"] == "rejected",
    "a Window agent relaying through a Window is a mislabelled run, not a measurement",
)
check(
    any("window_relay" in reason for reason in impossible.get("reasons", [])),
    f"the refusal names the impossible combination: {impossible.get('reasons')}",
)

# --- reproducibility ---------------------------------------------------------
first = run(clean)
second = run(clean)
check(
    json.dumps(first, sort_keys=True) == json.dumps(second, sort_keys=True),
    "the same raw data produces the same decision twice",
)

# --- the stricter contract ---------------------------------------------------
with tempfile.TemporaryDirectory() as directory:
    raw = Path(directory) / "raw"
    raw.mkdir()
    (raw / "r.json").write_text(json.dumps(clean), encoding="utf-8")
    strict = subprocess.run(
        [sys.executable, str(TOOL), "--input", str(raw), "--schema", str(SCHEMA),
         "--require-decision"],
        capture_output=True,
        text=True,
    )
    check(strict.returncode == 2, "--require-decision turns a refusal into a non-zero exit")

    empty = Path(directory) / "empty"
    empty.mkdir()
    nothing = subprocess.run(
        [sys.executable, str(TOOL), "--input", str(empty), "--schema", str(SCHEMA)],
        capture_output=True,
        text=True,
    )
    check(nothing.returncode != 0, "an empty input directory is an error, not an empty decision")

print(f"{checks - len(failures)}/{checks} checks passed")
if failures:
    print("\nFAIL: the decision tool does not enforce the evidence rules.")
    raise SystemExit(1)
print("PASS: every evidence rule is enforced, including the refusals.")
