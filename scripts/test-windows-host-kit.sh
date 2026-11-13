#!/usr/bin/env bash
# Build and exercise the Windows Host Kit (platforms/windows/host-kit) without
# building the Migo engine or V8: the tests link a strict fake of the public C
# ABI (tests/host_kit_fake), as the Linux Host Kit's do.
#
# Runs on Windows, from Git Bash with an MSVC developer environment (cl, link,
# the Windows SDK on PATH and in INCLUDE/LIB) -- PR CI's windows-host-kit job,
# or a Developer PowerShell's bash.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SOURCE_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
HOST_KIT_ROOT="$SOURCE_ROOT/platforms/windows/host-kit"

fail() {
    printf 'Windows Host Kit contract: FAIL: %s\n' "$*" >&2
    exit 1
}

for command in cmake ninja cl; do
    command -v "$command" > /dev/null 2>&1 || fail "missing command '$command' (run inside an MSVC developer environment)"
done
[[ -f "$HOST_KIT_ROOT/CMakeLists.txt" ]] || fail "no Host Kit at $HOST_KIT_ROOT"

BUILD_DIR="$(mktemp -d "${TMPDIR:-/tmp}/migo-windows-host-kit.XXXXXX")"
cleanup() { rm -rf -- "$BUILD_DIR"; }
trap cleanup EXIT

cmake -S "$HOST_KIT_ROOT/tests" -B "$BUILD_DIR" -G Ninja \
    -DCMAKE_BUILD_TYPE=Release \
    -DCMAKE_CXX_COMPILER=cl \
    -DMIGO_REPOSITORY_ROOT="$(cygpath -m "$SOURCE_ROOT")"
cmake --build "$BUILD_DIR" --parallel
ctest --test-dir "$BUILD_DIR" --output-on-failure

printf 'Windows Host Kit contract: PASS\n'
