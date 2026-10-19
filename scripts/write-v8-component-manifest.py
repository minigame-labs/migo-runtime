#!/usr/bin/env python3
"""Emit a verified Android V8 component manifest after a source build."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import pathlib
import platform
import re
import shlex
import subprocess
import sys
import tempfile

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent / "lib"))
import v8_source_proof  # noqa: E402  (path is set immediately above)


def declared_patches(
    lock: dict, patch_root: pathlib.Path
) -> tuple[list[dict], list[pathlib.Path]]:
    """Resolve the lock's declared patch set into hashed identities and paths.

    The lock carries both the id and the file name so that it is the single
    declaration of what an Android V8 build applies. This writer used to hold its
    own id-to-file mapping, which meant the lock listed three patches while
    scripts/build-v8-android.sh applied four -- the prebuilt-binding diff was
    absent from both this table and the lock, so the sealed manifest recorded a
    patch set the build had not used.
    """
    required = lock.get("required_patches")
    if not isinstance(required, list) or not required:
        raise RuntimeError("V8 lock declares no required_patches")
    identities = []
    files = []
    for entry in required:
        if not isinstance(entry, dict) or "id" not in entry or "file" not in entry:
            raise RuntimeError(
                f"required_patches entry must carry an id and a file: {entry!r}"
            )
        path = patch_root / entry["file"]
        if not path.is_file():
            raise RuntimeError(f"declared patch is missing: {path}")
        identities.append({"id": entry["id"], "sha256": hash_file(path)})
        files.append(path)
    return identities, files


def byte_sorted(values: set[str] | list[str]) -> list[str]:
    return sorted(values, key=lambda value: value.encode("utf-8"))


def run(
    command: list[str],
    label: str,
    *,
    allow_empty: bool = False,
    cwd: pathlib.Path | None = None,
) -> str:
    try:
        result = subprocess.run(
            command, check=False, text=True, capture_output=True, cwd=cwd
        )
    except OSError as error:
        raise RuntimeError(f"cannot run {label}: {error}") from error
    if result.returncode != 0:
        raise RuntimeError(f"{label} failed: {result.stderr.strip()}")
    output = " | ".join(line.strip() for line in result.stdout.splitlines() if line.strip())
    if not output and not allow_empty:
        raise RuntimeError(f"{label} returned empty output")
    return output


def hash_file(path: pathlib.Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(64 * 1024), b""):
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


def find_prebuilt(ndk: pathlib.Path) -> pathlib.Path:
    root = ndk / "toolchains/llvm/prebuilt"
    key = (platform.system().lower(), platform.machine().lower())
    names = {
        ("linux", "x86_64"): "linux-x86_64",
        ("linux", "amd64"): "linux-x86_64",
        ("linux", "aarch64"): "linux-aarch64",
        ("darwin", "arm64"): "darwin-arm64",
        ("darwin", "aarch64"): "darwin-arm64",
        ("darwin", "x86_64"): "darwin-x86_64",
    }
    preferred = names.get(key)
    if preferred and (root / preferred).is_dir():
        return root / preferred
    candidates = sorted(path for path in root.glob("*") if path.is_dir())
    if len(candidates) != 1:
        raise RuntimeError(f"cannot select a unique NDK prebuilt under {root}")
    return candidates[0]


def normalized_gn_arguments(value: str, api: int) -> list[str]:
    arguments = []
    keys: set[str] = set()
    for argument in shlex.split(value):
        if argument.startswith("android_ndk_root="):
            argument = "android_ndk_root=${ANDROID_NDK_HOME}"
        if "=" not in argument or not argument.split("=", 1)[0]:
            raise RuntimeError(f"GN arg must use key=value syntax: {argument!r}")
        key = argument.split("=", 1)[0]
        if key in keys:
            raise RuntimeError(f"duplicate GN argument key: {key}")
        keys.add(key)
        arguments.append(argument)
    arguments = byte_sorted(set(arguments))
    if f"android_ndk_api_level={api}" not in arguments:
        raise RuntimeError(f"GN args do not pin android_ndk_api_level={api}")
    return arguments


def without_ndk_path(value: str, ndk: pathlib.Path) -> str:
    """Replace the NDK's absolute location with the variable that names it.

    clang and lld print their InstalledDir, which is wherever this machine keeps
    the NDK, so two machines building the identical archive produced different
    manifests and therefore different component_ids. `normalized_gn_args` already
    substitutes ${ANDROID_NDK_HOME} for exactly this reason; the toolchain banners
    were simply missed.
    """
    return value.replace(str(ndk), "${ANDROID_NDK_HOME}")


def write_draft(path: pathlib.Path, value: object) -> None:
    path.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n", encoding="utf-8")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo-root", required=True, type=pathlib.Path)
    parser.add_argument("--rusty-v8-src", required=True, type=pathlib.Path)
    parser.add_argument("--ndk-home", required=True, type=pathlib.Path)
    parser.add_argument("--arch", required=True, choices=("aarch64", "x86_64"))
    parser.add_argument("--extra-gn-args", required=True)
    parser.add_argument("--archive", required=True, type=pathlib.Path)
    parser.add_argument("--binding", required=True, type=pathlib.Path)
    parser.add_argument("--output", required=True, type=pathlib.Path)
    parser.add_argument("--tool", type=pathlib.Path)
    parser.add_argument("--lock", type=pathlib.Path)
    # Paths whose provenance is established by something other than a patch -- the
    # pinned gn and its build receipt. Declared by the caller, in the same spelling
    # scripts/build-v8-android.sh passes to the shell-side proof, so one array in
    # that script feeds both.
    parser.add_argument("--accounted", action="append", default=[])
    # A patch this manifest does not declare, whose *created* paths are accounted for.
    # One vendored checkout serves every platform's V8 build, and OpenHarmony's
    # toolchain patch creates a file the Android declaration does not touch -- so
    # without this the proof below refuses a path that is explained by a committed
    # patch, just not by one this manifest claims. Same spelling and same rule as the
    # shell-side proof, because scripts/build-v8-android.sh feeds one array to both:
    # only created paths may be accounted for, since accounting for a path a foreign
    # patch merely modifies would skip content verification on a file this platform's
    # own patches may also touch.
    parser.add_argument("--accounted-patch", action="append", default=[])
    arguments = parser.parse_args()

    try:
        repo = arguments.repo_root.resolve()
        source = arguments.rusty_v8_src.resolve()
        ndk = arguments.ndk_home.resolve()
        lock_path = arguments.lock or repo / "contracts/artifact-manifest/android-v8.lock.json"
        lock = json.loads(lock_path.read_text(encoding="utf-8"))
        if lock.get("schema") != "migo-v8-build-lock/v1":
            raise RuntimeError(f"unsupported V8 source lock: {lock_path}")
        source_revision = v8_source_proof.head_revision(source, "rusty_v8 revision")
        v8_revision = v8_source_proof.head_revision(source / "v8", "V8 revision")
        version = package_version(source / "Cargo.toml")
        if source_revision != lock.get("rusty_v8_revision"):
            raise RuntimeError("rusty_v8 source revision does not match android-v8.lock.json")
        if v8_revision != lock.get("v8_revision"):
            raise RuntimeError("V8 source revision does not match android-v8.lock.json")
        if version != lock.get("rusty_v8_version"):
            raise RuntimeError("rusty_v8 version does not match android-v8.lock.json")
        api = lock.get("android_api")
        target = lock.get("targets", {}).get(arguments.arch)
        if api != 26 or not isinstance(target, dict):
            raise RuntimeError("V8 lock does not contain the Android API 26 target")
        patches, patch_files = declared_patches(
            lock, repo / "engine/third_party/v8-patches"
        )
        # Proves the sources really are HEAD plus exactly the patches this manifest
        # is about to claim. The previous check compared modified paths against a
        # hardcoded allowlist, which restated what the patches touch and could not
        # see an edit inside an allowed file.
        accounted = set(arguments.accounted)
        for glob in arguments.accounted_patch:
            accounted |= v8_source_proof.accounted_paths_from_patch(
                repo / "engine/third_party/v8-patches", glob
            )
        v8_source_proof.assert_tree_is_exactly_patched(
            source, patch_files, frozenset(accounted)
        )

        properties = ndk / "source.properties"
        properties_text = properties.read_text(encoding="utf-8")
        match = re.search(r"(?m)^Pkg\.Revision\s*=\s*(\S+)\s*$", properties_text)
        if not match:
            raise RuntimeError(f"NDK revision is missing from {properties}")
        prebuilt = find_prebuilt(ndk)
        clang = prebuilt / "bin" / f"{target['triple']}{api}-clang++"
        if not clang.is_file():
            clang = prebuilt / "bin/clang++"
        linker = prebuilt / "bin/ld.lld"
        recipe = repo / "scripts/build-v8-android.sh"
        component = {
            "schema": "migo-v8-component-manifest/v1",
            "component_id": "",
            "target": {
                "triple": target["triple"],
                "os": "android",
                "arch": arguments.arch,
                "abi": "android",
                "cpu_baseline": target["cpu_baseline"],
                "required_cpu_features": target["required_cpu_features"],
                "runtime_floor": {"android_api": str(api)},
            },
            "toolchain": {
                # Resolved inside the rusty_v8 tree, so rustup reports the
                # toolchain that tree pins rather than whichever one happens to be
                # the operator's default. Recording the ambient rustc made the
                # manifest non-deterministic: the same archive was described as
                # built with 1.95.0 and later with 1.93.0, while rusty_v8 pins
                # 1.89.0 and neither ambient version compiled anything in it.
                "rustc": run(
                    ["rustc", "--version", "--verbose"], "rustc", cwd=source
                ),
                "compiler": without_ndk_path(
                    run([str(clang), "--version"], "Android clang"), ndk
                ),
                "sdk": (
                    f"Android NDK {match.group(1)}; API {api} sysroot; "
                    f"source.properties sha256={hash_file(properties)}"
                ),
                "linker": without_ndk_path(
                    run([str(linker), "--version"], "Android linker"), ndk
                ),
            },
            "runtime": {
                "backend": "v8",
                "rusty_v8_version": version,
                "rusty_v8_revision": source_revision,
                "v8_revision": v8_revision,
                "normalized_gn_args": normalized_gn_arguments(arguments.extra_gn_args, api),
                "patches": patches,
            },
            "hashes": {
                "archive": hash_file(arguments.archive),
                "rust_binding": hash_file(arguments.binding),
            },
            "provenance": {
                "source_revision": source_revision,
                "build_recipe": "scripts/build-v8-android.sh",
                "build_recipe_sha256": hash_file(recipe),
                "licenses": ["BSD-3-Clause", "MIT"],
            },
        }

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
            prefix=".v8-component.", suffix=".json", dir=arguments.output.parent
        )
        os.close(descriptor)
        draft = pathlib.Path(draft_name)
        try:
            write_draft(draft, component)
            subprocess.run(
                [str(tool), "seal-v8-component", str(draft), str(arguments.output)],
                check=True,
            )
            subprocess.run(
                [
                    str(tool),
                    "verify-v8-component",
                    str(arguments.output),
                    str(arguments.archive),
                    str(arguments.binding),
                ],
                check=True,
            )
        finally:
            draft.unlink(missing_ok=True)
        print(f"V8 component manifest -> {arguments.output}")
    except (OSError, RuntimeError, ValueError, KeyError, subprocess.CalledProcessError) as error:
        print(f"V8 component manifest: {error}", file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
