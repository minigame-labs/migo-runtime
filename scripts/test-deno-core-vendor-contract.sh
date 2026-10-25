#!/usr/bin/env bash
# Zero-delay HTML tasks must reuse core's scheduler without silently changing
# Node's default minimum. Keep the vendored core reproducible from its published
# crate and the minimal patch, and prove that missing patches / stray edits fail.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
python3 "$ROOT/scripts/testing/check_deno_core_vendor.py" "$ROOT"
