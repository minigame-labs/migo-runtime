#!/usr/bin/env bash
# The G0 decision tool must refuse the data that cannot decide anything.
#
# Worker versus Window, the transport, the frame clock and the WebView host
# shape are unresolved by design, and the design says so in prose. Prose does
# not fail a build. This gate runs the tool that turns probe measurements into
# a decision, against generated matrices that each break one evidence rule, and
# checks that the tool says no.
#
# THE DRIFT THIS EXISTS TO CATCH is the one that has already happened to this
# project once, in another form: a rule that lives only in a document is a rule
# that gets satisfied by whoever is writing the report. "The simulator worked",
# "we ran the interesting half of the matrix" and "the means differed" all look
# like evidence in a summary. Each of them is a case in the suite below.
#
# It also runs on Linux, months before the first Mac. That is deliberate: the
# tool's rules are checkable without a device even though its input is not, and
# the alternative is discovering on lab day that the decision procedure has a
# bug in it.
#
# Host-only: python3, no device, no Apple toolchain.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

TOOL="tools/apple-probe-decision/decide.py"
TESTS="tools/apple-probe-decision/tests/test_decide.py"
SCHEMA="contracts/apple/performance-probe.schema.json"
ADMIT="tools/apple-probe-decision/admit.py"
ADMIT_TESTS="tools/apple-probe-decision/tests/test_admit.py"
CAPABILITY_SCHEMA="contracts/apple/capability-probe.schema.json"

for required in "$TOOL" "$TESTS" "$SCHEMA" "$ADMIT" "$ADMIT_TESTS" "$CAPABILITY_SCHEMA"; do
    if [[ ! -f "$required" ]]; then
        echo "FAIL: $required is missing; the G0 decision procedure cannot be checked." >&2
        exit 1
    fi
done

if ! command -v python3 >/dev/null 2>&1; then
    echo "FAIL: python3 is not available, so the decision procedure is unverified." >&2
    exit 1
fi

# The schema is the single source of the rules the tool enforces, so a malformed
# one would make every check below vacuous rather than red.
python3 - "$SCHEMA" <<'PY'
import json, sys
schema = json.load(open(sys.argv[1], encoding="utf-8"))
rules = schema["decision_rules"]
missing = [
    key
    for key in (
        "min_samples_per_arm",
        "min_arms_per_variable",
        "simulator_counts_as_evidence",
        "correctness_mismatch_disqualifies_arm",
        "confidence",
        "bootstrap_seed",
    )
    if key not in rules
]
if missing:
    raise SystemExit(f"FAIL: {sys.argv[1]} declares no {', '.join(missing)}")
if rules["simulator_counts_as_evidence"]:
    raise SystemExit(
        "FAIL: the schema admits simulator measurements as evidence. A simulator runs "
        "the host's engine on the host's CPU with the host's memory; it can answer an "
        "ABI question and none of the questions this decision asks."
    )
if rules["min_samples_per_arm"] < 20:
    raise SystemExit(
        f"FAIL: min_samples_per_arm is {rules['min_samples_per_arm']}. Below twenty, the "
        "interval this tool reports is wider than the effect it is looking for."
    )

# `held_fixed` is what makes "one variable at a time" a check rather than a
# sentence. Emptying it restores the state this tool shipped in, where the four
# entries of `variables` were held fixed and the device, the OS build, the
# payload size, the refresh rate and whether JIT was on were not -- so twenty
# samples from a slow phone and twenty from a fast one were the two arms of a
# transport comparison and a winner came out. The unit suite catches that; this
# check is here so the schema cannot be emptied and read as merely "configured".
held = schema["record"].get("held_fixed")
if not held:
    raise SystemExit(
        "FAIL: the schema lists nothing as held fixed, so a comparison may mix devices, "
        "OS builds, payload sizes and power states inside one arm."
    )
for field in ("hardware_identifier", "payload_class_bytes", "jit_enabled"):
    if field not in held:
        raise SystemExit(
            f"FAIL: {field} is not held fixed, so two records that differ in it are treated "
            "as two samples of the same measurement."
        )
if "thermal_state" in held:
    raise SystemExit(
        "FAIL: thermal_state is held fixed. It varies across the samples of a single arm by "
        "nature, so holding it fixed splits every arm below the sample floor and rejects "
        "every run -- a gate that refuses everything is not stricter, it is off."
    )
PY

# The admission side has the same rules and a different failure mode. decide.py
# picks a winner from arms that ran; admit.py decides which arms may run at all,
# and a permissive bug there does not look like a failure -- it looks like a
# matrix with more coverage.
python3 - "$CAPABILITY_SCHEMA" <<'PYCAP'
import json, sys
schema = json.load(open(sys.argv[1], encoding="utf-8"))
for section in ("probe_rules", "record", "capabilities", "answer", "admission"):
    if section not in schema:
        raise SystemExit(f"FAIL: {sys.argv[1]} has no {section!r} section")
if schema["probe_rules"]["simulator_counts_as_evidence"]:
    raise SystemExit(
        "FAIL: the capability schema admits simulator answers as evidence. The simulator runs "
        "the host's JavaScriptCore on the host's CPU, so it answers 'is JIT on' with the host's "
        "answer -- yes on every Mac, and nothing about a phone in Lockdown Mode."
    )
if not schema["admission"]["rules"]:
    raise SystemExit(
        "FAIL: the contract declares no admission rules, so every candidate would be admitted "
        "without anything having been checked. An empty rule set looks like a gate that passed."
    )
if "evidence" not in schema["answer"]["required"]:
    raise SystemExit(
        "FAIL: a capability answer may be written without evidence. 'available' with nothing "
        "behind it is indistinguishable from a probe that returned a default."
    )
PYCAP

python3 "$TESTS"
python3 "$ADMIT_TESTS"
