#!/usr/bin/env bash
# The macOS V8 source pin agrees with everything that already exists.
#
# THE DRIFT THIS EXISTS TO CATCH is a pin that is internally tidy and disagrees
# with its neighbours. contracts/artifact-manifest/apple-v8.lock.json names a
# rusty_v8 version, a V8 revision, two triples and a macOS floor. Every one of
# those numbers is also stated somewhere else in this repository, by something
# with more authority:
#
#   rusty_v8_version   engine/Cargo.lock -- cargo resolved it, so cargo is right
#   v8_revision        the other platforms' locks -- one V8 ABI per engine, or
#                      src_binding.rs binds to a different API per platform
#   macos floor        contracts/apple/deployment-floor.json, which is the single
#                      source for every Apple deployment target and already has a
#                      gate stopping consumers from carrying their own copy
#   the two triples    scripts/fetch-v8-archives.sh, which has to name them to
#                      fetch them, and scripts/build-apple-sdk.sh, which decides
#                      which darwin slices the engine is built for at all
#
# A lock that drifts from any of those is not obviously wrong when read on its
# own. It goes wrong later, in a way that reads as someone else's bug: a
# `v8_revision` a version behind produces an archive whose `src_binding.rs`
# describes a different V8 API, and the failure is a link error in a consumer's
# build naming neither this file nor the archive.
#
# WHY THE HASHES ARE NOT CHECKED HERE. There are none yet, deliberately. A
# component-manifest.json carries the sha256 of real bytes and no macOS runner
# has built them; .github/workflows/apple-v8-probe.yml exists to establish what
# that build costs and which gn arguments it accepts first. This gate checks the
# half that can be true today, and asserts that the artifact half is absent
# rather than half-present -- a lock carrying hashes for archives nobody has
# published would be the worse failure.
#
# Host-only: it reads JSON and two shell scripts, and asks build-apple-sdk.sh one
# question.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# NO `grep -q` ON THE READ END OF A PIPE. See the note in
# scripts/test-apple-angle-pin-contract.sh: with `pipefail` on it turns a live
# check into one that silently stops running.
pass() { printf '\033[0;32m[ok]\033[0m %s\n' "$*"; }
bad()  { printf '\033[0;31m[FAIL]\033[0m %s\n' "$*" >&2; }

run_audit() {
    audit_root="$1"
    lock="$audit_root/contracts/artifact-manifest/apple-v8.lock.json"
    floor="$audit_root/contracts/apple/deployment-floor.json"
    fetch="$audit_root/scripts/fetch-v8-archives.sh"
    cargo_lock="$audit_root/engine/Cargo.lock"
    sdk="$audit_root/scripts/build-apple-sdk.sh"

    for f in "$lock" "$floor" "$fetch" "$cargo_lock" "$sdk"; do
        [ -f "$f" ] || { printf 'VIOLATION missing-input: %s does not exist\n' "$f"; return 1; }
    done

    # Which darwin slices the engine is built for, asked of the script that
    # decides it rather than listed here.
    sdk_slices="$(bash "$sdk" --print-slices macos 2>/dev/null | tr '\n' ' ' || true)"

    # The peer locks, so the V8 revision has something to agree with.
    peers=""
    for peer in android ohos windows; do
        peer_lock="$audit_root/contracts/artifact-manifest/$peer-v8.lock.json"
        [ -f "$peer_lock" ] && peers="$peers$peer_lock,"
    done

    python3 - "$lock" "$floor" "$fetch" "$cargo_lock" "$sdk_slices" "$peers" <<'PY'
import json
import re
import sys

lock_path, floor_path, fetch_path, cargo_lock_path, sdk_slices_raw, peers_raw = sys.argv[1:7]
findings = 0


def report(identifier, message):
    global findings
    print(f"VIOLATION {identifier}: {message}")
    findings += 1


with open(lock_path, encoding="utf-8") as handle:
    lock = json.load(handle)

if lock.get("schema") != "migo-v8-build-lock/v1":
    report("schema-wrong", f"schema is {lock.get('schema')!r}")

# ---------------------------------------------------------- cargo is the authority
version = lock.get("rusty_v8_version")
with open(cargo_lock_path, encoding="utf-8") as handle:
    cargo_lock = handle.read()
# `name = "v8"` followed by its version, in the same [[package]] block.
resolved = re.search(r'\nname = "v8"\nversion = "([^"]+)"', cargo_lock)
if resolved is None:
    report("v8-crate-not-resolved", f"{cargo_lock_path} resolves no package named v8")
elif resolved.group(1) != version:
    report(
        "rusty-v8-version-disagrees",
        f"the lock says rusty_v8_version={version!r} and cargo resolved v8 "
        f"{resolved.group(1)!r}; cargo is the authority because it is what actually links",
    )

# ------------------------------------------------- one V8 ABI across the engine
peers = [p for p in peers_raw.split(",") if p]
if not peers:
    report("no-peer-locks", "no other platform V8 lock was found to agree with")
for peer_path in peers:
    with open(peer_path, encoding="utf-8") as handle:
        peer = json.load(handle)
    name = peer_path.rsplit("/", 1)[-1]
    if peer.get("v8_revision") != lock.get("v8_revision"):
        report(
            "v8-revision-disagrees",
            f"{name} pins V8 {peer.get('v8_revision')!r} and this lock pins "
            f"{lock.get('v8_revision')!r}; one engine cannot bind two V8 APIs",
        )
    # NOT rusty_v8_revision: windows-v8.lock.json legitimately pins a different
    # rusty_v8 commit at the same version, so equality there is not the rule and
    # asserting it would make this gate wrong rather than strict.

# ----------------------------------------------- the floor has a single source
with open(floor_path, encoding="utf-8") as handle:
    floor_doc = json.load(handle)
try:
    authoritative = floor_doc["platforms"]["macos"]["deployment_target"]
except (KeyError, TypeError):
    authoritative = None
    report("floor-source-unreadable", f"{floor_path} has no platforms.macos.deployment_target")

declared = (lock.get("macos") or {}).get("deployment_target")
if authoritative is not None and declared != authoritative:
    report(
        "macos-floor-disagrees",
        f"the lock says {declared!r} and contracts/apple/deployment-floor.json says "
        f"{authoritative!r}, which is the single source every Apple consumer derives from",
    )

# -------------------------------------------------------------- the two triples
targets = lock.get("targets") or {}
if not targets:
    report("no-targets", "the lock names no targets")
triples = sorted(entry.get("triple", "") for entry in targets.values())
for arch, entry in sorted(targets.items()):
    expected = f"{arch}-apple-darwin"
    if entry.get("triple") != expected:
        report(
            "triple-malformed",
            f"targets.{arch}.triple is {entry.get('triple')!r}, expected {expected!r}",
        )
    # The conservative pair every other platform's lock carries. Raising one is a
    # claim about what the compiler emitted, and belongs with the bytes.
    wanted = {"aarch64": ("armv8-a", ["neon"]), "x86_64": ("x86-64-v1", ["cmov", "sse2"])}
    if arch in wanted:
        baseline, features = wanted[arch]
        if entry.get("cpu_baseline") != baseline or entry.get("required_cpu_features") != features:
            report(
                "cpu-baseline-unmeasured",
                f"targets.{arch} claims baseline {entry.get('cpu_baseline')!r} / features "
                f"{entry.get('required_cpu_features')!r}; until an archive exists this must be "
                f"the conservative {baseline!r} / {features!r} that the other locks carry",
            )
    else:
        report("unknown-arch", f"targets has an arch this gate does not know: {arch!r}")

# `--print-slices macos` is what the engine is actually built for; a lock that
# pins V8 for a triple the SDK does not build is V8 nobody links, and the reverse
# is a slice with no V8.
sdk_slices = sorted(sdk_slices_raw.split())
if sdk_slices and sdk_slices != triples:
    report(
        "sdk-slices-disagree",
        f"build-apple-sdk.sh builds macOS slices {sdk_slices} and the lock pins {triples}",
    )

# ------------------------------------------- the fetch script has to name them
with open(fetch_path, encoding="utf-8") as handle:
    fetch_source = handle.read()
known = re.search(r"KNOWN_TARGETS=\(([^)]*)\)", fetch_source)
if known is None:
    report("fetch-targets-unreadable", "scripts/fetch-v8-archives.sh has no KNOWN_TARGETS array")
else:
    listed = known.group(1).split()
    for triple in triples:
        if triple not in listed:
            report(
                "triple-not-fetchable",
                f"{triple} is pinned but scripts/fetch-v8-archives.sh cannot fetch it",
            )

# ------------------------------------ the artifact half belongs elsewhere
#
# The reason this assertion exists changed on 2026-09-09 and the assertion did
# not, which is worth stating rather than quietly leaving a stale message behind.
# It used to say "no macOS runner has built any bytes yet". Runners have now built
# them, they are published, and scripts/fetch-v8-archives.sh downloads and
# verifies both -- so that reason is false while the rule is still right.
#
# The rule is right because of where this family keeps its hashes: every V8 lock
# (android, linux, ohos, windows) pins IDENTITY, and the sha256 of the bytes lives
# in the per-target component-manifest.json that the fetcher verifies against.
# Duplicating them here would create a second place to update and a second thing
# to disagree with the archive. The ANGLE lock does carry hashes, and that is a
# different family with a different fetcher -- not a precedent for this one.
for forbidden in ("release", "targets_hashes", "hashes"):
    if forbidden in lock:
        report(
            "artifact-half-present-too-early",
            f"the lock carries {forbidden!r}; a V8 lock pins identity and the sha256 of the "
            "bytes lives in engine/third_party/rusty_v8/<triple>/component-manifest.json, "
            "which scripts/fetch-v8-archives.sh verifies against",
        )
for arch, entry in sorted(targets.items()):
    for forbidden in ("sha256", "size_bytes", "asset"):
        if forbidden in entry:
            report(
                "artifact-half-present-too-early",
                f"targets.{arch} carries {forbidden!r} before anything has been built",
            )

print(findings)
raise SystemExit(1 if findings else 0)
PY
}

failures=0
output="$(run_audit "$ROOT" 2>&1)" && status=0 || status=$?
if [ "$status" -eq 0 ]; then
    pass "the macOS V8 source pin agrees with cargo, the peer locks, the deployment floor and the fetch script"
else
    bad "the macOS V8 pin disagrees with its neighbours:"
    printf '%s\n' "$output" | sed 's/^/    /' >&2
    failures=$((failures + 1))
fi

WORK="$(mktemp -d "${TMPDIR:-/tmp}/migo-apple-v8-pin.XXXXXX")"
trap 'rm -rf "$WORK"' EXIT

fixture() {
    dest="$WORK/$1"
    rm -rf "$dest"
    mkdir -p "$dest/scripts" "$dest/contracts/artifact-manifest" "$dest/contracts/apple" "$dest/engine"
    cp "$ROOT/scripts/fetch-v8-archives.sh" "$dest/scripts/"
    cp "$ROOT/scripts/build-apple-sdk.sh" "$dest/scripts/"
    cp "$ROOT/contracts/artifact-manifest/"*-v8.lock.json "$dest/contracts/artifact-manifest/"
    cp "$ROOT/contracts/apple/deployment-floor.json" "$dest/contracts/apple/"
    cp "$ROOT/engine/Cargo.lock" "$dest/engine/"
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

edit_lock() {
    python3 - "$1/contracts/artifact-manifest/apple-v8.lock.json" "$2" <<'EDIT'
import json, pathlib, sys
path, program = pathlib.Path(sys.argv[1]), sys.argv[2]
doc = json.loads(path.read_text())
exec(program, {"doc": doc})
path.write_text(json.dumps(doc, indent=2) + "\n")
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

dest="$(fixture staleversion)"
edit_lock "$dest" 'doc["rusty_v8_version"] = "144.0.0"'
expect_violation "the lock names a rusty_v8 cargo did not resolve" \
    rusty-v8-version-disagrees "$dest"

dest="$(fixture v8skew)"
edit_lock "$dest" 'doc["v8_revision"] = "0" * 40'
expect_violation "the V8 revision drifts from the other platforms" v8-revision-disagrees "$dest"

dest="$(fixture floorskew)"
edit_lock "$dest" 'doc["macos"]["deployment_target"] = "12.0"'
expect_violation "the macOS floor stops matching its single source" macos-floor-disagrees "$dest"

# The one a reader would call an improvement: a higher, entirely plausible
# baseline, claimed before anything has been compiled.
dest="$(fixture optimistic)"
edit_lock "$dest" '
doc["targets"]["x86_64"]["cpu_baseline"] = "x86-64-v2"
doc["targets"]["x86_64"]["required_cpu_features"] = ["cmov", "popcnt", "sse2", "sse4.2"]
'
expect_violation "a CPU baseline is raised without an archive to justify it" \
    cpu-baseline-unmeasured "$dest"

dest="$(fixture unfetchable)"
python3 - "$dest/scripts/fetch-v8-archives.sh" <<'FETCH'
import pathlib, sys
path = pathlib.Path(sys.argv[1])
text = path.read_text()
before = "aarch64-apple-darwin x86_64-apple-darwin "
assert text.count(before) == 1, text.count(before)
path.write_text(text.replace(before, ""))
FETCH
expect_violation "the pinned triples stop being fetchable" triple-not-fetchable "$dest"

dest="$(fixture earlyhashes)"
edit_lock "$dest" '
doc["release"] = "https://github.com/minigame-labs/migo/releases/download/v8-archives-apple"
doc["targets"]["aarch64"]["sha256"] = "0" * 64
'
expect_violation "hashes appear before any archive exists" \
    artifact-half-present-too-early "$dest"

dest="$(fixture badtriple)"
edit_lock "$dest" 'doc["targets"]["aarch64"]["triple"] = "aarch64-apple-ios"'
expect_violation "a triple stops being a macOS one" triple-malformed "$dest"

if [ "$failures" -ne 0 ]; then
    bad "$failures check(s) failed"
    exit 1
fi
echo "PASS: the macOS V8 source pin holds, and 7 injections were each seen to break it"
