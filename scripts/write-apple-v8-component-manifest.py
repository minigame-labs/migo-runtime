#!/usr/bin/env python3
"""Write the component manifest for a macOS librusty_v8.a.

Counterpart to write-{linux,ohos,windows}-v8-component-manifest.py, and the one
whose shape was decided before it existed: tools/artifact-manifest already carries
`validate_apple_v8_target`, which requires the triple, `os = "macos"`, `abi =
"darwin"`, the per-arch CPU baseline, and exactly one `macos` entry in
`runtime_floor` spelled `major.minor`. This writer produces what that validator
demands and then asks the tool to compute `component_id`, rather than computing an
identity the tool would disagree with.

TWO THINGS IT DOES DIFFERENTLY FROM ITS SIBLINGS, both because the Apple lock is
different on purpose:

  * It does not compare GN arguments against the lock. Every other platform's lock
    pins `normalized_gn_args`; apple-v8.lock.json deliberately does not, and says
    why in its own notes -- the argument list is exported by the recipe and
    recorded here as a description of what a real build used, rather than a claim
    typed in advance. So identity is verified (versions, revisions, triple) and the
    arguments are recorded.

  * It records the floor the ARCHIVE carries, not the floor the build was asked
    for. scripts/build-v8-apple.sh reads `minos` out of an object with vtool and
    refuses to continue when it disagrees with the contract, so by the time this
    runs the two agree -- and writing the observed value keeps this file a record
    of bytes rather than of intentions. The probe measured an archive carrying
    `minos 12.0` against a declared floor of 11.0, which is exactly the drift a
    manifest full of intentions would have described as compliant.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import pathlib
import subprocess
import sys
import tempfile

TARGETS = {
    "aarch64": {
        "triple": "aarch64-apple-darwin",
        "cpu_baseline": "armv8-a",
        "required_cpu_features": ["neon"],
    },
    "x86_64": {
        "triple": "x86_64-apple-darwin",
        "cpu_baseline": "x86-64-v1",
        "required_cpu_features": ["cmov", "sse2"],
    },
}


def run(command: list[str], label: str) -> str:
    try:
        completed = subprocess.run(
            command, check=True, capture_output=True, text=True, encoding="utf-8"
        )
    except (OSError, subprocess.CalledProcessError) as error:
        raise RuntimeError(f"{label} failed: {error}") from error
    return completed.stdout.strip()


def hash_file(path: pathlib.Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        # Chunked, because the archive is about 126 MiB and reading it whole to
        # hash it doubles that for no reason.
        for block in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def git_revision(path: pathlib.Path, label: str) -> str:
    return run(["git", "-C", str(path), "rev-parse", "HEAD"], f"reading {label} revision")


def package_version(cargo_toml: pathlib.Path) -> str:
    """The `version` of the first [package] table, read without a TOML parser.

    The same approach the sibling writers take: this file has to run on a build
    machine with nothing installed beyond what the build itself needs.
    """
    in_package = False
    for line in cargo_toml.read_text(encoding="utf-8").splitlines():
        stripped = line.strip()
        if stripped.startswith("["):
            in_package = stripped == "[package]"
            continue
        if in_package and stripped.startswith("version"):
            _, _, value = stripped.partition("=")
            return value.strip().strip('"')
    raise RuntimeError(f"no [package] version in {cargo_toml}")


def normalized_gn_arguments(value: str) -> list[str]:
    """The build's own args.gn, minus keys that name the machine.

    Sorted and de-duplicated so two builds of one revision on two machines produce
    the same list -- which is what makes the identity below comparable at all.
    """
    local_keys = {"clang_base_path"}
    arguments: list[str] = []
    for line in value.splitlines():
        stripped = line.strip()
        if not stripped or stripped.startswith("#"):
            continue
        key = stripped.split("=", 1)[0].strip()
        if key in local_keys:
            continue
        # Spaces around `=` are how gn writes args.gn and are not part of the
        # argument; normalising them here keeps a formatting change out of the
        # component identity.
        name, _, raw = stripped.partition("=")
        arguments.append(f"{name.strip()}={raw.strip()}")
    return sorted(dict.fromkeys(arguments))


def verify_against_lock(
    lock_path: pathlib.Path,
    *,
    arch: str,
    rusty_v8_version: str,
    rusty_v8_revision: str,
    v8_revision: str,
    deployment_target: str,
) -> None:
    """Refuses to describe a build that drifted from the pinned sources.

    Identity only, and that is the difference from the sibling writers rather than
    an omission: apple-v8.lock.json pins which rusty_v8, which V8 and which
    targets, and deliberately pins no GN arguments. Comparing against arguments it
    does not carry would be comparing against nothing while looking thorough.
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

    # The floor is stated in two files and one gate holds them equal. Refusing here
    # rather than picking one: a manifest that recorded a floor neither file agreed
    # with would be the only record of the disagreement.
    pinned_floor = (lock.get("macos") or {}).get("deployment_target")
    if pinned_floor != deployment_target:
        raise RuntimeError(
            f"{lock_path.name} pins macOS {pinned_floor!r} and the build used "
            f"{deployment_target!r}"
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
    observed_floor: str,
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
            # `macos`/`darwin` and not `apple`: rustc reports target_os = "macos"
            # and the triple's last component is darwin. `target_vendor = "apple"`
            # would be the wrong spelling because it covers iOS, and this component
            # exists only for macOS -- on iOS the content JavaScript runs in
            # WebKit's process to get the system JIT.
            "os": "macos",
            "arch": arch,
            "abi": "darwin",
            "cpu_baseline": target_spec["cpu_baseline"],
            "required_cpu_features": target_spec["required_cpu_features"],
            # An OS version rather than a library version, unlike Linux's
            # glibc/glibcxx pair: what a consumer's Mac must be new enough for is
            # the deployment target the archive was compiled against. This is the
            # OBSERVED value -- what vtool read out of the bytes -- and the recipe
            # has already refused to get this far if it disagreed with the contract.
            "runtime_floor": {"macos": observed_floor},
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
            # Apple needs none. Recorded as empty rather than omitted, because an
            # absent field and an empty one read the same to a person and
            # differently to a comparison -- and this platform having no patches is
            # a fact worth being able to see.
            "patches": [],
        },
        "hashes": {
            "archive": archive_sha256,
            "rust_binding": binding_sha256,
        },
        "provenance": {
            "source_revision": rusty_v8_revision,
            "build_recipe": "scripts/build-v8-apple.sh",
            "build_recipe_sha256": recipe_sha256,
            "licenses": ["BSD-3-Clause", "MIT"],
        },
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--arch", required=True, choices=sorted(TARGETS))
    parser.add_argument("--repo-root", required=True, type=pathlib.Path)
    parser.add_argument("--rusty-v8-src", required=True, type=pathlib.Path)
    parser.add_argument("--gn-args", required=True, help="the build's verbatim args.gn")
    parser.add_argument("--archive", required=True, type=pathlib.Path)
    parser.add_argument("--binding", required=True, type=pathlib.Path)
    parser.add_argument("--observed-floor", required=True, help="what vtool read, major.minor")
    parser.add_argument("--deployment-target", required=True)
    parser.add_argument("--output", required=True, type=pathlib.Path)
    parser.add_argument("--lock", required=True, type=pathlib.Path)
    parser.add_argument("--rustc-version", required=True)
    parser.add_argument("--compiler", required=True)
    parser.add_argument("--sdk", required=True)
    parser.add_argument("--linker", required=True)
    parser.add_argument("--tool", type=pathlib.Path)
    args = parser.parse_args()

    repo = args.repo_root.resolve()
    rusty_v8_src = args.rusty_v8_src.resolve()

    rusty_v8_revision = git_revision(rusty_v8_src, "rusty_v8")
    rusty_v8_version = package_version(rusty_v8_src / "Cargo.toml")
    v8_revision = git_revision(rusty_v8_src / "v8", "V8")

    verify_against_lock(
        args.lock,
        arch=args.arch,
        rusty_v8_version=rusty_v8_version,
        rusty_v8_revision=rusty_v8_revision,
        v8_revision=v8_revision,
        deployment_target=args.deployment_target,
    )

    recipe = repo / "scripts/build-v8-apple.sh"
    component = build_component(
        arch=args.arch,
        rusty_v8_version=rusty_v8_version,
        rusty_v8_revision=rusty_v8_revision,
        v8_revision=v8_revision,
        gn_args=normalized_gn_arguments(args.gn_args),
        archive_sha256=hash_file(args.archive),
        binding_sha256=hash_file(args.binding),
        observed_floor=args.observed_floor,
        rustc=args.rustc_version,
        compiler=args.compiler,
        sdk=args.sdk,
        linker=args.linker,
        recipe_sha256=hash_file(recipe),
    )

    # The identity is computed by the tool that validates it, never here. Two
    # implementations of one hash is two hashes, and the one that matters is the
    # one the verifier uses. Then the same tool is asked to verify the sealed
    # manifest against the actual files -- writing a manifest and not checking it
    # describes what the writer believed rather than what is on disk.
    tool = args.tool
    if tool is None:
        built = repo / "tools/artifact-manifest/target/release/migo-artifact-manifest"
        if not built.is_file():
            run(
                [
                    "cargo",
                    "build",
                    "--release",
                    "--locked",
                    "--manifest-path",
                    str(repo / "tools/artifact-manifest/Cargo.toml"),
                ],
                "building migo-artifact-manifest",
            )
        tool = built
    tool = tool.resolve()

    args.output.parent.mkdir(parents=True, exist_ok=True)
    descriptor, draft_name = tempfile.mkstemp(
        prefix=".apple-v8-component.", suffix=".json", dir=args.output.parent
    )
    os.close(descriptor)
    draft = pathlib.Path(draft_name)
    try:
        draft.write_text(json.dumps(component, indent=2, sort_keys=True) + "\n", encoding="utf-8")
        run([str(tool), "seal-v8-component", str(draft), str(args.output)], "sealing the manifest")
        run(
            [str(tool), "verify-v8-component", str(args.output), str(args.archive), str(args.binding)],
            "verifying the sealed manifest against the files it describes",
        )
    finally:
        draft.unlink(missing_ok=True)

    stamped = json.loads(args.output.read_text(encoding="utf-8"))
    print(f"verified Apple V8 component manifest -> {args.output}")
    print(f"component_id {stamped['component_id']}")
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except RuntimeError as error:
        print(f"ERROR: {error}", file=sys.stderr)
        sys.exit(1)
