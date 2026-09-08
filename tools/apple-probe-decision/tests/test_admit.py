#!/usr/bin/env python3
"""The admission tool's refusals and its cuts.

Two things have to be true and neither is obvious from reading the tool. It has
to REFUSE the ways a candidate set gets waved through -- an absent answer, an
answer with no evidence, simulator-only data, an empty rule set -- and it has to
actually CUT, which is the half a permissive bug leaves looking like success.

The fixtures are generated. A hand-written pair of capability records is a
fixture nobody reads and everybody trusts; generating them keeps the shape of
each case visible in the line that makes it.

Run:  python3 tools/apple-probe-decision/tests/test_admit.py
Gate: scripts/test-apple-probe-decision.sh
"""

from __future__ import annotations

import json
import subprocess
import sys
import tempfile
from pathlib import Path

HERE = Path(__file__).resolve().parent
TOOL = HERE.parent / "admit.py"
REPO_ROOT = HERE.parents[2]
CAPABILITY_SCHEMA = REPO_ROOT / "contracts" / "apple" / "capability-probe.schema.json"

failures: list[str] = []
checks = 0

SCHEMA = json.loads(CAPABILITY_SCHEMA.read_text(encoding="utf-8"))
CAPABILITIES = sorted(name for name in SCHEMA["capabilities"] if not name.startswith("_"))


def check(condition: bool, message: str) -> None:
    global checks
    checks += 1
    if not condition:
        failures.append(message)
        print(f"  FAIL  {message}")


def record(origin: str, **overrides) -> dict:
    """One well-formed capability record with everything available."""
    answers = {
        name: {"state": "available", "evidence": f"{name} was probed and answered yes"}
        for name in CAPABILITIES
    }
    if origin == "custom_scheme":
        answers["no_local_network_prompt"] = {
            "state": "unsupported",
            "evidence": "this origin opens no socket",
        }
    base = {
        "schema_version": 1,
        "run_id": "g0-1",
        "captured_at": "2026-09-09T00:00:00Z",
        "device_class": "device",
        "hardware_identifier": "iPhone14,5",
        "ram_bytes": 4 * 1024**3,
        "os_version": "15.2.1",
        "os_build": "19C63",
        "webkit_build": "612.4.9",
        "app_build": "0.1 (1)",
        "lockdown_mode": "off",
        "origin": origin,
        "capabilities": answers,
    }
    base.update(overrides)
    return base


def run(records: list[dict], *extra: str) -> dict:
    with tempfile.TemporaryDirectory() as directory:
        raw = Path(directory) / "raw"
        raw.mkdir()
        (raw / "records.json").write_text(json.dumps(records), encoding="utf-8")
        output = Path(directory) / "admission.json"
        result = subprocess.run(
            [sys.executable, str(TOOL), "--input", str(raw), "--output", str(output), *extra],
            capture_output=True,
            text=True,
        )
        if result.returncode not in (0, 2):
            raise AssertionError(f"the tool crashed: {result.stderr}")
        return json.loads(output.read_text(encoding="utf-8"))


def with_answer(base: dict, name: str, state: str) -> dict:
    clone = json.loads(json.dumps(base))
    clone["capabilities"][name] = {"state": state, "evidence": f"{name} probed as {state}"}
    return clone


both = [record("loopback"), record("custom_scheme")]

# --- it admits, and it says what it admitted on ------------------------------
clean = run(both)
check(
    clean["verdict"] in ("admitted", "provisional"),
    f"a complete pair of records admits something: {clean['verdict']}")
check(len(clean["admitted"]) > 0, "a device where everything works admits at least one candidate")
check(
    clean["rules_applied"] == len(SCHEMA["admission"]["rules"]),
    "the tool applies every rule the contract declares")
check(
    all(entry["runs_on"] for entry in clean["admitted"]),
    "an admitted candidate names the condition it runs on")

# --- it cuts -----------------------------------------------------------------
# The case the simulator run actually produced: the custom-scheme origin is a
# secure context and is NOT cross-origin isolated, so SharedArrayBuffer does not
# construct there. Every candidate whose topology needs it has to go, at that
# origin and only at that origin.
no_sab = [
    record("loopback"),
    with_answer(with_answer(record("custom_scheme"), "shared_array_buffer", "unsupported"),
                "atomics_wait", "unsupported"),
]
cut = run(no_sab)
eliminated = [entry["candidate"] for entry in cut["eliminated"]]
check(
    any(c["transport_owner"] == "io_worker" and c["origin"] == "custom_scheme"
        for c in eliminated),
    "the I/O-Worker topology is eliminated where SharedArrayBuffer does not construct")
check(
    not any(c["transport_owner"] == "io_worker" and c["origin"] == "loopback"
            for c in eliminated),
    "it is NOT eliminated at the origin where it does construct; a cut that took both "
    "origins would be reporting one origin's answer for the other")
check(
    any("shared_array_buffer" in reason
        for entry in cut["eliminated"] for blocked in entry["blocked_on"]
        for reason in blocked["reasons"]),
    "the elimination names the capability that caused it")

# A device with no JIT admits nothing: the whole lane exists because WebContent
# has it. A24 is why this is a device state and not an invariant.
lockdown = run([
    with_answer(record("loopback"), "jit_enabled", "unavailable"),
    with_answer(record("custom_scheme"), "jit_enabled", "unavailable"),
])
check(
    lockdown["verdict"] == "rejected" and not lockdown["admitted"],
    f"a device without JIT admits no Performance+ candidate: {lockdown['verdict']}")

# --- the refusals ------------------------------------------------------------
simulator = run([record("loopback", device_class="simulator"),
                 record("custom_scheme", device_class="simulator")])
check(simulator["verdict"] == "rejected", "simulator-only data admits nothing")
check(
    any("simulator" in reason for reason in simulator.get("reasons", [])),
    f"the refusal says the simulator is why: {simulator.get('reasons')}")

missing = json.loads(json.dumps(both))
del missing[0]["capabilities"]["atomics_wait"]
absent = run(missing)
check(absent["verdict"] == "rejected", "an absent capability rejects the run")
check(
    any("atomics_wait" in reason for reason in absent.get("reasons", [])),
    f"the refusal names the capability nobody answered: {absent.get('reasons')}")

evidence_free = json.loads(json.dumps(both))
evidence_free[0]["capabilities"]["jit_enabled"] = {"state": "available"}
no_evidence = run(evidence_free)
check(no_evidence["verdict"] == "rejected", "an answer with no evidence rejects the run")
check(
    any("evidence" in reason for reason in no_evidence.get("reasons", [])),
    f"the refusal says evidence is why: {no_evidence.get('reasons')}")

invented = json.loads(json.dumps(both))
invented[0]["capabilities"]["quantum_tunnelling"] = {"state": "available", "evidence": "no"}
unknown = run(invented)
check(unknown["verdict"] == "rejected", "a capability no contract declares rejects the run")

duplicate = run([record("loopback"), record("loopback"), record("custom_scheme")])
check(
    duplicate["verdict"] == "rejected",
    "two records for one condition are a disagreement, not a larger sample")

# The measurement order settles the version-sensitive assumptions per OS. One
# OS is one OS, and the tool says so rather than admitting for all of them.
one_os = run(both)
check(
    one_os["verdict"] == "provisional",
    f"one OS minor is not the required set: {one_os['verdict']}")
check(
    any("15.0" in reason for reason in one_os.get("refusals", [])),
    f"the caveat names the OS versions nobody measured: {one_os.get('refusals')}")

check(
    run(both, "--require-admission") is not None,
    "the stricter flag does not crash the tool")

# --- reproducibility ---------------------------------------------------------
check(
    json.dumps(run(both), sort_keys=True) == json.dumps(run(both), sort_keys=True),
    "the same records produce the same admission twice")

print(f"{checks - len(failures)}/{checks} checks passed")
if failures:
    print("\nFAIL: the admission tool does not enforce the evidence rules.")
    raise SystemExit(1)
print("PASS: gate 1's answers cut the candidate set, and refuse to when they cannot.")
