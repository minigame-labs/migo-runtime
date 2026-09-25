#!/usr/bin/env bash
# Package the assembled Apple SDK as the release asset
# `migo-<version>-apple-sdk.zip`: a SwiftPM package an app adds as a local
# package, with the engine, ANGLE and the WebContent producer already in it.
#
# Why a zip of the package rather than a remote SwiftPM URL. A remote package is
# a git repository whose root is Package.swift, with binary targets named by URL
# and checksum; this repository's package lives in platforms/apple and its
# binaries are built after the tag, so a manifest carrying their checksums would
# have to be committed after the release it describes. The Linux and Windows
# SDKs ship the same way -- an archive of what an integrator unpacks -- and a
# remote SwiftPM distribution can be layered on this asset later without
# changing what is in it.
#
# What goes in is what Package.swift names, and nothing it does not:
#
#   Package.swift, core/, Sources/ (with the generated producer bundle),
#   Tests/ (declared as test targets, so SwiftPM refuses the package without
#   them), Frameworks/ (MigoEngine.xcframework, the ANGLE xcframeworks and the
#   consumer helpers), README.md, and the repository's LICENSE and NOTICE.
#
# Not ProbeApp/, DeviceTestHost/ or WebContent/: the first two are Xcode
# projects for this repository's own measurements, and WebContent/ is the
# source the producer bundle in Sources/ is generated from.
#
# Reproducible: entries sorted, every timestamp SOURCE_DATE_EPOCH (default:
# HEAD's commit time), permissions reduced to 0644/0755, no owner, no extra
# fields -- two packagings of one build are byte-identical. Symbolic links are
# stored as links, because a framework bundle that has them means them.
#
# The assembled package must be complete: the receipt has to list the iOS
# device, iOS simulator and macOS groups, none of them diagnostic. A release
# that shipped half of that would be an SDK whose README describes a platform
# it cannot build for.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PACKAGE="$ROOT/platforms/apple"
OUTPUT_DIR=""

fail() {
  echo "package-apple-sdk: $*" >&2
  exit 1
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --output-dir) OUTPUT_DIR="${2:?--output-dir needs a directory}"; shift 2 ;;
    --package) PACKAGE="${2:?--package needs a directory}"; shift 2 ;;
    -h|--help)
      echo "usage: package-apple-sdk.sh --output-dir DIR [--package platforms/apple]"
      exit 0 ;;
    *) fail "unknown argument: $1" ;;
  esac
done
[[ -n "$OUTPUT_DIR" ]] || fail "--output-dir is required"

VERSION="$(tr -d '[:space:]' < "$ROOT/release/VERSION")"
EPOCH="${SOURCE_DATE_EPOCH:-$(git -C "$ROOT" log -1 --format=%ct)}"
[[ "$EPOCH" =~ ^[0-9]+$ ]] || fail "SOURCE_DATE_EPOCH must be non-negative Unix seconds, got: $EPOCH"
mkdir -p "$OUTPUT_DIR"
ASSET="$(cd "$OUTPUT_DIR" && pwd)/migo-$VERSION-apple-sdk.zip"

python3 - "$ROOT" "$PACKAGE" "$ASSET" "$EPOCH" <<'PY'
import json
import os
import pathlib
import stat
import sys
import time
import zipfile

root, package, asset, epoch = (pathlib.Path(sys.argv[1]), pathlib.Path(sys.argv[2]),
                               pathlib.Path(sys.argv[3]), int(sys.argv[4]))

def die(message):
    print(f"package-apple-sdk: {message}", file=sys.stderr)
    sys.exit(1)

receipt_path = package / "Frameworks/MigoEngine.xcframework/migo-build.json"
if not receipt_path.is_file():
    die(f"no assembled engine at {receipt_path.parent}; run scripts/build-apple-sdk.sh first")
receipt = json.loads(receipt_path.read_text())
groups = {entry["platform"]: entry["product"] for entry in receipt.get("slices", [])}
expected = {"ios": "performance-plus", "ios-simulator": "performance-plus", "macos": "macos-v8"}
if receipt.get("diagnostic") or not receipt.get("complete") or groups != expected:
    die(f"the assembled engine is not the complete shipping set: diagnostic={receipt.get('diagnostic')}, "
        f"groups={groups}; expected {expected}")
if receipt.get("configuration") != "Release":
    die(f"the assembled engine is a {receipt.get('configuration')} build; a release ships Release")
if not (package / "Sources/MigoApplePerformancePlus/Resources/producer-page.html").is_file():
    die("the WebContent producer bundle is not in Sources/MigoApplePerformancePlus/Resources")

INCLUDE = ["Package.swift", "README.md", "core", "Sources", "Tests", "Frameworks"]
SKIP_DIRS = {".build", ".swiftpm", "xcuserdata"}
PREFIX = "MigoApple"
DATE = time.gmtime(max(epoch, 315532800))[:6]  # zip cannot say anything before 1980

entries = []
for name in INCLUDE:
    source = package / name
    if not source.exists():
        die(f"the package has no {name}")
    if source.is_file():
        entries.append((f"{PREFIX}/{name}", source))
        continue
    for current, directories, files in os.walk(source, followlinks=False):
        directories[:] = sorted(d for d in directories if d not in SKIP_DIRS)
        base = pathlib.Path(current)
        for directory in list(directories):
            if (base / directory).is_symlink():
                entries.append((f"{PREFIX}/{(base / directory).relative_to(package)}", base / directory))
                directories.remove(directory)
        for file in sorted(files):
            if file == ".DS_Store":
                continue
            path = base / file
            entries.append((f"{PREFIX}/{path.relative_to(package)}", path))
for legal in ("LICENSE", "NOTICE"):
    entries.append((f"{PREFIX}/{legal}", root / legal))
entries.sort(key=lambda item: item[0])

temporary = asset.with_suffix(".zip.partial")
with zipfile.ZipFile(temporary, "w", compression=zipfile.ZIP_DEFLATED, compresslevel=9) as archive:
    for arcname, path in entries:
        info = zipfile.ZipInfo(arcname, date_time=DATE)
        info.create_system = 3  # Unix, so the mode below is read
        if path.is_symlink():
            info.external_attr = (stat.S_IFLNK | 0o777) << 16
            info.compress_type = zipfile.ZIP_STORED
            archive.writestr(info, os.readlink(path))
            continue
        executable = os.access(path, os.X_OK)
        info.external_attr = (stat.S_IFREG | (0o755 if executable else 0o644)) << 16
        info.compress_type = zipfile.ZIP_DEFLATED
        with path.open("rb") as handle:
            archive.writestr(info, handle.read(), compresslevel=9)
temporary.replace(asset)
print(f"package-apple-sdk: {asset.name}: {len(entries)} entries, "
      f"{asset.stat().st_size / 1048576:.1f} MiB (SOURCE_DATE_EPOCH={epoch})")
PY
