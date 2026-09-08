"""Packaging orchestration with temporary fixtures, never pretend Apple binaries.

The only mocked boundary is the unavailable Apple/Rust build toolchain. Real
scripts assemble copies and metadata; these tests make no Mach-O/link claims.
"""
from __future__ import annotations

import json
import importlib.util
import os
from pathlib import Path
import plistlib
import shutil
import subprocess
import tempfile
import unittest
from unittest import mock

ROOT = Path(__file__).resolve().parents[3]

TOOL = r'''#!/usr/bin/env python3
import json, os, pathlib, plistlib, shutil, sys
tool = pathlib.Path(sys.argv[0]).name
args = sys.argv[1:]
if tool == "uname":
    print("Darwin")
elif tool == "rustup":
    print("aarch64-apple-ios\naarch64-apple-ios-sim\nx86_64-apple-ios\naarch64-apple-darwin\nx86_64-apple-darwin")
elif tool == "cargo":
    if args[0] == "metadata":
        print(json.dumps({"packages": [{"name": "migo-capi", "targets": [{"kind": ["staticlib"], "name": "migo_capi"}]}]}))
    else:
        target = args[args.index("--target") + 1]
        profile = "release" if "--release" in args else "debug"
        dest = pathlib.Path("target") / target / profile / "libmigo_capi.a"
        dest.parent.mkdir(parents=True, exist_ok=True)
        dest.write_text("fixture archive " + target + " " + " ".join(args))
elif tool == "lipo":
    output = pathlib.Path(args[args.index("-output") + 1])
    output.write_bytes(b"\n".join(pathlib.Path(p).read_bytes() for p in args[1:args.index("-output")]))
elif tool == "xcodebuild":
    output = pathlib.Path(args[args.index("-output") + 1])
    if os.environ.get("FAIL_ENGINE_ASSEMBLY") and output.name == "MigoEngine.xcframework":
        print("injected engine assembly failure", file=sys.stderr)
        sys.exit(23)
    output.mkdir(parents=True)
    libraries = []
    for i, arg in enumerate(args):
        if arg not in ("-library", "-framework"):
            continue
        source = pathlib.Path(args[i+1])
        platform = next((p for p in ("ios-simulator", "macos", "ios") if p in str(source)), None)
        assert platform, source
        ident = {"ios": "ios-arm64", "ios-simulator": "ios-arm64_x86_64-simulator", "macos": "macos-arm64_x86_64"}[platform]
        dest = output / ident
        dest.mkdir()
        if source.is_dir(): shutil.copytree(source, dest/source.name)
        else: shutil.copy2(source, dest/source.name)
        entry = {"LibraryIdentifier": ident, "LibraryPath": source.name, "SupportedPlatform": "macos" if platform == "macos" else "ios", "SupportedArchitectures": ["arm64"] if platform == "ios" else ["arm64", "x86_64"]}
        if platform == "ios-simulator": entry["SupportedPlatformVariant"] = "simulator"
        if i+2 < len(args) and args[i+2] == "-headers":
            shutil.copytree(args[i+3], dest/"Headers")
            entry["HeadersPath"] = "Headers"
        libraries.append(entry)
    (output/"Info.plist").write_bytes(plistlib.dumps({"AvailableLibraries": libraries, "XCFrameworkFormatVersion": "1.0"}))
else:
    raise SystemExit("unexpected tool: " + tool)
'''


class SDKPackaging(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="migo-packaging-fixture-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        for path in ("contracts/apple", "include/migo", "platforms/apple/core", "platforms/apple/Sources", "platforms/apple/Tests", "platforms/apple/WebContent"):
            shutil.copytree(ROOT/path, self.root/path)
        (self.root/"scripts").mkdir()
        for name in ("build-apple-sdk.sh", "build-angle-apple.sh", "apple-sdk-package.py", "embed-apple-angle.sh", "test-apple-shipping-package-contract.sh"):
            if (ROOT/"scripts"/name).exists():
                shutil.copy2(ROOT/"scripts"/name, self.root/"scripts"/name)
        shutil.copy2(ROOT/"platforms/apple/Package.swift", self.root/"platforms/apple/Package.swift")
        (self.root/"contracts/artifact-manifest").mkdir()
        shutil.copy2(ROOT/"contracts/artifact-manifest/apple-angle.lock.json", self.root/"contracts/artifact-manifest/apple-angle.lock.json")
        (self.root/"engine").mkdir()
        for platform in ("ios", "ios-simulator", "macos"):
            angle = self.root/"engine/third_party"/f"angle-apple-{platform}"
            angle.mkdir(parents=True)
            for name in ("libEGL", "libGLESv2"):
                if platform == "macos":
                    (angle/f"{name}.dylib").write_text(f"fixture {platform} {name}")
                else:
                    bundle = angle/f"{name}.framework"
                    bundle.mkdir()
                    (bundle/name).write_text(f"fixture {platform} {name}")
                    (bundle/"Info.plist").write_bytes(plistlib.dumps({"CFBundleExecutable": name}))
        tools = self.root/"tools"
        tools.mkdir()
        for tool in ("uname", "rustup", "cargo", "lipo", "xcodebuild"):
            (tools/tool).write_text(TOOL)
            (tools/tool).chmod(0o755)
        self.env = dict(os.environ, PATH=f"{tools}:{os.environ['PATH']}", MIGO_APPLE_BUILD_ROOT=str(self.root/"build"))

    def build(self, platform, product=None, configuration="Debug", success=True, **env):
        args = ["bash", "scripts/build-apple-sdk.sh", "--platform", platform, "--configuration", configuration]
        if product:
            args += ["--product", product]
        result = subprocess.run(args, cwd=self.root, env=dict(self.env, **env), text=True, capture_output=True)
        if success:
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        else:
            self.assertNotEqual(result.returncode, 0, "unexpected success: " + result.stdout)
        return result

    @property
    def framework(self):
        return self.root/"platforms/apple/Frameworks/MigoEngine.xcframework"

    def slices(self):
        return plistlib.loads((self.framework/"Info.plist").read_bytes())["AvailableLibraries"]

    def test_sequential_builds_preserve_device_simulator_and_native_macos(self):
        for platform in ("ios", "ios-simulator", "macos"):
            self.build(platform)
        self.assertEqual({s["LibraryIdentifier"] for s in self.slices()}, {"ios-arm64", "ios-arm64_x86_64-simulator", "macos-arm64_x86_64"})
        receipt = json.loads((self.framework/"migo-build.json").read_text())
        self.assertEqual({s["platform"]: s["product"] for s in receipt["slices"]}, {"ios": "performance-plus", "ios-simulator": "performance-plus", "macos": "macos-v8"})
        for entry in self.slices():
            archive = (self.framework/entry["LibraryIdentifier"]/entry["LibraryPath"]).read_text()
            external_flags = "--no-default-features --features external-frames"
            if entry["SupportedPlatform"] == "ios":
                self.assertIn(external_flags, archive)
            else:
                self.assertNotIn(external_flags, archive)
        # The consumer receives an executable helper closure, not just a
        # producer-repository path that disappears when artifacts are copied.
        helper = self.framework.parent/"Scripts/embed-apple-angle.sh"
        result = subprocess.run(["bash", str(helper), "--frameworks-dir", str(self.framework.parent), "--destination", str(self.root/"Host.app/Contents/Frameworks"), "--architectures", "arm64 x86_64"], text=True, capture_output=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue((self.root/"Host.app/Contents/Frameworks/libGLESv2.dylib").is_file())

    def test_macos_performance_plus_cannot_replace_shipping_engine(self):
        self.build("macos")
        before = (self.framework/"Info.plist").read_bytes()
        result = self.build("macos", "performance-plus", success=False)
        self.assertIn("external-frames-diagnostic", result.stderr)
        self.assertEqual((self.framework/"Info.plist").read_bytes(), before)

    def test_diagnostic_macos_package_is_isolated(self):
        self.build("ios")
        before = (self.framework/"Info.plist").read_bytes()
        self.build("macos", "external-frames-diagnostic")
        package = self.root/"build/diagnostics/Debug/package"
        receipt = json.loads((package/"Frameworks/MigoEngine.xcframework/migo-build.json").read_text())
        self.assertTrue(receipt["diagnostic"])
        self.assertEqual(receipt["slices"][0]["product"], "external-frames-diagnostic")
        self.assertTrue((package/"Package.swift").is_file())
        self.assertEqual((self.framework/"Info.plist").read_bytes(), before)

    def test_failed_assembly_keeps_previous_artifact(self):
        self.build("ios")
        before = (self.framework/"Info.plist").read_bytes()
        self.build("ios-simulator", success=False, FAIL_ENGINE_ASSEMBLY="1")
        self.assertEqual((self.framework/"Info.plist").read_bytes(), before)

    def installed_package_bytes(self):
        package = self.root/"platforms/apple"
        generated = (package/"Frameworks", package/"Sources/MigoApplePerformancePlus/Resources")
        return {str(path.relative_to(package)): path.read_bytes()
                for directory in generated for path in directory.rglob("*") if path.is_file()}

    def test_missing_producer_preserves_previous_complete_sdk(self):
        self.build("ios")
        before = self.installed_package_bytes()
        shutil.rmtree(self.root/"platforms/apple/WebContent/PerformancePlus/src")
        result = self.build("ios-simulator", success=False)
        self.assertIn("src", result.stderr)
        self.assertEqual(self.installed_package_bytes(), before)

    def test_missing_embed_helper_preserves_previous_complete_sdk(self):
        self.build("ios")
        before = self.installed_package_bytes()
        (self.root/"scripts/embed-apple-angle.sh").unlink()
        result = self.build("ios-simulator", success=False)
        self.assertIn("embed-apple-angle.sh", result.stderr)
        self.assertEqual(self.installed_package_bytes(), before)

    def test_failed_diagnostic_rebuild_preserves_package_sources(self):
        self.build("macos", "external-frames-diagnostic")
        package = self.root/"build/diagnostics/Debug/package"
        def snapshot():
            return {str(path.relative_to(package)): path.read_bytes()
                    for path in package.rglob("*") if path.is_file()}
        before = snapshot()
        (self.root/"platforms/apple/Sources/MigoMacV8/Placeholder.swift").write_text("// changed source\n")
        shutil.rmtree(self.root/"platforms/apple/WebContent/PerformancePlus/src")
        self.build("macos", "external-frames-diagnostic", success=False)
        self.assertEqual(snapshot(), before)

    def test_configurations_do_not_mix(self):
        self.build("ios")
        self.build("ios-simulator", configuration="Release")
        self.assertEqual([s["LibraryIdentifier"] for s in self.slices()], ["ios-arm64_x86_64-simulator"])

    def test_missing_angle_pair_fails_before_replacing_engine(self):
        self.build("ios")
        before = (self.framework/"Info.plist").read_bytes()
        (self.root/"engine/third_party/angle-apple-macos/libGLESv2.dylib").unlink()
        result = self.build("macos", success=False)
        self.assertIn("libGLESv2", result.stderr)
        self.assertEqual((self.framework/"Info.plist").read_bytes(), before)

    def test_ios_angle_frameworks_have_importable_module_identity(self):
        self.build("ios")
        for name in ("libEGL", "libGLESv2"):
            xcf = self.framework.parent/f"ANGLELib{name[3:]}-ios.xcframework"
            libraries = plistlib.loads((xcf/"Info.plist").read_bytes())["AvailableLibraries"]
            for entry in libraries:
                modulemap = xcf/entry["LibraryIdentifier"]/entry["LibraryPath"]/"Modules/module.modulemap"
                self.assertIn(f"framework module {name}", modulemap.read_text())

    def assemble(self, *flags):
        return subprocess.run(["bash", "scripts/build-apple-sdk.sh", "--platform", "ios", "--assemble-only", *flags], cwd=self.root, env=self.env, text=True, capture_output=True)

    def test_complete_assembly_refuses_missing_shipping_groups(self):
        self.build("ios")
        result = self.assemble("--require-all-slices")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("missing required ios-simulator", result.stderr)

    def test_assembly_refuses_wrong_product_identity(self):
        self.build("ios")
        path = self.root/"build/performance-plus/Debug/ios/migo-build.json"
        receipt = json.loads(path.read_text())
        receipt["product"] = "macos-v8"
        path.write_text(json.dumps(receipt))
        result = self.assemble()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("product identity mismatch", result.stderr)

    def test_assembly_refuses_modified_archive(self):
        self.build("ios")
        (self.root/"build/performance-plus/Debug/ios/libmigo.a").write_text("changed after build")
        result = self.assemble()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("archive changed after build", result.stderr)

    def test_contract_detects_removed_runtime_edge_and_webkit_leak(self):
        manifest_path = self.root/"platforms/apple/Package.swift"
        manifest = manifest_path.read_text()
        def audit():
            return subprocess.run(["bash", str(ROOT/"scripts/test-apple-shipping-package-contract.sh")], env=dict(self.env, MIGO_APPLE_PACKAGE_CONTRACT_ROOT=str(self.root)), text=True, capture_output=True)
        baseline = audit()
        self.assertEqual(baseline.returncode, 0, baseline.stderr)
        edge = '.target(name: "libGLESv2", condition: .when(platforms: [.iOS])),'
        self.assertIn(edge, manifest)
        manifest_path.write_text(manifest.replace(edge, ""))
        result = audit()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("ANGLE dependency libGLESv2 is absent", result.stderr)
        manifest_path.write_text(manifest.replace('dependencies: [.product(name: "MigoAppleCore", package: "core")]', 'dependencies: [.product(name: "MigoAppleCore", package: "core"), "libEGL"]'))
        result = audit()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("MigoAppleWebKit reaches ANGLE dependency libEGL", result.stderr)


class ANGLEEmbedding(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="migo-angle-embed-fixture-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.frameworks = self.root/"Frameworks"
        self.destination = self.root/"Host.app/Contents/Frameworks"
        for name in ("libEGL", "libGLESv2"):
            xcf = self.frameworks/f"ANGLELib{name[3:]}-macos.xcframework"
            slice_dir = xcf/"macos-arm64_x86_64"
            slice_dir.mkdir(parents=True)
            (slice_dir/f"{name}.dylib").write_text(f"fixture bytes {name}")
            (xcf/"Info.plist").write_bytes(plistlib.dumps({"AvailableLibraries": [{"LibraryIdentifier": slice_dir.name, "LibraryPath": f"{name}.dylib", "SupportedPlatform": "macos", "SupportedArchitectures": ["arm64", "x86_64"]}]}))

    def embed(self, success=True):
        result = subprocess.run(["bash", str(ROOT/"scripts/embed-apple-angle.sh"), "--frameworks-dir", str(self.frameworks), "--destination", str(self.destination), "--architectures", "arm64 x86_64"], text=True, capture_output=True)
        if success:
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        else:
            self.assertNotEqual(result.returncode, 0)
        return result

    def test_dylibs_are_embedded_adjacent_with_exact_upstream_names(self):
        self.embed()
        self.assertEqual(sorted(p.name for p in self.destination.iterdir()), ["libEGL.dylib", "libGLESv2.dylib"])
        self.assertEqual((self.destination/"libGLESv2.dylib").read_text(), "fixture bytes libGLESv2")

    def test_missing_gles_partner_does_not_copy_egl(self):
        shutil.rmtree(self.frameworks/"ANGLELibGLESv2-macos.xcframework")
        result = self.embed(success=False)
        self.assertIn("ANGLELibGLESv2", result.stderr)
        self.assertFalse((self.destination/"libEGL.dylib").exists())

    def test_wrong_architecture_is_rejected(self):
        path = self.frameworks/"ANGLELibGLESv2-macos.xcframework/Info.plist"
        plist = plistlib.loads(path.read_bytes())
        plist["AvailableLibraries"][0]["SupportedArchitectures"] = ["arm64"]
        path.write_bytes(plistlib.dumps(plist))
        result = self.embed(success=False)
        self.assertIn("no unique macOS slice", result.stderr)
        self.assertFalse((self.destination/"libEGL.dylib").exists())


class PackagePublication(unittest.TestCase):
    def test_resource_publish_failure_restores_frameworks_and_resources(self):
        spec = importlib.util.spec_from_file_location("apple_sdk_package", ROOT/"scripts/apple-sdk-package.py")
        packager = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(packager)
        self.assertTrue(callable(getattr(packager, "publish_outputs", None)), "package publication needs a shared rollback transaction")
        with tempfile.TemporaryDirectory(prefix="migo-publication-fixture-") as tmp:
            root = Path(tmp)
            pairs = []
            for name in ("Frameworks", "Resources"):
                source, destination = root/"staged"/name, root/"installed"/name
                source.mkdir(parents=True)
                destination.mkdir(parents=True)
                (source/"content").write_text("new " + name)
                (destination/"content").write_text("old " + name)
                pairs.append((source, destination))
            rename = Path.rename
            def fail_resource_publication(path, destination):
                if path == pairs[1][0]:
                    raise OSError("injected resource publication failure")
                return rename(path, destination)
            with mock.patch.object(Path, "rename", fail_resource_publication):
                with self.assertRaisesRegex(OSError, "injected resource publication failure"):
                    packager.publish_outputs(pairs, root/"backups")
            for _, destination in pairs:
                self.assertEqual((destination/"content").read_text(), "old " + destination.name)


if __name__ == "__main__":
    unittest.main()
