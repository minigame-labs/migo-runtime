#!/usr/bin/env python3
"""Writes and verifies the x86_64-pc-windows-msvc V8 component manifest.

Counterpart to write-v8-component-manifest.py (Android) and
write-linux-v8-component-manifest.py (Linux). Each platform has its own writer
because each records a different toolchain and a different artifact shape; this
one describes a build that yields four files rather than a single archive.

It runs inside scripts/build-v8-windows.sh, immediately after the build, and
that placement is the point. A manifest asserts which sources and toolchain
produced these exact bytes; only the build knows that. Reconstructing one
afterwards from whatever the checkout happens to look like would produce a
provenance record that reads as authoritative while attesting to nothing --
and scripts/fetch-v8-archives.sh keys its integrity check on these manifests,
so a fabricated one would be trusted.

Unlike the Linux writer, this one verifies against contracts/artifact-manifest/
windows-v8.lock.json: the revisions, version and GN arguments in that lock are
what the Windows build is pinned to, and a build that drifted from them must not
be described as if it had not.
"""
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

TARGETS = {
    "x86_64": {
        "triple": "x86_64-pc-windows-msvc",
        "cpu_baseline": "x86-64-v1",
        "required_cpu_features": ["cmov", "sse2"],
        "msvc_arch_label": "x64",
    },
    "aarch64": {
        "triple": "aarch64-pc-windows-msvc",
        "cpu_baseline": "armv8-a",
        "required_cpu_features": ["neon"],
        "msvc_arch_label": "arm64, cross from x64 host",
    },
}


def recipe_cargo_features(recipe: str) -> dict[str, bool]:
    invocations = [shlex.split(line.strip()) for line in recipe.splitlines()
                   if line.strip().startswith("cargo build")]
    if len(invocations) != 1:
        raise RuntimeError("expected one unambiguous cargo build invocation in the Windows recipe")
    arguments = invocations[0]
    selected = set()
    for index, argument in enumerate(arguments):
        if argument == "--features":
            selected.update(arguments[index + 1].replace(",", " ").split())
        elif argument.startswith("--features="):
            selected.update(argument.split("=", 1)[1].replace(",", " ").split())
    features = {name: name in selected for name in ("simdutf", "v8_enable_pointer_compression")}
    features["use_custom_libcxx"] = "--no-default-features" not in arguments or "use_custom_libcxx" in selected
    return features


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


def git_revision(path: pathlib.Path, label: str) -> str:
    revision = run(["git", "-C", str(path), "rev-parse", "HEAD"], label)
    if not re.fullmatch(r"[0-9a-fA-F]{40}", revision):
        raise RuntimeError(f"{label} is not a full revision: {revision!r}")
    return revision


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
    """Splits a GN argument string into sorted `key=value` entries.

    Sorted and de-duplicated so the manifest records the argument *set*, not the
    order they happened to be concatenated in: two builds with the same
    arguments must produce the same manifest field.
    """
    arguments: dict[str, str] = {}
    for token in value.split():
        if "=" not in token:
            raise RuntimeError(f"GN argument is not key=value: {token!r}")
        key, _, argument_value = token.partition("=")
        arguments[key.strip()] = argument_value.strip()
    return [f"{key}={arguments[key]}" for key in sorted(arguments)]


def verify_against_lock(
    lock_path: pathlib.Path,
    *,
    arch: str,
    rusty_v8_version: str,
    rusty_v8_revision: str,
    v8_revision: str,
    gn_args: list[str],
    cargo_features: dict[str, bool],
) -> None:
    """Refuses to describe a build that drifted from the pinned sources.

    The lock is the statement of what a Windows V8 is allowed to be built from.
    Writing a manifest for anything else would make the manifest the weaker of
    the two records while looking like the stronger one.
    """
    lock = json.loads(lock_path.read_text(encoding="utf-8"))

    for field, actual in (
        ("rusty_v8_version", rusty_v8_version),
        ("rusty_v8_revision", rusty_v8_revision),
        ("v8_revision", v8_revision),
    ):
        expected = lock.get(field)
        if expected != actual:
            raise RuntimeError(
                f"{field} does not match {lock_path.name}: "
                f"lock has {expected!r}, build used {actual!r}"
            )

    expected_triple = TARGETS[arch]["triple"]
    target = (lock.get("targets") or {}).get(arch)
    if not isinstance(target, dict) or target.get("triple") != expected_triple:
        raise RuntimeError(f"{lock_path.name} does not pin the {expected_triple} target")

    # Every argument the lock pins must appear with the same value. The build may
    # pass more (paths, target dirs); it may not contradict the lock.
    built = dict(argument.split("=", 1) for argument in gn_args)
    for key, value in (lock.get("gn_args") or {}).items():
        expected = str(value).lower() if isinstance(value, bool) else str(value)
        actual = built.get(key)
        if actual is None:
            raise RuntimeError(f"the build did not pass GN argument {key!r} pinned by the lock")
        if actual.lower() != expected.lower():
            raise RuntimeError(
                f"GN argument {key!r} differs from the lock: "
                f"lock has {expected!r}, build used {actual!r}"
            )

    # use_custom_libcxx decides whether V8 carries its own libc++ -- the single
    # choice this whole build recipe exists around. It is a cargo feature, not a
    # GN argument (build.rs derives the GN value from it and overrides
    # EXTRA_GN_ARGS), so it is verified separately or not at all.
    for feature, expected_on in (lock.get("cargo_features") or {}).items():
        if bool(expected_on) != bool(cargo_features.get(feature)):
            raise RuntimeError(
                f"cargo feature {feature!r} differs from the lock: "
                f"lock expects {bool(expected_on)}, build used "
                f"{bool(cargo_features.get(feature))}"
            )


def build_component(
    *,
    arch: str,
    rusty_v8_version: str,
    rusty_v8_revision: str,
    v8_revision: str,
    gn_args: list[str],
    archive_sha256: str,
    binding_sha256: str,
    rustc: str,
    compiler: str,
    sdk: str,
    linker: str,
    recipe_sha256: str,
) -> dict:
    target_spec = TARGETS[arch]
    return {
        "schema": "migo-v8-component-manifest/v1",
        "component_id": "",
        "target": {
            "triple": target_spec["triple"],
            "os": "windows",
            "arch": arch,
            "abi": "msvc",
            "cpu_baseline": target_spec["cpu_baseline"],
            "required_cpu_features": target_spec["required_cpu_features"],
            # The MSVC runtime is a redistributable the host ships, not a floor
            # the loader enforces the way glibc is on Linux; the meaningful
            # constraint is which CRT this was compiled against.
            "runtime_floor": {"msvc_runtime": "MD (dynamic CRT)"},
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
            # Windows applies its patches to the checkout by the build script's
            # own apply-and-assert step rather than through this writer, so the
            # set is recorded as empty here rather than claimed falsely.
            "patches": [],
        },
        # migo-v8-component-manifest/v1 is a cross-platform schema defined in
        # tools/artifact-manifest and carries exactly these two digests. The
        # Windows build also produces rusty_v8.dll and its import library; those
        # are covered one level up, by the SDK package manifest that records
        # every staged file, the same way the Android and Linux SDKs cover the
        # files their V8 component manifest does not. Widening this schema for
        # one platform would change the shape every platform's manifest is
        # verified against.
        "hashes": {
            "archive": archive_sha256,
            "rust_binding": binding_sha256,
        },
        "provenance": {
            "source_revision": rusty_v8_revision,
            "build_recipe": "scripts/build-v8-windows.sh",
            "build_recipe_sha256": recipe_sha256,
            "licenses": ["BSD-3-Clause", "MIT"],
        },
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--arch", required=True, choices=sorted(TARGETS))
    parser.add_argument("--repo-root", required=True, type=pathlib.Path)
    parser.add_argument("--rusty-v8-src", required=True, type=pathlib.Path)
    parser.add_argument("--gn-args", required=True)
    parser.add_argument("--archive", required=True, type=pathlib.Path)
    parser.add_argument("--binding", required=True, type=pathlib.Path)
    parser.add_argument("--dll", required=True, type=pathlib.Path)
    parser.add_argument("--implib", required=True, type=pathlib.Path)
    parser.add_argument("--output", required=True, type=pathlib.Path)
    parser.add_argument("--lock", required=True, type=pathlib.Path)
    parser.add_argument("--msvc-version", required=True)
    parser.add_argument("--sdk-version", required=True)
    parser.add_argument("--rustc-version", required=True)
    parser.add_argument("--tool", type=pathlib.Path)
    arguments = parser.parse_args()

    repo = arguments.repo_root.resolve()
    source = arguments.rusty_v8_src.resolve()
    products = {
        "archive": arguments.archive.resolve(),
        "binding": arguments.binding.resolve(),
        "dll": arguments.dll.resolve(),
        "implib": arguments.implib.resolve(),
    }
    for label, path in products.items():
        if not path.is_file():
            raise RuntimeError(f"missing V8 {label}: {path}")
    if not arguments.lock.is_file():
        raise RuntimeError(f"missing Windows V8 source lock: {arguments.lock}")

    rusty_v8_version = package_version(source / "Cargo.toml")
    rusty_v8_revision = git_revision(source, "rusty_v8 revision")
    v8_revision = git_revision(source / "v8", "V8 revision")
    gn_args = normalized_gn_arguments(arguments.gn_args)
    # Read from the actual `cargo build` invocation, not from the file as a
    # whole: build-v8-windows.sh discusses --no-default-features in a comment
    # explaining why cargo refuses it here, and a whole-file search reads that
    # explanation as configuration -- concluding the feature is off when the
    # build leaves it on. Same class of mistake as grepping for a flag's name
    # instead of the command that would carry it.
    recipe = (repo / "scripts/build-v8-windows.sh").read_text(encoding="utf-8")
    cargo_features = recipe_cargo_features(recipe)

    verify_against_lock(
        arguments.lock.resolve(),
        arch=arguments.arch,
        rusty_v8_version=rusty_v8_version,
        rusty_v8_revision=rusty_v8_revision,
        v8_revision=v8_revision,
        gn_args=gn_args,
        cargo_features=cargo_features,
    )

    component = build_component(
        arch=arguments.arch,
        rusty_v8_version=rusty_v8_version,
        rusty_v8_revision=rusty_v8_revision,
        v8_revision=v8_revision,
        gn_args=gn_args,
        archive_sha256=hash_file(products["archive"]),
        binding_sha256=hash_file(products["binding"]),
        # Passed in rather than queried here: this script runs from WSL, and
        # `rustc --version` from there reports WSL's own Linux toolchain, not
        # the pinned Windows one (rust-toolchain.toml) that actually built the
        # archive. build-v8-windows.sh captures it from inside the batch file,
        # after cd'ing into the checkout so the pin takes effect.
        rustc=arguments.rustc_version,
        compiler=f"MSVC {arguments.msvc_version} (cl.exe, {TARGETS[arguments.arch]['msvc_arch_label']})",
        sdk=f"Windows SDK {arguments.sdk_version}",
        linker=f"MSVC link.exe {arguments.msvc_version}",
        recipe_sha256=hash_file(repo / "scripts/build-v8-windows.sh"),
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
        prefix=".windows-v8-component.", suffix=".json", dir=arguments.output.parent
    )
    os.close(descriptor)
    draft = pathlib.Path(draft_name)
    try:
        draft.write_text(json.dumps(component, indent=2, sort_keys=True) + "\n")
        subprocess.run([str(tool), "seal-v8-component", str(draft), str(arguments.output)], check=True)
        subprocess.run(
            [
                str(tool),
                "verify-v8-component",
                str(arguments.output),
                str(products["archive"]),
                str(products["binding"]),
            ],
            check=True,
        )
    finally:
        draft.unlink(missing_ok=True)
    print(f"verified Windows V8 component manifest -> {arguments.output}")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except RuntimeError as error:
        print(f"ERROR: {error}", file=sys.stderr)
        raise SystemExit(1) from error
