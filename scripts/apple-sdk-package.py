#!/usr/bin/env python3
"""Assemble the shipping platform/product matrix and its ANGLE runtime closure.

Native builds remain in build-apple-sdk.sh. This helper reads real staged
archives, refuses incompatible inputs, and publishes only after Xcode succeeds.
"""
from __future__ import annotations

import argparse
from contextlib import ExitStack
import hashlib
import json
import os
from pathlib import Path
import plistlib
import shutil
import subprocess
import tempfile


PRODUCTS = {"ios": "performance-plus", "ios-simulator": "performance-plus", "macos": "macos-v8"}


def digest(path):
    value = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            value.update(chunk)
    return value.hexdigest()


def header_digest(stage):
    files = sorted((stage / "headers").rglob("*"))
    return {str(path.relative_to(stage / "headers")): digest(path) for path in files if path.is_file()}


def check_runtime(root, platform):
    script = root / "scripts/build-angle-apple.sh"
    layout = subprocess.check_output(["bash", str(script), "--print-loader-layout", platform], text=True)
    for line in layout.splitlines():
        _, relative = line.split()
        binary = root / "engine/third_party" / f"angle-apple-{platform}" / relative
        if not binary.is_file():
            raise ValueError(f"missing ANGLE runtime: {binary}; install the pinned pair before building")
    # SwiftPM validates declared local binary paths even on macOS, where the
    # framework dependency is conditional. One iOS group is enough to resolve.
    if not any((root / "engine/third_party" / f"angle-apple-{p}/libEGL.framework/libEGL").is_file()
               for p in ("ios", "ios-simulator")):
        raise ValueError("the Swift package requires the iOS ANGLE framework pair; install ios or ios-simulator")


def record(args):
    stage = args.stage
    receipt = dict(platform=args.platform, product=args.product, configuration=args.configuration,
                   deployment_target=args.deployment_target, archive_sha256=digest(stage / "libmigo.a"),
                   headers=header_digest(stage))
    (stage / "migo-build.json").write_text(json.dumps(receipt, indent=2) + "\n")


def publish_outputs(pairs, backups):
    """Replace generated outputs together, restoring every prior output on failure."""
    backups.mkdir()
    installed = []
    try:
        for index, (source, destination) in enumerate(pairs):
            destination.parent.mkdir(parents=True, exist_ok=True)
            previous = backups / str(index)
            if destination.exists():
                destination.rename(previous)
            installed.append((destination, previous))
            source.rename(destination)
    except BaseException:
        for destination, previous in reversed(installed):
            if destination.is_dir():
                shutil.rmtree(destination)
            elif destination.exists():
                destination.unlink()
            if previous.exists():
                previous.rename(destination)
        raise


def assemble(args):
    diagnostic = args.product == "external-frames-diagnostic"
    products = {"macos": args.product} if diagnostic else PRODUCTS
    stages = []
    for platform, product in products.items():
        stage = args.build_root / product / args.configuration / platform
        # A stage with no receipt is a build that started and did not finish --
        # an interrupted one leaves the directory behind. Treated as absent
        # rather than read, because reading it raised a bare FileNotFoundError
        # naming migo-build.json, which reads as "the packager is broken" and is
        # actually "that group was never built". Assembling an iOS product on a
        # machine that cannot build the macOS one is a legitimate thing to want.
        receipt_path = stage / "migo-build.json"
        if not stage.exists() or not receipt_path.exists():
            if args.require_all_slices == "1":
                detail = "was never built" if not stage.exists() else (
                    f"left a stage with no receipt at {stage}; an interrupted build does that")
                raise ValueError(
                    f"missing required {platform} group ({product}, {args.configuration}): {detail}")
            continue
        receipt = json.loads(receipt_path.read_text())
        expected = dict(platform=platform, product=product, configuration=args.configuration)
        if any(receipt.get(key) != value for key, value in expected.items()):
            raise ValueError(f"staged product identity mismatch: {stage}")
        if receipt["archive_sha256"] != digest(stage / "libmigo.a"):
            raise ValueError(f"staged archive changed after build: {stage}")
        if not receipt["headers"] or receipt["headers"] != header_digest(stage):
            raise ValueError(f"staged headers changed after build: {stage}")
        if stages and receipt["headers"] != stages[0][1]["headers"]:
            raise ValueError("staged groups have different C ABI headers; rebuild the older groups")
        check_runtime(args.repo_root, platform)
        stages.append((stage, receipt))
    if not stages:
        raise ValueError("no built groups found for this product/configuration")

    package = args.output.parent.parent
    package.parent.mkdir(parents=True, exist_ok=True)
    with ExitStack() as cleanup:
        temporary = Path(tempfile.mkdtemp(prefix=".migo-package-", dir=package.parent))
        # Once publication commits, housekeeping must not turn that success
        # into a failed build with changed outputs (for example on a read-only
        # stale resource directory). A cleanup error may leave a temp directory.
        cleanup.callback(shutil.rmtree, temporary, ignore_errors=True)
        staged_package = temporary / "package"
        staged_frameworks = staged_package / "Frameworks"
        staged_frameworks.mkdir(parents=True)

        # The generated helper and resource payload are part of the SDK. Stage
        # them before native assembly, and commit them in the same transaction.
        # A missing producer/helper must not fail after replacing the engine.
        if not args.webcontent_source.is_dir():
            raise ValueError(f"missing WebContent producer source directory: {args.webcontent_source}")
        if diagnostic:
            package_source = args.repo_root / "platforms/apple"
            shutil.copy2(package_source / "Package.swift", staged_package / "Package.swift")
            for directory in ("core", "Sources", "Tests"):
                shutil.copytree(package_source / directory, staged_package / directory,
                                ignore=shutil.ignore_patterns(".build"))
        staged_resources = staged_package / args.webcontent_destination.relative_to(package)
        if staged_resources.exists():
            shutil.rmtree(staged_resources)
        shutil.copytree(args.webcontent_source, staged_resources)
        # The destination's own `.gitignore` is carried across, because publishing
        # replaces that directory wholesale and the source it is copied from has
        # no such file. Without this, every non-diagnostic Apple build deleted a
        # TRACKED file from the working tree and left the generated bundle
        # untracked -- which is the exact inverse of what that .gitignore says it
        # is for ("Nothing here is authored"), and it showed up as a push to a
        # checkout being refused for "unstaged changes" nobody had made.
        authored_ignore = args.webcontent_destination / ".gitignore"
        if authored_ignore.is_file():
            shutil.copy2(authored_ignore, staged_resources / ".gitignore")
        helpers = staged_frameworks / "Scripts"
        helpers.mkdir()
        for name in ("embed-apple-angle.sh", "apple-sdk-package.py"):
            shutil.copy2(args.repo_root / "scripts" / name, helpers / name)

        output = staged_frameworks / args.output.name
        command = ["xcodebuild", "-create-xcframework"]
        for stage, _ in stages:
            command += ["-library", str(stage / "libmigo.a"), "-headers", str(stage / "headers")]
        subprocess.run(command + ["-output", str(output)], check=True)
        receipt = dict(schema_version=1, diagnostic=diagnostic, configuration=args.configuration,
                       complete=set(products) == {r["platform"] for _, r in stages},
                       slices=[r for _, r in stages])
        (output / "migo-build.json").write_text(json.dumps(receipt, indent=2) + "\n")

        # Use the existing pinned ANGLE recipe, including its platform-family
        # layout. An isolated output keeps failure from deleting a usable SDK.
        subprocess.run(["bash", str(args.repo_root / "scripts/build-angle-apple.sh"), "--xcframework"],
                       env=dict(os.environ, MIGO_APPLE_FRAMEWORKS_DIR=str(staged_frameworks)), check=True)
        for name in ("libEGL", "libGLESv2"):
            xcf = staged_frameworks / f"ANGLELib{name[3:]}-ios.xcframework"
            if not xcf.is_dir():
                raise ValueError(f"missing package dependency: {xcf.name}")
            # ANGLE is dlopened, so no headers are imported. Its framework name
            # is still the SwiftPM binary target's module identity.
            for bundle in xcf.glob(f"*/{name}.framework"):
                modules = bundle / "Modules"
                modules.mkdir(exist_ok=True)
                (modules / "module.modulemap").write_text(f"framework module {name} {{ export * }}\n")

        # Diagnostic sources are generated too, so replace their whole package.
        # In the shipping source tree only Frameworks and Resources are generated.
        pairs = [(staged_package, package)] if diagnostic else [
            (staged_frameworks, args.output.parent),
            (staged_resources, args.webcontent_destination),
        ]
        publish_outputs(pairs, temporary / "previous")


def embed_angle(args):
    """Copy the macOS pair without changing ANGLE's runtime lookup layout."""
    architectures = set(args.architectures.split())
    if not architectures or not architectures <= {"arm64", "x86_64"}:
        raise ValueError("--architectures must name arm64 and/or x86_64")
    sources = []
    for name in ("libEGL", "libGLESv2"):
        xcf = args.frameworks_dir / f"ANGLELib{name[3:]}-macos.xcframework"
        plist = plistlib.loads((xcf / "Info.plist").read_bytes())
        candidates = [entry for entry in plist.get("AvailableLibraries", [])
                      if entry.get("SupportedPlatform") == "macos"
                      and not entry.get("SupportedPlatformVariant")
                      and architectures <= set(entry.get("SupportedArchitectures", []))]
        if len(candidates) != 1:
            raise ValueError(f"no unique macOS slice for {sorted(architectures)} in {xcf}")
        entry = candidates[0]
        if entry["LibraryPath"] != f"{name}.dylib":
            raise ValueError(f"{xcf} must contain the unwrapped {name}.dylib")
        source = xcf / entry["LibraryIdentifier"] / entry["LibraryPath"]
        if not source.resolve().is_relative_to(xcf.resolve()) or not source.is_file():
            raise ValueError(f"missing or unsafe ANGLE library: {source}")
        sources.append(source)

    # Validate both members before touching the host. Sign the copied files,
    # never the shared package artifacts. The app signs after this build phase.
    args.destination.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix=".migo-angle-", dir=args.destination) as tmp:
        for source in sources:
            dest = Path(tmp) / source.name
            shutil.copy2(source, dest)
            if args.sign:
                subprocess.run(["codesign", "--force", "--sign", args.sign, str(dest)], check=True)
        for source in sources:
            (Path(tmp) / source.name).replace(args.destination / source.name)
    print(f"embedded ANGLE macOS pair in {args.destination}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    check = commands.add_parser("check-runtime")
    check.add_argument("--repo-root", type=Path, required=True)
    check.add_argument("--platform", choices=PRODUCTS, required=True)
    stamp = commands.add_parser("record")
    stamp.add_argument("--stage", type=Path, required=True)
    for field in ("platform", "product", "configuration", "deployment-target"):
        stamp.add_argument("--" + field, required=True)
    package = commands.add_parser("assemble")
    for field in ("repo-root", "build-root", "output", "webcontent-source", "webcontent-destination"):
        package.add_argument("--" + field, type=Path, required=True)
    for field in ("configuration", "product", "require-all-slices"):
        package.add_argument("--" + field, required=True)
    embed = commands.add_parser("embed-angle")
    for field in ("frameworks-dir", "destination"):
        embed.add_argument("--" + field, type=Path, required=True)
    embed.add_argument("--architectures", required=True)
    embed.add_argument("--sign", default="")
    args = parser.parse_args()
    try:
        if args.command == "record":
            record(args)
        elif args.command == "assemble":
            assemble(args)
        elif args.command == "embed-angle":
            embed_angle(args)
        else:
            check_runtime(args.repo_root, args.platform)
    except (ValueError, OSError, subprocess.CalledProcessError) as error:
        parser.exit(1, f"apple-sdk packaging: {error}\n")


if __name__ == "__main__":
    main()
