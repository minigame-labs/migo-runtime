#!/usr/bin/env bash
# The Android C ABI package's contract gate, the counterpart of
# scripts/test-linux-sdk-contract.sh.
#
# Every check fails the build outright. There is no warn-and-continue path: a
# package that silently exports the wrong symbols, or embeds a mismatched
# snapshot, fails on a device we cannot see.
#
# Usage:
#   scripts/test-android-sdk-contract.sh [--arch aarch64|x86_64]
#   scripts/test-android-sdk-contract.sh --strict   # any skip is a failure
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

ARCH="aarch64"
STRICT=0
for arg in "$@"; do
    case "$arg" in
        --arch) shift; ARCH="${1:-aarch64}" ;;
        aarch64|x86_64) ARCH="$arg" ;;
        --strict) STRICT=1 ;;
    esac
done
case "$ARCH" in
    aarch64) ABI="arm64-v8a"; PUBLIC_ARCH="arm64" ;;
    x86_64)  ABI="x86_64";    PUBLIC_ARCH="x86_64" ;;
    *) echo "unsupported arch: $ARCH" >&2; exit 2 ;;
esac

PREFIX="${MIGO_ANDROID_PREFIX:-$REPO_ROOT/dist/migo-android-$PUBLIC_ARCH}"
STATIC_LIB="$PREFIX/lib/libmigo_capi.a"
MANIFEST="$PREFIX/share/migo/android-$ABI-manifest.json"
MANIFEST_TOOL="$REPO_ROOT/tools/artifact-manifest"

FAILURES=0
SKIPS=0
pass() { echo -e "\033[0;32mPASS\033[0m  $*"; }
fail() { echo -e "\033[0;31mFAIL\033[0m  $*"; FAILURES=$((FAILURES + 1)); }
skip() { echo -e "\033[0;33mSKIP\033[0m  $*"; SKIPS=$((SKIPS + 1)); }

# shellcheck source=scripts/lib/android-ndk.sh
source "$SCRIPT_DIR/lib/android-ndk.sh"
android_ndk_read_pin "$REPO_ROOT/contracts/artifact-manifest/android-v8.lock.json" || exit 1
# An absent pinned NDK leaves NDK_BIN empty rather than aborting: the checks that
# need it already treat it as a skip, and --strict is what turns a skip into a
# failure. Exiting here would take the manifest, export and snapshot checks -- none
# of which need an NDK -- down with it.
NDK_BIN=""
if android_ndk_resolve; then
    NDK_BIN="$(echo "$ANDROID_NDK_HOME"/toolchains/llvm/prebuilt/*/bin 2>/dev/null)"
fi

[[ -d "$PREFIX" ]] || {
    echo "no staged package at $PREFIX; run scripts/build-android-sdk.sh --arch $ARCH" >&2
    exit 1
}

# --- 1. The manifest describes one coherent machine ------------------------
# A V8 or snapshot built for another Android ABI links and then crashes on
# device with no provenance attached. verify-android-package rejects a mismatch.
if [[ -f "$MANIFEST" ]]; then
    if OUT="$(cargo run --quiet --offline --manifest-path "$MANIFEST_TOOL/Cargo.toml" \
            -- verify-android-package "$MANIFEST" "$PREFIX" 2>&1)"; then
        pass "manifest and staged artifact bytes are consistent ($OUT)"
    else
        fail "manifest failed validation: $OUT"
    fi
else
    fail "no artifact manifest at $MANIFEST"
fi

# --- 2. Export surface: the static library carries the documented ABI ------
# The migo_* text symbols the archive defines must equal the set the headers
# declare: no more (a leaked entry point) and no less (a header promise with no
# implementation). Hiding the non-migo internals is not a static archive's job
# -- the consumer links it into its own .so and applies its own version script
# -- so only the migo_* subset is compared, unlike the Linux .so gate.
#
# Read with host binutils `nm`, deliberately not the NDK's llvm-nm. Rust emits
# ELF objects that also carry an embedded LLVM-bitcode section, and the NDK's
# llvm-nm (LLVM 12) cannot parse the newer bitcode Rust (LLVM 22) writes, so it
# errors on every member. `nm` reads the ELF symbol table -- which is what a
# non-LTO link actually consumes -- across architectures via BFD, so it lists
# the aarch64 archive's symbols on an x86_64 host without choking on the
# bitcode.
if [[ -f "$STATIC_LIB" ]] && command -v nm >/dev/null 2>&1; then
    # include/migo/platform/android.h, not just the cross-platform include/migo/*.h:
    # it is the one platform header that declares a migo_* entry point
    # (migo_android_init_context), and it ships in this package (see the staged
    # tree above). Every other platform header declares only types, so widening
    # this glob does not pull in a symbol the other SDKs' contract scripts don't
    # already agree on.
    DECLARED="$(python3 "$REPO_ROOT/scripts/c-abi-entry-points.py" embedded "$REPO_ROOT/include/migo" \
            "$REPO_ROOT/include/migo/platform/android.h")"
    # `|| true`: grep exits non-zero when a broken archive defines no migo_
    # symbol, which must surface as an empty set the comparison below rejects,
    # not as the whole gate aborting under `set -e`.
    EXPORTED="$(nm "$STATIC_LIB" 2>/dev/null \
        | grep -E ' T migo_[a-z0-9_]+$' | awk '{print $3}' | sort -u || true)"
    if [[ "$DECLARED" == "$EXPORTED" ]]; then
        pass "static library defines exactly the declared migo_* set ($(echo "$DECLARED" | wc -l) entry points)"
    else
        fail "static library entry points differ from the headers:"
        diff <(echo "$DECLARED") <(echo "$EXPORTED") || true
    fi
else
    skip "export surface (no static library or nm)"
fi

# --- 3. Staged headers compile standalone under the NDK at the API floor ----
if [[ -n "$NDK_BIN" && -x "$NDK_BIN/$ARCH-linux-android26-clang" ]]; then
    if MIGO_INCLUDE_DIR="$PREFIX/include" CC="$NDK_BIN/$ARCH-linux-android26-clang" \
            CXX="$NDK_BIN/$ARCH-linux-android26-clang++" \
            bash "$SCRIPT_DIR/test-c-abi-surface-candidate.sh" --core >/dev/null 2>&1; then
        pass "staged headers compile standalone under the NDK (API $ARCH/26)"
    else
        fail "staged headers do not compile standalone under the NDK"
    fi
else
    skip "header compile (no NDK API-26 clang for $ARCH)"
fi

# --- 4. The manifest names the exact freshness-gated snapshot build input ---
# build-android-sdk.sh refuses a stale/pointer snapshot before compiling, so
# runtime-v8 cannot take its source-JS fallback in a package claiming embedded.
# This post-build check binds the manifest back to those exact input bytes.
if [[ -f "$MANIFEST" ]]; then
    SNAP="$REPO_ROOT/engine/crates/runtime-v8/snapshots/SNAPSHOT-full-android-$ARCH.bin"
    if [[ -f "$SNAP" ]]; then
        WANT="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["snapshots"][0]["bytes_hash"])' "$MANIFEST")"
        GOT="$(sha256sum "$SNAP" | cut -d" " -f1)"
        if [[ "$WANT" == "$GOT" ]]; then
            pass "manifest snapshot bytes_hash matches the freshness-gated build input"
        else
            fail "manifest snapshot bytes_hash does not match SNAPSHOT-full-android-$ARCH.bin"
        fi
    else
        fail "manifest names an embedded snapshot but $SNAP is missing"
    fi
fi

# --- 5. A third-party NDK consumer links against the package ---------------
# The strongest check: a host that sees only the public headers resolves every
# migo_* through find_package(migo) with the NDK toolchain and produces a
# loadable shared object. This is what a packaging defect -- a missing link
# library, a wrong CMake path, an unresolved entry point -- actually breaks.
CONSUMER="$REPO_ROOT/tests/c_host/android-package-consumer"
if [[ -x "$NDK_BIN/$ARCH-linux-android26-clang" ]] && command -v cmake >/dev/null 2>&1 \
        && [[ -f "$CONSUMER/build.sh" ]]; then
    CONSUMER_LOG="$(mktemp)"
    if ANDROID_ABI="$ABI" MIGO_ANDROID_PREFIX="$PREFIX" ANDROID_STL=c++_shared \
            bash "$CONSUMER/build.sh" >"$CONSUMER_LOG" 2>&1; then
        CONSUMER_SO="$(find "$CONSUMER/build" -name 'libconsumer.so' 2>/dev/null | head -1)"
        UNDEF="$(nm -D "$CONSUMER_SO" 2>/dev/null | grep -cE ' U migo_' || true)"
        if [[ -n "$CONSUMER_SO" && "$UNDEF" == "0" ]]; then
            pass "third-party find_package(migo) consumer links with every migo_* resolved"
        else
            fail "consumer linked but has $UNDEF unresolved migo_* symbols"
        fi
    else
        fail "third-party find_package(migo) consumer failed to build against the package"
        # Diagnostic only: this check has failed in CI while passing locally
        # with an identical pinned NDK, so surface the swallowed cmake/build
        # output instead of guessing at the cause.
        sed -e 's/^/    /' "$CONSUMER_LOG"
    fi
    rm -f "$CONSUMER_LOG"

    # The other C++ runtime a real host picks. `c++_static` is the natural
    # choice now that this SDK ships no `libc++_shared.so` of its own -- and it
    # does not link without one flag: `librusty_v8.a` carries Chromium's libc++,
    # the NDK's static libc++ brings its own, and exactly six symbols collide
    # (`std::runtime_error` and `std::logic_error`'s char-const* constructor,
    # copy constructor and copy assignment -- the ones libc++ explicitly
    # instantiates in stdexcept.cpp). The engine's own link tolerates them with
    # `--allow-multiple-definition`; a third-party consumer has no such flag
    # until someone tells them, which is what this documents and holds.
    #
    # Asserted as "the documented matrix links", not as "the undocumented ones
    # fail": a gate written the second way turns red the day someone makes
    # `c++_static` work without the flag, which is a fix, not a regression.
    STATIC_LOG="$(mktemp)"
    if ANDROID_ABI="$ABI" MIGO_ANDROID_PREFIX="$PREFIX" ANDROID_STL=c++_static \
            MIGO_CONSUMER_LINK_FLAGS="-Wl,--allow-multiple-definition" \
            bash "$CONSUMER/build.sh" >"$STATIC_LOG" 2>&1; then
        STATIC_SO="$(find "$CONSUMER/build" -name 'libconsumer.so' 2>/dev/null | head -1)"
        STATIC_UNDEF="$(nm -D "$STATIC_SO" 2>/dev/null | grep -cE ' U migo_' || true)"
        if [[ -n "$STATIC_SO" && "$STATIC_UNDEF" == "0" ]]; then
            pass "c++_static consumer links with -Wl,--allow-multiple-definition"
        else
            fail "c++_static consumer linked but has $STATIC_UNDEF unresolved migo_* symbols"
        fi
    else
        fail "c++_static consumer failed to link even with -Wl,--allow-multiple-definition;
      the package README documents that combination as supported"
        sed -e 's/^/    /' "$STATIC_LOG"
    fi
    rm -f "$STATIC_LOG"
else
    skip "consumer link (no NDK clang, cmake, or consumer example)"
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
echo "OK: Android SDK contract satisfied ($SKIPS skipped)"
