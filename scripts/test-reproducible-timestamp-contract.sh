#!/usr/bin/env bash
# A shipped artifact may not record when it was built, unless it is told when.
#
# `SOURCE_DATE_EPOCH` is the reproducible-builds convention -- honoured by tar,
# gzip, Gradle and rustc -- and the rule is simple: when it is set it *is* the build
# time, so two builds of one commit produce identical bytes. Phase 1's same-source
# rebuild comparison rests entirely on that, and a single wall clock anywhere in the
# artifact set defeats it for the whole release.
#
# Three shipped or committed artifacts carried one:
#
#   * `build-aar.sh` wrote `"sourceDateEpoch": <the epoch>` and then
#     `"buildTime": "<local wall clock>"` on the next line -- the input recorded and
#     unused for the one field that broke reproducibility, in local time, so the
#     same source differed between two timezones as well as between two minutes.
#     `build-aar.ps1` did the same on Windows.
#   * `generate-sbom.sh` stamped `metadata.timestamp`, and `release.yml` writes that
#     SBOM into the Android dist directory.
#   * `write-snapshot-manifest.sh` stamped `generated_at` into manifests that are
#     **committed** to the repository, so a regeneration always diffs and the
#     tracked file cannot be reproduced from the sources it describes.
#
# The rule enforced here: any script under `scripts/` that reads a clock must also
# name `SOURCE_DATE_EPOCH`, or be listed below as producing something that does not
# ship. The list is per-file and carries a reason each, rather than excluding a
# directory: a new report generator adds one line and states why, which is the
# forcing function. A new *artifact* generator cannot be added silently.
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

python3 - "$ROOT_DIR" <<'PY'
from __future__ import annotations

import pathlib
import re
import sys

root = pathlib.Path(sys.argv[1]).resolve()
scripts = root / "scripts"
if not scripts.is_dir():
    print("ERROR: scripts/ not found; this gate cannot check anything", file=sys.stderr)
    sys.exit(1)

# Reading a clock. Elapsed-time arithmetic reads one too, which is why the
# exemptions below are per file and explained rather than pattern-matched: a
# stopwatch and a build stamp look identical to a regular expression.
CLOCK = re.compile(
    r"datetime\.now\b|datetime\.utcnow\b|\btime\.time\(\)|\$\(\s*date\b|`date\b"
    r"|Get-Date\b|\[DateTime\]::Now|\[DateTime\]::UtcNow"
)

# Not shipped, and not committed. Each entry says what the file produces instead.
NOT_AN_ARTIFACT = {
    "scripts/ci/check_migo_test_suite.py": "a CI test-suite report read by a human, not packaged",
    "scripts/ci/compare_baseline.py": "a CI comparison report against a stored baseline",
    "scripts/ci/collect_metrics.sh": "device measurements, where elapsed wall time is the measurement",
    # A lab run, and the clock is naming the run rather than stamping an artifact.
    # Two runs of one commit MUST differ here: the timestamp becomes the records
    # filename, and pinning it to SOURCE_DATE_EPOCH would have this morning's run
    # overwrite the one before it under the same name. The records land in docs/,
    # which is gitignored, and nothing this script writes is packaged or committed.
    "scripts/run-apple-probe.sh": "a capability-gate lab run, where the clock names the run and two runs must not collide",
    # A stopwatch, which this gate's own header says looks identical to a build
    # stamp under a regular expression. The distinction is real here and was made
    # real rather than asserted: the duration was originally a field in
    # build-metadata.json, which would have made two builds of one commit differ.
    # It was moved out to a log line first, and only then does this line state
    # something true -- nothing the script writes carries a clock.
    "scripts/build-v8-apple.sh": "times its own build and prints the number; the metadata it writes carries no clock",
}

sources = sorted(
    path
    for pattern in ("*.sh", "*.py", "*.ps1")
    for path in scripts.rglob(pattern)
    if "__pycache__" not in path.parts
)
if not sources:
    print(
        "ERROR: no scripts found under scripts/; the scan would pass over nothing",
        file=sys.stderr,
    )
    sys.exit(1)

errors: list[str] = []
honoured: list[str] = []
exempt: list[str] = []
scanned_with_clock = 0

for path in sources:
    relative = str(path.relative_to(root))
    text = path.read_text(encoding="utf-8", errors="replace")

    # A gate that documents the pattern it forbids must not fail on its own prose,
    # and `#` starts a comment in all three languages scanned here.
    code = "\n".join(line.split("#", 1)[0] for line in text.splitlines())
    if not CLOCK.search(code):
        continue
    scanned_with_clock += 1

    if relative in NOT_AN_ARTIFACT:
        exempt.append(relative)
        continue
    if "SOURCE_DATE_EPOCH" in text:
        honoured.append(relative)
        continue

    offending = [
        f"{number}: {line.strip()}"
        for number, line in enumerate(text.splitlines(), start=1)
        if CLOCK.search(line.split("#", 1)[0])
    ]
    errors.append(
        f"{relative} reads a clock and never names SOURCE_DATE_EPOCH, so whatever it "
        f"writes differs between two builds of one commit -- "
        + "; ".join(offending[:3])
    )

# The exemption list must describe files that exist. A stale entry is a hole: it
# exempts nothing while looking like it accounts for something.
for relative in sorted(NOT_AN_ARTIFACT):
    path = root / relative
    if not path.is_file():
        errors.append(
            f"{relative} is exempted as {NOT_AN_ARTIFACT[relative]} but does not exist; "
            "a stale exemption accounts for nothing while looking like it does"
        )
    elif relative not in exempt:
        errors.append(
            f"{relative} is exempted as {NOT_AN_ARTIFACT[relative]} but no longer reads a "
            "clock; drop the exemption rather than leaving one that grants more than it "
            "needs to"
        )

if not honoured:
    errors.append(
        "no script honours SOURCE_DATE_EPOCH at all; either every artifact generator "
        "was removed or this scan has stopped matching, and the gate would pass "
        "vacuously"
    )

if errors:
    for error in errors:
        print(f"ERROR: {error}", file=sys.stderr)
    print(
        f"Reproducible timestamp contract: FAIL ({len(errors)} violation(s) across "
        f"{scanned_with_clock} script(s) that read a clock)",
        file=sys.stderr,
    )
    sys.exit(1)

print(
    f"Reproducible timestamp contract: PASS ({len(honoured)} artifact generator(s) honour "
    f"SOURCE_DATE_EPOCH, {len(exempt)} non-artifact script(s) exempted with a reason, "
    f"out of {len(sources)} scripts scanned)"
)
PY
