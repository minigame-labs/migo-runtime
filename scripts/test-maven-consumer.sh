#!/usr/bin/env bash
# A host app resolves Migo by its Maven coordinates, and builds.
#
# The publication contract proves what is in the staged repository. It cannot prove a
# host's Gradle takes it: that the POM resolves, the AAR's manifest merges, its classes
# compile against and dex into an app, and its natives package. Those failures reach a
# host as a broken first build, so tests/maven_consumer/android -- an app written the
# way a host writes one, depending on com.minigame-labs:migo-android and nothing from
# this tree -- is built against the staged repository, both ways a host can take the
# engine, and the APKs are read back:
#
#   bundled   migo-android, the default: lib/arm64-v8a/libmigo.so is in the APK, and
#             the app's own `abiFilters 'arm64-v8a'` keeps x86_64 out -- which is why
#             the SDK publishes one AAR and no per-ABI variant
#   deferred  migo-android-nojni: no libmigo.so at all, the engine being delivered at
#             runtime
#
# And both request exactly the permissions scripts/lib/android_permission_policy.py
# names for Full, maxSdkVersion included. The library's own merged manifest is not what a
# host ships: the host's merger adds what it infers from the AAR. A library manifest
# carries no targetSdkVersion, and for such a library the merger reads
# WRITE_EXTERNAL_STORAGE as implying READ_EXTERNAL_STORAGE -- unbounded, where WRITE
# stops at API 28 -- so every host was asking for storage on every Android version, and
# no gate that read the library's manifest could see it.
#
# Usage: test-maven-consumer.sh [REPOSITORY]   (default: dist/maven, where
# scripts/test-maven-publication-contract.sh --aar ... stages the real AARs)
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
REPO="$(cd "${1:-$ROOT/dist/maven}" 2>/dev/null && pwd)" || {
    echo "error: no staged repository at ${1:-$ROOT/dist/maven}; stage one with" >&2
    echo "  bash scripts/test-maven-publication-contract.sh --aar <aar> --nojni-aar <aar>" >&2
    exit 1
}
[[ -d "$REPO/com/minigame-labs/migo-android" ]] || {
    echo "error: $REPO holds no com.minigame-labs:migo-android" >&2
    exit 1
}

(cd "$ROOT/platforms/android" && ./gradlew --quiet \
    :maven-consumer:assembleBundledDebug :maven-consumer:assembleDeferredDebug \
    -PmigoMavenRepo="$REPO")

OUT="$ROOT/tests/maven_consumer/android/build/outputs/apk"
python3 - "$OUT/bundled/debug/maven-consumer-bundled-debug.apk" \
    "$OUT/deferred/debug/maven-consumer-deferred-debug.apk" <<'PY'
import sys
import zipfile

bundled, deferred = sys.argv[1:]
failures = []


def entries(path):
    try:
        with zipfile.ZipFile(path) as apk:
            return {name: apk.read(name) if name.endswith(".dex") else b"" for name in apk.namelist()}
    except (OSError, zipfile.BadZipFile) as error:
        failures.append(f"{path}: {error}")
        return {}


def has_sdk_classes(apk):
    return any(b"Lcom/migo/runtime/RuntimeConfig;" in data for name, data in apk.items() if name.endswith(".dex"))


with_engine = entries(bundled)
without = entries(deferred)
engines = lambda apk: sorted(name for name in apk if name.endswith("/libmigo.so"))

if engines(with_engine) != ["lib/arm64-v8a/libmigo.so"]:
    failures.append(f"the bundled APK carries {engines(with_engine)}, not lib/arm64-v8a/libmigo.so alone")
if engines(without):
    failures.append(f"the deferred APK carries an engine it should be delivered: {engines(without)}")
for label, apk in (("bundled", with_engine), ("deferred", without)):
    if apk and not has_sdk_classes(apk):
        failures.append(f"the {label} APK's dex holds no com.migo.runtime.RuntimeConfig")

for failure in failures:
    print(f"[FAIL] {failure}", file=sys.stderr)
if failures:
    sys.exit(1)
print("[ok] the bundled APK packages arm64's engine alone; the deferred one none; both dex the SDK")
PY
AAPT2="$(ls -d "${ANDROID_HOME:-${ANDROID_SDK_ROOT:-}}"/build-tools/*/aapt2 2>/dev/null | sort -V | tail -1)"
[[ -x "$AAPT2" ]] || { echo "error: no aapt2 under \$ANDROID_HOME/build-tools" >&2; exit 1; }
python3 - "$ROOT" "$AAPT2" "$OUT/bundled/debug/maven-consumer-bundled-debug.apk" \
    "$OUT/deferred/debug/maven-consumer-deferred-debug.apk" <<'PY'
import re
import subprocess
import sys
root, aapt2, *apks = sys.argv[1:]
sys.path.insert(0, root + "/scripts/lib")
from android_permission_policy import BASE_PERMISSIONS, FULL_PERMISSION_POLICY
expected = {**BASE_PERMISSIONS, **FULL_PERMISSION_POLICY}
line = re.compile(r"uses-permission(?:-sdk-23)?: name='([^']+)'(?: maxSdkVersion='(\d+)')?$")
failures = []
for apk in apks:
    dump = subprocess.run([aapt2, "dump", "permissions", apk], check=True, capture_output=True, text=True).stdout
    found = {}
    for text in dump.splitlines():
        match = line.match(text.strip())
        if match:
            found[match.group(1)] = match.group(2)
    label = apk.rsplit("/", 1)[-1]
    for name in sorted(set(found) - set(expected)):
        failures.append(f"{label} requests {name} (maxSdkVersion={found[name]}), which the policy does not name")
    for name in sorted(set(expected) - set(found)):
        failures.append(f"{label} does not request {name}, which the policy names")
    for name in sorted(set(found) & set(expected)):
        if found[name] != expected[name]:
            failures.append(f"{label} requests {name} up to maxSdkVersion={found[name]}, not {expected[name]}")
for failure in failures:
    print(f"[FAIL] {failure}", file=sys.stderr)
if failures:
    sys.exit(1)
print(f"[ok] both APKs request exactly the policy's {len(expected)} permissions, bounds included")
PY
echo "Maven consumer: PASS"
