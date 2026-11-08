#!/usr/bin/env bash
# The permission surface the AAR actually ships, read from Gradle's merged manifests.
#
# `test-permission-coverage-contract.sh` holds the *source* manifests to the policy,
# which is not the same claim: the manifest that reaches a consumer is the merged one,
# and a dependency, a `tools:` directive or an AGP change can add a `uses-permission`
# that no source manifest in this repository mentions. That gate also only checks that
# each policy entry is *present*, so a permission nobody declared on purpose passes it.
# Here the comparison is exact in both directions, per profile.
#
# The API levels are the consumer-visible consequence rather than an extra assertion:
# given an exact set with exact `maxSdkVersion` values, what an app requests on a given
# Android version is determined. They are asserted and printed because "which
# permissions does a Slim build ask for on Android 12" is the question a compliance
# reviewer actually has, and deriving it by hand from `maxSdkVersion` is where a
# reviewer makes a mistake.
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ANDROID_DIR="$ROOT_DIR/platforms/android"
TAG="[merged-manifest-permissions]"

if [[ ! -x "$ANDROID_DIR/gradlew" ]]; then
    echo "$TAG SKIP no Gradle wrapper at $ANDROID_DIR/gradlew"
    exit 0
fi

# The debug variants, because the release ones sit behind
# verifyMigoReleaseArtifactPackaging<Profile> -- which refuses unless a release build
# has staged verified inputs, so asking for them here would gate on whichever package
# an earlier run left behind. The Python below establishes that the build type cannot
# change the manifest, and compares against the release manifest whenever one is
# present.
(
    cd "$ANDROID_DIR"
    ./gradlew --quiet :library:processFullDebugManifest :library:processSlimDebugManifest
)

python3 - "$ROOT_DIR" "$@" <<'PY'
from __future__ import annotations

import pathlib
import re
import sys
import xml.etree.ElementTree as ET

root = pathlib.Path(sys.argv[1]).resolve()
self_test = len(sys.argv) > 2 and sys.argv[2] == "--self-test"

sys.path.insert(0, str(root / "scripts/lib"))
from android_permission_policy import (  # noqa: E402
    ANDROID_NS,
    BASE_PERMISSIONS,
    FULL_PERMISSION_POLICY,
    effective_permissions,
    manifest_permissions,
)

TAG = "[merged-manifest-permissions]"
LIBRARY = root / "platforms/android/library"
MERGED = LIBRARY / "build/intermediates/merged_manifest"


def merged_manifest(variant: str) -> pathlib.Path:
    """AGP 9 nests the output under the producing task; AGP 8 did not."""
    task = f"process{variant[0].upper()}{variant[1:]}Manifest"
    nested = MERGED / variant / task / "AndroidManifest.xml"
    return nested if nested.exists() else MERGED / variant / "AndroidManifest.xml"
API_LEVELS = (26, 28, 31)
MIN_SDK = "26"

EXPECTED = {
    "full": {**BASE_PERMISSIONS, **FULL_PERMISSION_POLICY},
    "slim": dict(BASE_PERMISSIONS),
}

failures: list[str] = []


def fail(message: str) -> None:
    failures.append(message)
    print(f"\033[0;31m{TAG} FAIL {message}\033[0m", file=sys.stderr)


def ok(message: str) -> None:
    print(f"\033[0;32m{TAG} PASS {message}\033[0m")


def info(message: str) -> None:
    print(f"\033[0;36m{TAG} {message}\033[0m")


# A debug merged manifest is only evidence about the shipped artifact if the build type
# cannot contribute to it. Asserted rather than assumed, and in two ways: an
# AndroidManifest.xml under a build-type source set would make these two different
# documents, and so would a build-type-scoped dependency, because a library dependency
# brings its own manifest into the merge. A `releaseImplementation` could otherwise add a
# permission to the shipped Release manifest alone while this gate stayed green.
info("no build type contributes to the library manifest")
build_type_manifests = sorted(
    path.relative_to(root).as_posix()
    for name in ("debug", "release")
    for path in [LIBRARY / "src" / name / "AndroidManifest.xml"]
    if path.exists()
)
if build_type_manifests:
    fail(f"build-type manifest source sets exist: {', '.join(build_type_manifests)}")
else:
    ok("neither src/debug nor src/release declares a manifest")

BUILD_TYPE_DEPENDENCY = re.compile(
    r"^\s*(debug|release)"
    r"(Implementation|Api|CompileOnly|RuntimeOnly|AnnotationProcessor)\b",
    re.MULTILINE,
)
scoped = sorted({
    "".join(match)
    for match in BUILD_TYPE_DEPENDENCY.findall(
        (LIBRARY / "build.gradle").read_text(encoding="utf-8")
    )
})
if scoped:
    fail(
        f"build-type-scoped dependencies exist ({', '.join(scoped)}); a dependency brings "
        "its own manifest into the merge, so the release merged manifest has to be "
        "compared directly rather than through the debug one"
    )
else:
    ok("no build-type-scoped dependency can contribute a manifest to one type only")

for profile, expected in EXPECTED.items():
    variant = f"{profile}Debug"
    merged = merged_manifest(variant)
    if not merged.is_file():
        fail(f"{variant} merged manifest was not produced: {merged}")
        continue
    source = merged.read_text(encoding="utf-8")

    # When a release build has run, hold the two to each other rather than resting on
    # the source-set argument alone.
    release = merged_manifest(f"{profile}Release")
    if release.is_file():
        if release.read_text(encoding="utf-8") == source:
            ok(f"{profile}: the release merged manifest is identical to the debug one")
        else:
            fail(f"{profile}: debug and release merged manifests differ")

    manifest = ET.fromstring(source)
    uses_sdk = manifest.find("uses-sdk")
    declared_min = uses_sdk.get(ANDROID_NS + "minSdkVersion") if uses_sdk is not None else None
    if declared_min != MIN_SDK:
        fail(f"{profile}: merged manifest declares minSdkVersion {declared_min!r}, expected {MIN_SDK!r}")
    else:
        ok(f"{profile}: merged manifest floor is API {MIN_SDK}")

    permissions, problems = manifest_permissions(source)
    for problem in problems:
        fail(f"{profile}: {problem}")

    if self_test:
        permissions["android.permission.READ_CONTACTS"] = None

    unexpected = sorted(set(permissions) - set(expected))
    missing = sorted(set(expected) - set(permissions))
    if unexpected:
        fail(f"{profile}: merged manifest requests permissions the policy does not: {', '.join(unexpected)}")
    if missing:
        fail(f"{profile}: merged manifest is missing: {', '.join(missing)}")
    if not unexpected and not missing:
        ok(f"{profile}: merged manifest declares exactly its {len(expected)} policy permissions")

    for name in sorted(set(expected) & set(permissions)):
        if permissions[name] != expected[name]:
            fail(
                f"{profile}: `{name}` maxSdkVersion is {permissions[name]!r}, "
                f"expected {expected[name]!r}"
            )

    for api in API_LEVELS:
        actual = effective_permissions(permissions, api)
        wanted = effective_permissions(expected, api)
        if actual != wanted:
            fail(
                f"{profile}: at API {api} it requests {sorted(actual)}, expected {sorted(wanted)}"
            )
        else:
            ok(f"{profile}: API {api} requests {len(actual)} permission(s)")
            for name in sorted(actual):
                print(f"    {name.rsplit('.', 1)[-1]}")

# The Slim promise, stated where it can be read rather than inferred from two sets.
slim_merged = merged_manifest("slimDebug")
if slim_merged.is_file():
    slim_permissions, _ = manifest_permissions(slim_merged.read_text(encoding="utf-8"))
    leaked = sorted(set(slim_permissions) & set(FULL_PERMISSION_POLICY))
    if leaked:
        fail(f"Slim merged manifest carries Full-only permissions: {', '.join(leaked)}")
    else:
        ok("Slim's merged manifest carries no Full-only permission")

if self_test:
    if failures:
        print(f"{TAG} self-test: an injected permission is rejected")
        sys.exit(0)
    sys.exit(f"{TAG} self-test: an injected permission was accepted")

if failures:
    sys.exit(f"\033[0;31m{TAG} {len(failures)} check(s) failed\033[0m")
print(f"\033[0;32m{TAG} ok\033[0m")
PY
