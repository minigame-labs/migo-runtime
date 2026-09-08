#!/usr/bin/env bash
# Host fixture tests. Native compilers and Xcode are mocked, not the packager.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
python3 "$ROOT/scripts/ci/tests/test_apple_sdk_packaging.py"
