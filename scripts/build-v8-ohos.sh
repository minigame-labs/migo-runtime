#!/usr/bin/env bash
# ============================================================
# Build the OpenHarmony librusty_v8.a that migo's OHOS target links against.
# Location: scripts/build-v8-ohos.sh
#
# Counterpart to scripts/build-v8-{android,linux,windows}.sh. It is the most
# involved of the four, because upstream Chromium has no OpenHarmony support at
# all: `target_os` is not in any whitelist and neither build/ nor buildtools/
# mentions it. Every decision below was forced by a build that failed without
# it; the progression is recorded so nobody re-derives it.
#
#   attempt 1  ninja  37/2358  SDK clang 15 rejects `-std=c++23` (only knows
#                              the draft spelling `c++2b`) and three warning
#                              flags Chromium passes unconditionally. This is a
#                              compiler-version wall: both the 5.1.0 and the
#                              6.1 SDK ship clang 15.0.4, so no SDK bump fixes
#                              it. => V8 must be built with Chromium's clang.
#   attempt 2  ninja  56/2352  Chromium's clang, but its vendored libc++ has no
#                              musl branch: `__locale` reports "unknown rune
#                              table for this platform".
#   attempt 3  ninja 638/2284  Switched to the SDK's libc++ (which is built for
#                              musl) -- but it is libc++ 15 and V8 145's cppgc
#                              needs <source_location>, added in libc++ 16.
#   attempt 4  ninja 811/2317  Back to Chromium's libc++ with the portable rune
#                              table the error message itself names. Remaining
#                              failures were only -Werror on code unused in
#                              this platform combination.
#   attempt 5                  treat_warnings_as_errors=false, as build.rs
#                              already does for Android aarch64.
#
# Two libc++ copies end up in the product: V8 keeps Chromium's, Skia uses the
# SDK's. That is the arrangement Android already ships -- both are static
# libraries that never exchange C++ standard library objects across the
# boundary, and the public surface is a C ABI.
#
# Output: engine/third_party/rusty_v8/<triple>/
#           librusty_v8.a + src_binding.rs
#
# Usage:
#   scripts/build-v8-ohos.sh [x86_64|aarch64]   (default: x86_64)
#   scripts/build-v8-ohos.sh --check            report the current archive
#
# Env:
#   RUSTY_V8_SRC    rusty_v8 checkout (default: ../rusty_v8_src)
#   OHOS_NDK_HOME   OpenHarmony SDK (default: probed by dev-setup-ohos.sh)
# ============================================================
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
ENGINE_ROOT="$PROJECT_ROOT/engine"
RUSTY_V8_SRC="${RUSTY_V8_SRC:-$PROJECT_ROOT/../rusty_v8_src}"

info() { echo -e "\033[0;36m[v8-ohos] $*\033[0m"; }
err()  { echo -e "\033[0;31m[v8-ohos] $*\033[0m" >&2; }
ok()   { echo -e "\033[0;32m[v8-ohos] $*\033[0m"; }

ARCH="x86_64"
CHECK_ONLY=0
case "${1:-}" in
    --check)  CHECK_ONLY=1 ;;
    aarch64)  ARCH="aarch64" ;;
    x86_64|"") ARCH="x86_64" ;;
    armv7|arm|loongarch64)
        # Recognised and declined on purpose, rather than falling through to
        # "unknown argument" -- a bare rejection reads like a gap in the script
        # and invites someone to re-derive this decision.
        #
        # The SDK ships clang drivers for four architectures but sysroot
        # libraries for three (loongarch64 has a driver and no lib directory),
        # and of those three only two matter here: aarch64 is every HarmonyOS
        # NEXT device, x86_64 is the emulator. NEXT has no 32-bit devices, and
        # LoongArch is not in the market this runtime targets.
        #
        # Each additional architecture costs another ~160 MB V8 archive, another
        # build lane, and another artifact that has to stay verified. Adding one
        # should be triggered by a real consumer, not by symmetry: add a case
        # here and the matching gn toolchain in build/toolchain/ohos/BUILD.gn.
        err "$1 is deliberately not built; only x86_64 (emulator) and aarch64 (device) are"
        err "see the comment at this line for the reasoning and how to add one"
        exit 1
        ;;
    *) err "unknown argument: $1"; exit 1 ;;
esac

RUST_TRIPLE="$ARCH-unknown-linux-ohos"
case "$ARCH" in
    x86_64)  GN_CPU="x64";   GN_TOOLCHAIN="clang_x64" ;;
    aarch64) GN_CPU="arm64"; GN_TOOLCHAIN="clang_arm64" ;;
esac
OUT_DIR="$ENGINE_ROOT/third_party/rusty_v8/$ARCH-linux-ohos"

if [[ $CHECK_ONLY -eq 1 ]]; then
    archive="$OUT_DIR/librusty_v8.a"
    if [[ -f "$archive" ]]; then
        echo "=== $archive"
        echo "  size: $(du -h "$archive" | cut -f1)"
        echo "  PartitionAlloc hijack symbols: \
$(nm --defined-only "$archive" 2>/dev/null | grep -c 'PartitionAllocFunctionsInternal' || true)"
    else
        echo "missing: $archive"
    fi
    exit 0
fi

# ---- prerequisites ----------------------------------------------------------
[[ -d "$RUSTY_V8_SRC" ]] || { err "rusty_v8 source not found: $RUSTY_V8_SRC"; exit 1; }

eval "$(bash "$SCRIPT_DIR/dev-setup-ohos.sh" | grep '^export OHOS_')"
[[ -n "${OHOS_SDK_NATIVE:-}" ]] || { err "OpenHarmony SDK not usable"; exit 1; }

CHROMIUM_CLANG_BIN="$RUSTY_V8_SRC/third_party/llvm-build/Release+Asserts/bin"
if [[ ! -x "$CHROMIUM_CLANG_BIN/clang++" ]]; then
    info "Chromium clang absent; fetching it with Chromium's own script"
    (cd "$RUSTY_V8_SRC" && python3 tools/clang/scripts/update.py)
fi
[[ -x "$CHROMIUM_CLANG_BIN/clang++" ]] || { err "Chromium clang unavailable"; exit 1; }

# Applied-ness is derived from each patch itself rather than from a path existing, and the
# set comes from the lock rather than from a literal here -- the same single declaration the
# Android build reads. That matters beyond tidiness: this script exports
# V8_PREBUILT_BINDING, a hook that exists only because
# migo-build-rs-prebuilt-binding.diff adds it to build.rs. Naming only the OpenHarmony
# toolchain patch here made the build silently depend on somebody having run the Android
# build in this shared checkout first; without it, pristine rusty_v8 ignores the variable,
# falls through to bindgen and fails on the SDK's clang 15 after an hour of compiling.
# shellcheck source=scripts/lib/v8-patch-apply.sh
source "$SCRIPT_DIR/lib/v8-patch-apply.sh"
V8_BUILD_LOCK="$PROJECT_ROOT/contracts/artifact-manifest/ohos-v8.lock.json"
V8_DECLARED_OUTPUT="$(v8_read_declared_patches "$V8_BUILD_LOCK")" \
    || { err "cannot read the declared patch set from $V8_BUILD_LOCK"; exit 1; }
[[ -n "$V8_DECLARED_OUTPUT" ]] || { err "the V8 lock declares no patches"; exit 1; }
mapfile -t V8_DECLARED_PATCHES <<<"$V8_DECLARED_OUTPUT"
info "ensuring the ${#V8_DECLARED_PATCHES[@]} declared patches are applied"
for glob in "${V8_DECLARED_PATCHES[@]}"; do
    v8_require_patch "$RUSTY_V8_SRC" "$ENGINE_ROOT/third_party/v8-patches" "$glob" \
        || { err "patch failed: $glob"; exit 1; }
done

# rusty_v8 pins its own rustc (1.89.0 at the time of writing), which is not
# migo's. A target installed for migo's toolchain is invisible here.
V8_TOOLCHAIN="$(cd "$RUSTY_V8_SRC" && rustup show active-toolchain | cut -d' ' -f1)"
V8_TOOLCHAIN_DIR="$(rustup toolchain list -v | grep -F "$V8_TOOLCHAIN" | awk '{print $NF}')"
if [[ ! -d "$V8_TOOLCHAIN_DIR/lib/rustlib/$RUST_TRIPLE" ]]; then
    # `rustup target list --installed` under-reports ohos targets, so the
    # presence of the std directory is the authority here.
    info "installing $RUST_TRIPLE std into $V8_TOOLCHAIN"
    rustup target add --toolchain "$V8_TOOLCHAIN" "$RUST_TRIPLE"
fi

# ---- compiler wrappers ------------------------------------------------------
# Chromium hardcodes `--target=` per current_os in
# //build/config/compiler/BUILD.gn and this tree has no extra_cflags argument
# to append after it, so the override happens in the driver. clang honours the
# last --target, verified with -dumpmachine.
WRAP_DIR="$OHOS_NDK_HOME/.migo-v8-wrappers"
mkdir -p "$WRAP_DIR"
for triple in x86_64-unknown-linux-ohos aarch64-unknown-linux-ohos; do
    case "$triple" in
        x86_64-*)  llvm_triple="x86_64-linux-ohos" ;;
        aarch64-*) llvm_triple="aarch64-linux-ohos" ;;
    esac
    cat > "$WRAP_DIR/$triple-clang" <<EOF
#!/usr/bin/env bash
# Generated by scripts/build-v8-ohos.sh -- do not edit.
exec "$CHROMIUM_CLANG_BIN/clang" "\$@" --target=$llvm_triple --sysroot="$OHOS_SDK_NATIVE/sysroot"
EOF
    cat > "$WRAP_DIR/$triple-clang++" <<EOF
#!/usr/bin/env bash
# Generated by scripts/build-v8-ohos.sh -- do not edit.
# _LIBCPP_PROVIDES_DEFAULT_RUNE_TABLE: Chromium's libc++ has no musl branch in
# its locale layer and says so by name in the error it would otherwise emit.
exec "$CHROMIUM_CLANG_BIN/clang++" "\$@" --target=$llvm_triple \\
    --sysroot="$OHOS_SDK_NATIVE/sysroot" -D_LIBCPP_PROVIDES_DEFAULT_RUNE_TABLE
EOF
    chmod +x "$WRAP_DIR/$triple-clang" "$WRAP_DIR/$triple-clang++"
done
info "wrappers: $WRAP_DIR"

# ---- build ------------------------------------------------------------------
GN_ARGS="custom_toolchain=\"//build/toolchain/ohos:$GN_TOOLCHAIN\""
GN_ARGS+=" ohos_sdk_native=\"$OHOS_SDK_NATIVE\""
GN_ARGS+=" ohos_clang_wrapper_dir=\"$WRAP_DIR\""
# The OpenHarmony toolchain reports current_os = "linux" so that Chromium's
# Linux configuration applies (OpenHarmony is a Linux kernel). That has one
# consequence which must be corrected here: v8/gni/snapshot_toolchain.gni
# decides a build is native when `current_os == host_os && current_cpu ==
# host_cpu`, which is true for the x64 OpenHarmony target on an x64 Linux host.
# V8 would then build its code generators -- bytecode_builtins_list_generator,
# gen-regexp-special-case, mksnapshot -- with the cross toolchain and try to
# run OpenHarmony binaries on the build machine. Pinning both toolchains states
# the intent explicitly rather than relying on that inference.
case "$(uname -m)" in
    x86_64)  HOST_GN_CPU="x64" ;;
    aarch64) HOST_GN_CPU="arm64" ;;
    *) err "unsupported build host: $(uname -m)"; exit 1 ;;
esac
GN_ARGS+=" host_toolchain=\"//build/toolchain/linux:clang_$HOST_GN_CPU\""
GN_ARGS+=" v8_snapshot_toolchain=\"//build/toolchain/linux:clang_$HOST_GN_CPU\""
# Code that is unreachable in this platform combination trips -Werror; build.rs
# already does the same for Android aarch64.
GN_ARGS+=" treat_warnings_as_errors=false"
GN_ARGS+=" use_glib=false is_cfi=false"
GN_ARGS+=" use_thin_lto=false chrome_pgo_phase=0"
GN_ARGS+=" exclude_unwind_tables=true"
GN_ARGS+=" v8_enable_sandbox=false"
GN_ARGS+=" v8_enable_pointer_compression=false"
GN_ARGS+=" rusty_v8_enable_simdutf=true"
GN_ARGS+=" icu_use_data_file=true icu_use_stub_data=true"
GN_ARGS+=" icu_copy_icudata_to_root_build_dir=true v8_depend_on_icu_data_file=true"
GN_ARGS+=" v8_monolithic=true v8_monolithic_for_shared_library=true"
GN_ARGS+=" v8_use_external_startup_data=false"
# PartitionAlloc's shim hijacks malloc/free process-wide. An engine embedded in
# a host application must not do that.
GN_ARGS+=" use_allocator_shim=false use_partition_alloc_as_malloc=false"

export V8_FROM_SOURCE=1
export GN_ARGS

# bindgen cannot run here, and that is a property of the SDK rather than a
# configuration to tune. The OpenHarmony SDK ships clang 15 (both 5.1 and 6.1 do), while
# V8 145's vendored libc++ announces "Libc++ only supports Clang 20 and later" and uses
# `__builtin_clzg`/`__builtin_ctzg`, added in clang 19. Measured: the C++ side builds to
# completion and then `build.rs` panics with
# `Unable to generate bindings: ClangDiagnostic(... '_Tp' does not refer to a value ...)`,
# after more than an hour of compilation. So the committed binding for this triple is the
# input, via the same `V8_PREBUILT_BINDING` hook the Android build uses.
#
# The binding is safe to share because it encodes V8's FFI ABI for a rusty_v8 revision,
# not a target's calling convention: this file is byte-identical to the Android one at
# the pinned revision, and both OpenHarmony triples are LP64. It is committed, so a
# revision bump replaces it deliberately rather than regenerating silently against
# whichever clang is on PATH.
PREBUILT_BINDING="$OUT_DIR/src_binding.rs"
if [[ ! -f "$PREBUILT_BINDING" ]]; then
    err "no committed binding at $PREBUILT_BINDING"
    err "the SDK's clang 15 cannot generate one: V8's vendored libc++ needs clang 20+."
    err "commit the binding for this triple, or build with a newer OpenHarmony SDK clang."
    exit 1
fi
export V8_PREBUILT_BINDING="$PREBUILT_BINDING"
info "V8_PREBUILT_BINDING = $PREBUILT_BINDING"

# Without these, build.rs downloads its own gn/ninja, a step that can stall.
[[ -x "$RUSTY_V8_SRC/third_party/v8_correct_gn/gn" ]] && \
    export GN="$RUSTY_V8_SRC/third_party/v8_correct_gn/gn"
command -v ninja >/dev/null 2>&1 && export NINJA="$(command -v ninja)"

# gn generates the entire build graph, so it is an input to every byte of the archive --
# and this build *exempts* that binary and its receipt from the source replay, because
# they are untracked files whose provenance is the receipt rather than a patch. An
# exemption with no compensating assertion is just an unverified input, which is why the
# Android build asserts the same pin. Same binary, same lock block, same check.
# shellcheck source=scripts/lib/gn-pin.sh
source "$SCRIPT_DIR/lib/gn-pin.sh"
gn_pin_read "$V8_BUILD_LOCK" || exit 1
if [[ -n "${GN:-}" ]]; then
    gn_pin_assert_binary "$GN" "$ENGINE_ROOT/third_party/gn-patches" || {
        err "the gn this build would use is not the one $V8_BUILD_LOCK pins"
        exit 1
    }
else
    err "no prefetched gn at $RUSTY_V8_SRC/third_party/v8_correct_gn/gn"
    err "build it with: bash scripts/build-gn.sh"
    exit 1
fi

# Outside the vendored checkout, the way build-v8-android.sh already does it. A log
# written into $RUSTY_V8_SRC is an untracked file no committed patch explains, so it
# makes every provenance gate over that tree -- including the Android build's own --
# refuse it, and the tree is shared by all four platforms.
BUILD_LOG="${TMPDIR:-/tmp}/migo-v8-ohos-build-${ARCH}.$$.log"
info "building (log: $BUILD_LOG)"
info "GN_ARGS = $GN_ARGS"
if ! (cd "$RUSTY_V8_SRC" && cargo build --release --target "$RUST_TRIPLE" --features simdutf > "$BUILD_LOG" 2>&1); then
    err "build failed. last failing step:"
    grep -m1 -A20 "FAILED:" "$BUILD_LOG" >&2 || tail -30 "$BUILD_LOG" >&2
    exit 1
fi

# ---- install ----------------------------------------------------------------
ARCHIVE="$(find "$RUSTY_V8_SRC/target/$RUST_TRIPLE/release" -name 'librusty_v8.a' -print -quit)"
BINDING="$(find "$RUSTY_V8_SRC/target/$RUST_TRIPLE/release" -name 'src_binding*.rs' -print -quit)"
[[ -n "$ARCHIVE" ]] || { err "build reported success but no librusty_v8.a was produced"; exit 1; }
[[ -n "$BINDING" ]] || { err "build reported success but no src_binding.rs was produced"; exit 1; }

mkdir -p "$OUT_DIR"
cp "$ARCHIVE" "$OUT_DIR/librusty_v8.a"
cp "$BINDING" "$OUT_DIR/src_binding.rs"

# ---- verify -----------------------------------------------------------------
# A build that succeeds is not evidence the archive is usable. Each check below
# corresponds to a way this has gone wrong on another platform.
HIJACK="$(nm --defined-only "$OUT_DIR/librusty_v8.a" 2>/dev/null \
    | grep -c 'PartitionAllocFunctionsInternal' || true)"
if [[ "$HIJACK" != "0" ]]; then
    err "archive defines $HIJACK PartitionAlloc shim symbols; it would hijack the host allocator"
    exit 1
fi
ok "no PartitionAlloc hijack symbols"

ok "installed $OUT_DIR/librusty_v8.a ($(du -h "$OUT_DIR/librusty_v8.a" | cut -f1))"

# ---- seal ------------------------------------------------------------------
# Binds these bytes to a source revision, a GN argument set and an SDK identity.
# Until this existed, `scripts/build-ohos-sdk.sh` recorded the absence as a known gap
# in its own package manifest: the archive shipped with nothing saying which V8 it is.
# The writer also runs the shared replay proof over the vendored checkout, which no
# OpenHarmony build had ever done, so a stray edit there could reach this archive
# unrecorded.
V8_COMPONENT_WRITER="$SCRIPT_DIR/write-ohos-v8-component-manifest.py"
python3 "$V8_COMPONENT_WRITER" \
    --repo-root "$PROJECT_ROOT" \
    --rusty-v8-src "$RUSTY_V8_SRC" \
    --sdk-home "$OHOS_NDK_HOME" \
    --arch "$ARCH" \
    --gn-args "$GN_ARGS" \
    --archive "$OUT_DIR/librusty_v8.a" \
    --binding "$OUT_DIR/src_binding.rs" \
    --compiler "$WRAP_DIR/$RUST_TRIPLE-clang++" \
    --linker "$CHROMIUM_CLANG_BIN/ld.lld" \
    --accounted 'third_party/v8_correct_gn/gn' \
    --accounted 'third_party/v8_correct_gn/gn-receipt.json' \
    --output "$OUT_DIR/component-manifest.json"
ok "component manifest -> $OUT_DIR/component-manifest.json"
