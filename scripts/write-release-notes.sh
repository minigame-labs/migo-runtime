#!/usr/bin/env bash
# Generate the release notes body from the staging directory that is about to be
# published.
#
# The asset table is derived, not written by hand. A hand-maintained list in a
# template is a third copy of the asset list -- release.yml just stopped keeping
# three of them -- and it would drift the first time a platform is added or its
# name changes. Reading dist/release/ means the notes describe what is actually
# being uploaded, or they do not build.
#
# `generate_release_notes: true` on the publish step appends GitHub's own
# commit-derived notes below whatever this produces, so this file supplies only the
# part GitHub cannot know: what the assets are, how to verify them, and the two
# things that surprise people (the source archives, and the profile/ABI story).
#
# Usage: scripts/write-release-notes.sh <staging-dir> <output-file>
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
# shellcheck source=scripts/lib/release-version.sh
source "$SCRIPT_DIR/lib/release-version.sh"

if [[ $# -ne 2 ]]; then
    echo "usage: $0 <staging-dir> <output-file>" >&2
    exit 2
fi
STAGING="$1"
OUTPUT="$2"
[[ -d "$STAGING" ]] || { echo "[release-notes] not a directory: $STAGING" >&2; exit 1; }

VERSION="$(read_release_version "$REPO_ROOT")"

# The curated record of what changed. `generate_release_notes: true` appends
# GitHub's commit-derived list below this file's output, but that is a wall of PR
# titles -- the CHANGELOG section is the part written for a reader. It is not
# optional: test-changelog-release-section-contract.sh already fails the build if
# `## v<version>` is missing, so an empty extraction here means that gate was
# bypassed and the notes should not be written rather than published without it.
CHANGELOG_SECTION="$(
    awk -v heading="## v${VERSION} " '
        index($0 " ", heading) == 1 { found = 1; next }
        found && (/^## / || /^---[[:space:]]*$/) { exit }
        found { print }
    ' "$REPO_ROOT/CHANGELOG.md" |
        sed -e '/./,$!d' |          # drop leading blank lines
        sed -e ':a' -e '/^\n*$/{$d;N;ba}'   # drop trailing blank lines
)"
if [[ -z "${CHANGELOG_SECTION//[[:space:]]/}" ]]; then
    echo "[release-notes] CHANGELOG.md has no '## v${VERSION}' section body" >&2
    echo "[release-notes] (test-changelog-release-section-contract.sh should have caught this)" >&2
    exit 1
fi

# One row per payload, in the order the release page shows them (GitHub sorts assets
# case-insensitively by name). Sidecars, checksums and version.json are deliberately
# not rows: they are described once in the verification section instead of repeated
# per platform.
ASSET_TABLE="$(
    python3 - "$STAGING" <<'PY'
import pathlib, sys

staging = pathlib.Path(sys.argv[1])
rows = []
for path in sorted(staging.iterdir(), key=lambda p: p.name.lower()):
    name = path.name
    if not path.is_file() or name.endswith(".attestation.json"):
        continue
    if name in ("SHA256SUMS.txt", "version.json") or name.endswith(".sbom.cdx.json"):
        continue
    size = f"{path.stat().st_size / 1048576:.0f} MB"
    if name.endswith(".aar"):
        what = "Android library for Java/Kotlin, `arm64-v8a` + `x86_64`"
    elif name.endswith("-apple-sdk.zip"):
        what = "Swift package for iOS (Performance+) and macOS (V8) -- unzip and add as a local package"
    elif "-capi-" in name:
        platform = name.split("-capi-")[1].rsplit(".tar.gz", 1)[0]
        what = f"C ABI SDK for `{platform}` -- headers, library, CMake package"
    else:
        what = "see the file"
    rows.append(f"| `{name}` | {size} | {what} |")
print("\n".join(rows))
PY
)"

cat > "$OUTPUT" <<NOTES
Runtime SDKs for Android, Linux, OpenHarmony, Windows, iOS and macOS.

## What changed in v$VERSION

$CHANGELOG_SECTION

## Assets

| File | Size | What it is |
| --- | --- | --- |
$ASSET_TABLE

One AAR is published, carrying both ABIs. A shipped app carries one: add
\`ndk { abiFilters 'arm64-v8a' }\` to your \`defaultConfig\` and the packaged native
library is about half the size, or publish an App Bundle and Play delivers per device.
There is no separate slim build -- the product profile is an internal build axis, not
a choice an integrator can make.

## iOS and macOS

\`migo-$VERSION-apple-sdk.zip\` unpacks to \`MigoApple/\`, a Swift package: add it in
Xcode with *File > Add Package Dependencies > Add Local*, then link
\`MigoApplePerformancePlus\` (iOS) or \`MigoMacV8\` (macOS) and put a \`MigoGameView\`
on screen. \`MigoApple/README.md\` has the integration steps. On macOS the app must be
signed with the hardened runtime and \`com.apple.security.cs.allow-jit\`, and embed ANGLE
with \`Frameworks/Scripts/embed-apple-angle.sh\`; the view refuses to start without the
entitlement and says why.

## Verifying a download

\`SHA256SUMS.txt\` covers every asset built by CI:

\`\`\`bash
sha256sum -c SHA256SUMS.txt 2>/dev/null | grep -v ': OK$' || echo "all verified"
\`\`\`

Every runtime payload also has an artifact-bound \`<asset>.sbom.cdx.json\` recording
its SHA-256, exact target/profile and reachable dependency graph. Provenance
\`<asset>.attestation.json\` sidecars record package identity; the checksum manifest
covers both payloads and sidecars. \`scripts/verify-release-assets.sh <tag>\` checks
that no published asset escapes that coverage.

## Source code (zip / tar.gz)

Those two links are generated by GitHub from the tag and cannot be removed. **Do not
build from them.** They omit submodules and Git LFS content, so a build from the zip
fails in ways that look like repository corruption. Clone the tag instead:

\`\`\`bash
git clone --branch v$VERSION --recurse-submodules https://github.com/minigame-labs/migo.git
\`\`\`

## Integration

See [migo-examples](https://github.com/minigame-labs/migo-examples) for runnable
samples, and [BUILD.md](https://github.com/minigame-labs/migo/blob/v$VERSION/BUILD.md)
for building from source.
NOTES

echo "[release-notes] wrote $(wc -l < "$OUTPUT") lines to $OUTPUT"
