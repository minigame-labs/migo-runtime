#!/usr/bin/env bash
# ============================================================
# Build the x86_64-pc-windows-msvc rusty_v8.lib that migo links on Windows.
# Location: scripts/build-v8-windows.sh
#
# Counterpart to scripts/build-v8-android.sh and scripts/build-v8-linux.sh.
# Like them it builds inside a real rusty_v8 checkout, because that is the only
# tree that can build V8 from source -- see "Why a checkout" below.
#
# WHY THIS EXISTS AT ALL (the measurement that forced it, 2026-07-23):
#
#   Linking Skia and the *prebuilt* rusty_v8 into one Windows binary fails:
#
#     skia.lib(core.SkExecutor.obj) : error LNK2005:
#       "void __cdecl std::terminate(void)" already defined in
#       libv8-....rlib(exception.obj)
#
#   Both sides carry a C++ runtime: Skia is built against the MSVC STL
#   (skia-bindings passes /MD), while the published rusty_v8 binaries bundle
#   V8's own libc++. That is a duplicate *definition*, so neither /OPT:REF nor
#   an import library can fix it -- only making both use the same C++ runtime
#   can, and the prebuilt cannot be changed. Building V8 here with
#   use_custom_libcxx=false puts V8 on the MSVC STL and the duplicate is gone
#   (measured: LNK2005 count 89 -> 0, and the LNK4098 libcmt conflict with it).
#
#   `migo-runtime-v8` links the prebuilt fine and its tests pass on Windows,
#   because a V8-only binary has nothing to collide with. Only Skia + V8 in one
#   binary needs this build.
#
# WHY A CHECKOUT AND NOT THE PUBLISHED CRATE:
#   The published `v8` crate cannot build V8 from source. Measured against a
#   real checkout it is missing 42912 tracked files, including
#   build/rust/known-target-triples.txt and the whole tools/win tree that gn
#   dereferences on Windows. Reconstructing it from another tree would also mix
#   provenance, which contracts/artifact-manifest/*.lock.json exists to prevent.
#   Separately, `use_custom_libcxx` is a *cargo feature*, not a GN argument
#   (build.rs derives the GN value from CARGO_FEATURE_USE_CUSTOM_LIBCXX and
#   overrides anything EXTRA_GN_ARGS says), and cargo refuses
#   --no-default-features for a package outside the workspace -- so the build
#   has to happen where v8 is the root package.
#
# PREREQUISITES (each was a real failure before it was one):
#   - RUSTY_V8_SRC_WIN: a rusty_v8 checkout ON A WINDOWS LOCAL DISK, with
#     submodules initialised. Not a UNC path: cargo produced no output in six
#     minutes over \\wsl.localhost and never created its target directory.
#     It must NOT carry another host's downloaded dependencies -- see --check.
#   - Visual Studio Build Tools with the C++ workload, and LLVM (clang-cl).
#   - GN and NINJA on the Windows side (see MIGO_WIN_GN / MIGO_WIN_NINJA).
#     They are passed explicitly because rusty_v8's own downloader cannot be
#     used here: tools/ninja_gn_binaries.py resolves CIPD through raw
#     http.client, which ignores HTTPS_PROXY, so on a machine that reaches the
#     internet only through a proxy it times out. Every other download in this
#     build goes through urllib (tools/download_file.py) and does honour it.
#   - Python: rusty_v8 resolves it via $PYTHON and otherwise calls "python3",
#     which Windows installs do not provide.
#
# Output: engine/third_party/rusty_v8/<target>/
#           rusty_v8.lib + src_binding.rs
#
# Usage:
#   scripts/build-v8-windows.sh [aarch64|x86_64]            # build and install (default x86_64)
#   scripts/build-v8-windows.sh [aarch64|x86_64] --check    # report readiness, build nothing
# ============================================================
set -euo pipefail

ARCH="x86_64"
CHECK_ONLY=0
for arg in "$@"; do
    case "$arg" in
        aarch64|x86_64) ARCH="$arg" ;;
        --check) CHECK_ONLY=1 ;;
        *) echo "usage: $0 [aarch64|x86_64] [--check]" >&2; exit 2 ;;
    esac
done

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
ENGINE_ROOT="$PROJECT_ROOT/engine"
case "$ARCH" in
    aarch64) TARGET="aarch64-pc-windows-msvc"; VS_ARM64_COMPONENT="Microsoft.VisualStudio.Component.VC.Tools.ARM64" ;;
    x86_64)  TARGET="x86_64-pc-windows-msvc";  VS_ARM64_COMPONENT="" ;;
esac
OUT_DIR="$ENGINE_ROOT/third_party/rusty_v8/$TARGET"
V8_BUILD_LOCK="$PROJECT_ROOT/contracts/artifact-manifest/windows-v8.lock.json"

# Windows-side paths. All DOS form: they are written into a batch file.
RUSTY_V8_SRC_WIN="${RUSTY_V8_SRC_WIN:-C:\\v8src}"
# Short by necessity, not tidiness: ninja calls the ANSI GetFullPathNameA,
# which is capped at MAX_PATH even with LongPathsEnabled, and gn_out plus
# Chromium's deepest headers overrun it from any longer root.
V8_TARGET_DIR_WIN="${MIGO_WIN_V8_TARGET_DIR:-C:\\v8o}"
# Same MAX_PATH constraint as the target directory, and it keeps this build on
# the same registry cache the rest of the Windows tooling already populated.
CARGO_HOME_WIN="${MIGO_WIN_CARGO_HOME:-C:\\cg}"
GN_WIN="${MIGO_WIN_GN:-C:\\migo-win-spike-tmp\\bin\\gn.exe}"
NINJA_WIN="${MIGO_WIN_NINJA:-C:\\migo-win-spike-tmp\\bin\\ninja.exe}"
PYTHON_WIN="${MIGO_WIN_PYTHON:-}"
PROXY="${MIGO_WIN_PROXY:-}"
# build.rs passes NUM_JOBS straight through to ninja's -j when set (see its
# ninja() helper). Unset, ninja and cargo both default to the logical CPU
# count; building two toolchains at once (host x64 for build-time codegen
# plus the arm64 target) roughly doubles peak clang-cl memory pressure over a
# single-arch build, and at full parallelism that starved this machine's RAM
# -- every crash was "LLVM ERROR: out of memory", not a compiler bug. Unlike
# PROXY this is not written into the batch file unconditionally: cargo
# already defaults NUM_JOBS sensibly for a single-toolchain build, so this
# only overrides it when the caller has actually hit the problem.
NUM_JOBS_WIN="${MIGO_WIN_NUM_JOBS:-}"

# printf, not `echo -e`: every path here is a DOS path, and `echo -e` expands
# backslash escapes inside the message -- "C:\v8src" prints as "C:<vtab>8src",
# silently corrupting the diagnostics that are supposed to tell you which tree
# was used.
info() { printf '\033[0;36m[v8-win] %s\033[0m\n' "$*"; }
ok()   { printf '\033[0;32m[v8-win] %s\033[0m\n' "$*"; }
err()  { printf '\033[0;31m[v8-win] %s\033[0m\n' "$*" >&2; }

# The Windows-side path as WSL sees it, for the readiness checks below.
win_to_unix() { printf '/mnt/%s' "$(printf '%s' "$1" | sed 's|\\|/|g; s|^\(.\):|\L\1|')"; }
find_windows_python() {
    local finder="/mnt/c/Windows/System32/where.exe" candidate
    [[ -f "$finder" ]] || return 1
    candidate="$("$finder" python.exe 2>/dev/null | tr -d '\r' | head -n 1)"
    [[ -n "$candidate" ]] || return 1
    printf '%s' "$candidate"
}
SRC_UNIX="$(win_to_unix "$RUSTY_V8_SRC_WIN")"

# ------------------------------------------------------------
# Readiness checks
# ------------------------------------------------------------
# These are separated from the build because every one of them failed once, and
# each failure surfaced far from its cause: a missing python became "program not
# found" inside build.rs, a Build Tools install under %ProgramFiles(x86)% became
# "No supported Visual Studio can be found", and a foreign host's downloaded
# toolchain became "[WinError 193] not a valid Win32 application" from a python
# script three layers down.
check_ready() {
    local failures=0

    # Same assertion build-v8-android.sh makes: the pin has to exist before the
    # build, not after it. Without it there is nothing to check the resulting
    # bytes against, and this build takes hours to discover that at the end.
    if [[ ! -f "$V8_BUILD_LOCK" ]]; then
        err "V8 source lock not found: $V8_BUILD_LOCK"; failures=$((failures + 1))
    fi

    if [[ ! -f "$SRC_UNIX/build.rs" ]]; then
        err "not a rusty_v8 checkout: $RUSTY_V8_SRC_WIN (no build.rs)"; failures=$((failures + 1))
    elif [[ ! -f "$SRC_UNIX/v8/include/v8-version.h" ]]; then
        err "v8 submodule not checked out in $RUSTY_V8_SRC_WIN"
        err "run: git -C <checkout> submodule update --init --recursive"
        failures=$((failures + 1))
    fi

    # A checkout copied from another host carries that host's *downloaded*
    # dependencies, which are git-ignored precisely because they are not source.
    # The Windows toolchain tarball extracts *over* them rather than replacing
    # them, leaving a Linux ELF `rustc` beside `rustc.exe` in the same bin
    # directory; Chromium's find_std_rlibs.py then picks the extensionless one
    # and the build dies with WinError 193. Refuse rather than build something
    # whose toolchain is half another platform's.
    local foreign=""
    if [[ -e "$SRC_UNIX/third_party/rust-toolchain/bin/rustc" \
       && ! -e "$SRC_UNIX/third_party/rust-toolchain/bin/rustc.exe" ]]; then
        foreign="third_party/rust-toolchain (extensionless rustc, no rustc.exe)"
    elif [[ -e "$SRC_UNIX/third_party/rust-toolchain/bin/rustc" ]]; then
        foreign="third_party/rust-toolchain (contains a non-Windows rustc)"
    fi
    if [[ -n "$foreign" ]]; then
        err "checkout carries another host's downloaded toolchain: $foreign"
        err "delete the git-ignored dependency directories and let this build refetch them"
        failures=$((failures + 1))
    fi

    for tool in "$GN_WIN" "$NINJA_WIN"; do
        [[ -f "$(win_to_unix "$tool")" ]] || { err "not found: $tool"; failures=$((failures + 1)); }
    done

    if [[ -z "$PYTHON_WIN" ]]; then
        local candidate
        if candidate="$(find_windows_python)"; then
            PYTHON_WIN="$candidate"
        else
            err "no Windows python found; set MIGO_WIN_PYTHON"; failures=$((failures + 1))
        fi
    fi

    local vswhere="/mnt/c/Program Files (x86)/Microsoft Visual Studio/Installer/vswhere.exe"
    if [[ ! -f "$vswhere" ]]; then
        err "vswhere not found -- install Visual Studio Build Tools with the C++ workload"
        failures=$((failures + 1))
    else
        # x86.x64 is required unconditionally: even the aarch64 cross-build runs
        # host x64 tools (cl.exe host, invoked via vcvarsamd64_arm64.bat) that
        # then emit ARM64 code. VS_ARM64_COMPONENT additionally requires the
        # ARM64 target component so the cross tools actually exist.
        local -a requires=(-requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64)
        [[ -n "$VS_ARM64_COMPONENT" ]] && requires+=(-requires "$VS_ARM64_COMPONENT")
        VS_ROOT="$("$vswhere" -products '*' "${requires[@]}" \
            -latest -format value -property installationPath 2>/dev/null | tr -d '\r')"
        if [[ -z "$VS_ROOT" ]]; then
            err "no Visual Studio carries the MSVC build tools this target needs ($ARCH)"
            failures=$((failures + 1))
        fi
    fi

    return "$failures"
}

if [[ "$CHECK_ONLY" == "1" ]]; then
    if check_ready; then ok "ready to build $TARGET"; exit 0; else err "not ready"; exit 1; fi
fi
check_ready || { err "prerequisites unmet; run --check for the list"; exit 1; }

# ------------------------------------------------------------
# Patches
# ------------------------------------------------------------
# Same mechanism the Android and Linux builds use: apply only the patches this
# platform needs, then assert each one took effect, so a patch that silently
# stops matching upstream fails the build rather than producing a V8 that
# quietly lacks it. Windows previously had no patch step at all -- its tree was
# edited by hand -- so a clean checkout could not reproduce the Windows V8 and
# the artifact-manifest provenance chain did not hold on this platform.
PATCH_DIR="$ENGINE_ROOT/third_party/v8-patches"
EXPORTS_DEF="$PATCH_DIR/rusty_v8-windows-exports.def"

# shellcheck source=scripts/lib/v8-patch-apply.sh
source "$SCRIPT_DIR/lib/v8-patch-apply.sh"

# The declared set comes from the lock, not from literals here. The lock already
# carried `required_patches` and nothing read it, so it stated a requirement it
# did not enforce while this script named its own three globs -- a
# declared-in-two-places shape that the Android build has since eliminated, and
# the reason a Windows V8 could have been built with a patch set the lock did
# not describe. A command substitution, so a parser that fails part-way cannot
# leave a truncated declaration behind.
V8_DECLARED_OUTPUT="$(v8_read_declared_patches "$V8_BUILD_LOCK")" \
    || { err "cannot read the declared patch set from $V8_BUILD_LOCK"; exit 1; }
[[ -n "$V8_DECLARED_OUTPUT" ]] || { err "the V8 lock declares no patches"; exit 1; }
mapfile -t V8_DECLARED_PATCHES <<<"$V8_DECLARED_OUTPUT"

apply_windows_patches() {
    # 0007 spans five files, so applied-ness may not key off any single one.
    local glob
    for glob in "${V8_DECLARED_PATCHES[@]}"; do
        v8_require_patch "$SRC_UNIX" "$PATCH_DIR" "$glob" || return 1
    done

    # The DLL exports exactly the C binding surface, listed in a generated file
    # (scripts/gen-windows-v8-def.sh) rather than hand-kept, so it cannot drift
    # from what rusty_v8 actually defines. BUILD.gn references it by this name.
    [[ -f "$EXPORTS_DEF" ]] || { err "missing export list: $EXPORTS_DEF"; return 1; }
    cp "$EXPORTS_DEF" "$SRC_UNIX/rusty_v8.def" || return 1
    echo "  ✓ staged rusty_v8.def ($(grep -c '^    ' "$EXPORTS_DEF") exports)"
}

apply_windows_patches || { err "patch stage failed"; exit 1; }

# ------------------------------------------------------------
# Build
# ------------------------------------------------------------
BATCH_UNIX="/mnt/c/migo-win-spike-tmp/migo-build-v8-windows.bat"
mkdir -p "$(dirname "$BATCH_UNIX")"
# Captured from inside the batch, after it cd's into the pinned checkout, so
# this records the toolchain rust-toolchain.toml actually selects. Querying
# `rustc --version` from this WSL shell would instead report WSL's own Linux
# toolchain -- a real, previously undiscovered bug: every Windows V8 manifest
# to date (including the shipped x86_64 one) has recorded the wrong compiler.
RUSTC_VERSION_FILE_WIN="C:\\migo-win-spike-tmp\\rustc-version-${TARGET}.txt"
RUSTC_VERSION_FILE_UNIX="$(win_to_unix "$RUSTC_VERSION_FILE_WIN")"

# rusty_v8's build.rs only adds clang's resource directory to the bindgen
# arguments when target_os == "linux" (see its bindgen setup); on Windows it
# passes -nostdinc++ with V8's vendored libc++ but no builtin headers. Bindgen
# then mis-parses the C++ namespace nesting: the binding it emits drops the
# enclosing scope (v8_String_WriteFlags_kNullTerminate becomes
# WriteFlags_kNullTerminate), loses cppgc_Visitor_Key entirely and lays
# cppgc_Visitor out as one byte. That does not fail in bindgen -- it surfaces
# much later as a static assertion inside the generated file computing
# `1_usize - 8_usize`, which reads like broken V8 sources.
#
# The binding describes V8's C++ API, so it is fixed by the V8 revision -- which
# windows-v8.lock.json pins and this build verifies. Reusing the committed one
# is therefore the same artifact bindgen would produce if it parsed correctly,
# and build-v8-linux.sh already does this for its own reason (older libclang).
BINDING_BACKUP="$OUT_DIR/src_binding.rs"
# The binding is arch/OS-independent (pure C++ API FFI surface, confirmed by
# diff across every existing archive directory this session -- see
# build-v8-linux.sh's own fallback for the same reasoning). A fresh arch's
# output dir has no committed binding of its own yet, so fall back to the
# x86_64 one rather than fail: bindgen still can't produce a correct one here
# regardless of target arch.
if [[ ! -f "$BINDING_BACKUP" && "$ARCH" != "x86_64" ]]; then
    BINDING_BACKUP="$ENGINE_ROOT/third_party/rusty_v8/x86_64-pc-windows-msvc/src_binding.rs"
fi
PREBUILT_BINDING_LINE=""
if [[ -f "$BINDING_BACKUP" ]]; then
    PREBUILT_BINDING_LINE="set \"V8_PREBUILT_BINDING=$(wslpath -w "$BINDING_BACKUP" 2>/dev/null || echo "$BINDING_BACKUP")\""
    info "reusing committed binding: $BINDING_BACKUP"
else
    err "no committed src_binding.rs at $BINDING_BACKUP"
    err "bindgen on Windows cannot produce a correct one (see the comment above)"
    exit 1
fi

# One definition, interpolated into the batch below and handed to the manifest
# writer. Writing them twice would let the recorded provenance drift from the
# arguments the build actually used -- silently, since nothing compares them.
GN_ARGS="v8_enable_webassembly=true v8_enable_pointer_compression=true v8_enable_i18n_support=true v8_enable_sandbox=false use_allocator_shim=false use_partition_alloc_as_malloc=false"
# target_cpu is NOT added here: rusty_v8's build.rs (see its CARGO_CFG_TARGET_ARCH
# handling) already pushes target_cpu="arm64" onto its own GN args whenever
# the Rust target is aarch64, the same way `cargo build --target` already
# selects the Rust side. Adding it here too would just duplicate a GN arg
# build.rs already emits.

# vcvars64.bat sets up host-x64-targeting-x64 tools; cross-compiling to arm64
# needs the host-x64-targeting-arm64 cross tools instead, which is a
# differently-named script in the same directory (there is no "vcvarsarm64.bat"
# unless the *host* itself is arm64, which this machine is not).
case "$ARCH" in
    aarch64) VCVARS_BAT="vcvarsamd64_arm64.bat" ;;
    x86_64)  VCVARS_BAT="vcvars64.bat" ;;
esac

PROXY_LINES=""
[[ -n "$PROXY" ]] && PROXY_LINES="set HTTPS_PROXY=${PROXY}
set HTTP_PROXY=${PROXY}"

NUM_JOBS_LINE=""
[[ -n "$NUM_JOBS_WIN" ]] && NUM_JOBS_LINE="set NUM_JOBS=${NUM_JOBS_WIN}"

# NOTE: this heredoc is unquoted so the shell expands ${...}; it must therefore
# contain no backticks, which the shell would run as a command substitution
# instead of writing to the file.
cat > "$BATCH_UNIX" <<BAT
@echo off
rem An inherited variable of this name makes %errorlevel% expand to it forever.
set "ERRORLEVEL="
call "${VS_ROOT}\\VC\\Auxiliary\\Build\\${VCVARS_BAT}" >nul

rem A whitelist, not a reordering. An Android NDK ships its own clang-cl and
rem clang resource directory, and the resolution inside bindgen does not consult
rem PATH order, so the NDK directory has to be ABSENT rather than merely later.
set "PATH=C:\\Program Files\\LLVM\\bin;%USERPROFILE%\\.cargo\\bin;%SystemRoot%\\system32;%SystemRoot%;%SystemRoot%\\System32\\Wbem"

rem PATH alone does not decide which libclang bindgen loads: rusty_v8's build.rs
rem resolves it through LIBCLANG_PATH and otherwise falls back to whatever the
rem clang-sys crate finds -- including the libclang shipped inside
rem v8\third_party\rust-toolchain, which is older than the Clang 20+ that V8's
rem vendored libc++ headers need. Parsing them with it does not fail loudly: it
rem silently mis-lays out C++ types, and the first sign is a static assertion
rem deep in the generated bindings ("Size of cppgc_Visitor" computing 1 - 8).
rem So the path is pinned here, not merely made reachable.
set "LIBCLANG_PATH=C:\\Program Files\\LLVM\\bin"

set CARGO_HOME=${CARGO_HOME_WIN}
set CARGO_TARGET_DIR=${V8_TARGET_DIR_WIN}
${PROXY_LINES}
${NUM_JOBS_LINE}

rem rusty_v8 resolves python via PYTHON and otherwise calls "python3"; Windows
rem installs python.exe and no python3.exe.
set PYTHON=${PYTHON_WIN}
${PREBUILT_BINDING_LINE}

rem With GN and NINJA set, need_gn_ninja_download() is false and the CIPD
rem resolve call -- the one download in this build that ignores HTTPS_PROXY,
rem because it uses raw http.client -- is never reached.
set GN=${GN_WIN}
set NINJA=${NINJA_WIN}

rem Chromium build/vs_toolchain.py probes only %ProgramFiles% for VS2022, while
rem Build Tools commonly installs under %ProgramFiles(x86)%. vs<year>_install is
rem that script's own documented override and is consulted first.
set "vs2022_install=${VS_ROOT}"

set V8_FROM_SOURCE=1
rem Kept in step with the Android and Linux recipes: WebAssembly and pointer
rem compression on, sandbox off, i18n on because rusty_v8's binding.cc includes
rem <unicode/locid.h> unconditionally and fails to compile without it.
rem use_allocator_shim / use_partition_alloc_as_malloc: Chromium's allocator
rem shim replaces malloc/free process-wide. Linked into a host it would free
rem pointers the host allocated with the system allocator, and it also makes
rem the archive redefine ucrt's malloc/free/realloc/calloc/_msize, which fails
rem the link outright. scripts/build-v8-linux.sh turns both off for the same
rem reason: an engine that hijacks its host's allocator is not embeddable.
rem treat_warnings_as_errors: building V8 against the MSVC STL is a
rem configuration upstream does not test, and it trips exactly one warning --
rem -Wctad-maybe-unsupported on std::atomic_ref, 78 times. libc++ annotates
rem that template as deduction-friendly and the MSVC STL does not; the code is
rem valid either way. Chromium exposes no per-warning argument, and rusty_v8's
rem own build.rs already turns this off whenever it uses a system clang. It
rem changes no generated code, only whether a warning fails the build.
set EXTRA_GN_ARGS=${GN_ARGS}

cd /d ${RUSTY_V8_SRC_WIN} || exit /b 90
rem After the cd, so rust-toolchain.toml's override is in effect -- see the
rem RUSTC_VERSION_FILE_WIN comment above for why this is captured here rather
rem than queried from WSL.
rustc --version --verbose > "${RUSTC_VERSION_FILE_WIN}" 2>&1
rem V8 is built WITH its bundled libc++ (the crate default), because it cannot
rem be compiled against the MSVC STL: measured 2026-07-24, clang-cl crashes
rem (frontend signal) on 32 torque-generated translation units, and before that
rem 78 -Wctad-maybe-unsupported errors. Chromium compiles those with
rem -D_HAS_EXCEPTIONS=0 and /std:c++23preview, a combination Microsoft does not
rem support for its own STL. So `use_custom_libcxx=false` is not an option here,
rem and the Skia/V8 std::terminate collision needs a different answer.
cargo build --release --target ${TARGET}
rem Capture before echoing: an echo succeeds and would otherwise become the
rem batch file's exit status, making a failed build look like a pass.
set CARGO_EXIT=%errorlevel%
echo === EXIT=%CARGO_EXIT% ===
exit /b %CARGO_EXIT%
BAT

info "building V8 for $TARGET in $RUSTY_V8_SRC_WIN (this takes several minutes)"
( cd "$(dirname "$BATCH_UNIX")" && cmd.exe /c "$(basename "$BATCH_UNIX")" )

# ------------------------------------------------------------
# Install artifacts
# ------------------------------------------------------------
GN_OUT_UNIX="$(win_to_unix "$V8_TARGET_DIR_WIN")/$TARGET/release/gn_out"
BINDING="$GN_OUT_UNIX/src_binding.rs"

# rusty_v8 is a shared_library here (see the BUILD.gn patch): the products are
# the DLL plus its import library, both in the build root rather than obj/.
# gn names the import library after the DLL, so accept either spelling instead
# of pinning one and failing opaquely if the toolchain uses the other.
DLL="$GN_OUT_UNIX/rusty_v8.dll"
# The static archive is a product of this build too, and it is what
# build-windows-sdk.sh actually links (see its header). Leaving it in gn_out and
# letting the SDK reach in there is how the tree ended up with a .lib and a .dll
# from two different builds: nothing tied them together, and nothing said so.
ARCHIVE="$GN_OUT_UNIX/obj/rusty_v8.lib"
IMPLIB=""
for candidate in "$GN_OUT_UNIX/rusty_v8.dll.lib" "$GN_OUT_UNIX/rusty_v8.lib"; do
    [[ -f "$candidate" ]] && { IMPLIB="$candidate"; break; }
done

[[ -f "$DLL" ]] || { err "rusty_v8.dll not found after build: $DLL"; exit 1; }
[[ -n "$IMPLIB" ]] || { err "no import library beside $DLL"; exit 1; }
[[ -f "$BINDING" ]] || { err "src_binding.rs not found after build: $BINDING"; exit 1; }
[[ -f "$ARCHIVE" ]] || { err "rusty_v8.lib not found after build: $ARCHIVE"; exit 1; }

mkdir -p "$OUT_DIR"
cp "$DLL" "$OUT_DIR/rusty_v8.dll"
cp "$IMPLIB" "$OUT_DIR/rusty_v8.dll.lib"
# In prebuilt mode this copy is an identity: build.rs placed the committed
# binding at $BINDING itself. Asserting that rather than trusting it, because
# the alternative -- silently replacing the known-good binding with one bindgen
# mis-generated -- is exactly the failure this reuse exists to prevent, and it
# would only surface on the next build.
if ! cmp -s "$BINDING" "$BINDING_BACKUP"; then
    err "the build produced a binding that differs from the committed one"
    err "  built:     $BINDING"
    err "  committed: $BINDING_BACKUP"
    err "refusing to overwrite it; V8_PREBUILT_BINDING did not take effect"
    exit 1
fi
cp "$BINDING" "$OUT_DIR/src_binding.rs"
cp "$ARCHIVE" "$OUT_DIR/rusty_v8.lib"
ok "archive -> $OUT_DIR/rusty_v8.lib ($(du -h "$OUT_DIR/rusty_v8.lib" | cut -f1))"
ok "dll     -> $OUT_DIR/rusty_v8.dll ($(du -h "$OUT_DIR/rusty_v8.dll" | cut -f1))"
ok "implib  -> $OUT_DIR/rusty_v8.dll.lib ($(du -h "$OUT_DIR/rusty_v8.dll.lib" | cut -f1))"
ok "binding -> $OUT_DIR/src_binding.rs"

# The component manifest is written HERE, in the build, not reconstructed
# afterwards: it asserts which sources and toolchain produced these exact bytes,
# and only the build knows that. Generating one later from whatever the checkout
# happens to look like would produce a provenance record that reads as
# authoritative while attesting to nothing -- and scripts/fetch-v8-archives.sh
# keys its integrity check on these manifests, so a fabricated one is trusted.
#
# Toolchain versions come from the installation this build actually used, not
# from constants: a manifest that names a compiler the build did not run is a
# worse record than one that names none.
# VS_ROOT is a DOS path (vswhere reports Windows form); reading it from this
# shell needs the same conversion every other Windows path here gets.
VS_ROOT_UNIX="$(win_to_unix "$VS_ROOT")"
MSVC_VERSION="$(ls "$VS_ROOT_UNIX/VC/Tools/MSVC" 2>/dev/null | sort -V | tail -1)"
[[ -n "$MSVC_VERSION" ]] || { err "cannot determine the MSVC toolset version under $VS_ROOT_UNIX"; exit 1; }
SDK_VERSION="$(ls "/mnt/c/Program Files (x86)/Windows Kits/10/Include" 2>/dev/null | sort -V | tail -1)"
[[ -n "$SDK_VERSION" ]] || { err "cannot determine the Windows SDK version"; exit 1; }
RUSTC_VERSION="$(cat "$RUSTC_VERSION_FILE_UNIX" 2>/dev/null)"
[[ -n "$RUSTC_VERSION" ]] || { err "cannot read the captured rustc version: $RUSTC_VERSION_FILE_UNIX"; exit 1; }
info "toolchain: MSVC $MSVC_VERSION, Windows SDK $SDK_VERSION"

python3 "$SCRIPT_DIR/write-windows-v8-component-manifest.py" \
    --arch "$ARCH" \
    --repo-root "$PROJECT_ROOT" \
    --rusty-v8-src "$SRC_UNIX" \
    --gn-args "$GN_ARGS" \
    --archive "$OUT_DIR/rusty_v8.lib" \
    --binding "$OUT_DIR/src_binding.rs" \
    --dll "$OUT_DIR/rusty_v8.dll" \
    --implib "$OUT_DIR/rusty_v8.dll.lib" \
    --output "$OUT_DIR/component-manifest.json" \
    --lock "$V8_BUILD_LOCK" \
    --msvc-version "$MSVC_VERSION" \
    --sdk-version "$SDK_VERSION" \
    --rustc-version "$RUSTC_VERSION"
ok "component manifest -> $OUT_DIR/component-manifest.json"
# Producing a .dll and an import library only proves the link succeeded. This
# links a C consumer against what was just built and runs it, so a DLL that
# cannot load fails here rather than inside a Rust test binary much later.
#
# It runs from this script because nothing else can: the gate needs MSVC and a
# Windows host, there is no Windows runner in CI, and for that reason it sat
# referenced by nothing at all -- passing when someone remembered it, silent
# otherwise.
info "proving the DLL loads"
bash "$SCRIPT_DIR/test-windows-v8-dll.sh"

ok "V8 build complete for $TARGET"
