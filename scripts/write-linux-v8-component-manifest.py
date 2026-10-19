#!/usr/bin/env python3
"""Emit and verify the Linux GNU V8 component manifest after a source build."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import pathlib
import re
import shlex
import subprocess
import sys
import tempfile

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent / "lib"))
import v8_source_proof  # noqa: E402  (path is set immediately above)


def run(command: list[str], label: str) -> str:
    try:
        result = subprocess.run(command, check=False, text=True, capture_output=True)
    except OSError as error:
        raise RuntimeError(f"cannot run {label}: {error}") from error
    if result.returncode != 0:
        raise RuntimeError(f"{label} failed: {result.stderr.strip()}")
    output = " | ".join(line.strip() for line in result.stdout.splitlines() if line.strip())
    if not output:
        raise RuntimeError(f"{label} returned empty output")
    return output


def hash_file(path: pathlib.Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1 << 20), b""):
            digest.update(chunk)
    return digest.hexdigest()


def package_version(cargo_toml: pathlib.Path) -> str:
    in_package = False
    for line in cargo_toml.read_text(encoding="utf-8").splitlines():
        stripped = line.strip()
        if stripped.startswith("["):
            in_package = stripped == "[package]"
        elif in_package:
            match = re.fullmatch(r'version\s*=\s*"([^"]+)"', stripped)
            if match:
                return match.group(1)
    raise RuntimeError(f"cannot find [package] version in {cargo_toml}")


def normalized_gn_arguments(value: str) -> list[str]:
    arguments: list[str] = []
    keys: set[str] = set()
    for argument in shlex.split(value):
        if "=" not in argument or not argument.split("=", 1)[0]:
            raise ValueError(f"GN arg must use key=value syntax: {argument!r}")
        key = argument.split("=", 1)[0]
        if key in keys:
            raise ValueError(f"duplicate GN argument key: {key}")
        keys.add(key)
        arguments.append(argument)
    if not arguments:
        raise ValueError("GN argument set must not be empty")
    return sorted(arguments, key=lambda item: item.encode("utf-8"))


def parse_patch(value: str) -> tuple[str, pathlib.Path]:
    patch_id, separator, path = value.partition("=")
    if not separator or not patch_id or not path:
        raise ValueError(f"patch must use id=path syntax: {value!r}")
    return patch_id, pathlib.Path(path)


def verify_source_changes(
    source: pathlib.Path, patches: list[tuple[str, pathlib.Path]]
) -> list[dict]:
    """Hash the declared patches, having proved the checkout is exactly them.

    The proof lives in scripts/lib/v8_source_proof.py, shared with the Android
    writer. It descends into submodules, which subsumes what used to be three
    separate checks here: that the `build` submodule is pristine, that the `v8`
    submodule is clean, and that top-level changes fall within declared paths. A
    Linux V8 build declares no build-submodule patches, so any change under
    `build/` is an undeclared change and is now reported by path rather than as a
    cryptic dirty-pointer status.

    Declaring no patches at all is meaningful and supported: it asserts the
    checkout is pristine.
    """
    identities: list[dict] = []
    patch_files: list[pathlib.Path] = []
    for patch_id, path in patches:
        path = path.resolve()
        if not path.is_file():
            raise RuntimeError(f"missing declared source patch: {path}")
        patch_files.append(path)
        identities.append({"id": patch_id, "sha256": hash_file(path)})
    v8_source_proof.assert_tree_is_exactly_patched(source, patch_files)
    identities.sort(key=lambda item: item["id"].encode("utf-8"))
    return identities


# Keyed on migo's own arch vocabulary (aarch64/x86_64), not GN's (arm64/x64)
# or Debian's sysroot suffix (arm64/amd64) -- neither of those is derivable
# from the other two, so scripts/build-v8-linux.sh resolves all three
# separately and only this one is threaded through to the manifest, matching
# every other platform's component manifest.
#
# cpu_baseline/required_cpu_features are a stated policy floor, not a
# property measured out of the archive: x86_64's "x86-64-v1" names the
# baseline the psABI itself guarantees for the 64-bit mode (cmov/sse2 are
# mandatory *because* they are part of that baseline, not an extra
# requirement layered on top) -- ARMv8-A is the equivalent floor for AArch64
# Linux (there is no pre-ARMv8-A 64-bit mode to fall back to), and NEON
# (Advanced SIMD) is mandatory in the ARMv8-A base architecture the same way
# cmov/sse2 are mandatory in x86-64-v1, not an optional extension.
#
# runtime_floor (glibc/glibcxx) is NOT keyed per arch: Debian ships one glibc
# release across every architecture a given suite supports, so the 2.31/
# 3.4.28 floor scripts/abi-floor-audit.py enforces for x86_64 already applies
# unchanged to the arm64 sysroot from the same bullseye release.
TARGETS = {
    "x86_64": {
        "triple": "x86_64-unknown-linux-gnu",
        "arch": "x86_64",
        "cpu_baseline": "x86-64-v1",
        "required_cpu_features": ["cmov", "sse2"],
    },
    "aarch64": {
        "triple": "aarch64-unknown-linux-gnu",
        "arch": "aarch64",
        "cpu_baseline": "armv8-a",
        "required_cpu_features": ["neon"],
    },
}


def build_component(
    *,
    arch: str,
    rusty_v8_version: str,
    rusty_v8_revision: str,
    v8_revision: str,
    gn_args: list[str],
    patches: list[dict],
    archive_sha256: str,
    binding_sha256: str,
    rustc: str,
    compiler: str,
    sdk: str,
    linker: str,
    recipe_sha256: str,
) -> dict:
    target = TARGETS[arch]
    return {
        "schema": "migo-v8-component-manifest/v1",
        "component_id": "",
        "target": {
            "triple": target["triple"],
            "os": "linux",
            "arch": target["arch"],
            "abi": "gnu",
            "cpu_baseline": target["cpu_baseline"],
            "required_cpu_features": sorted(target["required_cpu_features"]),
            "runtime_floor": {"glibc": "2.31", "glibcxx": "3.4.28"},
        },
        "toolchain": {
            "rustc": rustc,
            "compiler": compiler,
            "sdk": sdk,
            "linker": linker,
        },
        "runtime": {
            "backend": "v8",
            "rusty_v8_version": rusty_v8_version,
            "rusty_v8_revision": rusty_v8_revision,
            "v8_revision": v8_revision,
            "normalized_gn_args": gn_args,
            "patches": patches,
        },
        "hashes": {
            "archive": archive_sha256,
            "rust_binding": binding_sha256,
        },
        "provenance": {
            "source_revision": rusty_v8_revision,
            "build_recipe": "scripts/build-v8-linux.sh",
            "build_recipe_sha256": recipe_sha256,
            "licenses": ["BSD-3-Clause", "MIT"],
        },
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo-root", required=True, type=pathlib.Path)
    parser.add_argument("--rusty-v8-src", required=True, type=pathlib.Path)
    parser.add_argument("--arch", required=True, choices=sorted(TARGETS))
    parser.add_argument("--gn-args", required=True)
    parser.add_argument("--archive", required=True, type=pathlib.Path)
    parser.add_argument("--binding", required=True, type=pathlib.Path)
    parser.add_argument("--sysroot", required=True, type=pathlib.Path)
    parser.add_argument("--output", required=True, type=pathlib.Path)
    parser.add_argument("--patch", action="append", default=[])
    parser.add_argument("--compiler", default=os.environ.get("CXX", "clang++"))
    parser.add_argument("--linker", default=os.environ.get("MIGO_LLD", "ld.lld"))
    parser.add_argument("--tool", type=pathlib.Path)
    arguments = parser.parse_args()

    repo = arguments.repo_root.resolve()
    source = arguments.rusty_v8_src.resolve()
    archive = arguments.archive.resolve()
    binding = arguments.binding.resolve()
    sysroot = arguments.sysroot.resolve()
    for label, path in [("archive", archive), ("binding", binding)]:
        if not path.is_file():
            raise RuntimeError(f"missing V8 {label}: {path}")
    if not sysroot.is_dir():
        raise RuntimeError(f"missing Linux sysroot: {sysroot}")

    patches = verify_source_changes(source, [parse_patch(value) for value in arguments.patch])
    sysroot_recipe = source / "build/linux/sysroot_scripts/sysroots.json"
    if not sysroot_recipe.is_file():
        raise RuntimeError(f"missing Chromium sysroot identity: {sysroot_recipe}")
    sysroot_word = {"x86_64": "amd64", "aarch64": "arm64"}[arguments.arch]
    sdk = (
        f"Debian bullseye {sysroot_word} sysroot; "
        f"sysroots.json sha256={hash_file(sysroot_recipe)}"
    )
    component = build_component(
        arch=arguments.arch,
        rusty_v8_version=package_version(source / "Cargo.toml"),
        rusty_v8_revision=v8_source_proof.head_revision(source, "rusty_v8 revision"),
        v8_revision=v8_source_proof.head_revision(source / "v8", "V8 revision"),
        gn_args=normalized_gn_arguments(arguments.gn_args),
        patches=patches,
        archive_sha256=hash_file(archive),
        binding_sha256=hash_file(binding),
        rustc=run(["rustc", "--version", "--verbose"], "rustc"),
        compiler=run([arguments.compiler, "--version"], "Linux C++ compiler"),
        sdk=sdk,
        linker=run([arguments.linker, "--version"], "Linux linker"),
        recipe_sha256=hash_file(repo / "scripts/build-v8-linux.sh"),
    )

    tool = arguments.tool
    if tool is None:
        manifest = repo / "tools/artifact-manifest/Cargo.toml"
        subprocess.run(
            [
                "cargo", "build", "--manifest-path", str(manifest), "--locked", "--release",
                # The directory read below, whatever CARGO_TARGET_DIR says.
                "--target-dir", str(manifest.parent / "target"),
            ],
            check=True,
        )
        tool = repo / "tools/artifact-manifest/target/release/migo-artifact-manifest"
    tool = tool.resolve()
    arguments.output.parent.mkdir(parents=True, exist_ok=True)
    descriptor, draft_name = tempfile.mkstemp(
        prefix=".linux-v8-component.", suffix=".json", dir=arguments.output.parent
    )
    os.close(descriptor)
    draft = pathlib.Path(draft_name)
    try:
        draft.write_text(json.dumps(component, indent=2, sort_keys=True) + "\n")
        subprocess.run(
            [str(tool), "seal-v8-component", str(draft), str(arguments.output)],
            check=True,
        )
        subprocess.run(
            [str(tool), "verify-v8-component", str(arguments.output), str(archive), str(binding)],
            check=True,
        )
    finally:
        draft.unlink(missing_ok=True)
    print(f"verified Linux V8 component manifest -> {arguments.output}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
