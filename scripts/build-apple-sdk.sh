#!/usr/bin/env bash
# =============================================================================
# Build the Apple SDK: a Rust static library per slice, assembled into
# MigoEngine.xcframework alongside the C ABI headers and a module map.
#
# The deployment targets are READ from contracts/apple/deployment-floor.json,
# not written here. A copy in this file would be a second place the decision
# lives, and the one that silently wins on the build machine. The contract gate
# checks this by asking the script (`--print-deployment-target`) rather than by
# grepping it, so a copy reintroduced later would still have to agree.
#
# WHAT THIS DOES NOT DO, AND WHY IT SAYS SO.
# Building requires macOS: Rust's Apple targets need Xcode's linker and SDKs,
# and xcframework assembly needs xcodebuild. On any other host this exits with
# a message naming what is missing. It does not stub, skip, or report success --
# a build script that "passes" without producing bytes is how a Windows SDK
# shipped that loaded, resolved every entry point, and could attach nothing.
# =============================================================================
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

# The shared V8 materialiser: verifies an archive against its component manifest
# and exports a path named by that archive's own hash. Sourced the way
# build-ohos-sdk.sh sources it, so both platforms use one implementation of the
# rule rather than two that agree today.
# shellcheck source=scripts/lib/v8-materialise.sh
source "$SCRIPT_DIR/lib/v8-materialise.sh"
ENGINE_DIR="$REPO_ROOT/engine"
CONTRACT="$REPO_ROOT/contracts/apple/deployment-floor.json"
WEBCONTENT_SRC="$REPO_ROOT/platforms/apple/WebContent/PerformancePlus"
WEBCONTENT_DEST="$REPO_ROOT/platforms/apple/Sources/MigoApplePerformancePlus/Resources"
FRAMEWORKS_DIR="$REPO_ROOT/platforms/apple/Frameworks"

BUILD_ROOT="${MIGO_APPLE_BUILD_ROOT:-/tmp/migo-apple-build}"

PLATFORM=""
CONFIGURATION="Debug"
CODE_SIGNING="off"
PRODUCT=""
PRINT_TARGET=""
PRINT_SLICES=""
PRINT_PLATFORMS=0
ASSEMBLE_ONLY=0
REQUIRE_ALL_SLICES=0

err()  { printf '\033[0;31m[apple-sdk] %s\033[0m\n' "$*" >&2; }
ok()   { printf '\033[0;32m[apple-sdk] %s\033[0m\n' "$*"; }
info() { printf '\033[0;36m[apple-sdk] %s\033[0m\n' "$*"; }

usage() {
    cat <<'USAGE'
usage: build-apple-sdk.sh --platform <ios|ios-simulator|macos>
                          [--product <performance-plus|macos-v8|external-frames-diagnostic>]
                          [--configuration Debug|Release]
                          [--code-signing on|off]
                          [--assemble-only] [--require-all-slices]
       build-apple-sdk.sh --print-deployment-target <ios|macos>
       build-apple-sdk.sh --print-slices <ios|ios-simulator|macos>
       build-apple-sdk.sh --print-platforms

  --print-deployment-target  Print the deployment target this build would use,
                             read from contracts/apple/deployment-floor.json,
                             and exit. Runs on any host: it is how the contract
                             gate checks the script's real behaviour instead of
                             grepping it for a number.
  --print-slices             Print the Rust target triples this platform's
                             xcframework slice group is built from, one per
                             line, and exit. Also runs on any host, and for the
                             same reason: scripts/build-angle-apple.sh has to
                             build ANGLE for exactly these slices or the two
                             xcframeworks cannot be linked into one app, and a
                             second copy of the list is how that stops being
                             true without anyone noticing.
  --print-platforms          Print the xcframework slice groups this script
                             knows, one per line, and exit. The same question
                             one level up: scripts/build-angle-apple.sh, its CI
                             lane and the pin's contract gate all iterate "every
                             Apple platform", and three copies of that list is
                             three chances for one of them to quietly cover two.

Products, and why they are separate builds:
  performance-plus  --no-default-features --features external-frames.
                    The iOS fast lane: content JavaScript runs in WebKit's
                    WebContent process, so this archive links no JavaScript
                    engine at all. That is the product claim, and building the
                    default crate and reusing it here would quietly break it.
  macos-v8          default features. In-process V8 with JIT, which macOS
                    allows under the public hardened-runtime entitlement.
  external-frames-diagnostic
                    macOS external-frame tests only. Writes an isolated package
                    under $MIGO_APPLE_BUILD_ROOT/diagnostics/<configuration>/package;
                    never replaces the shipping MigoEngine.xcframework.

  There is deliberately no `webkit-host` product. That lane is WKWebView
  running migo-web-adapter; it drives no native renderer, so it links none of
  this. A product entry that built something for it would be building
  something it does not use.

  The default is `macos-v8` for --platform macos and `performance-plus` for the
  two iOS slices, which is the only combination each platform can actually run.

Slices, and why each exists:
  ios              aarch64-apple-ios          device
  ios-simulator    aarch64-apple-ios-sim,     Apple silicon and Intel Macs both
                   x86_64-apple-ios           run the simulator
  macos            aarch64-apple-darwin,      Apple silicon and Intel, as real
                   x86_64-apple-darwin        slices -- Rosetta is not a slice

Output: $MIGO_APPLE_BUILD_ROOT (default /tmp/migo-apple-build), with the
finished shipping xcframework in platforms/apple/Frameworks/. Staged groups
are namespaced by product/configuration/platform. Each assembly includes all
staged shipping groups of that configuration; use the same build root for
successive platform builds. --assemble-only assembles already built groups.
--require-all-slices refuses a partial shipping artifact.

Install pinned ANGLE first (scripts/fetch-apple-angle.sh <platform>). Both
runtime libraries for each assembled platform are required. The package also
declares the iOS framework pair, so install at least one iOS group when building
only macOS. No dependencies are downloaded by this build script.
USAGE
}

while [ $# -gt 0 ]; do
    case "$1" in
        --platform)      PLATFORM="${2:-}"; shift 2 ;;
        --product)       PRODUCT="${2:-}"; shift 2 ;;
        --configuration) CONFIGURATION="${2:-}"; shift 2 ;;
        --code-signing)  CODE_SIGNING="${2:-}"; shift 2 ;;
        --print-deployment-target) PRINT_TARGET="${2:-}"; shift 2 ;;
        --print-slices)  PRINT_SLICES="${2:-}"; shift 2 ;;
        --print-platforms) PRINT_PLATFORMS=1; shift ;;
        --assemble-only) ASSEMBLE_ONLY=1; shift ;;
        --require-all-slices) REQUIRE_ALL_SLICES=1; shift ;;
        -h|--help)       usage; exit 0 ;;
        *)               err "unknown argument: $1"; usage >&2; exit 2 ;;
    esac
done

# ---------------------------------------------------------------------------
# The floor, read from the contract
# ---------------------------------------------------------------------------

# The static library cargo will actually produce for `migo-capi`.
#
# Asked of cargo rather than written here. This script copied `libmigo.a` for its
# whole life while the crate's `[lib] name` has been `migo_capi`, so cargo emits
# `libmigo_capi.a` and the copy could only ever have failed -- which nobody saw,
# because the script had never run at all. A second hand-written copy of a name
# that lives in a manifest is the same defect waiting to happen again.
read_capi_staticlib_name() {
    # Metadata goes through a file and the program through the heredoc, the same
    # shape read_deployment_target uses. `python3 - <<EOF` takes its *program*
    # from stdin, so piping cargo into it hands the interpreter the JSON as
    # source text and leaves sys.stdin already consumed.
    local metadata
    metadata="$(mktemp)" || return 1
    if ! (cd "$ENGINE_DIR" && cargo metadata --format-version 1 --no-deps) >"$metadata"; then
        rm -f "$metadata"
        return 1
    fi
    python3 - "$metadata" <<'CAPILIB'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as handle:
    meta = json.load(handle)
for package in meta["packages"]:
    if package["name"] != "migo-capi":
        continue
    for target in package["targets"]:
        if "staticlib" in target["kind"]:
            print("lib" + target["name"] + ".a")
            raise SystemExit(0)
    print("migo-capi declares no staticlib target", file=sys.stderr)
    raise SystemExit(1)
print("migo-capi is not in the workspace metadata", file=sys.stderr)
raise SystemExit(1)
CAPILIB
    local status=$?
    rm -f "$metadata"
    return $status
}

read_deployment_target() {
    local platform="$1"
    python3 - "$CONTRACT" "$platform" <<'PY'
import json
import sys

path, platform = sys.argv[1], sys.argv[2]
with open(path, encoding="utf-8") as handle:
    contract = json.load(handle)
entry = (contract.get("platforms") or {}).get(platform)
if entry is None:
    print(f"unknown platform {platform!r}", file=sys.stderr)
    raise SystemExit(1)
target = entry.get("deployment_target")
if not target:
    print(f"{platform} declares no deployment_target", file=sys.stderr)
    raise SystemExit(1)
print(target)
PY
}

# The slices of each xcframework group, and the deployment-floor platform whose
# target they carry. A function rather than an inline case, because
# --print-slices has to answer from the same statement the build uses: the
# moment a slice list is readable in two places, the Apple engine and the Apple
# ANGLE builds can disagree about which architectures exist, and an xcframework
# whose slices do not match its neighbour's fails at the consumer's link step
# with no mention of either script.
resolve_platform() {
    case "$1" in
        ios)           RUST_TARGETS=(aarch64-apple-ios); FLOOR_PLATFORM="ios" ;;
        ios-simulator) RUST_TARGETS=(aarch64-apple-ios-sim x86_64-apple-ios); FLOOR_PLATFORM="ios" ;;
        macos)         RUST_TARGETS=(aarch64-apple-darwin x86_64-apple-darwin); FLOOR_PLATFORM="macos" ;;
        *) return 1 ;;
    esac
}

if [ -n "$PRINT_TARGET" ]; then
    read_deployment_target "$PRINT_TARGET" || exit 1
    exit 0
fi

# The one place the set of slice groups is written. resolve_platform above is
# the only other statement that knows them, and it is the one this agrees with
# by construction: every name here must resolve, and the check below says so.
APPLE_PLATFORMS="ios ios-simulator macos"

if [ "$PRINT_PLATFORMS" = "1" ]; then
    for candidate in $APPLE_PLATFORMS; do
        if ! resolve_platform "$candidate"; then
            err "internal: --print-platforms names '$candidate', which --platform rejects"
            exit 1
        fi
        printf '%s\n' "$candidate"
    done
    exit 0
fi

if [ -n "$PRINT_SLICES" ]; then
    if ! resolve_platform "$PRINT_SLICES"; then
        err "--print-slices must be ios, ios-simulator or macos"
        exit 2
    fi
    # `${a[@]+...}` because macOS ships bash 3.2, where expanding an array that
    # could be empty under `set -u` is an error rather than nothing.
    printf '%s\n' ${RUST_TARGETS[@]+"${RUST_TARGETS[@]}"}
    exit 0
fi

if [ -z "$PLATFORM" ]; then
    err "--platform is required"
    usage >&2
    exit 2
fi

case "$CONFIGURATION" in
    Debug|Release) ;;
    *) err "--configuration must be Debug or Release"; exit 2 ;;
esac

case "$CODE_SIGNING" in
    on|off) ;;
    *) err "--code-signing must be on or off"; exit 2 ;;
esac

if ! resolve_platform "$PLATFORM"; then
    err "--platform must be ios, ios-simulator or macos"
    exit 2
fi

DEPLOYMENT_TARGET="$(read_deployment_target "$FLOOR_PLATFORM")" || exit 1

# ---------------------------------------------------------------------------
# Host requirements, checked before anything is created
# ---------------------------------------------------------------------------

# The product decides which Cargo features this archive is built with, and the
# default is the only one its platform can run: macOS gets in-process V8, iOS
# gets the engine-free external-frame lane because Apple grants a JIT to
# WebKit's WebContent process and to nothing else.
if [ -z "$PRODUCT" ]; then
    case "$PLATFORM" in
        macos) PRODUCT="macos-v8" ;;
        *)     PRODUCT="performance-plus" ;;
    esac
fi
case "$PRODUCT" in
    performance-plus)
        if [ "$PLATFORM" = "macos" ]; then
            err "performance-plus ships on iOS only; use external-frames-diagnostic for macOS tests"
            exit 2
        fi
        # No default features: `profile-full` implies an embedded engine, and
        # this archive's entire claim is that it has none.
        cargo_feature_flags=(--no-default-features --features external-frames)
        ;;
    external-frames-diagnostic)
        if [ "$PLATFORM" != "macos" ]; then
            err "external-frames-diagnostic is a macOS test product"
            exit 2
        fi
        cargo_feature_flags=(--no-default-features --features external-frames)
        ;;
    macos-v8)
        cargo_feature_flags=()
        if [ "$PLATFORM" != "macos" ]; then
            err "macos-v8 is a macOS product: iOS grants a JIT to WebKit's WebContent"
            err "process and to no embedded engine, so this archive could not run there."
            exit 2
        fi
        ;;
    webkit-host)
        err "there is no webkit-host product to build: that lane is WKWebView running"
        err "migo-web-adapter, which drives no native renderer and links none of this."
        exit 2
        ;;
    *)
        err "unknown product: $PRODUCT (expected performance-plus, macos-v8 or external-frames-diagnostic)"
        exit 2
        ;;
esac

if [ "$(uname -s)" != "Darwin" ]; then
    err "Apple slices need macOS: Rust's Apple targets require Xcode's linker"
    err "and SDKs, and xcframework assembly requires xcodebuild."
    err ""
    err "This host is $(uname -s). Refusing to report a build that did not happen."
    err "Everything except the build is still checkable here:"
    err "  bash scripts/test-apple-deployment-floor-contract.sh"
    err "  bash scripts/test-apple-profile-policy-contract.sh"
    err "  bash scripts/test-c-abi-surface-candidate.sh"
    exit 1
fi

missing=0
required_tools=(xcodebuild python3)
if [ "$ASSEMBLE_ONLY" = "0" ]; then
    required_tools=(xcodebuild python3 lipo cargo rustup)
fi
for tool in ${required_tools[@]+"${required_tools[@]}"}; do
    if ! command -v "$tool" >/dev/null 2>&1; then
        err "required tool not on PATH: $tool"
        missing=1
    fi
done
[ "$missing" -eq 0 ] || exit 1

# Asked from `engine/`, which is where the build below runs cargo from -- and
# rustup resolves a toolchain from the working directory. `engine/rust-toolchain.toml`
# pins 1.95.0, so a machine whose *default* toolchain is something else has two
# different answers to "is this target installed", and this check was reading the
# one the build does not use. Measured on a Mac whose default was stable and whose
# pinned 1.95.0 had both darwin targets: the build refused a target it had.
#
# On CI the two agree, because the workflow's toolchain action installs the
# targets into the toolchain it also makes default -- which is exactly why a check
# that asks the wrong one stays green there.
if [ "$ASSEMBLE_ONLY" = "0" ]; then
installed_targets="$(cd "$ENGINE_DIR" && rustup target list --installed 2>/dev/null)"
for target in ${RUST_TARGETS[@]+"${RUST_TARGETS[@]}"}; do
    if ! printf '%s\n' "$installed_targets" | grep -qx "$target"; then
        err "Rust target not installed for the toolchain $ENGINE_DIR pins: $target"
        err "  (cd $ENGINE_DIR && rustup target add $target)"
        exit 1
    fi
done
fi

# ---------------------------------------------------------------------------
# Build
# ---------------------------------------------------------------------------

BUILD_ROOT="$(python3 -c 'import os, sys; print(os.path.abspath(sys.argv[1]))' "$BUILD_ROOT")" || exit 1
STAGE="$BUILD_ROOT/$PRODUCT/$CONFIGURATION/$PLATFORM"

# Check the runtime closure before an expensive compile or replacing any output.
python3 "$SCRIPT_DIR/apple-sdk-package.py" check-runtime \
    --repo-root "$REPO_ROOT" --platform "$PLATFORM" || exit 1

if [ "$ASSEMBLE_ONLY" = "0" ]; then
rm -rf "$STAGE"
mkdir -p "$STAGE/libs" "$STAGE/headers/migo"

info "platform            $PLATFORM ($CONFIGURATION)"
info "product             $PRODUCT (cargo ${cargo_feature_flags[*]:-default features})"
info "deployment target   $DEPLOYMENT_TARGET (from contracts/apple/deployment-floor.json)"
info "slices              ${RUST_TARGETS[*]}"

cargo_profile_flag=()
profile_dir="debug"
if [ "$CONFIGURATION" = "Release" ]; then
    cargo_profile_flag=(--release)
    profile_dir="release"
fi

# cd into engine/ deliberately: rust-toolchain.toml and .cargo/config.toml are
# both resolved from the working directory, so building with --manifest-path
# from the repo root would silently use the machine's default toolchain.
# Apple's clang, for the parts of the build that do not ask cc-rs.
#
# `engine/.cargo/config.toml` sets a bare `CC = "clang-18"` for the WSL/Ubuntu
# host, and cargo's `[env]` reaches EVERY target. That file already knows this
# and answers it with `CC_<target>` keys for Windows and Apple -- which works,
# because cc-rs resolves the target-qualified name first. It works only for
# cc-rs.
#
# skia-bindings does not go through cc-rs to pick the compiler for Skia itself:
# it hands GN a compiler and GN runs ninja, and what it hands over is the plain
# `CC`. So the first build that ever compiled Skia for an Apple target died 1429
# steps into ninja with
#
#     clang-18 -isysroot .../iPhoneOS18.5.sdk --target=aarch64-apple-ios ...
#     /bin/sh: clang-18: command not found
#
# Android does not have this problem because skia-bindings takes its compiler
# from the NDK there, which is why a bare `CC` has been survivable for years.
#
# Exported rather than added to `[env]`: cargo's `[env]` has no per-target form
# for the plain name, and a real environment variable beats a non-forcing
# `[env]` entry. `${CC:-clang}` so a caller who deliberately set one keeps it.
export CC="${CC:-clang}"
export CXX="${CXX:-clang++}"
info "C/C++ compiler      $CC / $CXX"

CAPI_STATICLIB="$(read_capi_staticlib_name)" || exit 1
info "cargo staticlib      $CAPI_STATICLIB (from cargo metadata)"

cd "$ENGINE_DIR" || exit 1

for target in ${RUST_TARGETS[@]+"${RUST_TARGETS[@]}"}; do
    info "cargo build $target"
    case "$FLOOR_PLATFORM" in
        ios)   export IPHONEOS_DEPLOYMENT_TARGET="$DEPLOYMENT_TARGET" ;;
        macos) export MACOSX_DEPLOYMENT_TARGET="$DEPLOYMENT_TARGET" ;;
    esac

    # macos-v8 is the only product here that links V8, and the only one that needs
    # an archive supplied. Without this, rusty_v8's build script decides for itself
    # -- downloading a prebuilt for the host triple, or building V8 from source
    # inside a packaging run that budgeted minutes for Skia and not an hour for V8.
    # Neither is a supply chain: what a shipped product links has to be the archive
    # with a component manifest behind it.
    #
    # `v8_materialise` verifies the archive against that manifest and exports a
    # path named by its own hash, so cargo reruns the v8 build script when the
    # archive's *value* changes rather than reusing an rlib built from a previous
    # one.
    unset RUSTY_V8_ARCHIVE RUSTY_V8_SRC_BINDING_PATH
    if [ "$PRODUCT" = "macos-v8" ]; then
        v8_dir="$REPO_ROOT/engine/third_party/rusty_v8/$target"
        if [ ! -f "$v8_dir/librusty_v8.a" ]; then
            info "$target: fetching the pinned V8 archive"
            if ! bash "$SCRIPT_DIR/fetch-v8-archives.sh" "$target"; then
                err "could not fetch the V8 archive for $target"
                exit 1
            fi
        fi
        if ! v8_materialise "$v8_dir" "$REPO_ROOT/engine/target/v8-materialised"; then
            err "the V8 archive for $target does not match its component manifest"
            exit 1
        fi
        info "$target: V8 materialised at ${V8_MATERIALISED_ARCHIVE#"$REPO_ROOT"/}"
        export RUSTY_V8_ARCHIVE="$V8_MATERIALISED_ARCHIVE"
        export RUSTY_V8_SRC_BINDING_PATH="$V8_MATERIALISED_BINDING"
    fi
    # `${a[@]+"${a[@]}"}` and not `"${a[@]}"`: macOS ships bash 3.2 as /bin/bash,
    # and there expanding an EMPTY array under `set -u` is an unbound-variable
    # error rather than nothing. Both of these are empty on real invocations --
    # `cargo_profile_flag` for a Debug build, `cargo_feature_flags` for the
    # macos-v8 product -- so two of the three documented ways to call this script
    # died here on the only OS that can run it. Nothing caught it because the
    # script had never run at all, and on Linux's bash 5 the plain form is fine.
    if ! cargo build -p migo-capi --target "$target" --locked \
        ${cargo_feature_flags[@]+"${cargo_feature_flags[@]}"} \
        ${cargo_profile_flag[@]+"${cargo_profile_flag[@]}"}; then
        err "cargo build failed for $target"
        exit 1
    fi
    cp "target/$target/$profile_dir/$CAPI_STATICLIB" "$STAGE/libs/libmigo-$target.a" || exit 1
done

# One archive per xcframework slice group. Device and simulator must stay
# separate archives -- an xcframework rejects a fat archive that mixes them,
# and lipo will happily produce one.
if [ "${#RUST_TARGETS[@]}" -gt 1 ]; then
    info "lipo ${#RUST_TARGETS[@]} slices into one archive"
    lipo -create "$STAGE"/libs/libmigo-*.a -output "$STAGE/libmigo.a" || exit 1
else
    cp "$STAGE/libs/libmigo-${RUST_TARGETS[0]}.a" "$STAGE/libmigo.a" || exit 1
fi

# Headers travel with the binary, so there is no vendored second copy to drift.
cp "$REPO_ROOT"/include/migo/*.h "$STAGE/headers/migo/" || exit 1
mkdir -p "$STAGE/headers/migo/platform"
cp "$REPO_ROOT"/include/migo/platform/*.h "$STAGE/headers/migo/platform/" || exit 1

# An umbrella DIRECTORY, not `umbrella header "migo/migo.h"` plus a hand-listed
# tail. Two reasons, and the first was invisible until this commit.
#
# `migo/migo.h` includes five of the fifteen headers staged above. The rest --
# external_frames.h and the eight platform descriptors -- are entry points a host
# includes directly, so under an umbrella header they have to be listed one by
# one. That list was written with two of the eight platform headers on it, and
# nothing noticed, because no target depended on this xcframework and the module
# was therefore never built. A module map is only checked when something imports
# the module.
#
# The second reason is what the list would have cost once it was built:
# `-Wincomplete-umbrella` fires for every header in the umbrella's directory tree
# that the module does not cover, so six of the eight platform headers would have
# produced a warning in every consumer's build, and adding a ninth would produce
# a seventh. Every header here is self-contained -- each includes only
# <migo/surface.h> -- so the directory form covers all of them, stays correct
# when one is added, and is derived rather than transcribed.
cat > "$STAGE/headers/module.modulemap" <<'MODULEMAP'
module MigoEngine {
    umbrella "migo"
    export *
}
MODULEMAP

python3 "$SCRIPT_DIR/apple-sdk-package.py" record \
    --stage "$STAGE" --platform "$PLATFORM" --product "$PRODUCT" \
    --configuration "$CONFIGURATION" --deployment-target "$DEPLOYMENT_TARGET" || exit 1
fi

# A diagnostic external-frame archive must never masquerade as macOS V8 in
# the shipping package. Copy the Swift sources into an explicitly separate
# package so CI can exercise the same ABI without claiming a V8 artifact.
# The assembler stages those sources before publishing any generated output.
if [ "$PRODUCT" = "external-frames-diagnostic" ]; then
    PACKAGE_DIR="$BUILD_ROOT/diagnostics/$CONFIGURATION/package"
    FRAMEWORKS_DIR="$PACKAGE_DIR/Frameworks"
    WEBCONTENT_DEST="$PACKAGE_DIR/Sources/MigoApplePerformancePlus/Resources"
fi

XCFRAMEWORK="$FRAMEWORKS_DIR/MigoEngine.xcframework"
# Copy only producer src/, keeping node tests and packet fixtures out of apps.
# Helpers, resources and all native artifacts are staged and published together;
# no fallible package mutation belongs after this call.
python3 "$SCRIPT_DIR/apple-sdk-package.py" assemble \
    --repo-root "$REPO_ROOT" --build-root "$BUILD_ROOT" \
    --configuration "$CONFIGURATION" --product "$PRODUCT" \
    --webcontent-source "$WEBCONTENT_SRC/src" --webcontent-destination "$WEBCONTENT_DEST" \
    --output "$XCFRAMEWORK" --require-all-slices "$REQUIRE_ALL_SLICES" || exit 1

if [ "$CODE_SIGNING" = "on" ]; then
    info "code signing requested; the signing identity and entitlements are the"
    info "host application's, not this SDK's. macOS V8 additionally requires"
    info "com.apple.security.cs.allow-jit in the embedding app's entitlements."
fi

ok "built $XCFRAMEWORK"
ok "product/configuration and every included group are recorded in migo-build.json"
exit 0
