"""Link-time cohort checks, separate from byte integrity and source provenance."""
import json
from pathlib import Path
import sys

try:
    import tomllib
except ImportError:
    raise SystemExit("V8 cohort checks require Python 3.11 or newer")


FIELDS = ("rusty_v8_version", "rusty_v8_revision", "v8_revision")
COMPRESSED_TARGETS = {
    f"{arch}-{suffix}"
    for arch in ("aarch64", "x86_64")
    for suffix in ("linux-android", "unknown-linux-gnu", "pc-windows-msvc")
}
UNCOMPRESSED_TARGETS = {
    f"{arch}-{suffix}"
    for arch in ("aarch64", "x86_64")
    for suffix in ("apple-darwin", "unknown-linux-ohos")
}
RUST_COMPRESSION_CFG = (
    'cfg(all(target_pointer_width = "64", not(target_env = "ohos"), '
    'any(target_os = "android", target_os = "linux", target_os = "windows")))'
)


def selected_cohort(root: Path) -> tuple:
    selected = [package for package in tomllib.loads((root / "engine/Cargo.lock").read_text())["package"]
                if package["name"] == "v8"]
    if len(selected) != 1:
        raise ValueError("Cargo must select exactly one v8 version")
    cohorts = []
    for platform in ("android", "apple", "ohos", "windows"):
        lock = json.loads((root / f"contracts/artifact-manifest/{platform}-v8.lock.json").read_text())
        cohorts.append(tuple(lock[field] for field in FIELDS))
    if len(set(cohorts)) != 1:
        raise ValueError("native platform locks select different V8 cohorts")
    if selected[0]["version"] != cohorts[0][0]:
        raise ValueError("Cargo and native locks select different V8 versions")
    return cohorts[0]


def verify_rust_compression_policy(root: Path) -> None:
    """Keep the native target table tied to the actual Rust feature declaration.

    Reject unrecognized policies rather than guessing how a changed cfg resolves.
    A new target policy must update this check and its target matrix together.
    """
    manifest = tomllib.loads((root / "engine/crates/runtime-v8/Cargo.toml").read_text())
    users = [(cfg, target["dependencies"]["deno_core"])
             for cfg, target in manifest.get("target", {}).items()
             if "deno_core" in target.get("dependencies", {})]
    if (len(users) != 1 or users[0][0] != RUST_COMPRESSION_CFG
            or "v8_enable_pointer_compression" not in users[0][1].get("features", [])):
        raise ValueError("Rust V8 pointer-compression target policy differs from the native ABI matrix")
    workspace = tomllib.loads((root / "engine/Cargo.toml").read_text())
    base = workspace["workspace"]["dependencies"]["deno_core"]
    if base.get("default-features", True) or "v8_enable_pointer_compression" in base.get("features", []):
        raise ValueError("Rust V8 pointer compression must be selected only by the target policy")


def verify_component(root: Path, manifest: Path) -> None:
    expected = selected_cohort(root)
    value = json.loads(manifest.read_text())
    actual = tuple(value["runtime"][field] for field in FIELDS)
    if actual != expected:
        raise ValueError(f"native component differs from selected cohort: actual={actual}, expected={expected}")
    verify_rust_compression_policy(root)
    triple = value["target"]["triple"]
    if triple not in COMPRESSED_TARGETS | UNCOMPRESSED_TARGETS:
        raise ValueError(f"unsupported V8 consumer target: {triple}")
    args = value["runtime"]["normalized_gn_args"]
    values = [arg.split("=", 1)[1] for arg in args if arg.startswith("v8_enable_pointer_compression=")]
    wanted = "true" if triple in COMPRESSED_TARGETS else "false"
    if values != [wanted]:
        raise ValueError(f"native pointer-compression ABI differs for {triple}: {values}, expected {wanted}")


def main() -> None:
    try:
        root, manifest = map(Path, sys.argv[1:])
        verify_component(root.resolve(), manifest.resolve())
    except (OSError, ValueError, KeyError, TypeError) as error:
        raise SystemExit(f"V8 component cohort rejected: {error}")


if __name__ == "__main__":
    main()
