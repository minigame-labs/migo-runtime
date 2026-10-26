#!/usr/bin/env bash
# Broad lockfile/build rules previously hid reproducible vendor inputs while
# browser reports stayed tracked. Verify source/output boundaries independently
# of the main Git index, including missing-checkout and nested-rule regressions.
# Raw vendor/patch bytes can contain required whitespace; exempt that data while
# proving authored engine code still fails the ordinary whitespace check.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
python3 -m unittest discover -s "$ROOT/scripts/lib" -p 'test_gitignore_policy.py' -v
python3 "$ROOT/scripts/testing/check_gitignore_contract.py"
