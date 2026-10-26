#!/usr/bin/env python3
"""Reject a stale native checkout before compiling the selected Cargo cohort."""
from pathlib import Path
import json
import subprocess
import sys
import tomllib

sys.path.insert(0, str(Path(__file__).resolve().parent / "lib"))
from v8_cohort import selected_cohort


def revision(source: Path) -> str:
    return subprocess.check_output(
        ["git", "-c", f"safe.directory={source.resolve()}", "-C", str(source), "rev-parse", "HEAD"],
        text=True,
    ).strip()


def verify(root: Path, source: Path) -> None:
    expected = selected_cohort(root)
    version = tomllib.loads((source / "Cargo.toml").read_text())["package"]["version"]
    actual = (version, revision(source), revision(source / "v8"))
    if actual != expected:
        raise ValueError(f"native checkout differs from selected cohort: actual={actual}, expected={expected}")
    print(f"native V8 source / Cargo / four platform locks: {expected[0]} PASS")


if __name__ == "__main__":
    try:
        verify(Path(sys.argv[1]).resolve(), Path(sys.argv[2]).resolve())
    except (OSError, ValueError, KeyError, subprocess.CalledProcessError) as error:
        raise SystemExit(f"V8 source cohort rejected: {error}")
