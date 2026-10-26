#!/usr/bin/env python3
"""Keep Linux FFI parsing on the same pinned target libc as GN compilation.

Without explicit target/sysroot arguments, ARM64 bindgen found host features.h
and failed on bits/wordsize.h after the entire C++ build had succeeded. Native
x64 parsing also silently used the host libc instead of the shipping floor.
"""
import argparse
import json
import os
from pathlib import Path
import shlex


TARGETS = {
    "x86_64-unknown-linux-gnu": ("amd64", "x86_64-linux-gnu"),
    "aarch64-unknown-linux-gnu": ("arm64", "aarch64-linux-gnu"),
}


def linux_arguments(source, target, environment):
    arch, multiarch = TARGETS[target]
    source = Path(source).resolve()
    recipe = source / "build/linux/sysroot_scripts/sysroots.json"
    entry = json.loads(recipe.read_text())[f"bullseye_{arch}"]
    sysroot = source / "build/linux" / entry["SysrootDir"]
    expected_stamp = entry["URL"].rstrip("/") + "/" + entry["Sha256Sum"]
    stamp = sysroot / ".stamp"
    if not stamp.is_file() or stamp.read_text().strip() != expected_stamp:
        raise ValueError(f"bindgen sysroot does not match the pinned recipe: {sysroot}")
    for header in ("features.h", f"{multiarch}/bits/wordsize.h"):
        if not (sysroot / "usr/include" / header).is_file():
            raise ValueError(f"bindgen sysroot is missing target header: {header}")
    # Match bindgen's target-specific environment precedence. Preserve quoted
    # include/resource paths, then make the recipe's target and sysroot final.
    keys = [f"BINDGEN_EXTRA_CLANG_ARGS_{target}",
            f"BINDGEN_EXTRA_CLANG_ARGS_{target.replace('-', '_')}",
            "BINDGEN_EXTRA_CLANG_ARGS"]
    inherited = next((environment[key] for key in keys if key in environment), "")
    return shlex.join(shlex.split(inherited) +
                      [f"--target={target}", f"--sysroot={sysroot}"])


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", type=Path, required=True)
    parser.add_argument("--target", choices=TARGETS, required=True)
    args = parser.parse_args()
    print(linux_arguments(args.source, args.target, os.environ))


if __name__ == "__main__":
    main()
