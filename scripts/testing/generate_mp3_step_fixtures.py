#!/usr/bin/env python3
"""Regenerate synthetic tone MP3 inputs; never overwrite PCM reference outputs.

The f32le references came from the recorded pre-cursor minimp3 implementation.
Replacing those with the implementation under test would erase the comparison.
"""
import argparse
from pathlib import Path
import subprocess

ROOT = Path(__file__).resolve().parents[2]


def main(output: Path):
    output.mkdir(parents=True, exist_ok=True)
    for name, source, channels, quality in [
        ('stereo-cbr', 'sine=frequency=997:sample_rate=44100:duration=0.12', 2, ['-b:a', '128k']),
        ('mono-vbr', 'sine=frequency=733:sample_rate=22050:duration=0.17', 1, ['-q:a', '2']),
    ]:
        subprocess.run([
            'ffmpeg', '-hide_banner', '-loglevel', 'error', '-nostdin', '-y',
            '-f', 'lavfi', '-i', source, '-ac', str(channels),
            '-c:a', 'libmp3lame', *quality, '-write_xing', '0',
            '-id3v2_version', '3', '-map_metadata', '-1', str(output / (name + '.mp3')),
        ], check=True)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    main(parser.parse_args().output.resolve())
