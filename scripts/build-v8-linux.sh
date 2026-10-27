#!/usr/bin/env bash
# ============================================================
# Build the linux-gnu librusty_v8.a that migo's Linux SDK links against.
# Location: scripts/build-v8-linux.sh
#
# Counterpart to scripts/build-v8-android.sh. It differs from whatever archive
# happens to sit in ../rusty_v8_src in two ways that both matter, and both of
# which force a full rebuild anyway -- so they are done in one pass:
#
#   1. SYSROOT. The archive previously used by host builds was compiled against
#      the build machine's glibc 2.39 headers. `nm --undefined-only` shows 27
#      undefined __isoc23_* references in it (platform-linux.o, string.o,
#      locale.o, ...). Those are renamed entry points introduced in glibc 2.38,
#      not versioned aliases, so nothing at link time can satisfy them against
#      the 2.31 loader floor that scripts/abi-floor-audit.py enforces.
#      use_sysroot=true builds against the Debian bullseye sysroot.
#
#   2. SHARED-OBJECT LINKAGE. An archive compiled for executables may use the
#      local-exec TLS model, whose relocations cannot appear in a shared object
#      -- which is why libmigo.so could not be linked at all. Position-
#      independent code plus a dynamic TLS model makes the same archive usable
#      from both an executable and a shared library.
#
# Usage:
#   scripts/build-v8-linux.sh [aarch64|x86_64]   # build and install (default: x86_64)
#   scripts/build-v8-linux.sh [aarch64|x86_64] --check
#
# Output: engine/third_party/rusty_v8/<arch>-linux-gnu/
#           librusty_v8.a + src_binding.rs + component-manifest.json
# ============================================================
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
ENGINE_ROOT="$PROJECT_ROOT/engine"

ARCH="x86_64"
CHECK_ONLY=0
for a in "$@"; do
    case "$a" in
        aarch64|x86_64) ARCH="$a" ;;
        --check) CHECK_ONLY=1 ;;
        *) echo "usage: $0 [aarch64|x86_64] [--check]" >&2; exit 2 ;;
    esac
done

# TARGET is the rust target triple cargo builds for; GN_CPU is what GN's own
# target_cpu variable expects (arm64/x64, not aarch64/x86_64); SYSROOT_ARCH is
# the suffix Chromium's sysroot_scripts uses for the Debian bullseye sysroot
# directory name -- all three vocabularies disagree with each other and with
# migo's own target-triple convention, and none of the three can be derived
# from either of the others.
case "$ARCH" in
    aarch64) TARGET="aarch64-unknown-linux-gnu"; GN_CPU="arm64"; SYSROOT_ARCH="arm64" ;;
    x86_64)  TARGET="x86_64-unknown-linux-gnu";  GN_CPU="x64";   SYSROOT_ARCH="amd64" ;;
esac
OUT_DIR="${MIGO_V8_OUT_ROOT:-$ENGINE_ROOT/third_party/rusty_v8}/$ARCH-linux-gnu"

RUSTY_V8_SRC="${RUSTY_V8_SRC:-$PROJECT_ROOT/../rusty_v8_src}"
GN_OUT="$RUSTY_V8_SRC/target/$TARGET/release/gn_out"
BACKUP_DIR="$RUSTY_V8_SRC/target/$TARGET/release/gn_out-pre-sdk-backup"

info() { echo -e "\033[0;36m[v8-linux] $*\033[0m"; }
err()  { echo -e "\033[0;31m[v8-linux] $*\033[0m" >&2; }

[[ -d "$RUSTY_V8_SRC" ]] || { err "rusty_v8 source not found: $RUSTY_V8_SRC"; exit 1; }

if (( CHECK_ONLY )); then
    for archive in "$GN_OUT/obj/librusty_v8.a" "$OUT_DIR/librusty_v8.a"; do
        [[ -f "$archive" ]] || { echo "missing: $archive"; continue; }
        echo "=== $archive"
        if [[ "$ARCH" == "x86_64" ]]; then
            echo "  glibc 2.38 entry points referenced: \
$(nm --undefined-only "$archive" 2>/dev/null | grep -c '__isoc23_' || true)"
            echo "  local-exec TLS relocations: \
$(objdump -r "$archive" 2>/dev/null | grep -cw 'R_X86_64_TPOFF32' || true)"
            echo "  local-dynamic TLS relocations: \
$(objdump -r "$archive" 2>/dev/null | grep -cw 'R_X86_64_DTPOFF32' || true)"
        else
            echo "  glibc 2.38 entry points referenced: \
$(nm --undefined-only "$archive" 2>/dev/null | grep -c '__isoc23_' || true)"
            echo "  TLS relocation types present (informational -- AArch64's dialect is\
 confirmed by readelf and the real whole-archive shared-link probe):"
            python3 "$SCRIPT_DIR/lib/v8_linux_tls.py" --arch "$ARCH" "$archive"
        fi
    done
    exit 0
fi

# V8's source tag predates bindgen's Clang 22 cursor fix. A fresh binding is
# required for this cohort; use the fixed generator rather than editing it.
python3 "$SCRIPT_DIR/verify-v8-source-cohort.py" "$PROJECT_ROOT" "$RUSTY_V8_SRC"
source "$SCRIPT_DIR/lib/v8-patch-apply.sh"
v8_require_patch "$RUSTY_V8_SRC" "$ENGINE_ROOT/third_party/v8-patches" \
    '0010-bindgen-clang22-lock.patch'

# ------------------------------------------------------------
# Preserve the existing archive. It is what dev-test-host.sh and the player
# link against today; if this build goes wrong, the host must still work.
# ------------------------------------------------------------
if [[ -f "$GN_OUT/obj/librusty_v8.a" && ! -d "$BACKUP_DIR" ]]; then
    info "backing up the current archive to $BACKUP_DIR"
    mkdir -p "$BACKUP_DIR/obj"
    cp "$GN_OUT/obj/librusty_v8.a" "$BACKUP_DIR/obj/"
    cp "$GN_OUT/src_binding.rs" "$BACKUP_DIR/" 2>/dev/null || true
fi

# ------------------------------------------------------------
# GN arguments
# ------------------------------------------------------------
GN_ARGS="is_official_build=true"
GN_ARGS+=" symbol_level=0"
# is_official_build defaults chrome_pgo_phase=2, which downloads a
# Chrome-specific profile from a GS bucket: irrelevant here and it fails.
GN_ARGS+=" chrome_pgo_phase=0"
GN_ARGS+=" use_thin_lto=false"
GN_ARGS+=" use_glib=false"
GN_ARGS+=" is_cfi=false"
GN_ARGS+=" exclude_unwind_tables=true"
GN_ARGS+=" v8_enable_sandbox=false"
GN_ARGS+=" v8_enable_pointer_compression=true"
GN_ARGS+=" v8_enable_webassembly=true"
GN_ARGS+=" rusty_v8_enable_simdutf=true"
# i18n stays on: rusty_v8's binding.cc unconditionally includes
# <unicode/locid.h>, so disabling it breaks the binding compile.
GN_ARGS+=" v8_enable_i18n_support=true"
# Migo supplies its filtered ICU package before the first isolate. Keep the
# build-time data file for mksnapshot, but omit a second full package from the
# archive; upstream .gn otherwise forces embedded ICU data.
GN_ARGS+=" icu_use_data_file=true icu_use_stub_data=true"
GN_ARGS+=" icu_copy_icudata_to_root_build_dir=true v8_depend_on_icu_data_file=true"

# target_cpu: the host is x86_64, so an aarch64 build is genuine cross-
# compilation. GN defaults target_cpu to host_cpu when unset, which is
# correct for x86_64 by accident, not by design -- stated explicitly for both
# so neither depends on that default. Chromium's own bundled clang
# (third_party/llvm-build) is inherently a cross-compiler; no separate
# aarch64-linux-gnu-gcc toolchain is required.
GN_ARGS+=" target_cpu=\"$GN_CPU\""

# (1) Build against the Debian bullseye sysroot. With use_sysroot=true,
# build/config/sysroot.gni resolves //build/linux/debian_bullseye_<arch>-
# sysroot from target_cpu -- the same sysroot scripts/lib/linux-sysroot.sh
# pins the rest of the engine at for x86_64. Debian ships one glibc/libstdc++
# version per release across every architecture it supports, so the same
# 2.31/3.4.28 loader floor scripts/abi-floor-audit.py enforces applies
# unchanged to the arm64 sysroot -- not re-derived per arch below.
GN_ARGS+=" use_sysroot=true"

# (2) Make the archive linkable into a shared object.
#
# V8 pins the TLS model of its hot thread-locals in source:
#
#   __attribute__((tls_model(V8_TLS_MODEL))) extern thread_local Isolate*
#       g_current_isolate_;
#
# A source-level attribute beats -ftls-model on a command line, so a model flag
# alone changes nothing. Three things were measured before arriving here, all
# recorded so they are not retried:
#
#   * `-ftls-model=global-dynamic` has no effect on these variables. Verified by
#     recompiling isolate.cc with and without it: byte-identical relocations.
#   * `extra_cflags` / `extra_asmflags` do not exist in this build tree at all.
#     They are Skia arguments, not Chromium ones; gn accepted them into args.gn
#     and no compiler command line ever saw them.
#   * Adding the attribute to the *definition* in isolate.cc (the declaration in
#     isolate.h has it, the definition does not) changes nothing either -- the
#     declaration's attribute already applies, as C++ requires.
#
# Measure with `grep -w R_X86_64_TPOFF32`, never a substring match:
# R_X86_64_DTPOFF32 contains "TPOFF32", so a loose match counts the
# local-dynamic relocations this build is trying to produce as failures. That
# error made a correct archive look broken through several rebuilds.
#
# V8's own switch is V8_TLS_USED_IN_LIBRARY (src/common/thread-local-storage.h):
# it selects "local-dynamic" and hides each variable behind a non-inlined
# getter, which is what V8 documents for "static archive linked into a shared
# library". BUILD.gn defines it when both arguments below are set. The monolith
# target that v8_monolithic declares is never built -- nothing rusty_v8 depends
# on references it -- so this buys the define without changing the artifact.
#
# -fPIC needs no argument: build/config/compiler/BUILD.gn already adds it for
# this platform, confirmed by the archive carrying zero absolute relocations.
#
# The ARM64 archive was observed to use TLSDESC relocations and passed a
# whole-archive shared-object link. The readelf gate below rejects TLSLE and
# an empty/wrong-architecture report, independent of host objdump support.
GN_ARGS+=" v8_monolithic=true"
GN_ARGS+=" v8_monolithic_for_shared_library=true"
# Required by v8_monolithic's own assertions.
GN_ARGS+=" v8_use_external_startup_data=false"

# (3) Do not replace the process allocator.
#
# Chromium's PartitionAlloc shim, on by default for linux, overrides global
# malloc/free. That is right for a browser which owns its whole process; it is
# wrong for a library embedded in someone else's, where it ends up freeing
# pointers the host allocated with the system allocator. Measured: with the shim
# on, a C host linking libmigo.so segfaults on a null dereference inside
# `allocator_shim::internal::PartitionAllocFunctionsInternal<>::Free` as soon as
# content loading crosses the boundary.
#
# An engine that hijacks its host's allocator is not embeddable, so this stays
# off regardless of the linkage form.
GN_ARGS+=" use_allocator_shim=false"
GN_ARGS+=" use_partition_alloc_as_malloc=false"

export EXTRA_GN_ARGS="$GN_ARGS"
export V8_FROM_SOURCE=1

# A previous release's binding cannot be carried across a V8 upgrade. Generate
# from the current C++ headers by default; an explicit prebuilt input must prove
# its version, source revision and bytes before a patch can make use of it.
if [[ -n "${V8_PREBUILT_BINDING:-}" ]]; then
    python3 "$SCRIPT_DIR/verify-v8-prebuilt-binding.py" \
        --source "$RUSTY_V8_SRC" --binding "$V8_PREBUILT_BINDING" \
        --manifest "${V8_PREBUILT_BINDING_MANIFEST:-$(dirname "$V8_PREBUILT_BINDING")/component-manifest.json}"
fi

# ------------------------------------------------------------
# Toolchain: gn, ninja, libclang
#
# rusty_v8's build.rs otherwise runs tools/ninja_gn_binaries.py, which fetches
# gn and ninja from chrome-infra-packages over a code path that ignores the
# proxy environment and hangs here. build-v8-android.sh solves this the same
# way: supply the binaries and never enter that download.
# ------------------------------------------------------------
PREFETCHED_GN="$RUSTY_V8_SRC/third_party/v8_correct_gn/gn"
GN_BIN="${V8_GN_PATH:-}"
[[ -z "$GN_BIN" && -x "$PREFETCHED_GN" ]] && GN_BIN="$PREFETCHED_GN"
[[ -z "$GN_BIN" ]] && GN_BIN="$(command -v gn 2>/dev/null || true)"
if [[ -n "$GN_BIN" && -x "$GN_BIN" ]]; then
    export GN="$GN_BIN"
    info "gn: $GN"
else
    err "no gn binary, and the chrome-infra download hangs. Set V8_GN_PATH."
    exit 1
fi

NINJA_BIN="$(command -v ninja 2>/dev/null || true)"
if [[ -n "$NINJA_BIN" ]]; then
    export NINJA="$NINJA_BIN"
    info "ninja: $NINJA"
else
    err "no ninja binary; install ninja-build"
    exit 1
fi

# bindgen parses V8's vendored libc++ headers, which need clang 19 or newer.
# The system libclang is older, so an NDK copy is borrowed for the parse only --
# it never touches the code generation for this host target. Moot when a
# prebuilt binding is already being reused (the common case, above), since
# bindgen never runs at all then.
LIBCLANG_DIR="${V8_LIBCLANG_PATH:-}"
if [[ -z "$LIBCLANG_DIR" ]]; then
    for cand in "$HOME/Android/android-ndk-r28c" "$HOME/Android/android-ndk-r29"; do
        if [[ -f "$cand/toolchains/llvm/prebuilt/linux-x86_64/lib/libclang.so" ]]; then
            LIBCLANG_DIR="$cand/toolchains/llvm/prebuilt/linux-x86_64/lib"
            break
        fi
    done
fi
if [[ -n "$LIBCLANG_DIR" && -f "$LIBCLANG_DIR/libclang.so" ]]; then
    export LIBCLANG_PATH="$LIBCLANG_DIR"
    info "libclang: $LIBCLANG_PATH"
fi

# ------------------------------------------------------------
# Force a regeneration when the arguments changed.
#
# rusty_v8's build.rs writes args.gn but reuses an existing build.ninja, so a
# changed argument lands in args.gn and never reaches a compiler command line.
# Measured: adding -DV8_TLS_USED_IN_LIBRARY produced a 25-second "build" that
# re-archived the same objects, args.gn showed the define, and build.ninja
# contained zero occurrences of it. Comparing against the generated ninja file
# rather than args.gn is deliberate -- args.gn is what was asked for, and
# build.ninja is what will actually be compiled.
# ------------------------------------------------------------
if [[ -f "$GN_OUT/build.ninja" ]]; then
    stale=0
    # The properties this build exists to produce. Both have to appear in a
    # compiler command line, not merely in args.gn. gn writes compile flags into
    # toolchain.ninja and the per-target files rather than build.ninja, so the
    # search covers the whole directory -- checking build.ninja alone reports
    # every build as stale and silently turns each one into a full rebuild.
    # `find -exec` rather than grep's --include: grep here may be ugrep, whose
    # --include matching differs, and a silently-empty search would report every
    # build as stale and turn each one into a needless full rebuild.
    for needle in "V8_TLS_USED_IN_LIBRARY" "debian_bullseye_${SYSROOT_ARCH}-sysroot" \
                  "USE_ALLOCATOR_SHIM=false"; do
        if ! find "$GN_OUT" -name '*.ninja' -exec grep -qF "$needle" {} + 2>/dev/null; then
            info "generated build files do not carry '$needle'; regenerating from scratch"
            stale=1
        fi
    done
    if [[ "$stale" -eq 1 ]]; then
        rm -rf "$GN_OUT"
    fi
fi

info "gn args: $GN_ARGS"
info "building V8 for $ARCH"

# Bindgen is a host executable parsing target headers. GN's sysroot arguments
# do not reach libclang; pass the same pinned target explicitly, scoped so
# cross arguments cannot leak into Cargo's host build dependencies.
python3 "$RUSTY_V8_SRC/build/linux/sysroot_scripts/install-sysroot.py" --arch "$SYSROOT_ARCH"
BINDGEN_TARGET_ARGS="$(python3 "$SCRIPT_DIR/lib/v8_bindgen.py" \
    --source "$RUSTY_V8_SRC" --target "$TARGET")"

cd "$RUSTY_V8_SRC"
env "BINDGEN_EXTRA_CLANG_ARGS_${TARGET}=$BINDGEN_TARGET_ARGS" \
    cargo build --release --target "$TARGET" -p v8 --features simdutf,v8_enable_pointer_compression

ARCHIVE="$GN_OUT/obj/librusty_v8.a"
[[ -f "$ARCHIVE" ]] || { err "build produced no archive at $ARCHIVE"; exit 1; }

# ------------------------------------------------------------
# Verify both properties before installing. An archive that silently fails
# either one would be discovered much later, in a link error or a load failure
# on a user's machine.
# ------------------------------------------------------------
isoc23_count="$(nm --undefined-only "$ARCHIVE" 2>/dev/null | grep -c '__isoc23_' || true)"
if [[ "$isoc23_count" -ne 0 ]]; then
    err "archive still references $isoc23_count glibc 2.38 entry points; the sysroot did not take effect"
    exit 1
fi
info "sysroot verified: 0 glibc 2.38 entry points referenced"

if [[ "$ARCH" == "x86_64" ]]; then
    # The relocation count is only a proxy; what matters is whether a shared
    # object can actually be produced. Ask the linker rather than infer.
    # -w and the full relocation name on purpose: R_X86_64_DTPOFF32 contains
    # "TPOFF32" as a substring, and a loose match counts local-dynamic
    # relocations -- which are exactly what this build is trying to produce --
    # as failures. That mistake reported a working archive as broken.
    tls_relocs="$(objdump -r "$ARCHIVE" 2>/dev/null | grep -cw 'R_X86_64_TPOFF32' || true)"
    if [[ "$tls_relocs" -ne 0 ]]; then
        err "archive still carries $tls_relocs local-exec TLS relocations;"
        err "V8_TLS_USED_IN_LIBRARY did not reach every translation unit."
        exit 1
    fi
    info "TLS verified: 0 local-exec relocations"
fi
python3 "$SCRIPT_DIR/lib/v8_linux_tls.py" --arch "$ARCH" "$ARCHIVE"

info "installing to $OUT_DIR"
mkdir -p "$OUT_DIR"
cp "$ARCHIVE" "$OUT_DIR/librusty_v8.a"
cp "$GN_OUT/src_binding.rs" "$OUT_DIR/src_binding.rs"

# Emit the same sealed component contract Android uses, with Linux's target and
# loader floors. The package generator embeds this complete identity; it no
# longer accepts the old three-field provenance note.
info "recording and verifying V8 component identity"
V8_SYSROOT="$RUSTY_V8_SRC/build/linux/debian_bullseye_${SYSROOT_ARCH}-sysroot"
[[ -d "$V8_SYSROOT" ]] || {
    err "the GN build did not materialize its Debian bullseye sysroot at $V8_SYSROOT"
    exit 1
}
PATCH_ARGS=(--patch "bindgen-clang22-lock=$ENGINE_ROOT/third_party/v8-patches/0010-bindgen-clang22-lock.patch")
PREBUILT_BINDING_PATCH="$ENGINE_ROOT/third_party/v8-patches/migo-build-rs-prebuilt-binding.diff"
if git -C "$RUSTY_V8_SRC" apply --reverse --check "$PREBUILT_BINDING_PATCH" \
        >/dev/null 2>&1; then
    PATCH_ARGS+=(--patch "migo-build-rs-prebuilt-binding=$PREBUILT_BINDING_PATCH")
fi
# Only relevant when cross-compiling (v8_snapshot_toolchain != default_toolchain,
# true for this aarch64 build on an x86_64 host, false for a native x86_64
# build) -- see the patch's own comment for why. Declared the same
# apply-if-present way as the prebuilt-binding patch above rather than
# assumed present: a native aarch64 build (e.g. on a real arm64 CI runner)
# does not need it, and the source-proof check must describe the tree that
# is actually there, not the tree a cross-compiling host happens to have.
CXX_TOOLCHAIN_PATCH="$ENGINE_ROOT/third_party/v8-patches/0003-compiler-use-custom-libcxx-for-v8.patch"
if git -C "$RUSTY_V8_SRC" apply --reverse --check "$CXX_TOOLCHAIN_PATCH" \
        >/dev/null 2>&1; then
    PATCH_ARGS+=(--patch "compiler-use-custom-libcxx-for-v8=$CXX_TOOLCHAIN_PATCH")
fi
# Same "declare if present, do not assume" treatment as the two patches above:
# part of migo's general bindgen-environment patch series, not target-specific,
# but declared rather than assumed since this build reuses a prebuilt binding
# and never runs bindgen itself.
BINDGEN_ENV_PATCH="$ENGINE_ROOT/third_party/v8-patches/0001-unset-BINDGEN_EXTRA_CLANG_ARGS-in-v8_s-bindgen.patch"
if git -C "$RUSTY_V8_SRC" apply --reverse --check "$BINDGEN_ENV_PATCH" \
        >/dev/null 2>&1; then
    PATCH_ARGS+=(--patch "unset-bindgen-extra-clang-args=$BINDGEN_ENV_PATCH")
fi
# Adds an OHOS toolchain file; unrelated to this build's own target but part
# of the same shared checkout other migo builds add it to. Declared, not
# assumed, same as the three patches above.
OHOS_TOOLCHAIN_PATCH="$ENGINE_ROOT/third_party/v8-patches/0008-ohos-toolchain.patch"
if git -C "$RUSTY_V8_SRC" apply --reverse --check "$OHOS_TOOLCHAIN_PATCH" \
        >/dev/null 2>&1; then
    PATCH_ARGS+=(--patch "ohos-toolchain=$OHOS_TOOLCHAIN_PATCH")
fi
# The other build.rs hunk migo-build-rs-prebuilt-binding.diff's own header
# comment names explicitly: extends the `target_os == "linux"` sysroot-install
# branch to also cover "android". Not exercised by this build (ARCH is never
# "android" here) but present in the shared checkout regardless.
ANDROID_SYSROOT_PATCH="$ENGINE_ROOT/third_party/v8-patches/0002-install-sysroot.patch"
if git -C "$RUSTY_V8_SRC" apply --reverse --check "$ANDROID_SYSROOT_PATCH" \
        >/dev/null 2>&1; then
    PATCH_ARGS+=(--patch "install-sysroot=$ANDROID_SYSROOT_PATCH")
fi
python3 "$SCRIPT_DIR/write-linux-v8-component-manifest.py" \
    --repo-root "$PROJECT_ROOT" \
    --rusty-v8-src "$RUSTY_V8_SRC" \
    --arch "$ARCH" \
    --gn-args "$GN_ARGS" \
    --archive "$OUT_DIR/librusty_v8.a" \
    --binding "$OUT_DIR/src_binding.rs" \
    --sysroot "$V8_SYSROOT" \
    --output "$OUT_DIR/component-manifest.json" \
    "${PATCH_ARGS[@]}"

info "done: $OUT_DIR/librusty_v8.a ($(stat -c %s "$OUT_DIR/librusty_v8.a") bytes)"
