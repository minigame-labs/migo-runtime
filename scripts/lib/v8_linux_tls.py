#!/usr/bin/env python3
"""Reject non-shared TLS relocations using readelf, including cross archives.

The host objdump could not decode ARM64 and its suppressed error produced an
empty report that looked safe. Require successful parsing and target relocs.
"""
import argparse
from collections import Counter
import re
import subprocess


def validate(counts, arch):
    prefix = {"aarch64": "R_AARCH64_", "x86_64": "R_X86_64_"}[arch]
    if not any(key.startswith(prefix) for key in counts):
        raise ValueError(f"readelf produced no {arch} relocations")
    tls = {key: value for key, value in counts.items() if key.startswith(prefix) and "TLS" in key}
    forbidden = [key for key in counts if key.startswith("R_AARCH64_TLSLE_")
                 or key == "R_X86_64_TPOFF32"]
    if forbidden:
        raise ValueError(f"local-exec TLS cannot be linked into a shared library: {forbidden}")
    return tls


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--arch", choices=("aarch64", "x86_64"), required=True)
    parser.add_argument("archive")
    args = parser.parse_args()
    counts = Counter()
    # Stream the potentially large relocation table; do not retain it in RAM.
    process = subprocess.Popen(["readelf", "-Wr", args.archive], stdout=subprocess.PIPE, text=True)
    for line in process.stdout:
        match = re.search(r"\bR_(?:AARCH64|X86_64)_\w+", line)
        if match:
            counts[match.group()] += 1
    if process.wait():
        raise RuntimeError("readelf could not decode the archive")
    print("shared-library TLS verified:", validate(counts, args.arch))


if __name__ == "__main__":
    main()
