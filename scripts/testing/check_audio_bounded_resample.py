#!/usr/bin/env python3
"""Compatibility CLI for the retired linear whole-file DSP experiment.

The linear owner and its tests have been removed after mature SRC adoption.
Run the current product Rubato gate; never compile an obsolete owner merely
so historical mutation anchors remain available.
"""
from __future__ import annotations
import argparse
from pathlib import Path
import check_audio_decode_rate as rate
import check_audio_io_renewal as support
ROOT=support.ROOT

def declarations(_originals):
    raise RuntimeError('linear DSP controls retired; use check_audio_product_rubato.declarations')

def main(work: Path, target: Path, positive_only: bool):
    import check_audio_product_rubato as current
    original=current.declarations
    try:
        if positive_only:
            current.declarations=lambda originals: []
        current.main(work, target, Path('/tmp/migo-w09-rubato-cargo'), False, False, False)
    finally:
        current.declarations=original

if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__)
    p.add_argument('--output',type=Path,required=True)
    p.add_argument('--target-dir',type=Path,required=True)
    p.add_argument('--positive-only',action='store_true')
    a=p.parse_args();main(a.output.resolve(),a.target_dir.resolve(),a.positive_only)
