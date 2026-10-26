#!/usr/bin/env bash
# CI must use the native build lock too. A separate action default kept r23
# selected after Ada's C++20 requirement forced the native cohort forward.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
source "$ROOT/scripts/lib/android-ndk.sh"
android_ndk_read_pin "$ROOT/contracts/artifact-manifest/android-v8.lock.json"
version="$ANDROID_NDK_PIN"
if [[ -n "${MIGO_REQUESTED_NDK_REVISION:-}" && "$MIGO_REQUESTED_NDK_REVISION" != "$version" ]]; then
    echo "::error::requested NDK $MIGO_REQUESTED_NDK_REVISION differs from native build lock $version" >&2
    exit 1
fi

sdkmanager "ndk;$version"
ndk="${ANDROID_HOME:?ANDROID_HOME must name the CI SDK}/ndk/$version"
toolchain="$ndk/toolchains/llvm/prebuilt/linux-x86_64"
# sdkmanager can silently no-op on an unknown revision. Check the installed
# identity as well as the executable, even when the directory has the right name.
if [[ "$(android_ndk_revision "$ndk")" != "$version" || ! -x "$toolchain/bin/clang" ]]; then
    echo "::error::sdkmanager did not install NDK $version at $ndk" >&2
    exit 1
fi

for var in ANDROID_NDK ANDROID_NDK_HOME ANDROID_NDK_ROOT ANDROID_NDK_LATEST_HOME; do
    echo "$var=$ndk" >> "${GITHUB_ENV:?GITHUB_ENV is required}"
done
echo "$toolchain/bin" >> "${GITHUB_PATH:?GITHUB_PATH is required}"
