#!/usr/bin/env bash
# Gate: the encoder behind `canvas.toDataURL()` writes images a decoder reads back.
#
# `engine/crates/runtime-v8/src/web/04_image_encode.js` is plain ESM with no imports, so Node can run it. The test decodes
# what it writes -- PNG exactly (Node's inflate and the PNG filters), JPEG by a small independent baseline decoder -- and
# requires the pixels back. The same bytes are decoded by the engine's own decoders on every platform in the conformance suite
# (`canvas2d-spec/to-data-url-*`); this is the check that needs no device.
#
# Drift this gate exists for: an encoder that is only ever looked at through its own decoder agrees with itself. This one
# is read back by Node's zlib, and the JPEG path by code that shares no line with the encoder's transform.
set -euo pipefail
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
command -v node > /dev/null || { echo "ERROR: node is required" >&2; exit 1; }
node "$ROOT_DIR/engine/crates/runtime-v8/tests/js/image-encode.test.mjs"
echo "PASS: canvas image encoder"
