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
        # Named the way the tool looks for records, which is the way
        # run-apple-probe.sh and the probe app write them. The fixture used to be
        # `records.json`; that stopped being a record the moment the tool learned
        # to ignore everything that is not one, and a fixture the tool cannot see
        # is a test that passes by testing nothing.
        (raw / "capability-fixture.json").write_text(json.dumps(records), encoding="utf-8")
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

# `not_probed` is nobody asking, and the contract says that must never be read
# as a no. It is also the ordinary case: no_local_network_prompt defaults to it
# whenever the operator did not attest, and the loopback origin requires it -- so
# reading it as a no eliminated every loopback candidate and reported it as
# though loopback had been ruled out.
unattested = run([
    with_answer(record("loopback"), "no_local_network_prompt", "not_probed"),
    record("custom_scheme"),
])
loopback_eliminated = [
    entry for entry in unattested["eliminated"] if entry["candidate"]["origin"] == "loopback"
]
check(
    not loopback_eliminated,
    f"a capability nobody probed does not eliminate a candidate: "
    f"{len(loopback_eliminated)} loopback candidates were eliminated")
loopback_conditional = [
    entry for entry in unattested["conditional"]
    if entry["candidate"]["origin"] == "loopback" and entry["unmeasured_on"]
]
check(
    bool(loopback_conditional),
    "it leaves them unmeasured instead, which is a different thing to fix: one needs "
    "another architecture, the other needs somebody to run the probe")
check(
    any("no_local_network_prompt" in reason
        for entry in loopback_conditional
        for block in entry["unmeasured_on"]
        for reason in block["reasons"]),
    "and it names the question nobody asked")

# The same capability answered `unavailable` -- the operator watched and saw the
# alert -- IS an elimination. The two must not collapse into each other.
attested_bad = run([
    with_answer(record("loopback"), "no_local_network_prompt", "unavailable"),
    record("custom_scheme"),
])
check(
    any(entry["candidate"]["origin"] == "loopback" for entry in attested_bad["eliminated"]),
    "an alert the operator actually saw does eliminate the loopback origin")

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

# --- the evidence directory is not a directory of records --------------------
#
# run-apple-probe.sh writes devicectl's own `devices-*.json`, `install-*.json`,
# `launch-*.json` and `copy-*.json` into the directory it then admits from, and
# writes this tool's `admission.json` there too. Reading every *.json meant the
# FIRST successful device run ended in `rejected`, naming a launch receipt for
# fields it was never going to have: a correct run, a complete set of records,
# and a verdict that reads as bad data. Nothing exercised it -- the runner's
# contract test stops at --dry-run.
# --- Lockdown Mode is a device state, not an admission invariant ---------------
#
# The contract says that in as many words, and until 2026-09-10 the tool did not
# implement it. Two runs of one phone -- Lockdown off and Lockdown on -- collided
# as "a disagreement this tool cannot resolve", because the condition key had no
# room for the state the record carries `lockdown_mode` to describe. Given room,
# they then turned every candidate from admitted into conditional and the run into
# `rejected`, which reads as "nothing works on this phone" when what was measured
# is "nothing works while its owner has Lockdown Mode on".

both_states = [
    record("loopback"),
    with_answer(record("loopback", lockdown_mode="on", run_id="g0-2"), "jit_enabled",
                "unavailable"),
]
two_states = run(both_states)
check(
    two_states["verdict"] != "rejected",
    "two device states of one phone are two conditions, not a disagreement: the run "
    f"came back {two_states['verdict']}",
)
check(
    len(two_states["admitted"]) > 0,
    "a candidate that runs with Lockdown off is admitted even though Lockdown on "
    "blocks it; the fallback there is the WebKit lane, not a verdict on the "
    "architecture",
)
check(
    any(entry.get("blocked_by_device_state") for entry in two_states["admitted"]),
    "the Lockdown answer is RECORDED on the candidates it blocks; admitting without "
    "saying so would lose the answer A24 asked for",
)
check(
    all(
        "lockdown=on" in blocked["condition"]
        for entry in two_states["admitted"]
        for blocked in entry.get("blocked_by_device_state", [])
    ),
    "the recorded device-state blocker names the state",
)

# The exemption is for the state and not for the phone: a blocker with Lockdown
# OFF still eliminates. Without this, "device state" would become a way to admit
# anything.
lockdown_off_blocked = run([with_answer(record("loopback"), "jit_enabled", "unavailable")])
check(
    len(lockdown_off_blocked["admitted"]) == 0,
    "a capability missing with Lockdown OFF still blocks; the exemption is for the "
    "state, not for the device",
)

# Two records for one state are still a disagreement. The key grew a field; it did
# not stop being a key.
same_state_twice = run([record("loopback"), record("loopback", run_id="g0-3")])
check(
    same_state_twice["verdict"] == "rejected",
    "two records for one device state remain a disagreement the tool refuses",
)


def run_beside_operational_artifacts(records: list[dict]) -> dict:
    with tempfile.TemporaryDirectory() as directory:
        raw = Path(directory) / "raw"
        raw.mkdir()
        (raw / "capability-fixture.json").write_text(json.dumps(records), encoding="utf-8")
        # Shaped like what devicectl --json-output actually writes.
        (raw / "launch-probe-1.json").write_text(
            json.dumps({"info": {"outcome": "success"}, "result": {"process": {"processIdentifier": 1}}}),
            encoding="utf-8")
        (raw / "devices-probe-1.json").write_text(
            json.dumps({"info": {"outcome": "success"}, "result": {"devices": []}}), encoding="utf-8")
        # And this tool's own output from the previous run of the same directory.
        (raw / "admission.json").write_text(json.dumps({"verdict": "provisional"}), encoding="utf-8")
        output = Path(directory) / "out.json"
        subprocess.run(
            [sys.executable, str(TOOL), "--input", str(raw), "--output", str(output)],
            capture_output=True, text=True)
        return json.loads(output.read_text(encoding="utf-8"))


beside = run_beside_operational_artifacts(both)
check(
    beside["verdict"] == run(both)["verdict"],
    f"a run's own devicectl output and a previous admission.json must not change the "
    f"verdict: {beside['verdict']} beside them, {run(both)['verdict']} without")
check(
    len(beside["admitted"]) == len(run(both)["admitted"]),
    "the same records admit the same candidates whether or not the run's logs sit beside them")

# --- reproducibility ---------------------------------------------------------
check(
    json.dumps(run(both), sort_keys=True) == json.dumps(run(both), sort_keys=True),
    "the same records produce the same admission twice")

print(f"{checks - len(failures)}/{checks} checks passed")
if failures:
    print("\nFAIL: the admission tool does not enforce the evidence rules.")
    raise SystemExit(1)
print("PASS: gate 1's answers cut the candidate set, and refuse to when they cannot.")
