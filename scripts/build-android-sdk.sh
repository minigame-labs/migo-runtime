#!/usr/bin/env bash
# Build the Android C ABI package: cross-compile capi to a static library and
# stage a package a third-party NDK host can consume through CMake, with a
# per-ABI artifact manifest.
#
# This is the Android counterpart of build-linux-sdk.sh. Android ships a static
# library rather than a versioned shared object -- an NDK host links it into its
# own .so -- and embeds a V8 startup snapshot, where Linux embeds none.
#
# Usage: scripts/build-android-sdk.sh [--arch aarch64|x86_64] [--prefix DIR]
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/v8-materialise.sh
source "$SCRIPT_DIR/lib/v8-materialise.sh"
# shellcheck source=scripts/lib/skia-external-icu.sh
source "$SCRIPT_DIR/lib/skia-external-icu.sh"
# shellcheck source=scripts/lib/cargo-target-dir.sh
source "$SCRIPT_DIR/lib/cargo-target-dir.sh"
# shellcheck source=scripts/lib/release-version.sh
source "$SCRIPT_DIR/lib/release-version.sh"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
ENGINE_DIR="$REPO_ROOT/engine"

ARCH="aarch64"
PREFIX=""
# Filled from the release-version source below unless `--version` overrides it.
VERSION=""
while [[ $# -gt 0 ]]; do
    case "$1" in
        --arch) ARCH="$2"; shift 2 ;;
        --prefix) PREFIX="$2"; shift 2 ;;
        --version) VERSION="$2"; shift 2 ;;
        *) echo "unknown argument: $1" >&2; exit 2 ;;
    esac
done

# The repository's single release-version source, unless the caller named one.
# No fallback default: a package labelled with a version nobody chose is worse
# than a build that refuses, and this script's default was `0.1.0` while the AAR
# built beside it reported something else entirely.
if [[ -z "$VERSION" ]]; then
    VERSION="$(read_release_version "$REPO_ROOT")"
fi

case "$ARCH" in
    aarch64) TARGET="aarch64-linux-android"; ABI="arm64-v8a"; PUBLIC_ARCH="arm64" ;;
    x86_64)  TARGET="x86_64-linux-android";  ABI="x86_64";    PUBLIC_ARCH="x86_64" ;;
    *) echo "unsupported arch: $ARCH (expected aarch64 or x86_64)" >&2; exit 2 ;;
esac
# The staged prefix directory name becomes the published asset name -- package-sdk.sh
# derives one from the other -- so it uses the public architecture word, not the
# Android ABI name. ABI stays what the NDK, cargo-ndk and Gradle read, and what names
# the in-package manifest and jniLibs layout; those are internal and Android mandates
# arm64-v8a there.
[[ -n "$PREFIX" ]] || PREFIX="$REPO_ROOT/dist/migo-android-$PUBLIC_ARCH"

info() { echo -e "\033[0;36m[android-sdk] $*\033[0m"; }
err() { echo -e "\033[0;31m[android-sdk] $*\033[0m" >&2; }

# shellcheck source=scripts/lib/android-ndk.sh
source "$SCRIPT_DIR/lib/android-ndk.sh"
android_ndk_read_pin "$REPO_ROOT/contracts/artifact-manifest/android-v8.lock.json" || exit 1
android_ndk_resolve || { err "cannot resolve the pinned Android NDK"; exit 1; }
NDK_BIN="$(echo "$ANDROID_NDK_HOME"/toolchains/llvm/prebuilt/*/bin)"
export ANDROID_NDK_HOME
# skia-bindings reads ANDROID_NDK (not _HOME) when picking its toolchain.
export ANDROID_NDK="$ANDROID_NDK_HOME"
export PATH="$NDK_BIN:$PATH"

# engine/.cargo/config.toml names <triple>-ar without a path; the NDK ships it
# as llvm-ar.
SHIM_DIR="$ENGINE_DIR/target/android-ar-shim"
mkdir -p "$SHIM_DIR"
ln -sf "$NDK_BIN/llvm-ar" "$SHIM_DIR/$TARGET-ar"
export PATH="$SHIM_DIR:$PATH"

V8_DIR="$ENGINE_DIR/third_party/rusty_v8/$TARGET"
V8_ARCHIVE="$V8_DIR/librusty_v8.a"
V8_BINDING="$V8_DIR/src_binding.rs"
[[ -f "$V8_ARCHIVE" ]] || { err "missing $V8_ARCHIVE"; exit 1; }
[[ -f "$V8_BINDING" ]] || { err "missing $V8_BINDING"; exit 1; }
V8_MANIFEST="$V8_DIR/component-manifest.json"
[[ -f "$V8_MANIFEST" ]] || {
    err "missing verified V8 component manifest $V8_MANIFEST"
    err "rebuild this platform's V8 artifact on the V8 build machine"
    exit 1
}
# Verified against its component manifest, then used from a content-addressed path,
# so cargo cannot reuse an rlib built from a previous archive at the same location.
v8_materialise "$V8_DIR" "$ENGINE_DIR/target/v8-materialised" || exit 1
export RUSTY_V8_ARCHIVE="$V8_MATERIALISED_ARCHIVE"
export RUSTY_V8_SRC_BINDING_PATH="$V8_MATERIALISED_BINDING"
skia_external_icu_export "$ENGINE_DIR" "$TARGET" || exit 1

MANIFEST_TOOL="${MIGO_ARTIFACT_MANIFEST_TOOL:-}"
if [[ -z "$MANIFEST_TOOL" ]]; then
    MANIFEST_TOOL_TARGET="${MIGO_ARTIFACT_MANIFEST_TARGET_DIR:-$REPO_ROOT/tools/artifact-manifest/target}"
    CARGO_TARGET_DIR="$MANIFEST_TOOL_TARGET" cargo build \
        --manifest-path "$REPO_ROOT/tools/artifact-manifest/Cargo.toml" \
        --locked --release
    MANIFEST_TOOL="$MANIFEST_TOOL_TARGET/release/migo-artifact-manifest"
fi
[[ -x "$MANIFEST_TOOL" ]] || { err "artifact manifest verifier is not executable: $MANIFEST_TOOL"; exit 1; }

info "verifying V8 component bytes before linking"
"$MANIFEST_TOOL" verify-v8-component \
    "$V8_MANIFEST" "$V8_ARCHIVE" "$V8_BINDING" >/dev/null

SNAPSHOT_BIN="$ENGINE_DIR/crates/runtime-v8/snapshots/SNAPSHOT-full-android-$ARCH.bin"
SNAPSHOT_MANIFEST="$SNAPSHOT_BIN.manifest.json"
[[ -f "$SNAPSHOT_MANIFEST" ]] || {
    err "missing snapshot identity $SNAPSHOT_MANIFEST (regenerate the snapshot)"
    exit 1
}
# Freshness-only CI accepts a Git LFS pointer as an artifact identity. A package
# build cannot: runtime-v8 must embed the real bytes, and a source-JS fallback
# must never be mislabeled as snapshot_policy=embedded in the package manifest.
# shellcheck source=scripts/lib/snapshot-fingerprint.sh
source "$SCRIPT_DIR/lib/snapshot-fingerprint.sh"
snapshot_require_materialized_snapshot "$SNAPSHOT_BIN"
info "verifying the target host/full snapshot before compiling"
bash "$SCRIPT_DIR/check-snapshot-freshness.sh" --product-profile full --os android "$ARCH"

CARGO_TARGET="$(cargo_target_dir "$ENGINE_DIR")" || exit 1
BUILD_METADATA="$CARGO_TARGET/$TARGET/release/migo-android-build-metadata.json"
python3 "$SCRIPT_DIR/write-android-build-metadata.py" \
    --repo-root "$REPO_ROOT" \
    --output "$BUILD_METADATA" \
    --ndk-home "$ANDROID_NDK_HOME" \
    --target-triple "$TARGET" \
    --build-recipe scripts/build-android-sdk.sh

info "building capi staticlib (release, $TARGET)"
# Built from inside engine/ so that engine/rust-toolchain.toml applies. It is
# resolved from the working directory, never from --manifest-path, so building
# from the repository root used whichever toolchain the machine defaults to --
# for a file whose own comment calls itself "Pinned for a frozen, reproducible
# build chain", that is the one thing it must not do. Shipped bytes were being
# produced by an unpinned compiler.
#
# ⚠ Moving into engine/ also activates engine/.cargo/config.toml's `[env]`,
# which sets a bare CC=clang-18 for every target. cc-rs would then compile the C
# dependencies for Android with the host compiler against /usr/include and fail
# on bits/libc-header-start.h. It does NOT fail on a developer machine that
# exports a CC pointing at the NDK, because an exported variable beats the
# non-forcing [env] -- so the failure is invisible locally and appears only on a
# clean runner. Pinning the target-scoped names is the fix: cc-rs resolves
# CC_<target> before bare CC, the same lever that config.toml already uses for
# the Windows target.
NDK_API="${MIGO_ANDROID_API:-26}"
NDK_CC="$NDK_BIN/${TARGET}${NDK_API}-clang"
[[ -x "$NDK_CC" ]] || { err "NDK clang driver not found: $NDK_CC"; exit 1; }
TARGET_U="${TARGET//-/_}"
(
    cd "$ENGINE_DIR"
    env \
        "CC_${TARGET_U}=$NDK_CC" \
        "CXX_${TARGET_U}=${NDK_CC}++" \
        "AR_${TARGET_U}=$NDK_BIN/llvm-ar" \
        cargo build -p migo-capi --release --target "$TARGET"
)
STATIC_LIB="$CARGO_TARGET/$TARGET/release/libmigo_capi.a"
[[ -f "$STATIC_LIB" ]] || { err "no static library produced"; exit 1; }
info "built: $STATIC_LIB ($(stat -c %s "$STATIC_LIB") bytes)"
# One ICU, V8's, as in every AAR the size gate checks: this archive is linked into a consumer's own
# .so, where nothing of ours looks again (scripts/lib/skia-external-icu.sh).
icu="$(python3 "$SCRIPT_DIR/lib/icu_packages.py" "$STATIC_LIB" --expect 1)" \
    || { err "the static library carries ICU data packages '$icu', not one: Skia must link V8's ICU"; exit 1; }

info "capturing the link line cargo uses"
CARGO_OUT="$CARGO_TARGET/$TARGET/release/migo-android-native-static-libs.txt"
# Same working directory and same compiler pins as the build above: this
# reports the link line a consumer must reproduce, so it has to describe the
# artifact that was actually built rather than a differently-configured one.
#
# CARGO_TERM_COLOR=never: this note is parsed by gen-android-package-metadata.py,
# not read by a person. dtolnay/rust-toolchain forces CARGO_TERM_COLOR=always in
# CI when the caller leaves it unset, which wraps the note (and the last -l token
# on the line) in ANSI codes; the trailing reset then glues onto that last token
# with no separating whitespace, survives the `-l` filter as a token that no
# longer equals its clean name, and lands in migo-config.cmake corrupting the
# next entry into one unresolvable linker argument (e.g. "-lc<ESC[0m>;log").
(
    cd "$ENGINE_DIR"
    env \
        "CC_${TARGET_U}=$NDK_CC" \
        "CXX_${TARGET_U}=${NDK_CC}++" \
        "AR_${TARGET_U}=$NDK_BIN/llvm-ar" \
        CARGO_TERM_COLOR=never \
        cargo rustc -p migo-capi --release --target "$TARGET" \
            -- --print native-static-libs
) > "$CARGO_OUT" 2>&1 \
    || { err "cargo did not report native-static-libs"; cat "$CARGO_OUT" >&2; exit 1; }
grep -q "native-static-libs:" "$CARGO_OUT" \
    || { err "no native-static-libs note in cargo output"; exit 1; }

info "staging package at $PREFIX"
rm -rf "$PREFIX"
mkdir -p "$PREFIX/include" "$PREFIX/lib"
cp -r "$REPO_ROOT/include/migo" "$PREFIX/include/"
cp "$STATIC_LIB" "$PREFIX/lib/"

# Two settings a consumer cannot infer from the package, and whose absence
# fails with errors that point somewhere else: find_package reports the package
# as missing, and the STL mismatch reports duplicate std:: symbols. Both were
# hit while verifying this package from a consumer's side, so they ship with it.
cat > "$PREFIX/README.md" <<'CONSUMER_README'
# Migo Android SDK (C ABI)

Headers, a static library and a CMake package for embedding Migo in an NDK app.

## Consume it

```cmake
find_package(migo REQUIRED)
target_link_libraries(your_target PRIVATE migo::migo)
```

```bash
cmake -S . -B build \
    -DCMAKE_TOOLCHAIN_FILE="$ANDROID_NDK_HOME/build/cmake/android.toolchain.cmake" \
    -DANDROID_ABI=<abi> -DANDROID_PLATFORM=android-26 \
    -DANDROID_STL=c++_shared \
    -DCMAKE_PREFIX_PATH=<this-prefix> -DCMAKE_FIND_ROOT_PATH=<this-prefix>
```

Two of those flags are not optional:

- **`-DCMAKE_FIND_ROOT_PATH`** — the NDK toolchain sets
  `CMAKE_FIND_ROOT_PATH_MODE_PACKAGE=ONLY`, which confines `find_package` to the
  find-root. Without it CMake reports the package as missing even though
  `CMAKE_PREFIX_PATH` points straight at it.
- **`-DANDROID_STL`** — this package needs an NDK libc++, and which one you pick
  changes what you have to do. Measured on this package, arm64-v8a, NDK r26:

  | `-DANDROID_STL` | result |
  |---|---|
  | `c++_shared` | links as-is |
  | `c++_static` | links **with** `-Wl,--allow-multiple-definition` |
  | `none` | does not link at all |

  `c++_static` fails on its own with exactly six duplicate symbols —
  `std::runtime_error` and `std::logic_error`'s `char const*` constructor, copy
  constructor and copy assignment. Those are the ones libc++ explicitly
  instantiates in `stdexcept.cpp`, and they collide because this library carries
  Chromium's libc++ inside V8's archive while your build brings the NDK's. The
  error names neither Migo nor the STL setting, which is why it is spelled out
  here.

  `none` leaves one symbol undefined
  (`std::__ndk1::__vector_base_common<true>::__throw_length_error()`): parts of
  this library were compiled against the NDK's libc++ headers, so an NDK libc++
  has to be present in some form.

  Both supported rows are held by `scripts/test-android-sdk-contract.sh`, so a
  packaging change cannot quietly break either.

`android-26` is the minimum supported platform level.

## Verify what you downloaded

Each release asset ships a `.attestation.json` recording the archive's
`package_file`, `package_size_bytes` and `package_sha256`. Check the archive
against it before use.
CONSUMER_README

python3 "$SCRIPT_DIR/gen-android-package-metadata.py" \
    --prefix "$PREFIX" --version "$VERSION" --arch "$ARCH" --cargo-output "$CARGO_OUT"
python3 "$SCRIPT_DIR/gen-android-package-metadata.py" --manifest \
    --prefix "$PREFIX" --version "$VERSION" --arch "$ARCH" --cargo-output "$CARGO_OUT" \
    --snapshot-bin "$SNAPSHOT_BIN" --snapshot-manifest "$SNAPSHOT_MANIFEST" \
    --v8-component-manifest "$V8_MANIFEST" --build-metadata "$BUILD_METADATA"

PACKAGE_MANIFEST="$PREFIX/share/migo/android-$ABI-manifest.json"
info "verifying the complete staged package tree"
"$MANIFEST_TOOL" verify-android-package "$PACKAGE_MANIFEST" "$PREFIX" >/dev/null

info "package staged:"
find "$PREFIX" -type f | sed "s#^$PREFIX#  <prefix>#" | sort
