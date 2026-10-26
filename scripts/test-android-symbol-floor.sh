#!/usr/bin/env bash
# =============================================================================
# Android minimum-API symbol floor gate.
#
# Every symbol libmigo.so imports from the platform must exist at the minSdk
# Migo claims (API 26). If it imports one that the platform only added later,
# the whole library fails to `dlopen` on a floor device -- the runtime does not
# start at all, and nothing above it runs. That failure is invisible on a newer
# device or emulator, so it cannot be a manual check; it has to be a gate.
#
# This caught exactly that on 2026-07-24: `libmigo.so` imported
# `android_get_device_api_level`, which is an exported libc symbol only from
# API 29 on (at API 21..=28 it is a `static inline` in <android/api-level.h>).
# On the API-26 emulator the load failed with "cannot locate symbol
# android_get_device_api_level", and neither the API-29 emulator nor the API-31
# phone showed it.
#
# The authority for "exists at API 26" is the NDK's own versioned sysroot stub
# libraries under .../sysroot/usr/lib/<triple>/26/. A symbol is allowed iff some
# API-26 stub library exports it (or libmigo.so defines it itself). This is not
# a hand-maintained allowlist: it is what the NDK guarantees is present at 26.
#
# Usage:
#   scripts/test-android-symbol-floor.sh <path/to/libmigo.so> [more.so ...]
#
# With no argument it checks every jniLibs/<abi>/libmigo.so a local AAR build
# staged. Fails closed: a missing NDK, missing stubs, or an unreadable library
# is an error, not a skip.
# =============================================================================
set -euo pipefail

MIN_API="${MIGO_MIN_API:-26}"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

err() { echo -e "\033[0;31m[floor] $*\033[0m" >&2; }
ok()  { echo -e "\033[0;32m[floor] $*\033[0m"; }
info(){ echo -e "\033[0;36m[floor] $*\033[0m"; }

# shellcheck source=scripts/lib/android-ndk.sh
source "$SCRIPT_DIR/lib/android-ndk.sh"
android_ndk_read_pin "$REPO_ROOT/contracts/artifact-manifest/android-v8.lock.json" || exit 1
android_ndk_resolve || { err "cannot resolve the pinned Android NDK"; exit 1; }
NDK_BIN="$ANDROID_NDK_HOME/toolchains/llvm/prebuilt/linux-x86_64/bin"
NM="$NDK_BIN/llvm-nm"
READELF="$NDK_BIN/llvm-readelf"
[[ -x "$NM" ]] || { err "llvm-nm not found at $NM"; exit 1; }
SYSROOT_LIB="$ANDROID_NDK_HOME/toolchains/llvm/prebuilt/linux-x86_64/sysroot/usr/lib"

# LLVM r30 prints ELF symbol versions. A default definition foo@@LIBC serves
# both foo and foo@LIBC; preserve the version when matching explicit imports.
# Stripping every @suffix would hide an unavailable version of an existing name.
defined_symbols() {
    "$NM" -D --defined-only "$1" | awk '$2 ~ /[A-Za-z]/ {
        symbol = $NF; print symbol;
        if (sub(/@@/, "@", symbol)) {
            print symbol;
            sub(/@.*/, "", symbol); print symbol;
        }
    }'
}

# Map an ABI to its NDK triple. The triple names the stub directory.
triple_for() {
    case "$1" in
        arm64-v8a|aarch64) echo aarch64-linux-android ;;
        x86_64)            echo x86_64-linux-android ;;
        armeabi-v7a)       echo arm-linux-androideabi ;;
        x86)               echo i686-linux-android ;;
        *) return 1 ;;
    esac
}

# Detect the ABI from the path (…/jniLibs/<abi>/libmigo.so) or the ELF header.
abi_of() {
    local so="$1" abi
    abi="$(basename "$(dirname "$so")")"
    if triple_for "$abi" >/dev/null 2>&1; then echo "$abi"; return; fi
    case "$("$NM" --help >/dev/null 2>&1; file -b "$so" 2>/dev/null)" in
        *x86-64*) echo x86_64 ;;
        *aarch64*|*ARM\ aarch64*) echo arm64-v8a ;;
        *) return 1 ;;
    esac
}

check_one() {
    local so="$1"
    [[ -f "$so" ]] || { err "not a file: $so"; return 1; }

    local abi triple stubdir
    abi="$(abi_of "$so")" || { err "cannot determine ABI for $so"; return 1; }
    triple="$(triple_for "$abi")" || { err "unsupported ABI '$abi' for $so"; return 1; }
    stubdir="$SYSROOT_LIB/$triple/$MIN_API"
    [[ -d "$stubdir" ]] || { err "no NDK stub libraries for API $MIN_API at $stubdir"; return 1; }

    # Symbols the platform guarantees at MIN_API: the union of every stub .so's
    # dynamic exports. Built fresh from the NDK, never hand-maintained.
    local allowed imported internal undefined violations
    allowed="$(for lib in "$stubdir"/*.so; do
        defined_symbols "$lib" 2>/dev/null
    done | LC_ALL=C sort -u)"

    # What libmigo.so imports, minus what it defines itself (symbols can appear
    # as both when different objects reference and provide them).
    #
    # Only STRONG undefined symbols (nm type `U`) matter. A WEAK undefined
    # symbol (`w`) that the loader cannot resolve becomes NULL rather than
    # failing the load, which is exactly how libc lets a binary reference a
    # newer entry point and fall back when it is absent -- `getrandom` (API 28)
    # and `copy_file_range` (API 29) appear this way and are safe on API 26.
    # A strong `U` is the one that aborts `dlopen`, so the gate must not flag
    # weak imports or it fails a library that actually loads.
    # Oboe/Ada now use the pinned NDK's shared C++ runtime. Its exports are
    # supplied by a bundled dependency, not by a platform stub. Walk only the
    # declared DT_NEEDED closure, require each nonplatform provider to ship
    # beside the entry, and check that provider's own imports at the same floor.
    # Unrelated sibling exports must never hide a missing platform import.
    local directory machine current needed candidate soname candidate_machine dynamic
    directory="$(dirname "$so")"
    machine="$("$READELF" -h "$so" | sed -n 's/^[[:space:]]*Machine:[[:space:]]*//p')"
    [[ -n "$machine" ]] || { err "cannot read ELF machine: $so"; return 1; }
    local -a pending=("$so")
    local -A visited=()
    undefined="" internal=""
    local index=0
    while (( index < ${#pending[@]} )); do
        current="${pending[$index]}"
        index=$((index + 1))
        [[ -z "${visited[$current]:-}" ]] || continue
        visited[$current]=1
        undefined+="$("$NM" -D --undefined-only "$current" | awk '$1 == "U" {print $2}')"$'\n' || return 1
        internal+="$(defined_symbols "$current")"$'\n' || return 1
        dynamic="$("$READELF" -d "$current")" || return 1
        while IFS= read -r needed; do
            [[ -n "$needed" ]] || continue
            if [[ "$needed" != "$(basename "$needed")" || "$needed" == . || "$needed" == .. ]]; then
                err "unsafe DT_NEEDED name in $current: $needed"; return 1
            fi
            [[ ! -f "$stubdir/$needed" ]] || continue
            candidate="$directory/$needed"
            [[ -f "$candidate" ]] || { err "missing bundled dependency $needed for $current"; return 1; }
            candidate_machine="$("$READELF" -h "$candidate" | sed -n 's/^[[:space:]]*Machine:[[:space:]]*//p')"
            [[ "$candidate_machine" == "$machine" ]] || { err "wrong ELF machine for $candidate"; return 1; }
            soname="$("$READELF" -d "$candidate" | sed -n 's/.*SONAME.*\[\(.*\)\].*/\1/p')"
            [[ -z "$soname" || "$soname" == "$needed" ]] || { err "SONAME differs for $candidate: $soname"; return 1; }
            pending+=("$candidate")
        done < <(printf '%s\n' "$dynamic" | sed -n 's/.*NEEDED.*Shared library: \[\(.*\)\].*/\1/p')
    done
    undefined="$(printf '%s\n' "$undefined" | sed '/^$/d' | LC_ALL=C sort -u)"
    internal="$(printf '%s\n' "$internal" | sed '/^$/d' | LC_ALL=C sort -u)"
    imported="$(LC_ALL=C comm -23 <(printf '%s\n' "$undefined") <(printf '%s\n' "$internal"))"

    # Anything imported that neither the API-MIN stubs nor libmigo itself provide.
    violations="$(LC_ALL=C comm -23 <(printf '%s\n' "$imported") <(printf '%s\n' "$allowed"))"

    if [[ -n "$violations" ]]; then
        err "$so ($abi): imports symbols not present at API $MIN_API:"
        printf '%s\n' "$violations" | sed 's/^/         /' >&2
        return 1
    fi
    ok "$so ($abi): all imports in ${#visited[@]} bundled ELF file(s) resolve at API $MIN_API"
    return 0
}

if [[ "${1:-}" == --self-test ]]; then
    python3 "$SCRIPT_DIR/testing/test_android_symbol_floor.py" "$ANDROID_NDK_HOME"
    exit $?
fi

TARGETS=("$@")
if [[ ${#TARGETS[@]} -eq 0 ]]; then
    mapfile -t TARGETS < <(find "$REPO_ROOT/engine/jniLibs" -name libmigo.so 2>/dev/null)
    [[ ${#TARGETS[@]} -gt 0 ]] || { err "no libmigo.so given and none staged under engine/jniLibs"; exit 1; }
    info "checking staged libraries: ${TARGETS[*]}"
fi

rc=0
for so in "${TARGETS[@]}"; do
    check_one "$so" || rc=1
done
exit "$rc"
