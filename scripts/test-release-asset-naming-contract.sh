#!/usr/bin/env bash
# Every file in the release staging directory must be a name a consumer is looking for.
#
# `release.yml` used to keep the asset list in three hand-copied places -- an
# 18-name presence check, a 20-pattern `files:` block, and the implicit coverage of
# `sha256sum *` -- so missing one of them published a set nobody had agreed to. The
# list now exists once, as the contents of the staging directory, and this gate is
# what makes that trustworthy: it refuses a directory holding anything that is not a
# published asset under the canonical scheme.
#
# It also subsumes the `rm -rf platforms/android/dist` precaution in release.yml. That
# existed because build-aar.sh does not clear its output directory, so a re-run or a
# checkout with history could leave an AAR from an older naming scheme sitting beside
# the real one with no way for a consumer to tell them apart. A stale name now fails
# here instead of being uploaded.
#
# The scheme:
#
#   migo-<version>-android.aar                       Java/Kotlin -- `.aar` already says
#                                                    "Android", so no api segment, and
#                                                    it is multi-ABI so no arch segment
#   migo-<version>-android-nojni.aar                 the same AAR with `jni/**` removed,
#                                                    for hosts that deliver the engine at
#                                                    runtime instead of shipping ~17 MB of
#                                                    download to users who may never open a
#                                                    mini-game. `nojni` names exactly what
#                                                    was deleted, which is what lets
#                                                    test-android-nojni-aar-contract.sh
#                                                    assert the relationship between the two
#   migo-<version>-jni-android-<arch>.tar.gz         the bytes `-nojni` does not carry. One
#                                                    slice each, so unlike the AARs these do
#                                                    take an arch segment -- same reason the
#                                                    capi packages do
#   migo-<version>-capi-<platform>-<arch>.tar.gz     C ABI
#   migo-<version>-apple-sdk.zip                     the Swift package for iOS and macOS.
#                                                    One asset, no arch segment, for the
#                                                    AAR's reason: the xcframework inside
#                                                    already carries every slice
#   <payload>.sbom.cdx.json                        artifact-bound dependency inventory
#   <payload>.attestation.json                     provenance sidecar
#
# plus exactly two clerical files, `version.json` and `SHA256SUMS.txt`, which carry no
# version segment on purpose: a consumer must be able to fetch
# .../releases/download/<tag>/SHA256SUMS.txt without first knowing the version string.
# Every other request for an exemption should be refused -- an unnamed file in the
# staging directory is an asset nobody decided to publish.
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

if [[ $# -ne 1 ]]; then
    echo "usage: $0 <staging-directory>" >&2
    exit 2
fi

python3 - "$ROOT_DIR" "$1" <<'PY'
from __future__ import annotations

import pathlib
import re
import sys

PLATFORMS = ("android", "linux", "windows", "ohos")
ARCHES = ("arm64", "x86_64")

# Clerical files, and why each is allowed to have no version segment.
CLERICAL = {
    "version.json": "the tag/sha/timestamp record, fetched at a predictable URL",
    "SHA256SUMS.txt": "the checksum manifest, fetched at a predictable URL",
}

root = pathlib.Path(sys.argv[1]).resolve()
staging = pathlib.Path(sys.argv[2])

version = (root / "release/VERSION").read_text(encoding="utf-8").strip()

AAR = re.compile(rf"^migo-{re.escape(version)}-android\.aar$")
NOJNI_AAR = re.compile(rf"^migo-{re.escape(version)}-android-nojni\.aar$")
JNI = re.compile(rf"^migo-{re.escape(version)}-jni-android-({'|'.join(ARCHES)})\.tar\.gz$")
APPLE = re.compile(rf"^migo-{re.escape(version)}-apple-sdk\.zip$")
CAPI = re.compile(
    rf"^migo-{re.escape(version)}-capi-({'|'.join(PLATFORMS)})-({'|'.join(ARCHES)})\.tar\.gz$"
)


def payload_ok(name: str) -> bool:
    return bool(
        AAR.match(name)
        or NOJNI_AAR.match(name)
        or JNI.match(name)
        or CAPI.match(name)
        or APPLE.match(name)
    )


def why_rejected(name: str) -> str:
    """The most specific diagnosis available, so a failure is actionable."""
    if name.endswith(".attestation.json"):
        covered = name[: -len(".attestation.json")]
        if payload_ok(covered):
            return ""
        return (
            f"attests `{covered}`, which is not itself a published asset name. A sidecar "
            "without its payload is a promise about a file nobody receives"
        )
    if name.endswith(".sbom.cdx.json"):
        covered = name[: -len(".sbom.cdx.json")]
        if payload_ok(covered):
            return ""
        return (
            f"describes `{covered}`, which is not itself a published asset name. An SBOM "
            "must bind one concrete payload rather than a release-shaped guess"
        )
    if name in CLERICAL:
        return ""
    if payload_ok(name):
        return ""

    # Shape is right but the version is not: the failure a shape-only check would miss,
    # and the one a copied-forward asset from the previous release produces.
    loose = re.match(
        rf"^migo-(\d+\.\d+\.\d+[^-]*)-(capi-|jni-)?({'|'.join(PLATFORMS)}|android|apple)",
        name,
    )
    if loose and loose.group(1) != version:
        return (
            f"names version `{loose.group(1)}` but release/VERSION is `{version}`. An asset "
            "carried over from an earlier release is indistinguishable from this one's "
            "once downloaded"
        )
    if name.endswith(".aar"):
        return (
            f"is neither `migo-{version}-android.aar` nor `migo-{version}-android-nojni.aar`. "
            "Exactly two AARs are published -- the multi-ABI one and the same build with "
            "`jni/**` deleted -- and neither takes an arch segment"
        )
    if name.endswith(".zip"):
        return (
            f"is not `migo-{version}-apple-sdk.zip`. Exactly one Apple asset is published: "
            "the Swift package, whose xcframework carries every iOS and macOS slice"
        )
    if name.endswith(".tar.gz"):
        return (
            f"is neither `migo-{version}-capi-<platform>-<arch>.tar.gz` nor "
            f"`migo-{version}-jni-android-<arch>.tar.gz`, with platform in "
            f"{list(PLATFORMS)} and arch in {list(ARCHES)}"
        )
    return (
        "is not a published asset name. If it is an internal build product it does not "
        "belong in the staging directory; if it should be published, the scheme in this "
        "script has to say so first"
    )


def main() -> int:
    if not staging.is_dir():
        print(f"Release asset naming contract: {staging} is not a directory", file=sys.stderr)
        return 1

    entries = sorted(p for p in staging.iterdir() if p.is_file())
    if not entries:
        print(
            f"Release asset naming contract: {staging} is empty. A gate over nothing is "
            "not a pass -- point this at the directory the release publishes from.",
            file=sys.stderr,
        )
        return 1

    failures = []
    for entry in entries:
        reason = why_rejected(entry.name)
        if reason:
            failures.append(f"{entry.name} {reason}")
        else:
            print(f"OK: {entry.name}")

    if failures:
        for line in failures:
            print(f"Release asset naming contract: {line}", file=sys.stderr)
        print(
            f"Release asset naming contract: FAIL ({len(failures)} of {len(entries)} "
            f"file(s) in {staging} are not publishable names)",
            file=sys.stderr,
        )
        return 1

    print(
        f"Release asset naming contract: PASS ({len(entries)} file(s) in {staging}, "
        f"every name canonical for release/VERSION = {version})"
    )
    return 0


sys.exit(main())
PY
