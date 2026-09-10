#!/usr/bin/env bash
# Every caller-owned output record documents that its header is an INPUT.
#
# THE DRIFT THIS EXISTS TO CATCH cost a lane iteration on 2026-09-06, and the
# reason it was expensive is that the failure named the wrong subsystem.
#
# Three entry points hand the caller a versioned record to fill in:
#
#   migo_query_capabilities              MigoCapabilities *out
#   migo_surface_release_query           MigoSurfaceReleaseStatus *out_status
#   migo_session_submit_external_frame   MigoFrameIngressOutcome *out_outcome
#
# In all three the record is CALLER-OWNED and its `struct_size` is an INPUT:
# `write_versioned_output` reads it out of the caller's storage to decide how
# many bytes it may write there, so a record left holding zeros -- what an
# uninitialised C struct holds, and what Swift's `MigoSurfaceReleaseStatus()`
# holds -- is refused with MIGO_ERROR_INVALID_ARGUMENT before the call looks at
# anything else. That refusal is correct and load-bearing: it is what keeps a
# host built against an older header safe when it calls a newer library.
#
# Only migo_query_capabilities said so. The first host code ever written against
# one of the other two -- MigoSurfaceAttachTests, the A2.2 attach evidence --
# zero-initialised the status record, and the lane reported "the retired surface
# never reported RELEASED", which is a statement about a renderer that was
# working perfectly. Reading the header it was written from would not have
# helped: `migo_surface_release_query` documented exactly one cause of
# MIGO_ERROR_INVALID_ARGUMENT, "if either pointer is NULL", and both pointers
# were non-NULL.
#
# The same omission was still sitting in `migo_session_submit_external_frame`,
# where it is worse: that call is per-frame on the Apple Performance+ path, so a
# host that gets it wrong rejects every frame it will ever produce while the
# transport underneath is working.
#
# WHY A GATE RATHER THAN THREE FIXED DOC EDITS. The rule is about a shape, and
# the shape recurs -- `MigoSyncOutcome` and `MigoResourceOutcome` are already
# declared in external_frames.h for entry points that do not exist yet, and each
# will arrive as a fourth and fifth instance of it. Nothing about writing one
# fails to compile, no test can see it, and the cost lands on whoever integrates
# next rather than on whoever wrote it.
#
# WHY THE SET IS DERIVED, not listed. A hand-written list of three would be
# correct today and silently incomplete on the day a fourth lands, which is the
# only day it matters. Both halves are read out of the headers themselves:
#
#   a VERSIONED STRUCT is a `typedef struct MigoX { ... } MigoX;` whose first
#   member is `uint32_t struct_size;` -- the convention capabilities.h states as
#   a static assert, "every versioned struct must begin with struct_size";
#
#   a CALLER-OWNED OUTPUT RECORD is a parameter that is a single, NON-const
#   pointer to one. Const-ness is the discriminator rather than the `out_` name:
#   the headers const-qualify every input record (`const MigoSurfaceDescriptor
#   *descriptor`) and never the outputs, and a name pattern would be a rule about
#   spelling rather than about the direction of the write.
#
# Both derivations can go blind -- a renamed first member, a parameter list this
# parser stops matching -- and a blind audit that reports nothing looks exactly
# like a clean one. So finding zero of either is itself a violation.
#
# Host-only: it reads header files and nothing else.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# NO `grep -q` ON THE READ END OF A PIPE. With `pipefail` on, the early exit
# turns a live check into one that silently does not run.
pass() { printf '\033[0;32m[ok]\033[0m %s\n' "$*"; }
bad()  { printf '\033[0;31m[FAIL]\033[0m %s\n' "$*" >&2; }

run_audit() {
    audit_root="$1"
    if [ ! -d "$audit_root/include/migo" ]; then
        printf 'VIOLATION headers-missing: %s/include/migo does not exist\n' "$audit_root"
        return 1
    fi

    python3 - "$audit_root" <<'PY'
import pathlib
import re
import sys

root = pathlib.Path(sys.argv[1])
findings = 0


def report(identifier, message):
    global findings
    print(f"VIOLATION {identifier}: {message}")
    findings += 1


headers = sorted(root.glob("include/migo/**/*.h"))
if not headers:
    report("headers-missing", f"no headers under {root}/include/migo")
    print(findings)
    raise SystemExit(1)

# ------------------------------------------------------- which structs are versioned
#
# capabilities.h states the convention as a compile-time assert: "every
# versioned struct must begin with struct_size". This reads the same fact off
# the definitions rather than trusting a name.
TYPEDEF = re.compile(r"typedef struct (Migo\w+) \{(.*?)\}\s*\1\s*;", re.S)
versioned = {}
for header in headers:
    text = header.read_text(encoding="utf-8")
    for match in TYPEDEF.finditer(text):
        body = match.group(2)
        for line in body.splitlines():
            stripped = line.strip()
            if not stripped or stripped.startswith(("/*", "*", "//")):
                continue
            if stripped == "uint32_t struct_size;":
                versioned[match.group(1)] = header
            break

if not versioned:
    report(
        "no-versioned-structs",
        "this audit reads versioned structs as a typedef whose first member is "
        "`uint32_t struct_size;` and found none, so it can see nothing and would "
        "pass whatever the headers said",
    )
    print(findings)
    raise SystemExit(1)

# ------------------------------------------- which parameters are caller-owned outputs
DECL = re.compile(r"\b(migo_\w+)\s*\(([^;{]*?)\)\s*;", re.S)
PARAM = re.compile(r"(const\s+)?(Migo\w+)\s*\*\s*(\w+)")

records = []
for header in headers:
    text = header.read_text(encoding="utf-8")
    for match in DECL.finditer(text):
        for raw in match.group(2).split(","):
            param = " ".join(raw.split())
            parsed = PARAM.fullmatch(param)
            if not parsed or parsed.group(1) or parsed.group(2) not in versioned:
                continue
            records.append((header, match.start(), match.group(1), parsed.group(2), parsed.group(3)))

if not records:
    report(
        "no-output-records-found",
        "no entry point takes a non-const pointer to a versioned struct. Either every "
        "caller-owned output record is gone, or this audit's parameter parse stopped "
        "matching the headers -- and a parse that matches nothing reports nothing",
    )
    print(findings)
    raise SystemExit(1)

# --------------------------------------------------- and what their documentation says
#
# The doc block is the /* ... */ that ends immediately above the declaration,
# separated by whitespace only. A declaration whose comment sits above something
# else is not documentation of this entry point.
for header, offset, function, struct, param in records:
    text = header.read_text(encoding="utf-8")
    # `offset` is the function NAME. A declaration begins earlier than that --
    # `MIGO_API MigoResult MIGO_CALL` sits in between -- so the block is found by
    # walking back to the nearest `*/` and then requiring that nothing but the
    # declaration's own leading tokens separates the two. A `;`, `{` or `}` in
    # that gap means the comment documents something else and this entry point
    # has none of its own.
    before = text[:offset]
    where = f"{header.relative_to(root)}: {function}({struct} *{param})"
    close = before.rfind("*/")
    open_at = before.rfind("/*", 0, close) if close >= 0 else -1
    if close < 0 or open_at < 0 or re.search(r"[;{}]", before[close + 2:]):
        report(
            "no-doc-block",
            f"{where} has no comment block of its own above it, so nothing tells a host "
            f"that {param}->struct_size is an input it must set",
        )
        continue
    doc = before[open_at:close + 2]
    if "struct_size" not in doc:
        report(
            "undocumented-struct-size",
            f"{where}: {param} is a caller-owned versioned record whose struct_size bounds "
            "the write into the caller's storage, and the documentation never mentions it. "
            "A host that leaves the record zeroed gets a refusal it cannot explain",
        )
        continue
    if "MIGO_ERROR_INVALID_ARGUMENT" not in doc:
        report(
            "undocumented-refusal",
            f"{where}: the documentation mentions struct_size but never names "
            "MIGO_ERROR_INVALID_ARGUMENT, which is what a host actually receives when it "
            "gets this wrong -- and what it will search this header for",
        )

print(findings)
if not findings:
    # Counted, not asserted. The success line used to name a number a human had
    # typed, and the number went stale the first time an output record was
    # added -- which is the exact failure mode every gate in this directory
    # exists to prevent, reproduced in a gate's own report.
    print(f"AUDITED {len(records)}")
raise SystemExit(1 if findings else 0)
PY
}

failures=0
output="$(run_audit "$ROOT" 2>&1)" && status=0 || status=$?
audited="$(printf '%s\n' "$output" | sed -n 's/^AUDITED //p')"
if [ "$status" -eq 0 ]; then
    pass "every caller-owned output record documents its struct_size input and its refusal"
else
    bad "a caller-owned output record's contract is not in its documentation:"
    printf '%s\n' "$output" | sed 's/^/    /' >&2
    failures=$((failures + 1))
fi

WORK="$(mktemp -d "${TMPDIR:-/tmp}/migo-output-record-doc.XXXXXX")"
trap 'rm -rf "$WORK"' EXIT

fixture() {
    dest="$WORK/$1"
    rm -rf "$dest"
    mkdir -p "$dest/include"
    cp -R "$ROOT/include/migo" "$dest/include/migo"
    printf '%s' "$dest"
}

expect_violation() {
    what="$1"; want_id="$2"; dest="$3"
    out="$(run_audit "$dest" 2>&1)" && rc=0 || rc=$?
    if [ "$rc" -eq 0 ]; then
        bad "injection '$what' did not turn the audit red"
        failures=$((failures + 1))
        return
    fi
    printf '%s\n' "$out" > "$WORK/last-audit.txt"
    if grep "^VIOLATION $want_id:" "$WORK/last-audit.txt" > /dev/null; then
        pass "injection '$what' -> $want_id"
    else
        bad "injection '$what' went red, but not as $want_id. What it reported:"
        printf '%s\n' "$out" | sed 's/^/    /' >&2
        failures=$((failures + 1))
    fi
}

edit() { # <fixture> <relative path> <python program over `text`>
    python3 - "$1/$2" "$3" <<'EDIT'
import pathlib, sys
path, program = pathlib.Path(sys.argv[1]), sys.argv[2]
scope = {"text": path.read_text()}
exec(program, scope)
path.write_text(scope["text"])
EDIT
}

dest="$(fixture control)"
if out="$(run_audit "$dest" 2>&1)"; then
    pass "the unmodified fixture is clean, so each injection below is the only difference"
else
    bad "the unmodified fixture is already red; no injection below proves anything:"
    printf '%s\n' "$out" | sed 's/^/    /' >&2
    failures=$((failures + 1))
fi

dest="$(fixture surfacedoc)"
# The exact regression: the doc block put back the way it read before
# 2026-09-06, which is the wording MigoSurfaceAttachTests was written against.
edit "$dest" include/migo/surface.h '
import re
marker = "MIGO_API MigoResult MIGO_CALL migo_surface_release_query"
end = text.index(marker)
start = text.rindex("/*", 0, end)
text = text[:start] + """/*
 * Read the authoritative release state. Never blocks, so it is safe to poll
 * from a UI thread or an event-loop idle handler.
 *
 * A release that has reached RELEASED stays valid and queryable after the
 * owning Session is destroyed. Session destruction refuses while it is still
 * PENDING. Returns
 * MIGO_ERROR_INVALID_ARGUMENT if either pointer is NULL. out_status is written
 * only on MIGO_OK, never partially.
 */
""" + text[end:]
'
expect_violation "release_query stops saying struct_size is an input" \
    undocumented-struct-size "$dest"

dest="$(fixture framedoc)"
edit "$dest" include/migo/external_frames.h '
text = text.replace("MIGO_ERROR_INVALID_ARGUMENT", "a refusal", 2)
'
expect_violation "submit_external_frame stops naming the error a host will receive" \
    undocumented-refusal "$dest"

dest="$(fixture nodoc)"
edit "$dest" include/migo/capabilities.h '
import re
text = re.sub(r"/\*\n \* Report what this library supports\..*?\*/\n", "", text, count=1, flags=re.S)
'
expect_violation "the block above migo_query_capabilities is deleted" no-doc-block "$dest"

# The one that matters most: an entry point that did not exist when this gate was
# written. A fixed list of three would pass this.
dest="$(fixture newentrypoint)"
edit "$dest" include/migo/capabilities.h '
text = text.replace(
    "MIGO_API MigoResult MIGO_CALL migo_query_capabilities(MigoCapabilities *out);",
    "MIGO_API MigoResult MIGO_CALL migo_query_capabilities(MigoCapabilities *out);\n"
    "\n"
    "/*\n"
    " * Report what this library supports, again, for a host that asked twice.\n"
    " */\n"
    "MIGO_API MigoResult MIGO_CALL migo_query_capabilities_again(MigoCapabilities *out_probe);")
'
expect_violation "a new entry point takes an output record and documents nothing about it" \
    undocumented-struct-size "$dest"

dest="$(fixture allconst)"
edit "$dest" include/migo/capabilities.h '
text = text.replace("migo_query_capabilities(MigoCapabilities *out)",
                    "migo_query_capabilities(const MigoCapabilities *out)")
'
edit "$dest" include/migo/surface.h '
text = text.replace("    MigoSurfaceReleaseStatus *out_status);",
                    "    const MigoSurfaceReleaseStatus *out_status);")
'
# Every output record, by hand, and that is the point of the injection rather
# than a shortcoming of it: adding one to a public header obliges whoever added
# it to extend this list, and an author who does not gets told by this check
# failing. The synchronous barrier's three arrived that way.
edit "$dest" include/migo/external_frames.h '
text = text.replace("MigoFrameIngressOutcome *out_outcome);",
                    "const MigoFrameIngressOutcome *out_outcome);")
text = text.replace("MigoSyncOutcome *out_outcome);",
                    "const MigoSyncOutcome *out_outcome);")
'
expect_violation "every output record turns const, so the audit can see none" \
    no-output-records-found "$dest"

dest="$(fixture unversioned)"
edit "$dest" include/migo/capabilities.h '
text = text.replace("uint32_t struct_size;", "uint32_t record_size;")
'
for rel in include/migo/surface.h include/migo/session.h include/migo/external_frames.h \
           include/migo/input.h include/migo/types.h; do
    edit "$dest" "$rel" '
text = text.replace("uint32_t struct_size;", "uint32_t record_size;")
'
done
for rel in "$ROOT"/include/migo/platform/*.h; do
    edit "$dest" "include/migo/platform/$(basename "$rel")" '
text = text.replace("uint32_t struct_size;", "uint32_t record_size;")
'
done
expect_violation "the versioned-struct convention is renamed out from under the audit" \
    no-versioned-structs "$dest"

if [ "$failures" -ne 0 ]; then
    bad "$failures check(s) failed"
    exit 1
fi
echo "PASS: $audited caller-owned output records each document their struct_size input, and 6 injections were each seen to break it"
