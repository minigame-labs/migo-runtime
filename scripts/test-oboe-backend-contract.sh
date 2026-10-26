#!/usr/bin/env bash
# Updating the C++ beneath oboe-rs 0.6 would retain its old inline builder
# layout. Keep Oboe native objects opaque, prove the source projection and run
# the actual C++ bridge against controllable failure/error-owner lifetimes.
# The host fixture is an ownership check; Android compilation/device results
# remain separate evidence.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
python3 "$ROOT/scripts/testing/check_oboe_vendor.py" "$ROOT"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
"${CXX:-c++}" -std=c++17 -pthread -O1 -g -fsanitize=address,undefined \
    -Wall -Wextra -Werror \
    -I"$ROOT/scripts/testing/fixtures/oboe" -I"$ROOT/engine/crates/audio/native" \
    "$ROOT/scripts/testing/fixtures/oboe/bridge_test.cpp" \
    "$ROOT/engine/crates/audio/native/oboe_bridge.cpp" -o "$WORK/bridge-test"
"$WORK/bridge-test"
echo 'Oboe C ABI: open/failure/exception/close/error-owner/AAudio/OpenSL ES fixture PASS (ASan/UBSan)'
