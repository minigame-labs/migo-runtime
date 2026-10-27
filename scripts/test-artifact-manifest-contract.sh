#!/usr/bin/env bash
# Fast fixture contract for Android artifact identity and build wiring.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
GENERATOR="$ROOT/scripts/generate-android-artifact-manifests.py"

fail() {
  echo "artifact manifest contract: $*" >&2
  exit 1
}

[[ -f "$GENERATOR" ]] || fail "missing generator: $GENERATOR"

TMP="$(mktemp -d "${TMPDIR:-/tmp}/migo-artifact-contract.XXXXXX")"
trap 'rm -rf "$TMP"' EXIT
TOOL_TARGET="$TMP/tool-target"
CARGO_TARGET_DIR="$TOOL_TARGET" cargo build \
  --manifest-path "$ROOT/tools/artifact-manifest/Cargo.toml" \
  --locked --quiet
TOOL="$TOOL_TARGET/debug/migo-artifact-manifest"
FIXTURE_ROOT="$TMP/repo"
PACKAGE_ROOT="$TMP/package"
OUTPUT_ROOT="$PACKAGE_ROOT/assets/migo/artifacts"
BUILD_METADATA="$TMP/build-metadata.json"
FAKE_NDK="$TMP/android-ndk"
V8_SOURCE="$TMP/rusty-v8-source"
V8_LOCK="$TMP/android-v8.lock.json"

python3 - "$TMP/snapshot.bin" "$TMP/librusty_v8.a" <<'PY'
import pathlib
import sys

pathlib.Path(sys.argv[1]).write_bytes(b"s" * 100001)
pathlib.Path(sys.argv[2]).write_bytes(b"v" * 100001)
PY
MIGO_V8_ARCHIVE="$TMP/librusty_v8.a" bash "$ROOT/scripts/write-snapshot-manifest.sh" \
  full android aarch64 "$TMP/snapshot.bin" host >/dev/null
python3 - "$TMP/snapshot.bin.manifest.json" <<'PY'
import json
import pathlib
import sys

value = json.loads(pathlib.Path(sys.argv[1]).read_text())
assert value["target_triple"] == "aarch64-linux-android"
assert value["generation_cpu_policy"] == "target-baseline"
assert value["normalized_parameters"] == sorted(value["normalized_parameters"])
assert len(value["external_references_sha256"]) == 64
assert len(value["bootstrap_inputs_sha256"]) == 64
PY

python3 - "$FIXTURE_ROOT" "$FAKE_NDK" "$V8_SOURCE" <<'PY'
import hashlib
import json
import pathlib
import re
import sys

root = pathlib.Path(sys.argv[1])
ndk = pathlib.Path(sys.argv[2])
v8_source = pathlib.Path(sys.argv[3])

def digest(data):
    return hashlib.sha256(data).hexdigest()

def write(path, data):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(data)

targets = {
    "aarch64": {
        "abi": "arm64-v8a",
        "triple": "aarch64-linux-android",
        "cpu_baseline": "armv8-a",
        "required_cpu_features": ["neon"],
    },
    "x86_64": {
        "abi": "x86_64",
        "triple": "x86_64-linux-android",
        "cpu_baseline": "x86-64-v1",
        "required_cpu_features": ["cmov", "sse2"],
    },
}
v8_recipe = b"#!/usr/bin/env bash\n# fixture V8 recipe\n"
aar_recipe = b"#!/usr/bin/env bash\n# fixture AAR recipe\n"
write(root / "scripts/build-v8-android.sh", v8_recipe)
write(root / "scripts/build-aar.sh", aar_recipe)

for arch, target in targets.items():
    component_root = root / "engine/third_party/rusty_v8" / target["triple"]
    archive = f"fixture-v8-archive-{arch}".encode()
    binding = f"fixture-rust-binding-{arch}".encode()
    write(component_root / "librusty_v8.a", archive)
    write(component_root / "src_binding.rs", binding)
    component = {
        "schema": "migo-v8-component-manifest/v1",
        "component_id": "",
        "target": {
            "triple": target["triple"],
            "os": "android",
            "arch": arch,
            "abi": "android",
            "cpu_baseline": target["cpu_baseline"],
            "required_cpu_features": target["required_cpu_features"],
            "runtime_floor": {"android_api": "26"},
        },
        "toolchain": {
            "rustc": "rustc 1.95.0 fixture",
            "compiler": "Android clang 12.0.8 fixture",
            "sdk": "Android NDK 23.2.8568313; API 26 sysroot",
            "linker": "LLD 12.0.8 fixture",
        },
        "runtime": {
            "backend": "v8",
            "rusty_v8_version": "145.0.0",
            "rusty_v8_revision": "e6a88b35dd3d7f2849a0df33a71d338701c55316",
            "v8_revision": "8defb67673c5483ae56258a2de01b07e947dc921",
            "normalized_gn_args": [
                "android_ndk_api_level=26",
                "is_official_build=true",
                "use_thin_lto=false",
            ],
            "patches": [{
                "id": "0001-unset-bindgen-extra-clang-args",
                "sha256": "1" * 64,
            }],
        },
        "hashes": {
            "archive": digest(archive),
            "rust_binding": digest(binding),
        },
        "provenance": {
            "source_revision": "e6a88b35dd3d7f2849a0df33a71d338701c55316",
            "build_recipe": "scripts/build-v8-android.sh",
            "build_recipe_sha256": digest(v8_recipe),
            "licenses": ["BSD-3-Clause", "MIT"],
        },
    }
    write(
        component_root / "component-input.json",
        (json.dumps(component, indent=2, sort_keys=True) + "\n").encode(),
    )

    native_root = root / "engine/jniLibs/full" / target["abi"]
    write(native_root / "libmigo.so", f"fixture-runtime-{arch}".encode())
    write(native_root / "libc++_shared.so", f"fixture-libcxx-{arch}".encode())

    snapshot = f"fixture-snapshot-{arch}".encode()
    snapshot_path = root / "engine/crates/runtime-v8/snapshots" / f"SNAPSHOT-full-android-{arch}.bin"
    write(snapshot_path, snapshot)
    snapshot_manifest = {
        "schema_version": 3,
        "snapshot_kind": "host",
        "profile": "full",
        "arch": arch,
        "target_triple": target["triple"],
        "generation_cpu_policy": "target-baseline",
        "normalized_parameters": sorted([
            f"--arch={arch}",
            "--cpu-policy=target-baseline",
            "--product-profile=full",
            "--runtime-kind=host",
            "--warmup=none",
        ]),
        "external_references_sha256": digest(f"external-{arch}".encode()),
        "bootstrap_inputs_sha256": digest(f"bootstrap-{arch}".encode()),
        "v8_archive_sha256": digest(archive),
        "snapshot_size": len(snapshot),
        "snapshot_sha256": digest(snapshot),
    }
    write(
        pathlib.Path(str(snapshot_path) + ".manifest.json"),
        (json.dumps(snapshot_manifest, indent=2, sort_keys=True) + "\n").encode(),
    )

write(ndk / "source.properties", b"Pkg.Desc = Android NDK\nPkg.Revision = 23.2.8568313\n")
clang = ndk / "toolchains/llvm/prebuilt/linux-x86_64/bin/aarch64-linux-android26-clang++"
x64_clang = ndk / "toolchains/llvm/prebuilt/linux-x86_64/bin/x86_64-linux-android26-clang++"
linker = ndk / "toolchains/llvm/prebuilt/linux-x86_64/bin/ld.lld"
write(clang, b"#!/usr/bin/env sh\necho 'Android clang version 12.0.8 fixture'\n")
write(x64_clang, b"#!/usr/bin/env sh\necho 'Android clang version 12.0.8 fixture'\n")
write(linker, b"#!/usr/bin/env sh\necho 'LLD 12.0.8 fixture'\n")
clang.chmod(0o755)
x64_clang.chmod(0o755)
linker.chmod(0o755)
write(v8_source / "Cargo.toml", b"[package]\nname = \"v8\"\nversion = \"145.0.0\"\n")
write(v8_source / "build.rs", b"fn main() {}\n")
write(v8_source / "v8/include/v8-version.h", b"#define V8_MAJOR_VERSION 14\n")
PY

python3 "$ROOT/scripts/write-android-build-metadata.py" \
  --repo-root "$FIXTURE_ROOT" \
  --output "$BUILD_METADATA" \
  --ndk-home "$FAKE_NDK" \
  --source-revision 0123456789abcdef0123456789abcdef01234567 >/dev/null

git -C "$V8_SOURCE/v8" init -q
git -C "$V8_SOURCE/v8" add include/v8-version.h
git -C "$V8_SOURCE/v8" -c user.name=fixture -c user.email=fixture@example.invalid \
  commit -qm "fixture v8"
V8_REVISION="$(git -C "$V8_SOURCE/v8" rev-parse HEAD)"
git -C "$V8_SOURCE" init -q
git -C "$V8_SOURCE" add Cargo.toml build.rs
git -C "$V8_SOURCE" update-index --add --cacheinfo 160000 "$V8_REVISION" v8
git -C "$V8_SOURCE" -c user.name=fixture -c user.email=fixture@example.invalid \
  commit -qm "fixture rusty_v8"
RUSTY_V8_REVISION="$(git -C "$V8_SOURCE" rev-parse HEAD)"
python3 - "$V8_LOCK" "$RUSTY_V8_REVISION" "$V8_REVISION" <<'PY'
import json
import pathlib
import sys

value = {
    "schema": "migo-v8-build-lock/v1",
    "rusty_v8_version": "145.0.0",
    "rusty_v8_revision": sys.argv[2],
    "v8_revision": sys.argv[3],
    "android_api": 26,
    "targets": {
        "aarch64": {
            "triple": "aarch64-linux-android",
            "cpu_baseline": "armv8-a",
            "required_cpu_features": ["neon"],
        },
        "x86_64": {
            "triple": "x86_64-linux-android",
            "cpu_baseline": "x86-64-v1",
            "required_cpu_features": ["cmov", "sse2"],
        },
    },
    "required_patches": [
        {
            "id": "fixture-source",
            "file": "fixture-source.patch",
        },
    ],
}
pathlib.Path(sys.argv[1]).write_text(json.dumps(value, indent=2) + "\n")
PY
# The writer now proves the source tree is HEAD plus exactly the declared patches,
# so the fixture has to actually carry a patch. It declares its own one-line patch
# against build.rs rather than the real V8 patches: those need their real target
# files at exact revisions, which a fast fixture cannot stage, and what is under
# test here is the proof mechanics rather than V8's patch contexts.
mkdir -p "$FIXTURE_ROOT/engine/third_party/v8-patches"
cat > "$FIXTURE_ROOT/engine/third_party/v8-patches/fixture-source.patch" <<'PATCH'
--- a/build.rs
+++ b/build.rs
@@ -1 +1,2 @@
 fn main() {}
+// fixture patch
PATCH
patch -p1 -d "$V8_SOURCE" --batch --forward --fuzz=0 \
  < "$FIXTURE_ROOT/engine/third_party/v8-patches/fixture-source.patch" >/dev/null \
  || fail "could not apply the fixture source patch"

printf '%s\n' "untracked source input" > "$V8_SOURCE/untracked-provenance.txt"
component_root="$FIXTURE_ROOT/engine/third_party/rusty_v8/aarch64-linux-android"
if python3 "$ROOT/scripts/write-v8-component-manifest.py" \
    --repo-root "$FIXTURE_ROOT" \
    --rusty-v8-src "$V8_SOURCE" \
    --ndk-home "$FAKE_NDK" \
    --arch aarch64 \
    --extra-gn-args "android_ndk_api_level=26 android_ndk_root=\"$FAKE_NDK\" is_official_build=true use_thin_lto=false" \
    --archive "$component_root/librusty_v8.a" \
    --binding "$component_root/src_binding.rs" \
    --output "$component_root/untrusted-component-manifest.json" \
    --tool "$TOOL" \
    --lock "$V8_LOCK" >"$TMP/untracked.out" 2>"$TMP/untracked.err"; then
  fail "V8 component writer accepted an untracked source input"
fi
grep -F "undeclared change" "$TMP/untracked.err" >/dev/null || \
  fail "untracked-source error did not say the change was undeclared"
grep -F "untracked-provenance.txt" "$TMP/untracked.err" >/dev/null || \
  fail "untracked-source error did not name the offending path"
rm -f "$V8_SOURCE/untracked-provenance.txt"

for arch in aarch64 x86_64; do
  case "$arch" in
    aarch64) v8_dir="aarch64-linux-android" ;;
    x86_64)  v8_dir="x86_64-linux-android" ;;
  esac
  component_root="$FIXTURE_ROOT/engine/third_party/rusty_v8/$v8_dir"
  python3 "$ROOT/scripts/write-v8-component-manifest.py" \
    --repo-root "$FIXTURE_ROOT" \
    --rusty-v8-src "$V8_SOURCE" \
    --ndk-home "$FAKE_NDK" \
    --arch "$arch" \
    --extra-gn-args "android_ndk_api_level=26 android_ndk_root=\"$FAKE_NDK\" is_official_build=true use_thin_lto=false" \
    --archive "$component_root/librusty_v8.a" \
    --binding "$component_root/src_binding.rs" \
    --output "$component_root/component-manifest.json" \
    --tool "$TOOL" \
    --lock "$V8_LOCK" >/dev/null
done

mv "$FIXTURE_ROOT/engine/third_party/rusty_v8/aarch64-linux-android/component-manifest.json" \
  "$FIXTURE_ROOT/engine/third_party/rusty_v8/aarch64-linux-android/component-manifest.saved"
if python3 "$GENERATOR" \
    --repo-root "$FIXTURE_ROOT" \
    --tool "$TOOL" \
    --output-root "$OUTPUT_ROOT" \
    --build-metadata "$BUILD_METADATA" \
    --product-profile full \
    --build-type release \
    --codegen-profile z \
    --arch arm64-v8a --arch x86_64 \
    >"$TMP/missing.out" 2>"$TMP/missing.err"; then
  fail "generator accepted a missing V8 component manifest"
fi
grep -F "component-manifest.json" "$TMP/missing.err" >/dev/null || \
  fail "missing-component error was not actionable"
mv "$FIXTURE_ROOT/engine/third_party/rusty_v8/aarch64-linux-android/component-manifest.saved" \
  "$FIXTURE_ROOT/engine/third_party/rusty_v8/aarch64-linux-android/component-manifest.json"

python3 "$GENERATOR" \
  --repo-root "$FIXTURE_ROOT" \
  --tool "$TOOL" \
  --output-root "$OUTPUT_ROOT" \
  --build-metadata "$BUILD_METADATA" \
  --product-profile full \
  --build-type release \
  --codegen-profile z \
  --arch arm64-v8a --arch x86_64

[[ -f "$OUTPUT_ROOT/slices/arm64-v8a.json" ]] || fail "missing arm64 slice manifest"
[[ -f "$OUTPUT_ROOT/slices/x86_64.json" ]] || fail "missing x86_64 slice manifest"
[[ -f "$OUTPUT_ROOT/package-index.json" ]] || fail "missing package index"
"$TOOL" verify-index "$OUTPUT_ROOT/package-index.json" "$PACKAGE_ROOT" >/dev/null

python3 - \
  "$PACKAGE_ROOT" \
  "$FIXTURE_ROOT" \
  "$TMP/migo-missing-jni.aar" \
  "$TMP/migo-full-release.aar" \
  "$TMP/migo-unindexed-jni.aar" <<'PY'
import pathlib
import sys
import zipfile

package_root = pathlib.Path(sys.argv[1])
fixture_root = pathlib.Path(sys.argv[2])

def add_package_assets(archive):
    for path in sorted(package_root.rglob("*")):
        if path.is_file():
            archive.write(path, path.relative_to(package_root).as_posix())

with zipfile.ZipFile(sys.argv[3], "w", compression=zipfile.ZIP_STORED) as archive:
    add_package_assets(archive)

with zipfile.ZipFile(sys.argv[4], "w", compression=zipfile.ZIP_STORED) as archive:
    add_package_assets(archive)
    for abi in ("arm64-v8a", "x86_64"):
        native_root = fixture_root / "engine/jniLibs/full" / abi
        for library in ("libmigo.so", "libc++_shared.so"):
            archive.write(native_root / library, f"jni/{abi}/{library}")

with zipfile.ZipFile(sys.argv[5], "w", compression=zipfile.ZIP_STORED) as archive:
    add_package_assets(archive)
    for abi in ("arm64-v8a", "x86_64"):
        native_root = fixture_root / "engine/jniLibs/full" / abi
        for library in ("libmigo.so", "libc++_shared.so"):
            archive.write(native_root / library, f"jni/{abi}/{library}")
    archive.writestr("jni/armeabi-v7a/libmigo.so", b"unindexed-runtime")
    archive.writestr("jni/armeabi-v7a/libc++_shared.so", b"unindexed-cxx")
PY
if python3 "$ROOT/scripts/verify-android-aar-manifests.py" \
    --aar "$TMP/migo-missing-jni.aar" \
    --index "$OUTPUT_ROOT/package-index.json" \
    --tool "$TOOL" >"$TMP/missing-jni.out" 2>"$TMP/missing-jni.err"; then
  fail "AAR verifier accepted a package without the claimed JNI binaries"
fi
grep -F "jni/arm64-v8a/libmigo.so" "$TMP/missing-jni.err" >/dev/null || \
  fail "missing-JNI error was not actionable"
if python3 "$ROOT/scripts/verify-android-aar-manifests.py" \
    --aar "$TMP/migo-unindexed-jni.aar" \
    --index "$OUTPUT_ROOT/package-index.json" \
    --tool "$TOOL" >"$TMP/unindexed-jni.out" 2>"$TMP/unindexed-jni.err"; then
  fail "AAR verifier accepted JNI binaries without a slice identity"
fi
grep -F "unindexed Migo JNI entries" "$TMP/unindexed-jni.err" >/dev/null || \
  fail "unindexed-JNI error was not actionable"
python3 "$ROOT/scripts/verify-android-aar-manifests.py" \
  --aar "$TMP/migo-full-release.aar" \
  --index "$OUTPUT_ROOT/package-index.json" \
  --tool "$TOOL" >/dev/null
"$TOOL" attest \
  "$TMP/migo-full-release.aar" \
  "$OUTPUT_ROOT/package-index.json" \
  "$TMP/migo-full-release.aar.attestation.json" >/dev/null
"$TOOL" verify-attestation \
  "$TMP/migo-full-release.aar.attestation.json" \
  "$TMP/migo-full-release.aar" \
  "$OUTPUT_ROOT/package-index.json" >/dev/null

python3 - "$OUTPUT_ROOT" "$TMP/migo-full-release.aar.attestation.json" <<'PY'
import json
import pathlib
import re
import sys

root = pathlib.Path(sys.argv[1])
attestation = json.loads(pathlib.Path(sys.argv[2]).read_text())
expected = {
    "arm64-v8a": ("aarch64", "armv8-a", ["neon"]),
    "x86_64": ("x86_64", "x86-64-v1", ["cmov", "sse2"]),
}
for abi, (arch, baseline, features) in expected.items():
    raw = (root / "slices" / f"{abi}.json").read_text()
    value = json.loads(raw)
    assert value["target"]["arch"] == arch
    assert value["target"]["cpu_baseline"] == baseline
    assert value["target"]["required_cpu_features"] == features
    assert value["target"]["runtime_floor"] == {"android_api": "26"}
    assert re.fullmatch(r"[0-9a-fA-F]{40}", value["runtime"]["v8_revision"])
    assert attestation["package_sha256"] not in raw
PY

grep -F "generate-android-artifact-manifests.py" "$ROOT/scripts/build-aar.sh" >/dev/null || \
  fail "build-aar.sh does not stage verified manifests"
grep -F "android-v8.lock.json" "$ROOT/scripts/build-v8-android.sh" >/dev/null || \
  fail "build-v8-android.sh does not enforce the pinned source lock"
grep -F "component-manifest.json" "$ROOT/scripts/build-v8-android.sh" >/dev/null || \
  fail "build-v8-android.sh does not emit a component manifest"
grep -F "generated/migoArtifactManifest/assets" "$ROOT/platforms/android/library/build.gradle" >/dev/null || \
  fail "Gradle does not package generated manifest assets"
grep -F "verifyMigoReleaseArtifactPackaging" "$ROOT/platforms/android/library/build.gradle" >/dev/null || \
  fail "Gradle release tasks do not require verified manifest staging"
grep -F "migoVerifiedReleasePackaging" "$ROOT/platforms/android/library/build.gradle" >/dev/null || \
  fail "Gradle has no explicit verified release packaging gate"
grep -F 'task.name == "bundle${variantName}ReleaseAar"' "$ROOT/platforms/android/library/build.gradle" >/dev/null || \
  fail "Gradle release AAR bundle does not depend on the manifest gate"
grep -F "'verify-index'" "$ROOT/platforms/android/library/build.gradle" >/dev/null || \
  fail "Gradle does not run the canonical package-index verifier"
grep -F "identity.hashes?.runtime_binary" "$ROOT/platforms/android/library/build.gradle" >/dev/null || \
  fail "Gradle does not compare release JNI inputs with slice identities"
grep -F -- "-PmigoVerifiedReleasePackaging=true" "$ROOT/scripts/build-aar.sh" >/dev/null || \
  fail "build-aar.sh does not authorize the verified Gradle release path"
grep -F -- "-PmigoArtifactManifestTool=" "$ROOT/scripts/build-aar.sh" >/dev/null || \
  fail "build-aar.sh does not give Gradle the canonical manifest verifier"
grep -F "test-artifact-manifest-contract.sh" "$ROOT/.github/workflows/pr-ci.yml" >/dev/null || \
  fail "PR CI does not run the artifact manifest contract"
grep -F "test-artifact-manifest-contract.sh" "$ROOT/.github/workflows/release.yml" >/dev/null || \
  fail "release CI does not run the artifact manifest contract"
grep -F "test-android-release-manifest-gate.sh" "$ROOT/.github/workflows/pr-ci.yml" >/dev/null || \
  fail "Android PR CI does not exercise the Gradle release gate graph"
grep -F "test-android-release-manifest-gate.sh" "$ROOT/.github/workflows/release.yml" >/dev/null || \
  fail "Android release CI does not exercise the Gradle release gate graph"

# The committed JSON schema and the Rust validator are two statements of "which V8
# component targets exist", and the unenforced one had already drifted: the schema's
# target `oneOf` named Android and Linux while the validator had been accepting Windows,
# so a Windows manifest was valid to the tool and invalid to the document meant to
# describe it. Adding OpenHarmony is what surfaced that. Equated here rather than checked
# with a JSON-schema library, because the drift is *between the two statements* and this
# needs no dependency the CI image does not already have.
python3 - "$ROOT" <<'PY' || fail "the component schema and the Rust validator disagree on which targets exist"
import json, pathlib, re, sys

root = pathlib.Path(sys.argv[1])
contracts = root / "contracts/artifact-manifest"
component = json.loads((contracts / "v8-component-schema-v1.json").read_text())
defs = json.loads((contracts / "schema-v1.json").read_text())["$defs"]

refs = [entry["$ref"].rsplit("/", 1)[-1] for entry in component["properties"]["target"]["oneOf"]]
if not refs:
    sys.exit("the component schema's target oneOf is empty")
schema_pairs = set()
for ref in refs:
    definition = defs[ref]
    for variant in definition.get("oneOf", [definition]):
        properties = variant["properties"]
        schema_pairs.add((properties["os"]["const"], properties["abi"]["const"]))

source = (root / "tools/artifact-manifest/src/lib.rs").read_text()
body = re.search(r"fn validate_v8_component_target.*?\n}\n", source, re.DOTALL)
if body is None:
    sys.exit("cannot find validate_v8_component_target in lib.rs")
rust_pairs = set(re.findall(r'\("([a-z0-9_]+)", "([a-z0-9_]+)"\) =>', body.group(0)))
if not rust_pairs:
    sys.exit("extracted no target arms from validate_v8_component_target")

if schema_pairs != rust_pairs:
    sys.exit(
        f"schema-only targets: {sorted(schema_pairs - rust_pairs)}; "
        f"validator-only targets: {sorted(rust_pairs - schema_pairs)}"
    )
print(f"component schema and validator agree on {len(rust_pairs)} target(s)")
PY

# Every consumer of a V8 archive must take it from the verified, content-addressed
# materialisation rather than from wherever the file happens to sit. Enumerated rather
# than listed, the way the NDK pin is: the defect this closes is that
# `build-android-so.sh` -- the script that produces the AAR's native library -- selected
# its archive with an existence test while the SDK scripts beside it verified theirs, so
# a script added later must not be able to reintroduce that quietly.
python3 - "$ROOT" <<'PY' || fail "a shipping build consumes a V8 archive without materialising it first"
import pathlib, re, sys

root = pathlib.Path(sys.argv[1])
scripts = root / "scripts"
if not (scripts / "lib/v8-materialise.sh").is_file():
    sys.exit("missing scripts/lib/v8-materialise.sh")

# Not "every script that mentions the variable": that predicate is too strong and would
# fail correct code, which is the mistake the NDK pin gate already made and corrected. A
# developer archive is explicitly allowed for a local executable -- it just cannot make a
# provenance claim, so it must not reach a package. The discriminator is therefore what a
# script *produces*, and each non-producer is exempted by name with a reason. A stale
# exemption is an error, because one that no longer applies grants more than it needs to.
EXEMPT = {
    "dev-run-c-host.sh": "runs a local C host; may use a development archive",
    "dev-run-qt-host.sh": "runs the local Qt host; may use a development archive",
    "dev-run-player.sh": "runs the local player; may use a development archive",
    "dev-test-host.sh": "runs host suites against the local linux-gnu archive",
    "test-capi-platform-contract.sh": "a contract check, not a producer",
    "build-snapshot.ps1": (
        "runs migo-snapshot-gen on a Windows *host* solely to produce .bin snapshot files, "
        "not to link or ship a component that embeds the archive -- unlike "
        "build-windows-sdk.sh/build-windows-sdk-native.sh it has no packaging counterpart "
        "to hold it to the materialised-path bar, and there is no PowerShell port of "
        "v8_materialise_windows (scripts/lib/v8-materialise.sh) yet. It also clears the "
        "variable first so an Android archive cannot leak into a host build."
    ),
}

# Both spellings, because the first version of this enumeration globbed `*.sh` and matched
# `RUSTY_V8_ARCHIVE="` -- so it could not see `build-android-so.ps1`, which set the variable
# from a `Test-Path` check on the path that produces the AAR's native library, nor
# `build-windows-sdk.sh`, which writes `set RUSTY_V8_ARCHIVE=` into a batch heredoc. That is
# a *scope* hole rather than a weak assertion, and it is the second time one has been found
# here: the NDK pin gate had the identical blind spot for `.ps1`.
# Assignments only. A pattern loose enough to match a *read* also matches
# `Print-Info "RUSTY_V8_ARCHIVE = ..."` and a save/restore of the variable, which reports
# correct code as an offender -- a check that fails on correct code is worse than none.
SETTERS = [
    re.compile(r'(?m)^\s*(?:export\s+)?RUSTY_V8_ARCHIVE="([^"]*)"'),   # bash
    re.compile(r'(?m)^\s*\$env:RUSTY_V8_ARCHIVE\s*=\s*(\S+)'),          # PowerShell
    re.compile(r'(?m)^\s*set\s+RUSTY_V8_ARCHIVE=(\S+)'),               # batch heredoc
]
MATERIALISED = ("$V8_MATERIALISED_ARCHIVE", "$v8.Archive")

consumers = {}
for path in sorted(list(scripts.glob("*.sh")) + list(scripts.glob("*.ps1"))):
    # This contract is the one file allowed to name the variable freely -- it has to quote
    # it to search for it -- the same exemption the NDK pin gate makes for itself.
    if path.name == "test-artifact-manifest-contract.sh":
        continue
    text = path.read_text(encoding="utf-8")
    values = [value.strip() for pattern in SETTERS for value in pattern.findall(text)
              if value.strip()]
    if values:
        consumers[path.name] = (values, text)
if not consumers:
    sys.exit("found no script setting RUSTY_V8_ARCHIVE -- the enumeration is broken")

stale = sorted(name for name in EXEMPT if name not in consumers)
if stale:
    sys.exit(f"exemptions that no longer consume a V8 archive: {stale}")

def _unbrace(value: str) -> str:
    # `${VAR}` and `$VAR` name the same thing; the batch-heredoc setters in this
    # repo consistently brace their interpolations (see build-windows-sdk.sh),
    # so the prefix check below must not care which form a script used.
    if value.startswith("${") and value.endswith("}"):
        return "$" + value[2:-1]
    return value

offenders = []
for name in sorted(set(consumers) - set(EXEMPT)):
    values, text = consumers[name]
    # A prefix match, not equality: Windows consumers cannot assign the bare
    # materialised value -- it is a POSIX path, and RUSTY_V8_ARCHIVE there is read
    # by a native MSVC toolchain that has no notion of one, so it must be a
    # derivative (`$V8_MATERIALISED_ARCHIVE_DOS`, a DOS-path conversion computed on
    # its own line and assigned with no other text, so this prefix match cannot
    # also accept an unrelated variable that merely happens to start the same way).
    if any(not _unbrace(value).startswith(MATERIALISED) for value in values):
        offenders.append(f"{name} -> {values}")
    elif "v8_materialise" not in text and "Resolve-MigoMaterialisedV8" not in text:
        offenders.append(f"{name} uses the materialised path but never materialises")

if offenders:
    sys.exit("; ".join(offenders))
print(
    f"{len(set(consumers) - set(EXEMPT))} shipping V8 consumer(s) materialise first, "
    f"{len(EXEMPT)} exempted by name"
)
PY

echo "artifact manifest contract: ok"
