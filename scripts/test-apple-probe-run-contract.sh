#!/usr/bin/env bash
# The runner's refusals, executed rather than read.
#
# scripts/run-apple-probe.sh drives measurement gate 1 end to end, and two of its
# rules are the reason it exists rather than conveniences on top of it:
#
#   * A device run without both attestations records `not_probed` for the two
#     answers no API reports. The loopback origin requires the local-network
#     answer, so admit.py reports every loopback candidate as unmeasured -- and an
#     unmeasured arm on the page reads as an arm the device ruled out. That is not
#     hypothetical: it is what the first unattended run of this gate produced.
#
#   * Simulator records are not evidence (contracts/apple/capability-probe.
#     schema.json). The way that rule fails is never an argument about it; it is a
#     file that ended up in the directory the evidence is read from.
#
# Both are checked by RUNNING the script, because a gate that greps for the string
# "fail" in a script has checked that somebody wrote the word. The runner resolves
# and validates its whole plan before it looks for an Apple tool, which is what
# makes that possible on a Linux CI runner with no Xcode.
#
# The third check is the one that goes stale silently: the app's flag names and the
# names the runner passes are two files, and a runner passing `--migo-lockdown-mode`
# to an app that reads `--migo-lockdown` is a run that attests nothing, refuses
# nothing, and writes a record saying `unknown`. So the flag set is derived from
# both sides and required to agree, which also requires each name to appear exactly
# once in the parser -- a second copy is the same drift wearing a different shape.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
RUNNER="$ROOT/scripts/run-apple-probe.sh"
PARSER="$ROOT/platforms/apple/core/Sources/MigoProbeHarness/MigoProbeLaunchOptions.swift"
VIEW="$ROOT/platforms/apple/ProbeApp/Sources/MigoProbeViewController.swift"

fail() {
  echo "FAIL: $*" >&2
  exit 1
}

for path in "$RUNNER" "$PARSER" "$VIEW"; do
  [[ -f "$path" ]] || fail "$path is missing"
done

# Runs the runner and returns its exit status, with output captured.
LAST_OUTPUT=""
run_runner() {
  set +e
  LAST_OUTPUT="$(bash "$RUNNER" "$@" 2>&1)"
  local status=$?
  set -e
  return $status
}

expect_refusal() {
  local what="$1"
  shift
  if run_runner "$@"; then
    fail "$what was accepted: the runner exited 0 for '$*' and printed:
$LAST_OUTPUT"
  fi
  case "$LAST_OUTPUT" in
    FAIL:*) ;;
    *) fail "$what was refused without saying why: '$*' produced:
$LAST_OUTPUT" ;;
  esac
}

plan_value() {
  local key="$1"
  printf '%s\n' "$LAST_OUTPUT" | sed -n "s/^$key=//p"
}

echo "[1/7] a device run refuses to proceed unattested"
expect_refusal "a device run with no attestation" --device dummy-udid --dry-run
expect_refusal "a device run attesting only Lockdown Mode" \
  --device dummy-udid --lockdown off --dry-run
expect_refusal "a device run attesting only the alert" \
  --device dummy-udid --local-network-prompt none --dry-run
# The refusal must name both flags when both are absent, because an operator who
# adds only the one the message mentioned runs into the same wall twice.
run_runner --device dummy-udid --dry-run || true
case "$LAST_OUTPUT" in
  *--lockdown*--local-network-prompt*) ;;
  *) fail "the refusal for a wholly unattested run names only one of the two flags:
$LAST_OUTPUT" ;;
esac

echo "[2/7] an attestation this script does not understand is refused, not defaulted"
expect_refusal "--lockdown=maybe" \
  --device dummy-udid --lockdown maybe --local-network-prompt none --dry-run
expect_refusal "--local-network-prompt=yes" \
  --device dummy-udid --lockdown off --local-network-prompt yes --dry-run
expect_refusal "a run id that could name another file" \
  --simulator --run-id "../evil" --dry-run
expect_refusal "a run id with a separator" --simulator --run-id "a/b" --dry-run

echo "[3/7] one run is one target"
expect_refusal "no target at all" --dry-run
expect_refusal "both targets" --simulator --device dummy-udid --dry-run
expect_refusal "an option nobody defined" --simulator --pretend --dry-run

echo "[4/7] simulator output cannot become evidence"
EVIDENCE_REL="docs/performance/apple/g0/capability"
expect_refusal "a simulator run aimed at the evidence directory" \
  --simulator --out "$EVIDENCE_REL" --dry-run
expect_refusal "a simulator run aimed inside the evidence directory" \
  --simulator --out "$EVIDENCE_REL/today" --dry-run

run_runner --simulator --dry-run || fail "a plain simulator dry run failed:
$LAST_OUTPUT"
SIM_ADMIT="$(plan_value admit_input)"
[[ -z "$SIM_ADMIT" ]] || fail \
  "a simulator run resolved an admission input ($SIM_ADMIT). Relying on admit.py to
exclude the records afterwards is the split the contract warns about: the exclusion
is a note in the output, and the verdict is what gets read"
SIM_DIR="$(plan_value records_dir)"
case "$SIM_DIR" in
  *"$EVIDENCE_REL"*) fail "the default simulator directory is inside the evidence tree: $SIM_DIR" ;;
  "") fail "the simulator plan resolved no records directory" ;;
  *) ;;
esac

echo "[5/7] a device run admits from the directory it collected into"
# With an explicit --out, deliberately. On a default run the collection directory
# and the evidence directory are the same path, so a plan that admits from a
# hardcoded evidence directory agrees with itself and the check passes -- which is
# what an injection of exactly that bug proved. The two can only be told apart
# where they are allowed to differ.
ELSEWHERE="$(mktemp -d)"
trap 'rm -rf "$ELSEWHERE"' EXIT
run_runner --device dummy-udid --lockdown off --local-network-prompt none \
  --run-id gate-check --out "$ELSEWHERE" --dry-run \
  || fail "an attested device dry run failed:
$LAST_OUTPUT"
DEV_DIR="$(plan_value records_dir)"
DEV_ADMIT="$(plan_value admit_input)"
DEV_ARGS="$(plan_value app_arguments)"
DEV_FILE="$(plan_value record_file)"
[[ -n "$DEV_ADMIT" ]] || fail "a device run resolved no admission input, so its records decide nothing"
[[ "$DEV_ADMIT" == "$DEV_DIR" ]] || fail \
  "the device run collects into $DEV_DIR and admits from $DEV_ADMIT. A collector and a
decider pointed at two directories is a verdict about the previous lab day"
for expected in --migo-autorun --migo-run-id=gate-check --migo-lockdown=off \
  --migo-local-network-prompt=none; do
  case " $DEV_ARGS " in
    *" $expected "*) ;;
    *) fail "the launch arguments do not carry $expected: $DEV_ARGS" ;;
  esac
done
[[ "$DEV_FILE" == "$DEV_DIR/capability-gate-check.json" ]] || fail \
  "the plan expects the records at $DEV_FILE, which is not the run id's file under
the collection directory"

echo "[6/7] the flags the runner passes are the flags the app reads"
python3 - "$RUNNER" "$PARSER" "$VIEW" <<'PYTHON' || fail "the runner and the parser disagree about the flags"
import re
import sys

runner_path, parser_path, view_path = sys.argv[1:4]
runner = open(runner_path, encoding="utf-8").read()
parser = open(parser_path, encoding="utf-8").read()
view = open(view_path, encoding="utf-8").read()


def without_comments(text: str, marker: str) -> str:
    """Prose is not the table.

    Both files explain these flags in comments, and a name in a sentence is not a
    second definition of it. Only the code is compared -- which is also why the
    check below can insist a name appears exactly once.
    """
    return "\n".join(
        line for line in text.splitlines() if not line.lstrip().startswith(marker))


flag = re.compile(r"--migo-[a-z-]+")
parser_code = without_comments(parser, "//")
runner_code = without_comments(runner, "#")

parser_names = flag.findall(parser_code)
duplicates = sorted({name for name in parser_names if parser_names.count(name) > 1})
if duplicates:
    print(
        f"{', '.join(duplicates)} appears more than once in the parser. The flag table is "
        "the single source; a second copy is how a name gets changed in one place",
        file=sys.stderr)
    raise SystemExit(1)

declared = set(parser_names)
passed = set(flag.findall(runner_code))
if declared != passed:
    only_app = ", ".join(sorted(declared - passed)) or "none"
    only_runner = ", ".join(sorted(passed - declared)) or "none"
    print(
        "the app and the runner do not agree about the flag set.\n"
        f"  the app reads and the runner never passes: {only_app}\n"
        f"  the runner passes and the app does not read: {only_runner}\n"
        "A flag the app does not read is silently ignored: the run proceeds, records "
        "`unknown`, and the admission calls the arms that needed it unmeasured",
        file=sys.stderr)
    raise SystemExit(1)

# The run id rule is written on both sides because the app refusing a name the
# runner chose costs a launch with the devices already in hand. Two spellings of
# one rule, so they are compared rather than trusted.
allowed = re.search(r'charactersIn:\s*\n?\s*"([^"]+)"', parser)
maximum = re.search(r"runIdMaxLength\s*=\s*(\d+)", parser)
pattern = re.search(r"\^\[([^\]]+)\]\{1,(\d+)\}\$", runner_code)
if not (allowed and maximum and pattern):
    print(
        "one side no longer states the run-id rule in a form the other can be compared "
        f"against (parser charset={bool(allowed)} max={bool(maximum)} runner={bool(pattern)})",
        file=sys.stderr)
    raise SystemExit(1)

def expand(spec: str) -> set[str]:
    out, index = set(), 0
    while index < len(spec):
        if index + 2 < len(spec) and spec[index + 1] == "-":
            out.update(chr(c) for c in range(ord(spec[index]), ord(spec[index + 2]) + 1))
            index += 3
        else:
            out.add(spec[index])
            index += 1
    return out

if expand(allowed.group(1)) != expand(pattern.group(1)):
    print(
        "the app and the runner accept different characters in a run id, so the runner can "
        "name a run the app refuses at launch",
        file=sys.stderr)
    raise SystemExit(1)
if maximum.group(1) != pattern.group(2):
    print(
        f"the app allows {maximum.group(1)} characters in a run id and the runner "
        f"{pattern.group(2)}",
        file=sys.stderr)
    raise SystemExit(1)

# The file the collector waits for is named by the app. A rename on either side is
# a poll that never succeeds and a timeout with the devices in hand.
written = re.search(r'"([A-Za-z][A-Za-z0-9-]*-)\\\(.*?\)(\.json)"', view)
if not written:
    print(
        "the app no longer builds the records filename in a form this gate can read, so "
        "nothing checks that the collector waits for the file the app writes",
        file=sys.stderr)
    raise SystemExit(1)
awaited = f'{written.group(1)}$RUN_ID{written.group(2)}'
if awaited not in runner_code:
    print(
        f"the app writes {written.group(1)}<run id>{written.group(2)} and the runner waits for "
        f"something else; expected to find {awaited!r}",
        file=sys.stderr)
    raise SystemExit(1)
PYTHON

echo "[7/7] a record left by an earlier run cannot pass as this run's"
# Extracted from the runner and executed, rather than grepped for. The rule it
# encodes is the one whose failure is invisible: a repeated --run-id leaves the
# previous run's records in the container, the poll matches them on the first
# try, and the transfer wins the race against an app that has not finished
# writing. The second evidence run on the first real phone did exactly that and
# reported success, with timings byte-identical to a build made eight minutes
# earlier. A greps-for-the-string check would have passed against a predicate
# that always exits 0.
python3 - "$RUNNER" <<'PYTHON'
import datetime, json, os, pathlib, re, subprocess, sys, tempfile

runner = pathlib.Path(sys.argv[1]).read_text()
# The one `python3 -c '...'` in the runner, with its single-quoted program.
match = re.search(r"python3 -c '\n(.*?)\n' \"\$COPY_DEST\" \"\$LAUNCH_EPOCH\"", runner, re.S)
if not match:
    print(
        "the runner no longer carries a freshness predicate this gate can execute, so "
        "nothing checks that a stale record is refused",
        file=sys.stderr)
    raise SystemExit(1)

with tempfile.TemporaryDirectory() as tmp:
    tmp = pathlib.Path(tmp)
    predicate = tmp / "freshness.py"
    predicate.write_text(match.group(1) + "\n")
    # Any instant will do -- the cases below are all relative to it -- so this
    # takes SOURCE_DATE_EPOCH when the build names one. A test that needs a
    # reference point rather than the current time has no business reading a
    # clock that makes two runs of one commit differ.
    now = int(os.environ.get("SOURCE_DATE_EPOCH")
              or datetime.datetime.now(datetime.timezone.utc).timestamp())

    def stamp(offset):
        return datetime.datetime.fromtimestamp(
            now + offset, datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")

    def verdict(records):
        path = tmp / "records.json"
        path.write_text(json.dumps(records))
        return subprocess.run(
            [sys.executable, str(predicate), str(path), str(now)]).returncode

    cases = [
        ("records written after the launch", [{"captured_at": stamp(5)},
                                              {"captured_at": stamp(9)}], 0),
        ("a single record, not a list", {"captured_at": stamp(5)}, 0),
        ("a record from eight minutes ago", [{"captured_at": stamp(-480)}], 1),
        # The one that matters most: one origin re-ran and the other did not, so
        # half the evidence is this run's and half is not. Taking the newest
        # stamp -- or the first -- accepts it.
        ("one fresh record and one stale", [{"captured_at": stamp(-480)},
                                            {"captured_at": stamp(9)}], 1),
    ]
    for what, records, expected in cases:
        got = verdict(records)
        if got != expected:
            print(
                f"the freshness predicate {'accepted' if got == 0 else 'refused'} "
                f"{what}, and it should have {'accepted' if expected == 0 else 'refused'} it",
                file=sys.stderr)
            raise SystemExit(1)
PYTHON

echo "PASS: the runner refuses what it has to refuse, and passes what the app reads"
