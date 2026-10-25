#!/usr/bin/env bash
# Ada upstream forced /MT and a global NODEFAULTLIB into embedding DLLs.
# Keep the narrow CRT correction reproducible; withheld patches and stray
# edits must fail the same byte-for-byte proof as the core/V8 forks.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
python3 "$ROOT/scripts/testing/check_deno_core_vendor.py" "$ROOT" ada-url-patches
