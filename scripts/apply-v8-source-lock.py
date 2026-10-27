#!/usr/bin/env python3
"""Apply Apple's top-level V8 source patches without a Bash 4 dependency."""
from pathlib import Path
import argparse
import sys

sys.path.insert(0, str(Path(__file__).resolve().parent / "lib"))
import v8_source_proof

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--source", required=True, type=Path)
parser.add_argument("--lock", required=True, type=Path)
parser.add_argument("--patch-dir", required=True, type=Path)
args = parser.parse_args()
patches = v8_source_proof.locked_patches(args.lock, args.patch_dir)
v8_source_proof.apply_locked_top_level_patches(args.source.resolve(), patches)
print(f"verified source patches: {len(patches)}")
