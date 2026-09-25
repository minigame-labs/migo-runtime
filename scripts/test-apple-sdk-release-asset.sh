#!/usr/bin/env bash
# The published Apple SDK is what an integrator can use, checked on the asset
# itself rather than on the tree it was packed from.
#
# Every other Apple gate looks at platforms/apple in a checkout. An integrator
# never sees that: they download migo-<version>-apple-sdk.zip, unpack it and add
# it to Xcode as a local package. So this unpacks the asset and asks the
# integrator's questions of what came out:
#
#   1. Is it the whole SDK? The engine receipt lists the iOS device, iOS
#      simulator and macOS groups, Release, none diagnostic; the producer
#      bundle, both privacy manifests, the ANGLE helper, LICENSE and NOTICE are
#      there -- and nothing that belongs to this repository and not to an app
#      is (ProbeApp, DeviceTestHost, WebContent sources, build directories).
#   2. Does each product build from it, for its platform, and can an app sign
#      what it builds? Every resource bundle the build produces is signed ad
#      hoc, as an app's build signs it; the producer bundle once shipped in a
#      directory named `Resources`, which a flat iOS bundle cannot hold and
#      codesign refuses. (macOS host only.)
#   3. Does a game run from it? The macOS game-view gate, pointed at the
#      unpacked package: MigoGameView under a hardened runtime with allow-jit,
#      ANGLE installed by the SDK's own helper. (macOS host only.)
#
# On a host that is not a Mac only (1) runs, and says so.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ASSET="${1:?usage: test-apple-sdk-release-asset.sh <migo-VERSION-apple-sdk.zip>}"
[[ -f "$ASSET" ]] || { echo "FAIL: no asset at $ASSET" >&2; exit 1; }

fail() {
  echo "FAIL: $*" >&2
  exit 1
}

WORK="$(mktemp -d "${TMPDIR:-/tmp}/migo-apple-asset.XXXXXX")"
trap 'rm -rf "$WORK"' EXIT

# unzip keeps the Unix modes and symbolic links the packager stored.
unzip -q "$ASSET" -d "$WORK" || fail "the asset does not unpack"
PACKAGE="$WORK/MigoApple"
[[ -d "$PACKAGE" ]] || fail "the asset has no MigoApple/ root; an integrator adds that directory"

echo "[1/3] the asset is the whole SDK and only the SDK"
python3 - "$PACKAGE" <<'PY' || exit 1
import json, pathlib, sys
package = pathlib.Path(sys.argv[1])
problems = []
required = [
    "Package.swift", "LICENSE", "NOTICE", "README.md", "core/Package.swift",
    "Frameworks/MigoEngine.xcframework/Info.plist",
    "Frameworks/MigoEngine.xcframework/migo-build.json",
    "Frameworks/ANGLELibEGL-ios.xcframework/Info.plist",
    "Frameworks/ANGLELibGLESv2-ios.xcframework/Info.plist",
    "Frameworks/ANGLELibEGL-macos.xcframework/Info.plist",
    "Frameworks/ANGLELibGLESv2-macos.xcframework/Info.plist",
    "Frameworks/Scripts/embed-apple-angle.sh",
    "Frameworks/Scripts/apple-sdk-package.py",
    "Sources/MigoApplePerformancePlus/ProducerBundle/producer-page.html",
    "Sources/MigoApplePerformancePlus/ProducerBundle/engine/boot.mjs",
    "Sources/MigoApplePerformancePlus/PrivacyInfo.xcprivacy",
    "Sources/MigoMacV8/PrivacyInfo.xcprivacy",
    "Sources/MigoApplePerformancePlus/MigoGameView.swift",
    "Sources/MigoMacV8/MigoGameView.swift",
]
for relative in required:
    if not (package / relative).is_file():
        problems.append(f"missing {relative}")
for forbidden in ("ProbeApp", "DeviceTestHost", "WebContent", ".build", ".swiftpm"):
    hits = [str(p.relative_to(package)) for p in package.rglob(forbidden)]
    if hits:
        problems.append(f"carries {forbidden}, which is this repository's and not an app's: {hits[:3]}")
if not (package / "Frameworks/Scripts/embed-apple-angle.sh").stat().st_mode & 0o111:
    problems.append("Frameworks/Scripts/embed-apple-angle.sh lost its executable bit")
receipt_path = package / "Frameworks/MigoEngine.xcframework/migo-build.json"
if receipt_path.is_file():
    receipt = json.loads(receipt_path.read_text())
    groups = {entry["platform"]: entry["product"] for entry in receipt.get("slices", [])}
    expected = {"ios": "performance-plus", "ios-simulator": "performance-plus", "macos": "macos-v8"}
    if groups != expected or not receipt.get("complete") or receipt.get("diagnostic"):
        problems.append(f"the engine is not the complete shipping set: {groups}")
    if receipt.get("configuration") != "Release":
        problems.append(f"the engine is a {receipt.get('configuration')} build")
for problem in problems:
    print(f"  {problem}", file=sys.stderr)
if problems:
    sys.exit(f"FAIL: the asset is not a usable SDK ({len(problems)} problem(s))")
print("  complete: iOS device + simulator (Performance+) and macOS (V8), Release, with its resources")
PY

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "SKIP [2/3] and [3/3]: building and running need macOS"
  exit 0
fi

echo "[2/3] each product builds from the asset, for its own platform"
build_product() {
  local scheme="$1" destination="$2" log="$WORK/build-$1.log"
  (cd "$PACKAGE" && xcodebuild build -scheme "$scheme" -destination "$destination" \
      -derivedDataPath "$WORK/dd-$scheme" -skipPackagePluginValidation \
      CODE_SIGNING_ALLOWED=NO > "$log" 2>&1) \
    || { tail -40 "$log"; fail "$scheme does not build from the asset for $destination"; }
  echo "  $scheme builds for $destination"
}
build_product MigoApplePerformancePlus "generic/platform=iOS"
build_product MigoApplePerformancePlus "generic/platform=iOS Simulator"
build_product MigoMacV8 "platform=macOS"
signed=0
while IFS= read -r -d '' bundle; do
  copy="$WORK/signing/$(basename "$(dirname "$bundle")")/$(basename "$bundle")"
  mkdir -p "$(dirname "$copy")"
  cp -R "$bundle" "$copy"
  codesign --force --sign - "$copy" > "$WORK/codesign.log" 2>&1 \
    || { cat "$WORK/codesign.log"; fail "an app cannot sign $(basename "$bundle") as built for $(basename "$(dirname "$bundle")")"; }
  signed=$((signed + 1))
done < <(find "$WORK"/dd-*/Build/Products -maxdepth 2 -name '*.bundle' -type d -print0)
((signed > 0)) || fail "the builds produced no resource bundle; the producer bundle is missing"
echo "  $signed resource bundle(s) sign as an app's build signs them"

echo "[3/3] a game runs from the asset"
bash "$ROOT/scripts/test-macos-game-view.sh" --package "$PACKAGE"

echo "PASS: $(basename "$ASSET") is a complete SDK that builds for iOS and macOS and runs a game"
