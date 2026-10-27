# shellcheck shell=bash
# Resolving the pinned Android NDK, wherever the SDK happens to live.
# Location: scripts/lib/android-ndk.sh
#
# Seven scripts independently defaulted ANDROID_NDK_HOME to `$HOME/Android/Ndk`, a
# path that exists on none of the machines this has been built on: every successful
# build depended on the variable already being set in the environment. Nothing
# asserted which NDK it pointed at either, so the V8 archive and the AAR could be
# produced by different NDKs with nothing noticing -- and the NDK is recorded in the
# component manifest as part of the artifact's identity.
#
# So the version is pinned in the build lock and the path is *found*, in the
# standard SDK layouts, rather than guessed. An explicit ANDROID_NDK_HOME still
# wins, because a caller may have the pinned NDK somewhere unusual -- but it is
# checked against the pin like any other candidate, so an override cannot silently
# substitute a different toolchain.

_android_ndk_err() { printf '  ✗ %s\n' "$*" >&2; }

# The NDK's own record of what it is. `Pkg.Revision` is the identity the component
# manifest already stores, so this is the same fact the artifact is stamped with
# rather than a directory name that happens to look like a version.
android_ndk_revision() {
    local ndk="$1" properties="$1/source.properties"
    [[ -f "$properties" ]] || return 1
    sed -n 's/^Pkg\.Revision[[:space:]]*=[[:space:]]*\([^[:space:]]*\).*/\1/p' \
        "$properties" | head -1
}

# Reads the pinned version out of a build lock into ANDROID_NDK_PIN.
android_ndk_read_pin() {
    local lock="$1"
    [[ -f "$lock" ]] || { _android_ndk_err "missing build lock: $lock"; return 1; }
    ANDROID_NDK_PIN="$(python3 - "$lock" <<'PY'
import json, sys

lock = json.load(open(sys.argv[1]))
try:
    print(lock["ndk"]["version"])
except KeyError as missing:
    sys.exit(f"build lock has no ndk {missing}")
PY
)" || { _android_ndk_err "cannot read the ndk pin from $lock"; return 1; }
}

# Sets ANDROID_NDK_HOME to an NDK whose own Pkg.Revision equals the pin, and
# LIBCLANG_PATH to that NDK's libclang.
#
# The second is part of the same toolchain. bindgen (clang-sys) parses the NDK's
# libc++ and bionic headers when it generates a binding for an Android target --
# Skia's, compiled by every Android build -- and those headers are written for the
# NDK's own clang: r30's libc++ uses __builtin_clzg/__builtin_ctzg (clang 19). Left
# to find one, clang-sys took ubuntu-24.04's libclang 18 on CI, and the binding
# failed there while a Mac, whose Xcode libclang is newer, built. A LIBCLANG_PATH
# already in the environment is not kept: an inherited one is how a mismatched
# parser arrives. (build-v8-android.sh still takes V8_LIBCLANG_PATH for V8's.)
# android_ndk_read_pin must have run.
android_ndk_resolve() {
    local -a candidates=()
    [[ -n "${ANDROID_NDK_HOME:-}" ]] && candidates+=("$ANDROID_NDK_HOME")
    [[ -n "${ANDROID_NDK_ROOT:-}" ]] && candidates+=("$ANDROID_NDK_ROOT")
    local root
    for root in "${ANDROID_HOME:-}" "${ANDROID_SDK_ROOT:-}" "$HOME/Android/Sdk" \
                "$HOME/Library/Android/sdk"; do
        [[ -n "$root" ]] && candidates+=("$root/ndk/$ANDROID_NDK_PIN")
    done

    local candidate revision
    for candidate in "${candidates[@]}"; do
        [[ -d "$candidate" ]] || continue
        revision="$(android_ndk_revision "$candidate")" || continue
        if [[ "$revision" == "$ANDROID_NDK_PIN" ]]; then
            ANDROID_NDK_HOME="$candidate"
            export ANDROID_NDK_HOME
            android_ndk_libclang || return 1
            return 0
        fi
        _android_ndk_err "$candidate is NDK $revision, the lock pins $ANDROID_NDK_PIN"
    done
    _android_ndk_err "no Android NDK $ANDROID_NDK_PIN found"
    _android_ndk_err "looked at: ${candidates[*]:-(nothing)}"
    _android_ndk_err "install it with: sdkmanager 'ndk;$ANDROID_NDK_PIN'"
    _android_ndk_err "or set ANDROID_NDK_HOME to that NDK"
    return 1
}

# Exports LIBCLANG_PATH as the resolved NDK's lib directory, the one holding its
# libclang (.so on Linux, .dylib on macOS) beside lib/clang/<major>/include, the
# builtin headers libclang finds relative to itself.
android_ndk_libclang() {
    local lib
    for lib in "$ANDROID_NDK_HOME"/toolchains/llvm/prebuilt/*/lib; do
        if [[ -f "$lib/libclang.so" || -f "$lib/libclang.dylib" ]]; then
            LIBCLANG_PATH="$lib"
            export LIBCLANG_PATH
            return 0
        fi
    done
    _android_ndk_err "the NDK at $ANDROID_NDK_HOME has no libclang for bindgen"
    return 1
}

# Path to the pinned NDK's llvm-readelf, or non-zero with a reason.
#
# Gates that read section sizes and dynamic entries out of a shipped .so must not
# take whatever `readelf` is on PATH. GNU binutils and LLVM do not print the same
# text for the same file -- GNU renders Android's DT tags as raw hex (`60000011`)
# where LLVM names them -- so a gate matching on that output reads a different
# answer on a machine with different tools installed, and reads it silently.
#
# Two size gates used to resolve their own: ANDROID_NDK_HOME if set, else the
# first llvm-readelf found anywhere under $HOME/Android, else PATH. Each of the
# three steps is a different toolchain, and the last is a different vendor.
#
# The search deliberately does not pass `-type f`. In NDK r23 `llvm-readelf` is a
# symlink to `llvm-readobj`, so `-type f` matches nothing, and that one flag is
# what sent both gates all the way down to `/usr/bin/readelf` on every machine
# that has an NDK -- the fallback chain never reported that it had been used.
#
#   $1  path to the artifact-manifest lock that carries the NDK pin
android_ndk_readelf() {
    local lock="$1"
    android_ndk_read_pin "$lock" || return 1
    android_ndk_resolve || return 1

    local candidate
    candidate="$(find "$ANDROID_NDK_HOME/toolchains/llvm/prebuilt" \
        -name llvm-readelf 2>/dev/null | head -1)"
    if [[ -z "$candidate" ]]; then
        _android_ndk_err "no llvm-readelf under $ANDROID_NDK_HOME/toolchains/llvm/prebuilt"
        return 1
    fi
    printf '%s\n' "$candidate"
}
