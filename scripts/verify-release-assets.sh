#!/usr/bin/env bash
# No published asset may be unverifiable.
#
# Concretely: every asset is covered either by an entry in SHA256SUMS.txt or by its
# own `<asset>.attestation.json`, and none is covered by neither.
#
# Stating it that way, rather than as "there must be a SHA256SUMS.txt covering
# everything", is deliberate. The two mechanisms are not competing designs; each fits a
# different provenance model. A sidecar is produced next to its own artifact by whoever
# built it, so it composes across build machines. A single checksum manifest needs one
# publisher that sees every asset.
#
# Both are load-bearing today, and neither is a stopgap. An earlier version of this
# comment explained the pair as a consequence of Windows and OpenHarmony being built by
# hand, and predicted the sidecar branch would empty out as each platform moved into CI.
# Neither half still holds: release.yml now builds every platform, Windows and
# OpenHarmony included, each fetching and verifying its own librusty_v8 through
# scripts/fetch-v8-archives.sh -- and the sidecars come from Android, which publishes
# `<asset>.attestation.json` beside every artifact it builds *in* CI, by design.
#
# The practical consequence of getting that backwards is deleting the sidecar branch of
# this check, on the theory that it has no members left, and silently dropping the
# provenance Android's release assets actually carry.
#
# This is also the only check that can catch an asset uploaded outside the process,
# because it reads what was published rather than what a workflow intended. It
# deliberately does NOT carry a list of expected asset names: that list was just
# removed from three places in release.yml, and putting a fourth copy here would undo
# the point. Coverage is reported for a human to read; only the invariant is enforced.
#
# Usage:
#   scripts/verify-release-assets.sh dist/release      # pre-flight, a staged directory
#   scripts/verify-release-assets.sh v0.9.1            # post-flight, a published tag
#
# The argument is a directory if one exists at that path, otherwise a tag. No flag,
# because both are the same question asked at two moments.
set -euo pipefail

REPO="${MIGO_RELEASE_REPO:-minigame-labs/migo}"

if [[ $# -ne 1 ]]; then
    echo "usage: $0 <tag|staged-directory>" >&2
    exit 2
fi
TARGET="$1"

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

if [[ -d "$TARGET" ]]; then
    SOURCE="directory $TARGET"
    ( cd "$TARGET" && find . -maxdepth 1 -type f -printf '%f\n' ) | sort > "$WORK/assets.txt"
    if [[ -f "$TARGET/SHA256SUMS.txt" ]]; then
        cp "$TARGET/SHA256SUMS.txt" "$WORK/SHA256SUMS.txt"
    fi
else
    SOURCE="published tag $TARGET"
    # No auth: the repository is public and `gh` is not assumed present.
    if ! curl -fsSL "https://api.github.com/repos/$REPO/releases/tags/$TARGET" \
        -o "$WORK/release.json"; then
        echo "verify-release-assets: cannot read release $TARGET from $REPO." >&2
        echo "A tag that does not exist is not a passing release -- check the tag name." >&2
        exit 1
    fi
    python3 - "$WORK/release.json" <<'PY' > "$WORK/assets.txt"
import json, sys
release = json.load(open(sys.argv[1], encoding="utf-8"))
for name in sorted(asset["name"] for asset in release.get("assets", [])):
    print(name)
PY
    if grep -qx "SHA256SUMS.txt" "$WORK/assets.txt"; then
        curl -fsSL "https://github.com/$REPO/releases/download/$TARGET/SHA256SUMS.txt" \
            -o "$WORK/SHA256SUMS.txt"
    fi
fi

python3 - "$SOURCE" "$WORK/assets.txt" "$WORK/SHA256SUMS.txt" <<'PY'
from __future__ import annotations

import pathlib
import sys

SIDECAR = ".attestation.json"

source = sys.argv[1]
assets = [line for line in pathlib.Path(sys.argv[2]).read_text(encoding="utf-8").split("\n") if line]

sums_path = pathlib.Path(sys.argv[3])
checksummed: set[str] = set()
if sums_path.is_file():
    for line in sums_path.read_text(encoding="utf-8").splitlines():
        # `sha256sum` output: "<hash>  <name>", two spaces, name may contain spaces.
        parts = line.split("  ", 1)
        if len(parts) == 2:
            checksummed.add(parts[1].strip())

present = set(assets)
if not assets:
    print(f"verify-release-assets: {source} has no assets. An empty release is not a pass.",
          file=sys.stderr)
    sys.exit(1)

# GitHub attaches "Source code (zip/tar.gz)" to every tag automatically and offers no
# way to remove them, so they are not in the assets list and need no exemption here.
uncovered: list[str] = []
by_sums: list[str] = []
by_sidecar: list[str] = []
clerical: list[str] = []

for name in assets:
    if name == "SHA256SUMS.txt":
        clerical.append(name)
        continue
    if name.endswith(SIDECAR):
        # A sidecar is itself verifiable only if its payload is here; a sidecar whose
        # payload is absent is a promise about a file nobody can download.
        payload = name[: -len(SIDECAR)]
        if payload in present:
            clerical.append(name)
        else:
            uncovered.append(f"{name} attests `{payload}`, which is not in this release")
        continue
    if name in checksummed:
        by_sums.append(name)
    elif f"{name}{SIDECAR}" in present:
        by_sidecar.append(name)
    else:
        uncovered.append(
            f"{name} has no SHA256SUMS.txt entry and no {name}{SIDECAR}, so a consumer "
            "cannot tell whether the bytes they received are the bytes that were built"
        )

for name in by_sums:
    print(f"OK: {name} (SHA256SUMS.txt)")
for name in by_sidecar:
    print(f"OK: {name} (own attestation sidecar -- platform not yet built in CI)")

if uncovered:
    for line in uncovered:
        print(f"verify-release-assets: {line}", file=sys.stderr)
    print(
        f"verify-release-assets: FAIL ({len(uncovered)} of {len(assets)} asset(s) in "
        f"{source} are unverifiable)",
        file=sys.stderr,
    )
    sys.exit(1)

# Reported, never asserted: an expected-name list here would be the fourth copy of the
# asset list that release.yml just stopped keeping.
platforms = sorted({
    part
    for name in by_sums + by_sidecar
    for part in ("android", "linux", "windows", "ohos", "apple")
    if f"-{part}-" in name or name.endswith(f"-{part}.aar")
})
print(
    f"verify-release-assets: PASS ({len(assets)} asset(s) in {source}; "
    f"{len(by_sums)} covered by SHA256SUMS.txt, {len(by_sidecar)} by their own sidecar, "
    f"{len(clerical)} clerical)"
)
print(f"verify-release-assets: platforms present: {', '.join(platforms) or 'none detected'}")
if by_sidecar:
    print(
        "verify-release-assets: the sidecar-covered assets above are the ones still built "
        "by hand. They disappear from this list as their platforms move into CI."
    )
PY
