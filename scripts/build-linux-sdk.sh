#!/usr/bin/env bash
# Build the Linux SDK artifacts against the pinned sysroot and stage a package
# a non-cargo host can consume.
#
# Usage: scripts/build-linux-sdk.sh [aarch64|x86_64] [--prefix DIR]
#
# aarch64 ships without an embedded V8 startup snapshot (snapshot_policy:
# "none", the same policy Windows currently has): generating one requires
# *running* migo-snapshot-gen natively on real AArch64 Linux hardware
# (scripts/gen-snapshot.sh's own doc comment), which this build does not have
# access to yet. x86_64's snapshot embedding is unaffected and stays
# mandatory.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
ENGINE_DIR="$REPO_ROOT/engine"

# shellcheck source=scripts/lib/v8-materialise.sh
source "$SCRIPT_DIR/lib/v8-materialise.sh"

ARCH="x86_64"
PREFIX=""
while [[ $# -gt 0 ]]; do
    case "$1" in
        aarch64|x86_64) ARCH="$1"; shift ;;
        --prefix) PREFIX="$2"; shift 2 ;;
        *) echo "[linux-sdk] unknown argument: $1" >&2; exit 2 ;;
    esac
done
# TARGET (rust triple), SYSROOT_KEY (sysroots.json key) and SNAPSHOT_POLICY
# disagree in vocabulary the same way build-v8-linux.sh's ARCH/TARGET/
# GN_CPU/SYSROOT_ARCH do, and are resolved the same way: from migo's own
# aarch64/x86_64 spelling, not derived from each other.
case "$ARCH" in
    aarch64) TARGET="aarch64-unknown-linux-gnu"; SYSROOT_KEY="bullseye_arm64"; SNAPSHOT_POLICY="none" ;;
    x86_64)  TARGET="x86_64-unknown-linux-gnu";  SYSROOT_KEY="bullseye_amd64"; SNAPSHOT_POLICY="embedded" ;;
esac
# Public-facing directory/asset name only -- "arm64", matching Android's and
# OpenHarmony's own aarch64->arm64 word (see build-ohos-sdk.sh's
# public_ohos_arch). Every internal identity (--arch below, the manifest's own
# "arch" field, the Rust validator's target.arch) stays "aarch64", migo's own
# vocabulary and the one the rust target triple actually uses; only the
# published path/filename gets the public word.
PUBLIC_ARCH="$ARCH"
[[ "$ARCH" == "aarch64" ]] && PUBLIC_ARCH="arm64"
[[ -n "$PREFIX" ]] || PREFIX="$REPO_ROOT/dist/migo-linux-$PUBLIC_ARCH"

info() { echo -e "\033[0;36m[linux-sdk] $*\033[0m"; }

export MIGO_SYSROOT_KEY="$SYSROOT_KEY"
# shellcheck source=scripts/lib/linux-sysroot.sh
source "$SCRIPT_DIR/lib/linux-sysroot.sh"
migo_sysroot_export

LINK_DIR="$ENGINE_DIR/target/sysroot-links-$ARCH"
migo_sysroot_link_dir "$LINK_DIR"
export LIBRARY_PATH="$LINK_DIR:${LIBRARY_PATH:-}"
# -B, not just -L: the driver searches -B prefixes for libraries ahead of its
# own built-in directories, which is what keeps `-lstdc++` from resolving to the
# host GCC's copy. A plain -L lands after those built-ins and loses the race.
RUSTFLAGS_SYSROOT_LINK+=" -C link-arg=-B$LINK_DIR -C link-arg=-L$LINK_DIR"

# SDK packages accept only the archive installed by build-v8-linux.sh together
# with its verified component manifest. An arbitrary development archive may be
# useful for a local executable, but it cannot make a reproducible support-floor
# or provenance claim and therefore must never enter a published SDK.
V8_SDK_DIR="$ENGINE_DIR/third_party/rusty_v8/$ARCH-linux-gnu"
V8_ARCHIVE="$V8_SDK_DIR/librusty_v8.a"
V8_BINDING="$V8_SDK_DIR/src_binding.rs"
V8_MANIFEST="$V8_SDK_DIR/component-manifest.json"
[[ -f "$V8_ARCHIVE" ]] || { echo "[linux-sdk] no V8 archive at $V8_ARCHIVE" >&2; exit 1; }
[[ -f "$V8_BINDING" ]] || { echo "[linux-sdk] no V8 Rust binding at $V8_BINDING" >&2; exit 1; }
[[ -f "$V8_MANIFEST" ]] || {
    echo "[linux-sdk] no verified V8 component manifest at $V8_MANIFEST" >&2
    echo "[linux-sdk] rebuild V8 on the V8 build machine before packaging" >&2
    exit 1
}
[[ "$(stat -c %s "$V8_ARCHIVE")" -gt 1000000 ]] \
    || { echo "[linux-sdk] V8 archive looks like a stub or LFS pointer" >&2; exit 1; }
# Materialised under a path that is its own hash. That is what lets the sha256 stamp
# and the `cargo clean -p v8` below it go: cargo reruns the v8 build script when the
# *value* of RUSTY_V8_ARCHIVE changes, so different bytes are now a different path
# rather than a staleness the build has to detect and force.
if ! v8_materialise "$V8_SDK_DIR" "$ENGINE_DIR/target/v8-materialised"; then
    echo "[linux-sdk] cannot use the V8 archive at $V8_ARCHIVE" >&2
    exit 1
fi
export RUSTY_V8_ARCHIVE="$V8_MATERIALISED_ARCHIVE"
export RUSTY_V8_SRC_BINDING_PATH="$V8_MATERIALISED_BINDING"

MANIFEST_TOOL="${MIGO_ARTIFACT_MANIFEST_TOOL:-}"
if [[ -z "$MANIFEST_TOOL" ]]; then
    MANIFEST_TOOL_TARGET="${MIGO_ARTIFACT_MANIFEST_TARGET_DIR:-$REPO_ROOT/tools/artifact-manifest/target}"
    CARGO_TARGET_DIR="$MANIFEST_TOOL_TARGET" cargo build \
        --manifest-path "$REPO_ROOT/tools/artifact-manifest/Cargo.toml" \
        --locked --release
    MANIFEST_TOOL="$MANIFEST_TOOL_TARGET/release/migo-artifact-manifest"
fi
[[ -x "$MANIFEST_TOOL" ]] || {
    echo "[linux-sdk] artifact manifest verifier is not executable: $MANIFEST_TOOL" >&2
    exit 1
}

info "verifying V8 component bytes before linking"
"$MANIFEST_TOOL" verify-v8-component \
    "$V8_MANIFEST" "$V8_ARCHIVE" "$V8_BINDING" >/dev/null
V8_SYSROOT_IDENTITY="$(python3 - "$V8_MANIFEST" <<'PY'
import json
import pathlib
import sys

manifest = json.loads(pathlib.Path(sys.argv[1]).read_text(encoding="utf-8"))
print(manifest["toolchain"]["sdk"])
PY
)"
if [[ "$V8_SYSROOT_IDENTITY" != "$MIGO_SYSROOT_IDENTITY" ]]; then
    echo "[linux-sdk] V8 component sysroot identity does not match the engine sysroot" >&2
    echo "[linux-sdk] V8:     $V8_SYSROOT_IDENTITY" >&2
    echo "[linux-sdk] engine: $MIGO_SYSROOT_IDENTITY" >&2
    exit 1
fi

# The NDK must not be visible, or skia-bindings picks its toolchain over ours.
unset ANDROID_NDK ANDROID_NDK_HOME

SNAPSHOT_BIN=""
SNAPSHOT_MANIFEST=""
if [[ "$SNAPSHOT_POLICY" == "embedded" ]]; then
    SNAPSHOT_BIN="$ENGINE_DIR/crates/runtime-v8/snapshots/SNAPSHOT-full-linux-$ARCH.bin"
    SNAPSHOT_MANIFEST="$SNAPSHOT_BIN.manifest.json"
    [[ -f "$SNAPSHOT_MANIFEST" ]] || {
        echo "[linux-sdk] missing snapshot identity $SNAPSHOT_MANIFEST (regenerate the snapshot)" >&2
        exit 1
    }
    # Freshness-only CI accepts a Git LFS pointer as an artifact identity. A package
    # build cannot: runtime-v8 must embed the real bytes, and a source-JS fallback
    # must never be mislabeled as snapshot_policy=embedded in the package manifest.
    # shellcheck source=scripts/lib/snapshot-fingerprint.sh
    source "$SCRIPT_DIR/lib/snapshot-fingerprint.sh"
    snapshot_require_materialized_snapshot "$SNAPSHOT_BIN"
    info "verifying the target host/full/linux snapshot before compiling"
    bash "$SCRIPT_DIR/check-snapshot-freshness.sh" --product-profile full --os linux "$ARCH"
else
    info "snapshot_policy=none for $ARCH -- no SNAPSHOT-full-linux-$ARCH.bin exists yet" \
        "(needs a run on real AArch64 Linux hardware); runtime-v8/build.rs will fail" \
        "safe and fall back to loading JS from source"
fi

export RUSTFLAGS="${RUSTFLAGS:-} $RUSTFLAGS_SYSROOT_LINK"

# No stamp file and no `cargo clean -p v8` here any more. Both existed because cargo
# reruns the v8 build script on a change to the *value* of RUSTY_V8_ARCHIVE rather than
# to the file at that path, so a V8 rebuilt in place left cargo reusing an rlib built
# from the previous archive -- which cost a debugging cycle over a staged libmigo.so
# still carrying an allocator shim the rebuild had removed. The archive is now
# materialised at a content-addressed path, so different bytes *are* a different value
# and cargo's own rule is correct. A workaround for a staleness that can no longer
# happen is worse than none: it would go on clearing the crate for no reason.

info "sysroot: $MIGO_SYSROOT"
info "building capi staticlib (release, $TARGET)"
cargo build --manifest-path "$ENGINE_DIR/Cargo.toml" \
    -p migo-capi --release --target "$TARGET"

info "built: $ENGINE_DIR/target/$TARGET/release/libmigo_capi.a"

# The floor is a property of linked output -- a static archive carries no symbol
# versions of its own. This binary is built with the sysroot toolchain purely so
# there is something authoritative to audit; the out-of-tree consumers built with
# a stock compiler cannot serve, because their own objects legitimately carry the
# build host's newer symbols.
info "building the reference consumer used for the floor audit"
cargo build --manifest-path "$ENGINE_DIR/Cargo.toml" \
    -p migo-c-host-example --release --target "$TARGET"

info "capturing the link line cargo uses"
LINK_NOTE="$ENGINE_DIR/target/$TARGET/release/native-static-libs.txt"
# CARGO_TERM_COLOR=never: this note is parsed, not read by a person. Left unset,
# CI's toolchain setup forces CARGO_TERM_COLOR=always, which wraps the note in
# ANSI codes -- the trailing reset glues onto the last -l token with no
# separating whitespace and corrupts the parsed link line. See build-android-sdk.sh
# for the reproduction that found this.
CARGO_TERM_COLOR=never cargo rustc --manifest-path "$ENGINE_DIR/Cargo.toml" \
    -p migo-capi --lib --release --target "$TARGET" --crate-type staticlib -- \
    --print native-static-libs > "$LINK_NOTE" 2>&1
grep -q 'native-static-libs:' "$LINK_NOTE" \
    || { echo "[linux-sdk] cargo did not report native-static-libs" >&2; exit 1; }

# The repository's single release-version source. Read rather than derived from a
# crate manifest, and with no fallback: a default here ships a package labelled
# with a version nobody chose, which is how the Linux and HarmonyOS SDKs could
# have been built as `0.1.0` from a tree that was not.
# shellcheck source=scripts/lib/release-version.sh
source "$SCRIPT_DIR/lib/release-version.sh"

VERSION="$(read_release_version "$REPO_ROOT")"

# ------------------------------------------------------------
# libmigo.so
#
# Linked here from the static library rather than produced by a cdylib crate,
# for one reason: rustc gives every cdylib a version script of its own listing
# each reachable `#[no_mangle]` symbol, the linker takes the union with ours,
# and the result exported 338 symbols -- `diplomat_*`, `temporal_rs_*` and
# `rusty_v8_*` from dependency crates alongside the 20 documented entry points.
# `--exclude-libs,ALL` does not reach them either. Linking here makes migo.map
# the only version script, so the exported surface is exactly what the headers
# declare.
#
# Two details that are not obvious:
#   * No --whole-archive. The static library carries ICU twice (V8's copy and
#     Skia's), and pulling every member in produces duplicate definitions.
#     `-u` on each entry point gives the linker roots to resolve from, so it
#     pulls the same object set cargo's own link does.
#   * --no-undefined, so a missing symbol fails here rather than at dlopen on a
#     machine we cannot see.
#   * --gc-sections is load-bearing, not an optimisation. skia-bindings compiles
#     one translation unit containing wrappers for JPEG, PDF and pathops, and
#     Skia here is built without those features, so that object references
#     symbols that do not exist (SkJpegEncoder::Encode, SkPDF::AttributeList::*,
#     SkOpBuilder::resolve, ...). An executable survives because the unreferenced
#     wrappers are collected before their references have to resolve; a shared
#     library treats every global as a potential entry point and keeps them. The
#     version script above demotes everything but `migo_*` to local, which is
#     what lets section GC drop them here too.
#
#     This is the failure the C ABI runtime slice recorded as "adding Skia's
#     archives forces skia-bindings' translation unit to be linked whole". The
#     mechanism is different: the object is always linked, and what matters is
#     whether the linker may discard the parts of it nothing calls.
# ------------------------------------------------------------
# lld, not the default GNU ld.
#
# GNU ld produces a malformed symbol-version table for this link: imported
# symbols (snd_pcm_*, sendto, pthread_detach, ...) come out marked as defined in
# *ABS* carrying their originating library's version index, and the resulting
# library cannot be linked against at all -- `invalid version 11 (max 2)` from
# GNU ld, `corrupt input file: version definition index 3 out of bounds` from
# lld. Nothing in the static archive defines those symbols, so the entries are
# the linker's own. lld given the identical inputs produces a well-formed table
# and a library external consumers link cleanly.
#
# The version script cannot simply be dropped to avoid this: without it,
# --gc-sections has nothing to collect and the link fails on the JPEG/PDF
# wrappers described below.
#
# Resolved once by migo_sysroot_lld_bin (linux-sysroot.sh), which
# RUSTFLAGS_SYSROOT_LINK above already used for the cargo-driven links --
# same binary here so this script never asks the two links to agree by
# coincidence.
LLD_BIN="$(migo_sysroot_lld_bin)" || exit 1

BUILD_METADATA="$ENGINE_DIR/target/$TARGET/release/migo-linux-build-metadata.json"
python3 "$SCRIPT_DIR/write-linux-build-metadata.py" \
    --repo-root "$REPO_ROOT" \
    --output "$BUILD_METADATA" \
    --compiler "$CXX" \
    --linker "$LLD_BIN" \
    --sysroot "$MIGO_SYSROOT_IDENTITY"

info "linking libmigo.so (linker: $LLD_BIN)"
SO_MAP="$ENGINE_DIR/crates/capi/migo.map"
ENTRY_POINTS=$(python3 "$SCRIPT_DIR/c-abi-entry-points.py" embedded "$REPO_ROOT/include/migo")
UNDEF_ARGS=()
for entry in $ENTRY_POINTS; do
    UNDEF_ARGS+=("-Wl,-u,$entry")
done
NATIVE_LIBS=$(sed -n 's/.*native-static-libs: *//p' "$LINK_NOTE" | head -1)
SO_BUILD="$ENGINE_DIR/target/$TARGET/release/libmigo.so.$VERSION"

# --target is required here for exactly the reason linux-sysroot.sh's own
# comment on RUSTFLAGS_SYSROOT_LINK gives: clang without one compiles for the
# host's own default triple, and -B/-L only reorder candidates a
# target-aware driver already considers -- they cannot make a target-unaware
# one look in the right per-triple directory at all. Measured cross-compiling
# arm64 on this x86_64 host: omitting it left clang defaulting to
# x86_64-linux-gnu, which does not exist under the arm64-only sysroot, so it
# found neither crt objects nor libc/libdl/libpthread/libm/librt/libutil --
# `ld.lld: error: cannot open crti.o`, `unable to find library -ldl`, etc.
# -B/-L $SYSROOT_LIB_DIR are added on top, not left to --target's implicit
# multiarch guess, matching RUSTFLAGS_SYSROOT_LINK's own belt-and-suspenders
# choice rather than relying on undocumented driver heuristics.
SYSROOT_LIB_DIR="$MIGO_SYSROOT/usr/lib/$MIGO_SYSROOT_LIB_TRIPLET"

# --strip-all: this library is linked here by hand rather than by cargo, so the
# `strip = "symbols"` that [profile.release] asks for never reached it. The
# Android library is built through `cargo ndk` and does get stripped, so the two
# platforms shipped differently -- an omission of this link step, not a decision
# taken about Linux. Measured on the 0.9.1 x86_64 package: 79.5MB -> 57.6MB, and
# the 22MB is .strtab (16.4MB) plus .symtab (3.2MB) plus leftover .debug_*.
#
# Nothing reads any of it. Every consumer of this file goes through the dynamic
# tables -- abi-floor-audit.py's floor/exports/needed and the package contract's
# SONAME check all run objdump -T or -p -- or through raw bytes, as the
# snapshot-embedding contract does when it searches .rodata for the blob. A
# shared object keeps .dynsym/.dynstr through a strip, and the version script
# below still decides what is exported: `objdump -T` output is byte-identical
# before and after, all 24 migo_* exports included.
#
# Left to the linker rather than a separate strip pass because the aarch64
# package is cross-linked on an x86_64 release runner. A host binutils `strip`
# cannot read that output, llvm-strip is not guaranteed to be installed there,
# and this script already requires lld -- which can, on both targets.
# shellcheck disable=SC2086  # NATIVE_LIBS is a flag list and must word-split
"$CXX" --target="$MIGO_SYSROOT_LIB_TRIPLET" --ld-path="$LLD_BIN" -shared -o "$SO_BUILD" \
    -Wl,--version-script="$SO_MAP" \
    -Wl,-soname,libmigo.so.1 \
    -Wl,--no-undefined \
    -Wl,--gc-sections \
    -Wl,--strip-all \
    "${UNDEF_ARGS[@]}" \
    "$ENGINE_DIR/target/$TARGET/release/libmigo_capi.a" \
    $NATIVE_LIBS \
    -L "$LINK_DIR" -B "$LINK_DIR" \
    -L "$SYSROOT_LIB_DIR" -B "$SYSROOT_LIB_DIR" \
    --sysroot="$MIGO_SYSROOT"
info "linked: $SO_BUILD ($(stat -c %s "$SO_BUILD") bytes)"

info "staging package at $PREFIX"
rm -rf "$PREFIX"
mkdir -p "$PREFIX/include" "$PREFIX/lib" "$PREFIX/share/migo"
cp -r "$REPO_ROOT/include/migo" "$PREFIX/include/"
cp "$ENGINE_DIR/target/$TARGET/release/libmigo_capi.a" "$PREFIX/lib/libmigo.a"
cp "$SO_BUILD" "$PREFIX/lib/libmigo.so.$VERSION"
# soname -> real file, and the unversioned name a `-lmigo` link resolves.
ln -sf "libmigo.so.$VERSION" "$PREFIX/lib/libmigo.so.1"
ln -sf "libmigo.so.1" "$PREFIX/lib/libmigo.so"

# --shared: `-lmigo` resolves to libmigo.so unless a consumer asks for
# --static, so the CMake imported target has to describe the same default or the
# two integrations would disagree about what `migo::migo` is.
python3 "$SCRIPT_DIR/gen-linux-package-metadata.py" \
    --arch "$ARCH" --cargo-output "$LINK_NOTE" --prefix "$PREFIX" --version "$VERSION" --shared

info "recording the artifact manifest"
NEEDED_FILE="$ENGINE_DIR/target/$TARGET/release/dt-needed.txt"
# Taken from libmigo.so: the manifest describes what the shipped library needs,
# not what one particular consumer happened to link.
python3 "$SCRIPT_DIR/abi-floor-audit.py" needed "$SO_BUILD" > "$NEEDED_FILE"
SNAPSHOT_ARGS=()
if [[ "$SNAPSHOT_POLICY" == "embedded" ]]; then
    SNAPSHOT_ARGS=(--snapshot-bin "$SNAPSHOT_BIN" --snapshot-manifest "$SNAPSHOT_MANIFEST")
fi
python3 "$SCRIPT_DIR/gen-linux-package-metadata.py" --manifest \
    --arch "$ARCH" --prefix "$PREFIX" --version "$VERSION" \
    --needed-from "$NEEDED_FILE" --sysroot "$MIGO_SYSROOT_IDENTITY" \
    --v8-component-manifest "$V8_MANIFEST" --build-metadata "$BUILD_METADATA" \
    "${SNAPSHOT_ARGS[@]}"

PACKAGE_MANIFEST="$PREFIX/share/migo/linux-$ARCH-manifest.json"
info "verifying the complete staged package tree"
"$MANIFEST_TOOL" verify-linux-package "$PACKAGE_MANIFEST" "$PREFIX" >/dev/null

info "staged:"
find "$PREFIX" -type f -o -type l | sed "s|$PREFIX|  <prefix>|" | sort
