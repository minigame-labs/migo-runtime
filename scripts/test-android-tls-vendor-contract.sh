#!/usr/bin/env bash
# The upstream verifier AAR changes the host's global networkSecurityConfig,
# and its POM omits Kotlin classes the bytecode calls. The original helper also
# selects an explicit trust store. Package a reproducible source-patched JNI
# jar, keep the host's policy, and prove the Rust/JVM cohort, runtime and
# R8 names cannot silently drift.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
python3 "$ROOT/scripts/lib/android_tls_vendor.py"
python3 -m unittest discover -s "$ROOT/scripts/lib" -p 'test_android_*deps.py' -v
python3 -m unittest discover -s "$ROOT/scripts/lib" -p 'test_android_tls_vendor.py' -v
