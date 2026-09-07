#!/usr/bin/env bash
# The Java reader of the native stats packet must read each field where Rust writes it.
#
# WHY THIS EXISTS: the packet crosses the JNI boundary as a raw little-endian
# `byte[]` -- `NativeBridge.getDebugStats` hands the array over and nothing on the
# Java side can be derived from the Rust struct at build time. So the layout is
# written twice, and both spellings compile: `RenderMetricsSnapshot::as_le_bytes`
# is a list of slice assignments, `StatsProtocol` is a list of `int` constants, and
# no compiler on either side relates them.
#
# What a disagreement costs is not a crash. It is a number: the debug overlay and
# `PerformanceSnapshot` would report a neighbouring counter under the wrong name,
# indefinitely, and every existing test would stay green -- the Java test used to
# assert the offsets it was written against rather than the ones the engine uses,
# so a reader reading `input_coalesced` out of `canvas2d_snapshot_forced_readbacks`
# agreed with its own test perfectly. The measurement instruments are the thing
# claims about memory and CPU are argued from, so a silently wrong one is worse
# than a missing one.
#
# The append-only convention is what makes this checkable rather than merely
# advisory: every version so far has added fields at the tail, so an offset that
# moves is either a deliberate reordering -- which must update both sides -- or a
# mistake. Either way the two lists stop matching and this fails.
#
# WHAT IT CHECKS, and why in this direction:
#   1. Every `OFFSET_<FIELD>` in StatsProtocol.java names a field the Rust writer
#      writes, at exactly that byte. One-way on purpose: a Rust field no Java
#      reader displays is unread telemetry, not a defect, and demanding a constant
#      for it would be busywork that makes the gate resented. A Java offset with
#      no matching Rust field is the actual bug -- it reads someone else's bytes.
#   2. MAGIC, VERSION, HEADER_LEN and BYTE_LEN agree. These are what decide
#      whether a packet is accepted at all, so a stale VERSION rejects every
#      packet a newer engine sends and the overlay simply stops updating.
#   3. Neither side is empty. An extraction that silently matches nothing prints
#      the same "PASS" as a clean one, and this repo has shipped an assertion lane
#      that never ran.
#   4. The readers hold no offset literals of their own. Naming the constants is
#      what puts them under check; a call site that goes back to `buf.getInt(132)`
#      is outside it again while this gate still passes.
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

python3 - "$REPO_ROOT" <<'PY'
from __future__ import annotations

import pathlib
import re
import sys

root = pathlib.Path(sys.argv[1])
stats_rs = root / "engine/crates/shared/src/stats.rs"
protocol_java = (
    root
    / "platforms/android/library/src/main/java/com/migo/runtime/internal/StatsProtocol.java"
)
readers = [
    root / "platforms/android/library/src/main/java/com/migo/runtime/PerformanceSnapshot.java",
    root / "platforms/android/library/src/main/java/com/migo/runtime/DebugOverlayView.java",
]

TAG = "[stats-offsets]"
failures: list[str] = []


def fail(message: str) -> None:
    failures.append(message)


rust = stats_rs.read_text()
java = protocol_java.read_text()

# The writer: `bytes[132..136].copy_from_slice(&self.input_coalesced.to_le_bytes());`
written = {
    match.group(3): (int(match.group(1)), int(match.group(2)))
    for match in re.finditer(
        r"bytes\[(\d+)\.\.(\d+)\]\.copy_from_slice\(&self\.(\w+)\.to_le_bytes\(\)\)",
        rust,
    )
}
if not written:
    fail(f"{stats_rs}: no `bytes[a..b] = self.<field>` assignments found; nothing to check against")

# The reader: `public static final int OFFSET_INPUT_COALESCED = 132;`
declared = {
    match.group(1): int(match.group(2))
    for match in re.finditer(
        r"public static final int OFFSET_(\w+)\s*=\s*(\d+);",
        java,
    )
}
if not declared:
    fail(f"{protocol_java}: no OFFSET_ constants found; the gate would pass vacuously")

header = {}
for name in ("MAGIC", "VERSION", "HEADER_LEN", "BYTE_LEN"):
    match = re.search(rf"public static final int {name}\s*=\s*(0x[0-9A-Fa-f]+|\d+);", java)
    if match is None:
        fail(f"{protocol_java}: {name} is missing")
    else:
        header[name] = int(match.group(1), 0)

for constant, offset in sorted(declared.items(), key=lambda item: item[1]):
    field = constant.lower()
    if field not in written:
        fail(
            f"{protocol_java}: OFFSET_{constant} names `{field}`, which "
            f"`RenderMetricsSnapshot::as_le_bytes` does not write -- the reader would take "
            f"another field's bytes"
        )
        continue
    start, end = written[field]
    if start != offset:
        fail(
            f"{protocol_java}: OFFSET_{constant} = {offset}, but the engine writes "
            f"`{field}` at {start}..{end}"
        )
    if end - start != 4:
        fail(f"{stats_rs}: `{field}` occupies {end - start} bytes; every reader assumes 4")

expected_header = {
    "MAGIC": r"pub const MAGIC: u16 = (0x[0-9A-Fa-f]+|\d+);",
    "VERSION": r"pub const VERSION: u16 = (\d+);",
    "HEADER_LEN": r"pub const HEADER_LEN: usize = (\d+);",
}
for name, pattern in expected_header.items():
    match = re.search(pattern, rust)
    if match is None:
        fail(f"{stats_rs}: {name} is missing")
        continue
    value = int(match.group(1), 0)
    if name in header and header[name] != value:
        fail(f"{protocol_java}: {name} = {header[name]}, but the engine declares {value}")

payload = re.search(r"pub const PAYLOAD_LEN: usize = (\d+);", rust)
if payload is None:
    fail(f"{stats_rs}: PAYLOAD_LEN is missing")
elif "HEADER_LEN" in header and "BYTE_LEN" in header:
    total = int(payload.group(1)) + header["HEADER_LEN"]
    if header["BYTE_LEN"] != total:
        fail(
            f"{protocol_java}: BYTE_LEN = {header['BYTE_LEN']}, but the engine's header "
            f"plus payload is {total}"
        )

# A reader that spells a *payload* offset itself is outside every check above. Literals
# below HEADER_LEN are the magic and the version, which are read by position and checked
# above by value.
header_len = header.get("HEADER_LEN", 4)
for reader in readers:
    source = reader.read_text()
    for match in re.finditer(r"\.get(?:Int|Short)\((\d+)\s*[+)]", source):
        if int(match.group(1)) >= header_len:
            fail(
                f"{reader}: reads the payload at literal offset {match.group(1)}; use a "
                f"StatsProtocol.OFFSET_ constant so this gate can check it"
            )

if failures:
    print(f"\033[0;31m{TAG} FAIL\033[0m", file=sys.stderr)
    for message in failures:
        print(f"  - {message}", file=sys.stderr)
    sys.exit(1)

print(
    f"\033[0;32m{TAG} PASS\033[0m "
    f"({len(declared)} Java offsets, each matching one of {len(written)} engine-written "
    f"fields; magic/version/header/total agree)"
)
PY
