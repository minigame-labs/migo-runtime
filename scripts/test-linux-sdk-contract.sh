#!/usr/bin/env bash
# The Linux SDK's contract gate. The loader floor it upholds is defined by
# scripts/abi-floor-audit.py.
#
# Every check fails the build outright. There is no warn-and-continue path: an
# artifact that silently misses the loader floor is worse than no artifact,
# because the consumer discovers it at load time on a machine we cannot see.
#
# Usage:
#   scripts/test-linux-sdk-contract.sh [aarch64|x86_64]            # skips
#                                       shared-object checks when no libmigo.so
#                                       is staged (default arch: x86_64)
#   scripts/test-linux-sdk-contract.sh [aarch64|x86_64] --strict   # any skip
#                                       is a failure
#
# --strict is the mode the C ABI v1 freeze gate runs in.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
AUDIT="$SCRIPT_DIR/abi-floor-audit.py"
ARCH="x86_64"
STRICT=0
for arg in "$@"; do
    case "$arg" in
        aarch64|x86_64) ARCH="$arg" ;;
        --strict) STRICT=1 ;;
        *) echo "usage: $0 [aarch64|x86_64] [--strict]" >&2; exit 2 ;;
    esac
done
case "$ARCH" in
    aarch64) TARGET="aarch64-unknown-linux-gnu" ;;
    x86_64)  TARGET="x86_64-unknown-linux-gnu" ;;
esac
# Public-facing directory word only, matching build-linux-sdk.sh's own
# aarch64->arm64 rendering (see its PUBLIC_ARCH comment). $ARCH keeps migo's
# internal spelling everywhere else below -- the manifest filename included,
# since gen-linux-package-metadata.py names it from args.arch, not the public
# word.
PUBLIC_ARCH="$ARCH"
[[ "$ARCH" == "aarch64" ]] && PUBLIC_ARCH="arm64"
PREFIX="${MIGO_PREFIX:-$REPO_ROOT/dist/migo-linux-$PUBLIC_ARCH}"

FAILURES=0
SKIPS=0
pass() { echo -e "\033[0;32mPASS\033[0m  $*"; }
fail() { echo -e "\033[0;31mFAIL\033[0m  $*"; FAILURES=$((FAILURES + 1)); }
skip() { echo -e "\033[0;33mSKIP\033[0m  $*"; SKIPS=$((SKIPS + 1)); }

[[ -d "$PREFIX" ]] || {
    echo "no staged package at $PREFIX; run scripts/build-linux-sdk.sh $ARCH" >&2
    exit 1
}

MANIFEST="$PREFIX/share/migo/linux-$ARCH-manifest.json"
SHARED_LIB="$PREFIX/lib/libmigo.so"
FLOOR_BINARY="$REPO_ROOT/engine/target/$TARGET/release/migo-c-host"

# --- 1. Loader floor -------------------------------------------------------
# Measured on the sysroot-built reference consumer: symbol versions bind at link
# time, so a static archive has none of its own to check.
if [[ -x "$FLOOR_BINARY" ]]; then
    if python3 "$AUDIT" floor "$FLOOR_BINARY"; then
        pass "loader floor (sysroot-built reference consumer)"
    else
        fail "loader floor: $FLOOR_BINARY requires symbols above GLIBC_2.31 / GLIBCXX_3.4.28"
    fi
else
    fail "no sysroot-built reference consumer at $FLOOR_BINARY; run scripts/build-linux-sdk.sh"
fi

if [[ -f "$SHARED_LIB" ]]; then
    if python3 "$AUDIT" floor "$SHARED_LIB"; then
        pass "loader floor (libmigo.so)"
    else
        fail "loader floor: libmigo.so requires symbols above the floor"
    fi
fi

# --- 2. Export surface -----------------------------------------------------
# The shared library must expose the documented ABI and nothing else. Leaking a
# Rust, V8, Skia or ICU symbol would let a host bind to it, turning an internal
# change into an ABI break.
if [[ -f "$SHARED_LIB" ]]; then
    DECLARED="$(python3 "$REPO_ROOT/scripts/c-abi-entry-points.py" embedded "$REPO_ROOT/include/migo")"
    EXPORTED="$(python3 "$AUDIT" exports "$SHARED_LIB" | sort -u)"
    if [[ "$DECLARED" == "$EXPORTED" ]]; then
        pass "export surface is exactly the declared migo_* set"
    else
        fail "export surface differs from the headers:"
        diff <(echo "$DECLARED") <(echo "$EXPORTED") || true
    fi
else
    skip "export surface (no libmigo.so staged yet)"
fi

# --- 3. soname and version chain -------------------------------------------
if [[ -f "$SHARED_LIB" ]]; then
    SONAME="$(objdump -p "$SHARED_LIB" | awk '/SONAME/ {print $2}')"
    if [[ "$SONAME" == "libmigo.so.1" ]]; then
        pass "soname is libmigo.so.1"
    else
        fail "soname is '$SONAME', expected libmigo.so.1"
    fi
    if [[ -L "$PREFIX/lib/libmigo.so" && -L "$PREFIX/lib/libmigo.so.1" ]]; then
        pass "version symlink chain intact"
    else
        fail "version symlink chain incomplete under $PREFIX/lib"
    fi
else
    skip "soname and version chain (no libmigo.so staged yet)"
fi

# --- 3b. The manifest describes one coherent machine -----------------------
# Section 4 below checks the manifest against the library it ships beside. This
# checks the manifest against itself: arch, ABI, CPU baseline, loader floors,
# snapshot policy, and -- the one that matters most -- that the V8 built into
# the package targets the same triple as the package. A V8 for another OS or
# arch links often enough to reach a user and then fails with no provenance.
MANIFEST_TOOL="$REPO_ROOT/tools/artifact-manifest"
if [[ -f "$MANIFEST" ]]; then
    if MANIFEST_TOOL_OUT="$(cargo run --quiet --offline \
            --manifest-path "$MANIFEST_TOOL/Cargo.toml" \
            -- verify-linux-package "$MANIFEST" "$PREFIX" 2>&1)"; then
        pass "manifest and staged artifact bytes are consistent ($MANIFEST_TOOL_OUT)"
    else
        fail "manifest failed validation: $MANIFEST_TOOL_OUT"
    fi
else
    fail "no artifact manifest at $MANIFEST"
fi

# --- 4. Declared dependencies match reality --------------------------------
# The manifest is a claim this gate verifies, not documentation that drifts.
if [[ -f "$MANIFEST" ]]; then
    AUDIT_TARGET="$SHARED_LIB"
    [[ -f "$AUDIT_TARGET" ]] || AUDIT_TARGET="$FLOOR_BINARY"
    if [[ -e "$AUDIT_TARGET" ]]; then
        DECLARED_DEPS="$(python3 -c '
import json, sys
print("\n".join(json.load(open(sys.argv[1]))["dynamic_dependencies"]))' "$MANIFEST" | sort)"
        ACTUAL_DEPS="$(python3 "$AUDIT" needed "$AUDIT_TARGET" | sort)"
        if [[ "$DECLARED_DEPS" == "$ACTUAL_DEPS" ]]; then
            pass "manifest dependency list matches DT_NEEDED of $(basename "$AUDIT_TARGET")"
        else
            fail "manifest dependency list does not match DT_NEEDED:"
            diff <(echo "$DECLARED_DEPS") <(echo "$ACTUAL_DEPS") || true
        fi
    else
        fail "nothing to cross-check the manifest against"
    fi
else
    fail "no artifact manifest at $MANIFEST"
fi

# --- 5. Staged headers compile standalone ----------------------------------
if MIGO_INCLUDE_DIR="$PREFIX/include" bash "$SCRIPT_DIR/test-c-abi-surface-candidate.sh" \
        >/dev/null 2>&1; then
    pass "staged headers compile standalone under C11 and C++17"
else
    fail "staged headers do not compile standalone"
    MIGO_INCLUDE_DIR="$PREFIX/include" bash "$SCRIPT_DIR/test-c-abi-surface-candidate.sh" || true
fi

echo
if (( STRICT && SKIPS )); then
    echo "FAIL: --strict was requested but $SKIPS check(s) were skipped"
    exit 1
fi
if (( FAILURES )); then
    echo "FAIL: $FAILURES contract violation(s)"
    exit 1
fi
echo "OK: Linux SDK contract satisfied ($SKIPS skipped)"
