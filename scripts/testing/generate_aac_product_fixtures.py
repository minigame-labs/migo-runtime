#!/usr/bin/env python3
"""Reproduce the original W09 AAC media and freeze the product's test subset.

The output is necessary test input, not a build cache. Generation logs, source
PCM and unused candidate media stay in the caller's separate work directory.
"""
from __future__ import annotations
import argparse
import hashlib
import json
from pathlib import Path
import shutil
import check_audio_complete_codecs as complete


def generate(work: Path, output: Path) -> list[dict]:
    work.mkdir(parents=True, exist_ok=False)
    env = {}
    complete.fixtures(work, env)
    source = Path(env['W09_COMPLETE_FIXTURES'])
    rows = json.loads((source / 'complete-m4a.json').read_text())
    rows += [r for r in json.loads((source / 'elementary.json').read_text()) if r['file'].endswith('.aac')]
    output.mkdir(parents=True, exist_ok=False)
    names = [r['file'] for r in rows] + ['valid-many.m4a', 'edit-raw.m4a']
    names += [f'adts-{i}.f32le' for i in range(3)]
    for name in names:
        shutil.copy2(source / name, output / name)
    (output / 'manifest.json').write_text(json.dumps(rows, indent=2) + '\n')
    (output / 'sha256.json').write_text(json.dumps({p.name: hashlib.sha256(p.read_bytes()).hexdigest()
        for p in sorted(output.iterdir()) if p.is_file()}, indent=2) + '\n')
    return rows


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--work', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    generate(args.work.resolve(), args.output.resolve())
