#!/usr/bin/env bash
# ffmpeg's own test pattern at 32x16, in the formats the image crate decodes for Migo: lossy
# WebP (VP8, what mini-game packages ship), lossless WebP (VP8L) and GIF. Generated, so they
# carry nobody's artwork; committed, so the tests need no ffmpeg.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"
pattern=(-loglevel error -y -f lavfi -i "testsrc=size=32x16:rate=1" -frames:v 1)
ffmpeg "${pattern[@]}" testsrc-lossy.webp
ffmpeg "${pattern[@]}" -lossless 1 testsrc-lossless.webp
ffmpeg "${pattern[@]}" testsrc.gif
