#!/usr/bin/env bash
# Two packagings of one staged tree must produce identical bytes, and the archive
# must say nothing about who built it.
#
# The `migo-sdk-<os>-<arch>.tar.gz` on every release up to v0.9.1 was a `tar` typed on the
# release machine, and the published Linux archive shows what that costs: the
# release user's own user/group as
# the owner of every entry and the build machine's wall clock as every mtime. The
# attestation beside it swears to a `package_sha256` that nobody receiving it can
# arrive at independently, which is the one thing an attestation is for.
#
# scripts/package-sdk.sh is now the only path that produces those assets, and this is
# what holds it reproducible. It runs against a synthetic staged prefix rather than a
# real 300 MB tree: what is under test is the packaging, and a fixture small enough to
# build in a temp directory means this runs on every pull request instead of only
# where a release happens to be staged.
#
# The epoch-sensitivity case is not decoration. Without it, "two runs agree" would
# also pass for a script that recorded a constant unrelated to its input, and the
# gate would be measuring nothing.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# The expected asset names below are the derivation under test, so they must come from
# the same version source package-sdk.sh reads. Hardcoding one here would let the
# contract pass against a package labelled with a version nobody chose.
# shellcheck source=scripts/lib/release-version.sh
source "$ROOT/scripts/lib/release-version.sh"
RELEASE_VERSION="$(read_release_version "$ROOT")"
PASSED=0

pass() { printf '\033[0;32mPASS\033[0m  %s\n' "$*"; PASSED=$((PASSED + 1)); }
fail() { printf '\033[0;31mFAIL\033[0m  %s\n' "$*" >&2; exit 1; }

WORK="$(mktemp -d)"
trap 'rm -rf -- "$WORK"' EXIT

TOOL_TARGET="${MIGO_ARTIFACT_MANIFEST_TARGET_DIR:-$ROOT/tools/artifact-manifest/target}"
if [[ -z "${MIGO_ARTIFACT_MANIFEST_TOOL:-}" ]]; then
    if ! CARGO_TARGET_DIR="$TOOL_TARGET" cargo build \
        --manifest-path "$ROOT/tools/artifact-manifest/Cargo.toml" \
        --locked --release >/dev/null; then
        fail "the artifact manifest tool does not build; packaging cannot be checked"
    fi
    export MIGO_ARTIFACT_MANIFEST_TOOL="$TOOL_TARGET/release/migo-artifact-manifest"
fi

# A staged prefix as the build scripts leave one: headers, libraries, a soname
# symlink chain, and exactly one package manifest under share/migo.
stage_prefix() {
    local dir="$1"
    mkdir -p "$dir/lib/pkgconfig" "$dir/include/migo" "$dir/share/migo"
    printf 'fixture archive\n' > "$dir/lib/libmigo.a"
    printf 'fixture shared object\n' > "$dir/lib/libmigo.so.0.0.0"
    ln -s libmigo.so.0.0.0 "$dir/lib/libmigo.so.1"
    ln -s libmigo.so.1 "$dir/lib/libmigo.so"
    printf 'Name: migo\n' > "$dir/lib/pkgconfig/migo.pc"
    printf '#define MIGO_FIXTURE 1\n' > "$dir/include/migo/migo.h"
    printf '{"schema":"fixture/v1","version":"0.0.0"}\n' \
        > "$dir/share/migo/fixture-x86_64-manifest.json"
}

package() {
    local prefix="$1" out="$2" epoch="$3"
    mkdir -p "$out"
    SOURCE_DATE_EPOCH="$epoch" bash "$ROOT/scripts/package-sdk.sh" \
        "$prefix" --output-dir "$out" >/dev/null
}

sha_of() { sha256sum "$1" | cut -d' ' -f1; }

EPOCH_A=1700000000
EPOCH_B=1800000000
ASSET="migo-${RELEASE_VERSION}-capi-fixture-x86_64.tar.gz"

PREFIX="$WORK/dist/migo-fixture-x86_64"
stage_prefix "$PREFIX"

package "$PREFIX" "$WORK/out-a1" "$EPOCH_A"
package "$PREFIX" "$WORK/out-a2" "$EPOCH_A"
package "$PREFIX" "$WORK/out-b" "$EPOCH_B"

[[ -f "$WORK/out-a1/$ASSET" ]] \
    || fail "asset name is not derived from the staged prefix; expected $ASSET"
pass "asset name derived from the staged prefix ($ASSET)"

SHA_A1="$(sha_of "$WORK/out-a1/$ASSET")"
SHA_A2="$(sha_of "$WORK/out-a2/$ASSET")"
SHA_B="$(sha_of "$WORK/out-b/$ASSET")"

if [[ "$SHA_A1" != "$SHA_A2" ]]; then
    fail "two packagings of one tree under SOURCE_DATE_EPOCH=$EPOCH_A differ ($SHA_A1 vs $SHA_A2)"
fi
pass "two packagings of one tree under one epoch are byte-identical"

if [[ "$SHA_A1" == "$SHA_B" ]]; then
    fail "changing SOURCE_DATE_EPOCH changed nothing, so the comparison above proves nothing"
fi
pass "SOURCE_DATE_EPOCH reaches the archive, so the comparison above is not vacuous"

# Packaging one tree twice cannot see a umask leak, because the modes are the same
# both times. Two stagings of identical content under different modes can: the
# staged trees are assembled with plain `cp` and `mkdir`, so on a builder with
# umask 002 the group-write bit is set where it is not under 022, and GNU tar
# records that bit.
UMASK_A="$WORK/dist/migo-umask-x86_64"
stage_prefix "$UMASK_A"
find "$UMASK_A" -type d -exec chmod 700 {} +
find "$UMASK_A" -type f -exec chmod 600 {} +
package "$UMASK_A" "$WORK/out-mode-a" "$EPOCH_A"

find "$UMASK_A" -type d -exec chmod 775 {} +
find "$UMASK_A" -type f -exec chmod 664 {} +
package "$UMASK_A" "$WORK/out-mode-b" "$EPOCH_A"

MODE_ASSET="migo-${RELEASE_VERSION}-capi-umask-x86_64.tar.gz"
if [[ "$(sha_of "$WORK/out-mode-a/$MODE_ASSET")" != "$(sha_of "$WORK/out-mode-b/$MODE_ASSET")" ]]; then
    fail "the same content staged under different permissions produced different archives, so the builder's umask reaches the published bytes"
fi
pass "permission differences between two stagings of one tree do not change the archive"

python3 - "$WORK/out-mode-a/$MODE_ASSET" <<'PY'
import sys
import tarfile

offenders = []
with tarfile.open(sys.argv[1], "r:gz") as handle:
    members = handle.getmembers()
    for member in members:
        if member.issym():
            continue
        expected = 0o755 if (member.isdir() or member.mode & 0o100) else 0o644
        if member.mode != expected:
            offenders.append(f"{member.name}: {member.mode:04o} not {expected:04o}")
if offenders:
    print("ERROR: recorded modes are not normalised:", file=sys.stderr)
    for offender in offenders[:5]:
        print(f"  {offender}", file=sys.stderr)
    sys.exit(1)
print(f"checked {len(members)} entries")
PY
pass "every entry records 0644 or 0755, never the builder's umask"

python3 - "$WORK/out-a1/$ASSET" "$EPOCH_A" <<'PY'
import sys
import tarfile

archive, epoch = sys.argv[1], int(sys.argv[2])
offenders = []
with tarfile.open(archive, "r:gz") as handle:
    members = handle.getmembers()
    if not members:
        sys.exit("the archive is empty, so every assertion below would pass over nothing")
    for member in members:
        if member.uid or member.gid or member.uname or member.gname:
            offenders.append(
                f"{member.name}: uid={member.uid} gid={member.gid} "
                f"uname={member.uname!r} gname={member.gname!r}"
            )
        elif member.mtime != epoch:
            offenders.append(f"{member.name}: mtime={member.mtime} not {epoch}")
if offenders:
    print("ERROR: the archive records who built it or when:", file=sys.stderr)
    for offender in offenders[:5]:
        print(f"  {offender}", file=sys.stderr)
    sys.exit(1)
print(f"checked {len(members)} entries")
PY
pass "no entry records an owner name, uid, gid or mtime of its own"

# gzip stores a timestamp and the original file name in its own header, where tar
# cannot see them and they still change the bytes. Byte 4..8 is MTIME, and bit 3 of
# the flags byte is set when a name follows.
python3 - "$WORK/out-a1/$ASSET" <<'PY'
import struct
import sys

with open(sys.argv[1], "rb") as handle:
    header = handle.read(10)
if len(header) < 10 or header[:2] != b"\x1f\x8b":
    sys.exit("not a gzip stream")
flags = header[3]
mtime = struct.unpack("<I", header[4:8])[0]
problems = []
if mtime:
    problems.append(f"gzip header carries mtime {mtime}")
if flags & 0x08:
    problems.append("gzip header carries the original file name")
if problems:
    sys.exit("ERROR: " + "; ".join(problems))
PY
pass "the gzip header carries neither a timestamp nor the source file name"

ATTESTATION="$WORK/out-a1/$ASSET.attestation.json"
python3 - "$ATTESTATION" "$WORK/out-a1/$ASSET" "$SHA_A1" <<'PY'
import json
import sys

path, package, expected = sys.argv[1], sys.argv[2], sys.argv[3]
with open(path, encoding="utf-8") as handle:
    attestation = json.load(handle)
if attestation.get("package_sha256") != expected:
    sys.exit(
        f"attestation swears to {attestation.get('package_sha256')} but the archive is {expected}"
    )
import os

if attestation.get("package_size_bytes") != os.path.getsize(package):
    sys.exit("attestation records a size the archive does not have")
PY
pass "the attestation's hash and size are the archive's own"

expect_failure() {
    local description="$1"; shift
    if bash "$ROOT/scripts/package-sdk.sh" "$@" >/dev/null 2>&1; then
        fail "$description was accepted"
    fi
    pass "$description is refused"
}

NO_MANIFEST="$WORK/dist/migo-nomanifest-x86_64"
stage_prefix "$NO_MANIFEST"
rm "$NO_MANIFEST/share/migo/fixture-x86_64-manifest.json"
expect_failure "a staged tree with no package manifest" "$NO_MANIFEST" --output-dir "$WORK/out-none"

TWO_MANIFESTS="$WORK/dist/migo-twomanifests-x86_64"
stage_prefix "$TWO_MANIFESTS"
cp "$TWO_MANIFESTS/share/migo/fixture-x86_64-manifest.json" \
   "$TWO_MANIFESTS/share/migo/second-x86_64-manifest.json"
expect_failure "a staged tree carrying two package manifests" "$TWO_MANIFESTS" --output-dir "$WORK/out-two"

UNNAMED="$WORK/dist/sdk-x86_64"
stage_prefix "$UNNAMED"
expect_failure "a staged tree whose name no asset name can be derived from" "$UNNAMED" --output-dir "$WORK/out-unnamed"

printf '\n\033[0;32mOK\033[0m: SDK release packaging contract satisfied (%d checks)\n' "$PASSED"
