#!/usr/bin/env bash
# ============================================================
# 从源码裁剪编译 rusty_v8 的 Android 静态库 (librusty_v8.a)
# Location: scripts/build-v8-android.sh
#
# 站在 termux-packages 的 patch 之上(允许自定义 NDK / sysroot /
# libcxx / jumbo build),但 GN_ARGS 完全可控并追加体积优化。
#
# 产出:engine/third_party/rusty_v8/<arch>/librusty_v8.a + src_binding.rs
#      + component-manifest.json (verified source/toolchain/GN provenance)
#       (覆盖现有预编译 archive)
#
# 前置:
#   - RUSTY_V8_SRC 指向已 clone + submodule 完成的 rusty_v8 源码树
#     （没有机器相关的默认值，必须显式设置）
#   - ANDROID_NDK_HOME 指向 NDK
#   - patch 目录:engine/third_party/v8-patches/
#
# 用法:
#   ./scripts/build-v8-android.sh aarch64               # official_build 优化版
#   ./scripts/build-v8-android.sh aarch64 --reproduce   # 先复现(不加 official_build)
#   V8_KEEP_I18N=1 ./scripts/build-v8-android.sh aarch64 # 保留 i18n(默认关闭)
# ============================================================
set -euo pipefail

# ------------------------------------------------------------
# Paths
# ------------------------------------------------------------
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
ENGINE_ROOT="$PROJECT_ROOT/engine"
PATCH_DIR="$ENGINE_ROOT/third_party/v8-patches"
V8_OUT_DIR="$ENGINE_ROOT/third_party/rusty_v8"
V8_COMPONENT_WRITER="$SCRIPT_DIR/write-v8-component-manifest.py"
V8_BUILD_LOCK="$PROJECT_ROOT/contracts/artifact-manifest/android-v8.lock.json"

RUSTY_V8_SRC="${RUSTY_V8_SRC:-$PROJECT_ROOT/../rusty_v8_src}"
ANDROID_API="${ANDROID_API:-26}"

# ------------------------------------------------------------
# Logging
# ------------------------------------------------------------
info()  { echo -e "\033[0;36m[V8] $1\033[0m"; }
ok()    { echo -e "\033[0;32m[V8-OK] $1\033[0m"; }
warn()  { echo -e "\033[0;33m[V8-WARN] $1\033[0m"; }
err()   { echo -e "\033[0;31m[V8-ERR] $1\033[0m"; }

# ------------------------------------------------------------
# Args
# ------------------------------------------------------------
ARCH="aarch64"
REPRODUCE=false
for a in "$@"; do
    case "$a" in
        aarch64|x86_64) ARCH="$a" ;;
        --reproduce)    REPRODUCE=true ;;
        *) err "unknown arg: $a"; exit 1 ;;
    esac
done

case "$ARCH" in
    aarch64) TARGET="aarch64-linux-android"; GN_CPU="arm64" ;;
    x86_64)  TARGET="x86_64-linux-android";  GN_CPU="x64" ;;
esac

# ------------------------------------------------------------
# Sanity checks
# ------------------------------------------------------------
[[ -d "$RUSTY_V8_SRC" ]]        || { err "RUSTY_V8_SRC not found: $RUSTY_V8_SRC"; exit 1; }
[[ -f "$RUSTY_V8_SRC/build.rs" ]] || { err "not a rusty_v8 tree (no build.rs): $RUSTY_V8_SRC"; exit 1; }
[[ -d "$PATCH_DIR" ]]           || { err "patch dir not found: $PATCH_DIR"; exit 1; }
[[ -f "$V8_COMPONENT_WRITER" ]] || { err "component writer not found: $V8_COMPONENT_WRITER"; exit 1; }
[[ -f "$V8_BUILD_LOCK" ]]       || { err "V8 source lock not found: $V8_BUILD_LOCK"; exit 1; }
python3 "$SCRIPT_DIR/verify-v8-source-cohort.py" "$PROJECT_ROOT" "$RUSTY_V8_SRC"
[[ "$ANDROID_API" == "26" ]]    || { err "verified Android V8 builds require ANDROID_API=26"; exit 1; }

# The NDK supplies the target compiler, sysroot and linker, all recorded in the
# component manifest, so which NDK is used is part of what the artifact claims to
# be. Found by its own Pkg.Revision rather than defaulted to a path.
# shellcheck source=scripts/lib/android-ndk.sh
source "$SCRIPT_DIR/lib/android-ndk.sh"
android_ndk_read_pin "$V8_BUILD_LOCK" || exit 1
android_ndk_resolve || { err "cannot resolve the pinned Android NDK"; exit 1; }
info "using Android NDK $ANDROID_NDK_PIN at $ANDROID_NDK_HOME"

# submodule completeness
if [[ ! -f "$RUSTY_V8_SRC/v8/include/v8-version.h" ]]; then
    err "v8 submodule not checked out yet (no v8/include/v8-version.h)"
    err "run: cd $RUSTY_V8_SRC && git submodule update --init --recursive"
    exit 1
fi

V8_VER="$(awk '/#define V8_MAJOR_VERSION/{a=$3} /#define V8_MINOR_VERSION/{b=$3} /#define V8_BUILD_NUMBER/{c=$3} /#define V8_PATCH_LEVEL/{d=$3} END{print a"."b"."c"."d}' "$RUSTY_V8_SRC/v8/include/v8-version.h")"
info "V8 version: $V8_VER  (target=$TARGET, api=$ANDROID_API, reproduce=$REPRODUCE)"

# ------------------------------------------------------------
# Apply termux patches
# ------------------------------------------------------------
# All patches apply at the rusty_v8 root with -p1.  The search-files
# diff is for deno's vendored layout only and is NOT applied here.
# shellcheck source=scripts/lib/v8-patch-apply.sh
source "$SCRIPT_DIR/lib/v8-patch-apply.sh"

# Read from the lock, never restated here. The lock used to declare three patches
# while this script applied four: the prebuilt-binding diff was in neither the lock
# nor the manifest writer's table, so a sealed manifest recorded a patch set the
# build had not actually used. A second literal list is how that happens, so there
# is one declaration and both the applier and the drift gate read it.
#
# The lock's own notes explain each entry. Deliberately not declared, and therefore
# refused by the drift gate if they ever appear in the tree:
# 0004 is required again on 150.4: without its declare_args entries, GN ignores
# the custom NDK root/version and the manifest would identify the wrong SDK.
# Historical jumbo patches were never selected by a shipping recipe and have
# been removed. The build uses the upstream translation-unit graph.
# Via a command substitution, not `mapfile < <(...)`: a process substitution's exit
# status is not what `||` sees, so a parser that printed a valid prefix and then
# failed on a malformed entry would leave a truncated declaration behind and pass
# the non-empty guard. This build would then apply fewer patches than the lock
# requires and only find out at manifest time, after producing artifacts.
V8_DECLARED_OUTPUT="$(v8_read_declared_patches "$V8_BUILD_LOCK")" \
    || { err "cannot read the declared patch set from $V8_BUILD_LOCK"; exit 1; }
[[ -n "$V8_DECLARED_OUTPUT" ]] || { err "the V8 lock declares no patches"; exit 1; }
mapfile -t V8_DECLARED_PATCHES <<<"$V8_DECLARED_OUTPUT"

# Paths in the rusty_v8 tree whose provenance is established by something other
# than a patch. The pinned gn and its receipt are identified by the receipt
# itself, checked below. Passed explicitly at the call site rather than through a
# variable the library reads, so no exported value can grant an exemption. Named
# exactly rather than as a directory prefix, so a new file appearing beside them
# fails the gate instead of inheriting the exemption.
V8_ACCOUNTED_ARGS=(
    --accounted 'third_party/v8_correct_gn/gn'
    --accounted 'third_party/v8_correct_gn/gn-receipt.json'
    # One checkout serves every platform's V8 build, and the OpenHarmony one creates
    # a GN toolchain file this declaration does not touch. Accounted from the patch
    # itself rather than by naming the path, so it cannot drift from what that patch
    # creates; the library refuses to account for a path a foreign patch merely
    # modifies, which is the case that would skip real verification.
    --accounted-patch '0008-ohos-toolchain.patch'
)

apply_patches() {
    info "ensuring termux v8 patches are applied"
    # Applied-ness is re-derived on every run rather than stamped: gn/submodule
    # operations during the build can reset build/ and silently drop a patch.
    local glob
    for glob in "${V8_DECLARED_PATCHES[@]}"; do
        v8_require_patch "$RUSTY_V8_SRC" "$PATCH_DIR" "$glob" || exit 1
    done
    # Applying each patch proves each of them landed; it does not prove nothing
    # else did. An edit no committed patch explains cannot be reproduced from a
    # clean checkout, so it must not reach a release artifact silently.
    info "checking the source tree is HEAD plus exactly those patches"
    v8_assert_tree_is_exactly_patched "$RUSTY_V8_SRC" "$PATCH_DIR" \
        "${V8_ACCOUNTED_ARGS[@]}" "${V8_DECLARED_PATCHES[@]}" || {
        err "the rusty_v8 tree carries changes the committed patches do not explain"
        err "commit them as patches under $PATCH_DIR, or revert them"
        exit 1
    }
    ok "patches verified, tree explained (jumbo skipped)"
}

apply_patches

# ------------------------------------------------------------
# NDK wiring: symlink user's NDK into rusty_v8's expected location
# so build.rs skips the r26c auto-download (it checks for
# third_party/android_ndk/.../aarch64-linux-android24-clang++).
# ------------------------------------------------------------
NDK_HOST="linux-x86_64"
LINK="$RUSTY_V8_SRC/third_party/android_ndk"
mkdir -p "$RUSTY_V8_SRC/third_party"
if [[ ! -e "$LINK" ]]; then
    ln -sf "$ANDROID_NDK_HOME" "$LINK"
    info "linked NDK -> third_party/android_ndk"
fi
if [[ ! -e "$LINK/toolchains/llvm/prebuilt/$NDK_HOST/bin/aarch64-linux-android24-clang++" ]]; then
    warn "NDK lacks android24-clang++; build.rs may try to download r26c"
fi

# ------------------------------------------------------------
# GN args
#   Base set mirrors termux's working Android recipe.
#   Size/perf policy (locked decisions):
#     - JIT (TurboFan/Maglev/Sparkplug): KEPT  (performance first)
#     - WebAssembly: KEPT  (games depend on it)
#     - pointer compression: KEPT  (memory)
#     - i18n: DISABLED  (no game uses Intl; drops V8-side ICU)
#     - is_official_build: ENABLED unless --reproduce (the main new win)
# ------------------------------------------------------------
NDK_VER="$(awk -F= '/^Pkg\.Revision/{gsub(/[[:space:]]/, "", $2); split($2, parts, "."); print parts[1]; exit}' \
    "$ANDROID_NDK_HOME/source.properties" 2>/dev/null || true)"
[[ -n "$NDK_VER" ]] || { err "cannot read NDK major revision from source.properties"; exit 1; }

GN_ARGS="android_ndk_api_level=$ANDROID_API"
GN_ARGS+=" android_ndk_root=\"$ANDROID_NDK_HOME\""
GN_ARGS+=" android_ndk_version=\"r${NDK_VER}\""
# use_jumbo_build is not an upstream declared argument in this source cohort.
GN_ARGS+=" v8_enable_webassembly=true"
GN_ARGS+=" rusty_v8_enable_simdutf=true"
GN_ARGS+=" icu_use_data_file=true icu_use_stub_data=true"
GN_ARGS+=" icu_copy_icudata_to_root_build_dir=true v8_depend_on_icu_data_file=true"
GN_ARGS+=" v8_enable_pointer_compression=true"

# i18n: rusty_v8's src/binding.cc UNCONDITIONALLY includes
# <unicode/locid.h> and calls icu::Locale (exposes V8 locale API to
# Rust/deno_core). Disabling i18n removes the ICU headers and breaks the
# binding compile ("'unicode/locid.h' file not found"). So i18n stays ON
# by default; the ICU *data* (8.5M) is stripped separately in stage 2,
# which is where the real size win is. Set V8_NO_I18N=1 only if you also
# patch binding.cc to drop its ICU usage.
if [[ "${V8_NO_I18N:-0}" == "1" ]]; then
    GN_ARGS+=" v8_enable_i18n_support=false"
    warn "i18n: DISABLED (V8_NO_I18N=1) — requires patched binding.cc"
else
    GN_ARGS+=" v8_enable_i18n_support=true"
    info "i18n: ENABLED (required by rusty_v8 binding.cc)"
fi

if [[ "$REPRODUCE" == false ]]; then
    # The size win we did not have before.
    GN_ARGS+=" is_official_build=true"
    GN_ARGS+=" symbol_level=0"
    # is_official_build defaults chrome_pgo_phase=2, which downloads a
    # Chrome-specific PGO profile from a GS bucket (fails + irrelevant to
    # our V8 lib). Disable PGO; keep official_build's other size wins.
    GN_ARGS+=" chrome_pgo_phase=0"
    # NB: snapshot compression needs v8_use_zlib=true (extra dep); the
    # blob saving is small, so we skip it to avoid pulling zlib into V8.
    GN_ARGS+=" v8_enable_sandbox=false"
    # Drop C++ unwind tables (.eh_frame). Chromium keeps them on Android
    # even for official builds (exclude_unwind_tables defaults to
    # `is_official_build && !is_android` = false here) for crash
    # reporting. Our .so runs with panic=abort and never unwinds C++
    # exceptions, so the per-function CFI in .eh_frame is dead weight
    # (~1.6 MB of V8). This does NOT affect V8's JS-level unwinder API
    # (v8-unwinder.h), which is separate.
    GN_ARGS+=" exclude_unwind_tables=true"
    # ThinLTO OFF (measured 2026-07-14: no stress fps gain). The migo-vs-Chromium
    # stress gap is NOT V8 execution — pure-JS microbenches (mono/mega/GC) run
    # EQUAL-OR-FASTER on migo's V8 14.5 than on webview's V8 11.4. The gap is the
    # rendering/command-stream path, not V8, so PGO/ThinLTO don't help. Keeping
    # native objects → links with plain NDK lld 12, portable + reproducible.
    # (To re-experiment with ThinLTO: use_thin_lto=true + point migo's linker at
    # rusty_v8's own ld.lld 22 via engine/.cargo/config.toml --ld-path.)
    GN_ARGS+=" use_thin_lto=false"
    info "optimized build: +is_official_build +symbol_level=0 (pgo off, no unwind tables, no thin-lto)"
else
    info "reproduce build: termux-equivalent args only (no official_build)"
fi

export EXTRA_GN_ARGS="$GN_ARGS"
export V8_FROM_SOURCE=1
# NB: do NOT set CLANG_BASE_PATH to the NDK clang. That var is for the
# HOST toolchain (mksnapshot/torque run on the build machine); the NDK
# clang targets Android. termux leaves it to build.rs, which finds a
# system clang or downloads Chromium's. We do the same.

# GN + NINJA: build.rs auto-downloads these from chrome-infra-packages
# via ninja_gn_binaries.py, which uses http.client (does NOT honor the
# https_proxy env var) and times out here. We pre-fetch the *correct*
# gn (V8 14.5 needs gn >= 2315 for path_exists(); skia's bundled gn 2175
# is too old) into the source tree and point GN at it. Precedence:
#   V8_GN_PATH env > prefetched third_party/v8_correct_gn/gn > system gn
PREFETCHED_GN="$RUSTY_V8_SRC/third_party/v8_correct_gn/gn"
GN_BIN="${V8_GN_PATH:-}"
[[ -z "$GN_BIN" && -x "$PREFETCHED_GN" ]] && GN_BIN="$PREFETCHED_GN"
[[ -z "$GN_BIN" ]] && GN_BIN="$(command -v gn 2>/dev/null || true)"

if [[ -n "$GN_BIN" && -x "$GN_BIN" ]]; then
    export GN="$GN_BIN"
else
    err "no gn binary available and chrome-infra download is blocked."
    err "build the pinned gn: ./scripts/build-gn.sh"
    exit 1
fi

# gn generates the whole build graph, so an unpinned gn is an unrecorded input to
# every artifact this script produces. Logging the version it happened to find is
# not enough, and neither is checking that version: gn prints a commit position
# taken from `git describe HEAD` with no dirty marker, so a gn built from the
# pinned commit but without the pinned patches reports the same string. The build
# receipt scripts/build-gn.sh writes beside the binary is what ties it to a patch
# set.
# shellcheck source=scripts/lib/gn-pin.sh
source "$SCRIPT_DIR/lib/gn-pin.sh"
gn_pin_read "$V8_BUILD_LOCK" || exit 1
gn_pin_assert_binary "$GN" "$ENGINE_ROOT/third_party/gn-patches" || {
    err "gn at $GN does not match the pin in $(basename "$V8_BUILD_LOCK")"
    err "build the pinned gn: ./scripts/build-gn.sh"
    exit 1
}
info "using gn: $GN ($("$GN" --version), pinned and receipted)"

# ninja: prefer system, else error (we don't rely on the blocked download)
NINJA_BIN="$(command -v ninja 2>/dev/null || true)"
if [[ -n "$NINJA_BIN" ]]; then
    export NINJA="$NINJA_BIN"
    info "using ninja: $NINJA ($("$NINJA" --version 2>/dev/null))"
else
    err "no ninja binary; install ninja-build"
    exit 1
fi

info "EXTRA_GN_ARGS = $EXTRA_GN_ARGS"

# ------------------------------------------------------------
# Build via cargo (rusty_v8's build.rs drives gn + ninja)
# ------------------------------------------------------------
# LIBCLANG_PATH: rusty_v8's top-level bindgen (build.rs:233) parses V8's
# vendored libc++ headers, which require Clang 19+ (they use
# __builtin_clzg/__builtin_ctzg and #warn "Libc++ only supports Clang 20
# and later"). The system libclang is 18 (too old) and the NDK r23 one is
# 12 (far too old). NDK r28c ships libclang 19.0.1 — use it. Overridable
# via V8_LIBCLANG_PATH.
#
# When no suitable libclang exists we fall back to the content-addressed
# binding committed under engine/third_party/rusty_v8/<arch>/, but only
# after verifying it against its own component manifest (see
# verify_prebuilt_binding below). Reusing it unverified would let a
# release build consume an unidentified native input, which the artifact
# contract forbids.
LIBCLANG_MIN_MAJOR=19
PREBUILT_BINDING="$V8_OUT_DIR/$TARGET/src_binding.rs"
PREBUILT_MANIFEST="$V8_OUT_DIR/$TARGET/component-manifest.json"

# Read the rusty_v8 checkout's revision WITHOUT invoking git: that tree is
# owned by another uid, so git refuses it ("dubious ownership") and adding
# a safe.directory exception would mutate this machine's git config.
read_rusty_v8_revision() {
    local head_file="$RUSTY_V8_SRC/.git/HEAD" head ref
    [[ -f "$head_file" ]] || return 1
    head="$(<"$head_file")"
    if [[ "$head" == ref:* ]]; then
        ref="${head#ref:}"; ref="${ref#"${ref%%[![:space:]]*}"}"
        if [[ -f "$RUSTY_V8_SRC/.git/$ref" ]]; then
            head="$(<"$RUSTY_V8_SRC/.git/$ref")"
        elif [[ -f "$RUSTY_V8_SRC/.git/packed-refs" ]]; then
            head="$(awk -v r="$ref" '$2 == r {print $1; exit}' \
                "$RUSTY_V8_SRC/.git/packed-refs")"
        else
            return 1
        fi
    fi
    head="${head//[[:space:]]/}"
    [[ "$head" =~ ^[0-9a-fA-F]{40}$ ]] || return 1
    printf '%s\n' "$head"
}

# Fail closed unless the candidate binding is exactly the artifact its
# manifest describes, produced from exactly this rusty_v8 revision.
verify_prebuilt_binding() {
    local why="$1"
    local fail_hint=(
        "regenerating the binding requires a Clang ${LIBCLANG_MIN_MAJOR}+ libclang;"
        "point V8_LIBCLANG_PATH at the lib/ directory of one (it must contain"
        "libclang.so, and a sibling bin/clang so bindgen can find the matching"
        "clang resource headers)."
    )
    if [[ ! -f "$PREBUILT_MANIFEST" ]]; then
        err "$why, and no component manifest to verify a prebuilt binding against:"
        err "  missing $PREBUILT_MANIFEST"
        printf '%s\n' "${fail_hint[@]}" >&2
        exit 1
    fi
    if [[ ! -f "$PREBUILT_BINDING" ]]; then
        err "$why, and no prebuilt binding to fall back to: missing $PREBUILT_BINDING"
        printf '%s\n' "${fail_hint[@]}" >&2
        exit 1
    fi

    local want_hash want_rev got_hash got_rev
    want_hash="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["hashes"]["rust_binding"])' \
        "$PREBUILT_MANIFEST" 2>/dev/null || true)"
    want_rev="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["runtime"]["rusty_v8_revision"])' \
        "$PREBUILT_MANIFEST" 2>/dev/null || true)"
    if [[ ! "$want_hash" =~ ^[0-9a-f]{64}$ || ! "$want_rev" =~ ^[0-9a-fA-F]{40}$ ]]; then
        err "$why, and $PREBUILT_MANIFEST does not declare a usable"
        err "hashes.rust_binding + runtime.rusty_v8_revision pair"
        printf '%s\n' "${fail_hint[@]}" >&2
        exit 1
    fi

    got_hash="$(sha256sum "$PREBUILT_BINDING" | awk '{print $1}')"
    if [[ "$got_hash" != "$want_hash" ]]; then
        err "$why, and the prebuilt binding does NOT match its manifest:"
        err "  $PREBUILT_BINDING"
        err "  expected sha256 $want_hash"
        err "  actual   sha256 $got_hash"
        err "refusing to build against an unidentified binding."
        printf '%s\n' "${fail_hint[@]}" >&2
        exit 1
    fi

    got_rev="$(read_rusty_v8_revision || true)"
    if [[ -z "$got_rev" ]]; then
        err "$why, and cannot read the rusty_v8 revision from $RUSTY_V8_SRC/.git"
        err "refusing to reuse a binding whose source revision is unknown."
        printf '%s\n' "${fail_hint[@]}" >&2
        exit 1
    fi
    if [[ "${got_rev,,}" != "${want_rev,,}" ]]; then
        err "$why, and the prebuilt binding was generated from a different source:"
        err "  manifest runtime.rusty_v8_revision = $want_rev"
        err "  $RUSTY_V8_SRC is at                 = $got_rev"
        err "the binding encodes V8's FFI ABI, so reusing it across revisions is unsafe."
        printf '%s\n' "${fail_hint[@]}" >&2
        exit 1
    fi

    ok "prebuilt binding verified: sha256=$got_hash rusty_v8=$got_rev"
    ok "  (matches $PREBUILT_MANIFEST)"
}

# Report a libclang directory's major version, or nothing if undeterminable.
# clang_getClangVersion() is the authority: the SONAME/filename can lie
# (Chromium's rust-toolchain libclang.so.22.0.0git embeds a 21.0.0git
# string literal but reports 22 through the API).
libclang_major() {
    local dir="$1"
    python3 - "$dir/libclang.so" <<'PY' 2>/dev/null || true
import ctypes, re, sys
class CXString(ctypes.Structure):
    _fields_ = [("data", ctypes.c_void_p), ("private_flags", ctypes.c_uint)]
try:
    lib = ctypes.CDLL(sys.argv[1])
    lib.clang_getClangVersion.restype = CXString
    lib.clang_getCString.restype = ctypes.c_char_p
    lib.clang_getCString.argtypes = [CXString]
    text = lib.clang_getCString(lib.clang_getClangVersion()).decode()
except Exception:
    raise SystemExit(1)
match = re.search(r"clang version (\d+)", text)
if not match:
    raise SystemExit(1)
print(match.group(1))
PY
}

# A libclang is only usable if bindgen can find the matching builtin headers, and
# that is a separate fact from its version. Chromium's
# `third_party/rust-toolchain/lib/libclang.so` reports clang 22 -- it passes any
# version floor -- and ships **no sibling `bin/clang`**, so rusty_v8's build.rs
# `-print-resource-dir` probe finds nothing and bindgen falls back to the NDK's clang
# 12 builtin headers. The binding it then emits is wrong rather than absent:
# `cppgc_Visitor` sized 1, nested enums missing their `v8_String_` prefixes, 840 items
# instead of 870, and a `1_usize - 8_usize` overflow. A misconfigured libclang does not
# fail loudly, it corrupts the FFI ABI -- so the resource directory is required to
# resolve, not assumed from a version number.
libclang_resource_dir() {
    local dir="$1" clang resource
    for clang in "$dir/../bin/clang" "$dir/../bin/clang++"; do
        [[ -x "$clang" ]] || continue
        resource="$("$clang" -print-resource-dir 2>/dev/null || true)"
        [[ -n "$resource" && -d "$resource" ]] || continue
        printf '%s' "$resource"
        return 0
    done
    return 1
}

LIBCLANG_DIR="${V8_LIBCLANG_PATH:-}"
if [[ -z "$LIBCLANG_DIR" ]]; then
    # prefer an NDK that ships clang >= 19
    for cand in "$HOME/Android/android-ndk-r28c" "$HOME/Android/android-ndk-r29"; do
        if [[ -f "$cand/toolchains/llvm/prebuilt/$NDK_HOST/lib/libclang.so" ]]; then
            LIBCLANG_DIR="$cand/toolchains/llvm/prebuilt/$NDK_HOST/lib"
            break
        fi
    done
fi

USE_PREBUILT_BINDING=false
if [[ -n "$LIBCLANG_DIR" && -f "$LIBCLANG_DIR/libclang.so" ]]; then
    LIBCLANG_MAJOR="$(libclang_major "$LIBCLANG_DIR")"
    if [[ -z "$LIBCLANG_MAJOR" ]]; then
        # Cannot tell the version -> do NOT guess. An too-old libclang does
        # not merely fail; it silently emits a WRONG binding (see below).
        warn "cannot determine clang version of $LIBCLANG_DIR/libclang.so"
        warn "not guessing: falling back to the verified prebuilt binding"
        USE_PREBUILT_BINDING=true
    elif (( LIBCLANG_MAJOR < LIBCLANG_MIN_MAJOR )); then
        warn "libclang at $LIBCLANG_DIR is clang $LIBCLANG_MAJOR (< $LIBCLANG_MIN_MAJOR)"
        USE_PREBUILT_BINDING=true
    elif ! LIBCLANG_RESOURCE_DIR="$(libclang_resource_dir "$LIBCLANG_DIR")"; then
        warn "libclang at $LIBCLANG_DIR is clang $LIBCLANG_MAJOR but has no usable sibling clang"
        warn "bindgen would silently use another toolchain's builtin headers and emit a"
        warn "binding that is wrong rather than missing; falling back to the verified prebuilt one"
        USE_PREBUILT_BINDING=true
    else
        export LIBCLANG_PATH="$LIBCLANG_DIR"
        info "LIBCLANG_PATH = $LIBCLANG_PATH (clang $LIBCLANG_MAJOR, resource dir $LIBCLANG_RESOURCE_DIR) — regenerating binding"
    fi
else
    warn "no libclang found for bindgen"
    USE_PREBUILT_BINDING=true
fi

if [[ "$USE_PREBUILT_BINDING" == true ]]; then
    verify_prebuilt_binding "no Clang ${LIBCLANG_MIN_MAJOR}+ libclang is available to regenerate the binding"
    export V8_PREBUILT_BINDING="$PREBUILT_BINDING"
    BINDING_ORIGIN="prebuilt (verified against component manifest)"
    info "V8_PREBUILT_BINDING = $V8_PREBUILT_BINDING"
else
    BINDING_ORIGIN="regenerated by bindgen via $LIBCLANG_PATH"
fi

# bindgen needs the NDK sysroot to find Android headers.
BINDGEN_SYSROOT="$ANDROID_NDK_HOME/toolchains/llvm/prebuilt/$NDK_HOST/sysroot"
export BINDGEN_EXTRA_CLANG_ARGS="--target=$TARGET --sysroot=$BINDGEN_SYSROOT"
TARGET_U="$(echo "$TARGET" | tr 'a-z-' 'A-Z_')"
export "BINDGEN_EXTRA_CLANG_ARGS_${TARGET_U}=$BINDGEN_EXTRA_CLANG_ARGS"

# Ensure the android target is installed.
rustup target add "$TARGET" >/dev/null 2>&1 || true

info "starting cargo build (first run downloads gn/sysroot/android_platform, slow)"
pushd "$RUSTY_V8_SRC" >/dev/null

BUILD_LOG="${TMPDIR:-/tmp}/migo-v8-build-${ARCH}.$$.log"
set +e
cargo build --release --target "$TARGET" --features simdutf,v8_enable_pointer_compression -vv 2>&1 | tee "$BUILD_LOG" | \
    grep --line-buffered -iE "error|warning: |Compiling v8|Finished|gn gen|ninja|downloading|cloning" | tail -100
RC=${PIPESTATUS[0]}
set -e
popd >/dev/null

if [[ $RC -ne 0 ]]; then
    err "cargo build failed (rc=$RC). full log: $BUILD_LOG"
    err "last 20 lines:"
    tail -20 "$BUILD_LOG"
    exit $RC
fi

# ------------------------------------------------------------
# Locate + copy artifacts
# ------------------------------------------------------------
GN_OUT="$RUSTY_V8_SRC/target/$TARGET/release/gn_out/obj/librusty_v8.a"
[[ -f "$GN_OUT" ]] || GN_OUT="$(find "$RUSTY_V8_SRC/target/$TARGET/release" -name 'librusty_v8.a' -print -quit 2>/dev/null)"
# build.rs writes the binding to gn_out/src_binding.rs on both paths
# (bindgen and V8_PREBUILT_BINDING). Prefer that exact location; the
# find fallback would otherwise pick an arbitrary first match, which can
# be a stale copy under target/release/build/*/out.
BINDING="$RUSTY_V8_SRC/target/$TARGET/release/gn_out/src_binding.rs"
[[ -f "$BINDING" ]] || BINDING="$(find "$RUSTY_V8_SRC/target/$TARGET/release" -name 'src_binding*.rs' -print -quit 2>/dev/null)"

[[ -f "$GN_OUT" ]] || { err "librusty_v8.a not found after build"; exit 1; }

DEST="$V8_OUT_DIR/$TARGET"
mkdir -p "$DEST"
# back up the existing (termux) archive once
if [[ -f "$DEST/librusty_v8.a" && ! -f "$DEST/librusty_v8.a.termux-bak" ]]; then
    cp "$DEST/librusty_v8.a" "$DEST/librusty_v8.a.termux-bak"
    info "backed up existing archive -> librusty_v8.a.termux-bak"
fi

cp "$GN_OUT" "$DEST/librusty_v8.a"
ok "archive -> $DEST/librusty_v8.a ($(ls -lh "$DEST/librusty_v8.a" | awk '{print $5}'))"
[[ -n "$BINDING" && -f "$BINDING" ]] || { err "src_binding.rs not found after build"; exit 1; }
cp "$BINDING" "$DEST/src_binding.rs"
ok "binding -> $DEST/src_binding.rs  [$BINDING_ORIGIN]"
# When we fed build.rs the prebuilt binding, the copy must be byte-identical
# to it -- otherwise something rewrote the binding mid-build and the manifest
# we are about to seal would attest to an input we never verified.
if [[ "$USE_PREBUILT_BINDING" == true ]]; then
    if ! cmp -s "$PREBUILT_BINDING" "$DEST/src_binding.rs"; then
        err "the binding produced by the build differs from the verified prebuilt one"
        err "  verified: $PREBUILT_BINDING"
        err "  produced: $DEST/src_binding.rs"
        exit 1
    fi
    ok "produced binding is byte-identical to the verified prebuilt input"
else
    # Regenerated. A binding that differs from the recorded one is either a real V8
    # ABI change or a misconfigured libclang, and the two are indistinguishable from
    # the bytes alone -- so this stops rather than seals a manifest over an FFI surface
    # nobody compared. Set MIGO_V8_BINDING_CHANGE_EXPECTED=1 when the difference is the
    # point (a V8 bump), which makes accepting a new ABI a deliberate act.
    regenerated_hash="$(sha256sum "$DEST/src_binding.rs" | awk '{print $1}')"
    recorded_hash="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["hashes"]["rust_binding"])' \
        "$PREBUILT_MANIFEST" 2>/dev/null || true)"
    if [[ -z "$recorded_hash" ]]; then
        info "no recorded binding to compare against; this build establishes it"
    elif [[ "$regenerated_hash" == "$recorded_hash" ]]; then
        ok "regenerated binding matches the recorded one (sha256=$regenerated_hash)"
    elif [[ "${MIGO_V8_BINDING_CHANGE_EXPECTED:-0}" == "1" ]]; then
        warn "regenerated binding differs from the recorded one, and that was declared expected"
        warn "  recorded:    $recorded_hash"
        warn "  regenerated: $regenerated_hash"
    else
        err "the regenerated binding differs from the one $PREBUILT_MANIFEST records"
        err "  recorded:    $recorded_hash"
        err "  regenerated: $regenerated_hash"
        err "a wrong libclang corrupts the FFI ABI silently, so this is not accepted on"
        err "trust. Diff the two, and if the change is intended, re-run with"
        err "MIGO_V8_BINDING_CHANGE_EXPECTED=1."
        exit 1
    fi
fi

python3 "$V8_COMPONENT_WRITER" \
    --repo-root "$PROJECT_ROOT" \
    --rusty-v8-src "$RUSTY_V8_SRC" \
    --ndk-home "$ANDROID_NDK_HOME" \
    --arch "$ARCH" \
    --extra-gn-args "$EXTRA_GN_ARGS" \
    "${V8_ACCOUNTED_ARGS[@]}" \
    --archive "$DEST/librusty_v8.a" \
    --binding "$DEST/src_binding.rs" \
    --output "$DEST/component-manifest.json" \
    --lock "$V8_BUILD_LOCK"
ok "component manifest -> $DEST/component-manifest.json"

ok "V8 build complete for $ARCH (reproduce=$REPRODUCE)"
echo "next: rebuild libmigo.so via scripts/build-android-so.sh $([ "$ARCH" = aarch64 ] && echo arm64-v8a || echo x86_64)"
