#!/usr/bin/env python3
"""Emit and verify the OpenHarmony V8 component manifest after a source build.

Closes the gap `scripts/build-ohos-sdk.sh` recorded in its own package manifest: the
shipped archive's embedded V8 was bound to no source revision and no GN argument set,
so the provenance chain that holds on Android and Linux had no OpenHarmony link.

The declared patch set is read from `contracts/artifact-manifest/ohos-v8.lock.json`
rather than passed in, for the reason the Android writer does the same: a caller that
restates the patch set can disagree with the build about what was applied, and this
manifest's whole purpose is to attest what was.
"""

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

LOCK_SCHEMA = "migo-v8-build-lock/v1"


def run(command: list[str], label: str, *, cwd: pathlib.Path | None = None) -> str:
    try:
        result = subprocess.run(
            command, check=False, text=True, capture_output=True, cwd=cwd
        )
    except OSError as error:
        raise RuntimeError(f"cannot run {label}: {error}") from error
    if result.returncode != 0:
        raise RuntimeError(f"{label} failed: {result.stderr.strip()}")
    output = " | ".join(line.strip() for line in result.stdout.splitlines() if line.strip())
    if not output:
        raise RuntimeError(f"{label} returned empty output")
    return output


def without_local_paths(value: str, source: pathlib.Path) -> str:
    """Replace this machine's rusty_v8 location with its variable.

    clang prints its `InstalledDir`, and the clang this platform uses is Chromium's,
    inside the vendored checkout -- so two machines building identical bytes would
    otherwise record different toolchains and produce different `component_id`s. The
    Android writer normalises `${ANDROID_NDK_HOME}` for exactly this reason, and the
    banners were the part it had to be told about twice.
    """
    return value.replace(str(source), "${RUSTY_V8_SRC}")


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


def normalized_gn_arguments(value: str, sdk_home: pathlib.Path) -> list[str]:
    """Sorted key=value arguments with the SDK path replaced by its variable.

    Two machines building identical bytes must produce identical manifests, and the
    OpenHarmony GN args embed the SDK location three times (`ohos_sdk_native`,
    `ohos_clang_wrapper_dir`, and the toolchain label's prefix). This is the same
    substitution the Android writer applies to the NDK path, and skipping it is how a
    `component_id` ends up depending on somebody's home directory.
    """
    arguments: list[str] = []
    keys: set[str] = set()
    home = str(sdk_home)
    for argument in shlex.split(value):
        if "=" not in argument or not argument.split("=", 1)[0]:
            raise ValueError(f"GN arg must use key=value syntax: {argument!r}")
        key = argument.split("=", 1)[0]
        if key in keys:
            raise ValueError(f"duplicate GN argument key: {key}")
        keys.add(key)
        arguments.append(argument.replace(home, "${OHOS_NDK_HOME}"))
    if not arguments:
        raise ValueError("GN argument set must not be empty")
    return sorted(arguments, key=lambda item: item.encode("utf-8"))


def declared_patches(
    lock: dict, patch_root: pathlib.Path
) -> tuple[list[dict], list[pathlib.Path]]:
    required = lock.get("required_patches")
    if not isinstance(required, list) or not required:
        raise RuntimeError("OpenHarmony V8 lock declares no required_patches")
    identities: list[dict] = []
    files: list[pathlib.Path] = []
    for entry in required:
        if not isinstance(entry, dict) or "id" not in entry or "file" not in entry:
            raise RuntimeError(
                f"required_patches entry must carry an id and a file: {entry!r}"
            )
        matches = sorted(patch_root.glob(entry["file"]))
        if len(matches) != 1:
            raise RuntimeError(
                f"declared patch {entry['file']!r} matched {len(matches)} files"
            )
        identities.append({"id": entry["id"], "sha256": hash_file(matches[0])})
        files.append(matches[0])
    identities.sort(key=lambda item: item["id"].encode("utf-8"))
    return identities, files


def sdk_identity(sdk_home: pathlib.Path, lock: dict) -> str:
    """The SDK's own record of what it is, held to the lock's pin.

    The SDK supplies the sysroot and the musl libc the archive links against, so an
    unpinned one would let two different toolchains produce artifacts that claim the
    same identity -- the defect the Android NDK pin exists to prevent.
    """
    package = sdk_home / "native/oh-uni-package.json"
    if not package.is_file():
        raise RuntimeError(f"missing OpenHarmony SDK identity: {package}")
    described = json.loads(package.read_text(encoding="utf-8"))
    version = described.get("version")
    api = str(described.get("apiVersion"))
    pinned = lock.get("sdk", {})
    if version != pinned.get("version") or api != str(pinned.get("api_version")):
        raise RuntimeError(
            f"OpenHarmony SDK is {version} (API {api}), the lock pins "
            f"{pinned.get('version')} (API {pinned.get('api_version')})"
        )
    return (
        f"OpenHarmony native SDK {version}; API {api} sysroot; "
        f"oh-uni-package.json sha256={hash_file(package)}"
    )


def build_component(
    *,
    arch: str,
    target: dict,
    ohos_api: str,
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
    return {
        "schema": "migo-v8-component-manifest/v1",
        "component_id": "",
        "target": {
            "triple": target["triple"],
            # `linux`/`ohos` because that is what the compiler reports and what the
            # engine's own cfg selects on; `os = "ohos"` would be a third spelling.
            "os": "linux",
            "arch": arch,
            "abi": "ohos",
            "cpu_baseline": target["cpu_baseline"],
            "required_cpu_features": sorted(target["required_cpu_features"]),
            "runtime_floor": {"ohos_api": ohos_api},
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
            "build_recipe": "scripts/build-v8-ohos.sh",
            "build_recipe_sha256": recipe_sha256,
            "licenses": ["BSD-3-Clause", "MIT"],
        },
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo-root", required=True, type=pathlib.Path)
    parser.add_argument("--rusty-v8-src", required=True, type=pathlib.Path)
    parser.add_argument("--sdk-home", required=True, type=pathlib.Path)
    parser.add_argument("--arch", required=True, choices=["x86_64", "aarch64"])
    parser.add_argument("--gn-args", required=True)
    parser.add_argument("--archive", required=True, type=pathlib.Path)
    parser.add_argument("--binding", required=True, type=pathlib.Path)
    parser.add_argument("--output", required=True, type=pathlib.Path)
    parser.add_argument("--compiler", required=True, type=pathlib.Path)
    parser.add_argument("--linker", required=True, type=pathlib.Path)
    parser.add_argument("--tool", type=pathlib.Path)
    parser.add_argument("--lock", type=pathlib.Path)
    # Paths whose provenance is established by something other than a patch -- the
    # pinned gn and its receipt, identified by that receipt. An argument rather than a
    # variable the writer hardcodes, so a caller that needs an exemption has to say so.
    parser.add_argument("--accounted", action="append", default=[])
    arguments = parser.parse_args()

    repo = arguments.repo_root.resolve()
    source = arguments.rusty_v8_src.resolve()
    sdk_home = arguments.sdk_home.resolve()
    archive = arguments.archive.resolve()
    binding = arguments.binding.resolve()
    for label, path in [("archive", archive), ("binding", binding)]:
        if not path.is_file():
            raise RuntimeError(f"missing V8 {label}: {path}")

    lock_path = arguments.lock or repo / "contracts/artifact-manifest/ohos-v8.lock.json"
    lock = json.loads(lock_path.read_text(encoding="utf-8"))
    if lock.get("schema") != LOCK_SCHEMA:
        raise RuntimeError(f"unsupported V8 source lock: {lock_path}")
    target = lock.get("targets", {}).get(arguments.arch)
    if not isinstance(target, dict):
        raise RuntimeError(f"lock declares no {arguments.arch} target: {lock_path}")

    source_revision = v8_source_proof.head_revision(source, "rusty_v8 revision")
    v8_revision = v8_source_proof.head_revision(source / "v8", "V8 revision")
    version = package_version(source / "Cargo.toml")
    if source_revision != lock.get("rusty_v8_revision"):
        raise RuntimeError(f"rusty_v8 source revision does not match {lock_path.name}")
    if v8_revision != lock.get("v8_revision"):
        raise RuntimeError(f"V8 source revision does not match {lock_path.name}")
    if version != lock.get("rusty_v8_version"):
        raise RuntimeError(f"rusty_v8 version does not match {lock_path.name}")

    patches, patch_files = declared_patches(
        lock, repo / "engine/third_party/v8-patches"
    )
    # The same replay proof the Android and Linux writers use: materialise every declared
    # path at HEAD, apply the declared patches, and require byte equality with the
    # worktree. Nothing had ever held the OpenHarmony build to it, so a stray edit in the
    # vendored checkout could have reached this archive unrecorded.
    #
    # Every patch applied to the shared checkout is declared in this platform's lock, so
    # the replay verifies all of their contents. An earlier version instead exempted the
    # *paths* Android's patches touch, which left those files -- `build.rs` among them --
    # unchecked while this manifest claimed a smaller patch set: arbitrary edits there
    # would have been sealed. Exempting a path skips verification; declaring a patch
    # verifies it, and one checkout serving four platforms means the honest statement of
    # what produced these bytes includes what was applied to it.
    v8_source_proof.assert_tree_is_exactly_patched(
        source, patch_files, frozenset(arguments.accounted)
    )

    component = build_component(
        arch=arguments.arch,
        target=target,
        ohos_api=str(lock["ohos_api"]),
        rusty_v8_version=version,
        rusty_v8_revision=source_revision,
        v8_revision=v8_revision,
        gn_args=normalized_gn_arguments(arguments.gn_args, sdk_home),
        patches=patches,
        archive_sha256=hash_file(archive),
        binding_sha256=hash_file(binding),
        # Resolved with the working directory inside the vendored checkout, so rustup
        # reports the toolchain that tree pins rather than whatever is on PATH. The
        # Android manifest once claimed 1.95.0 while rusty_v8 pins 1.89.0, so neither
        # recorded version had compiled anything in it.
        rustc=run(["rustc", "--version", "--verbose"], "rustc", cwd=source),
        compiler=without_local_paths(
            run([str(arguments.compiler), "--version"], "OpenHarmony C++ compiler"),
            source,
        ),
        sdk=sdk_identity(sdk_home, lock),
        linker=without_local_paths(
            run([str(arguments.linker), "--version"], "OpenHarmony linker"), source
        ),
        recipe_sha256=hash_file(repo / "scripts/build-v8-ohos.sh"),
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
        prefix=".ohos-v8-component.", suffix=".json", dir=arguments.output.parent
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
    print(f"verified OpenHarmony V8 component manifest -> {arguments.output}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
