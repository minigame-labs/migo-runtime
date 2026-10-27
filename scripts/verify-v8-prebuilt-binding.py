#!/usr/bin/env python3
"""Refuse an old or unidentified FFI binding before a costly V8 build."""
from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path
import subprocess
import tomllib


def verify(source: Path, binding: Path, manifest: Path) -> None:
    source = source.resolve()
    recorded = json.loads(manifest.read_text())
    version = tomllib.loads((source / "Cargo.toml").read_text())["package"]["version"]
    revision = subprocess.check_output(
        ["git", "-c", f"safe.directory={source}", "-C", str(source), "rev-parse", "HEAD"],
        text=True,
    ).strip()
    runtime = recorded["runtime"]
    if runtime["rusty_v8_version"] != version:
        raise ValueError(f"binding is for V8 {runtime['rusty_v8_version']}, source is {version}")
    if runtime["rusty_v8_revision"] != revision:
        raise ValueError("binding source revision does not match the checkout")
    actual = hashlib.sha256(binding.read_bytes()).hexdigest()
    if recorded["hashes"]["rust_binding"] != actual:
        raise ValueError("binding bytes do not match their component manifest")
    print(f"verified prebuilt V8 binding: {version} {revision}")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("source", "binding", "manifest"):
        parser.add_argument(f"--{name}", required=True, type=Path)
    args = parser.parse_args()
    try:
        verify(args.source, args.binding, args.manifest)
    except (OSError, ValueError, KeyError, subprocess.CalledProcessError) as error:
        parser.exit(1, f"prebuilt V8 binding rejected: {error}\n")
