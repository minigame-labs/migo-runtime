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
echo "Maven consumer: PASS"
