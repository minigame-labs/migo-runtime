"""Cross-check production CPAL/output/session Rust without an Apple SDK.

The engine-free product graph is checked separately. This isolates native Rust
APIs from unrelated Skia/Ada C++ builds; it does not link or execute iOS code.
Resolve from the workspace lock, then reject any changed dependency identity.
"""
from __future__ import annotations

import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tomllib


def identity(package: dict) -> tuple:
    return tuple(package.get(key) for key in ("name", "version", "source", "checksum"))


def check(root: Path, scratch: Path) -> None:
    consumer = scratch / "ios-audio-source"
    (consumer / "src").mkdir(parents=True)
    audio = root / "engine/crates/audio/src"
    shared = root / "engine/crates/shared"
    cpal = root / "engine/third_party/cpal-0.18.2"
    # JSON strings are valid TOML/Rust string literals for these file paths.
    audio_manifest = tomllib.loads((audio.parent / "Cargo.toml").read_text())
    avf = audio_manifest["target"]['cfg(target_os = "ios")']["dependencies"]["objc2-avf-audio"]
    assert avf["default-features"] is False
    avf_dependency = ("objc2-avf-audio = { version = " + json.dumps(avf["version"])
                      + ", default-features = false, features = " + json.dumps(avf["features"]) + " }\n")
    manifest = f'''[package]
name = "migo-ios-audio-source-check"
version = "0.0.0"
edition = "2024"
[workspace]
[dependencies]
shared = {{ package = "migo-shared", path = {json.dumps(str(shared))}, default-features = false }}
cpal = {{ path = {json.dumps(str(cpal))}, default-features = false }}
crossbeam-utils = "0.8"
ringbuf = "0.5"
tracing = "0.1"
'''
    (consumer / "Cargo.toml").write_text(manifest + avf_dependency)
    (consumer / "src/lib.rs").write_text(
        '#![allow(dead_code)]\n' + ''.join(
            f'#[path = {json.dumps(str(audio / (name + ".rs")))}]\n'
            f'pub mod {name};\n'
            for name in ("apple_session_policy", "apple_session", "output", "platform")
        )
    )
    lock = root / "engine/Cargo.lock"
    original = lock.read_bytes()
    shutil.copyfile(lock, consumer / "Cargo.lock")
    # Removing unrelated workspace entries/addition of this probe may update
    # the temporary lock. Every remaining identity must still match the real
    # workspace before accepting the subsequent --locked check.
    metadata = subprocess.run(
        ["cargo", "metadata", "--format-version", "1", "--filter-platform", "aarch64-apple-ios"],
        cwd=consumer, check=True, stdout=subprocess.PIPE, text=True,
    )
    packages = json.loads(metadata.stdout)["packages"]
    selected = [package for package in packages if package["name"] == "cpal"]
    assert len(selected) == 1 and Path(selected[0]["manifest_path"]).resolve() == cpal / "Cargo.toml", \
        "the source check selected a different CPAL backend"
    resolved = tomllib.loads((consumer / "Cargo.lock").read_text())
    expected = {identity(package) for package in tomllib.loads(original.decode())["package"]}
    unexpected = [identity(package) for package in resolved["package"]
                  if package["name"] != "migo-ios-audio-source-check"
                  and identity(package) not in expected]
    assert not unexpected, f"isolated probe drifted from workspace dependencies: {unexpected}"
    environment = os.environ.copy()
    environment.setdefault("CARGO_TARGET_DIR", str(scratch / "ios-audio-target"))
    subprocess.run(["cargo", "check", "--locked", "--target", "aarch64-apple-ios"],
                   cwd=consumer, env=environment, check=True)
    assert lock.read_bytes() == original, "the source probe changed the real workspace lock"
    print(f"iOS production CPAL/output/session Rust: {len(resolved['package']) - 1} "
          "workspace dependency identities; --locked target check PASS (no native/device claim)")


if __name__ == "__main__":
    check(Path(sys.argv[1]).resolve(), Path(sys.argv[2]).resolve())
