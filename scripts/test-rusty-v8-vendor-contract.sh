#!/usr/bin/env bash
# The Windows DLL callback patch must reach the Rust crate Migo actually uses,
# not only the separate checkout that builds the native archive. Verify the
# published archive, recorded Rust-only consumer projection/patches and actual
# Cargo selection. Native source builds use their separate source lock.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
python3 "$ROOT/scripts/testing/check_deno_core_vendor.py" "$ROOT" rusty-v8-rust-patches
python3 "$ROOT/scripts/testing/check_windows_v8_callbacks.py" "$ROOT"
python3 -m unittest discover -s "$ROOT/scripts/ci/tests" -p 'test_v8_consumer.py' -v
