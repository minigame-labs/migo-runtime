#!/usr/bin/env bash
# =============================================================================
# Build ANGLE for Apple platforms from source: libEGL and libGLESv2, as a
# framework bundle on iOS and a shared library on macOS -- upstream's own split,
# not this script's, and the reason is under "WHAT THE PRODUCT ACTUALLY IS".
# Location: scripts/build-angle-apple.sh
#
# It also carries the patches ANGLE needs to compile for this project's macOS
# deployment floor; see the "Patches" section further down for what and why.
#
# WHY THIS EXISTS AT ALL:
#   .github/workflows/apple-sdk.yml asked rustc, on 2026-09-05, what a non-Rust
#   consumer must link alongside the engine on each Apple platform:
#
#     macOS  -lc++ -framework ApplicationServices -framework OpenGL -liconv ...
#     iOS    -lc++ -framework CoreFoundation -framework CoreGraphics
#            -framework CoreText -framework ImageIO
#            -framework MobileCoreServices -framework UIKit -liconv ...
#
#   There is no GL framework in the iOS list, because there is no GL framework
#   on iOS. Skia is configured for its GL backend; macOS satisfies that by
#   linking the legacy `OpenGL` framework, and iOS has nothing to link. ANGLE
#   over Metal is what fills that gap, and it is also why "just use system GL
#   for a macOS presenter first" buys iOS nothing -- that shortcut links a thing
#   iOS does not have.
#
#   The Windows side of this repository already has the shape being copied here:
#   scripts/build-angle-windows.sh, contracts/artifact-manifest/windows-angle.lock.json
#   and scripts/fetch-windows-angle.sh.
#
# WHY SHARED LIBRARIES AND NOT ANGLE'S STATIC TARGETS.
#   Not a preference. Every presenter in this repository resolves EGL at RUNTIME
#   and then hands `eglGetProcAddress` to `glow::Context::from_loader_function`:
#
#     crates/platform/src/android/presenter.rs   libEGL.so
#     crates/platform/src/linux/presenter.rs     libEGL.so.1
#     crates/platform/src/ohos/presenter.rs      libEGL.so
#     crates/platform/src/windows/presenter.rs   libEGL.dll   (ANGLE)
#
#   ANGLE does publish `//:angle_static` (libEGL_static + libGLESv2_static), and
#   linking that instead would mean the Apple presenter alone resolved GL at link
#   time while the other four resolved it at load time. The dynamic form is the
#   one the engine already knows how to consume, so it is the one built here.
#
# WHAT IS MEASURED RATHER THAN ASSUMED, and where the measurement came from.
#   .github/workflows/apple-angle-probe.yml (PR #185, run 33948370655) ran a
#   checkout and three `gn gen`s on a free `macos-15` runner before any of this
#   was written, because blind-writing a build script for a checkout nobody has
#   performed is the anti-pattern the neighbouring Apple lanes were fixed for six
#   times in one round. What it established:
#
#     - The image starts with 42 GiB free; `fetch --no-history angle` with
#       target_os limited to mac and ios costs 12 GB and 233 s, leaving 29 GiB.
#       "A hosted macOS runner is too small for this" was inherited from the x86
#       image and is false.
#     - `fetch angle` names its solution `.`, so the checkout IS the directory
#       `fetch` was run in -- unlike `fetch chromium`, which creates `src/`. The
#       probe's first run assumed the chromium shape and died after a
#       four-minute fetch. The fix is not a different guess: `.gclient` says
#       which, so `.gclient` is what this script reads.
#     - `target_os="ios"` alone fails at gn import time; it must be paired with
#       `target_environment` (device or simulator). That is why iOS is two
#       configurations, and it happens to be the same split an xcframework
#       already forces -- it refuses a fat archive mixing device and simulator.
#     - One `ninja libEGL libGLESv2` for iOS device arm64 is 334 s and 100 MB.
#
# WHAT THE PRODUCT ACTUALLY IS, and why this script does not name it.
#   ANGLE's own `angle_shared_library` template says it plainly:
#
#     # On ios, define an ios_framework_bundle instead of a shared library.
#     target_type = "shared_library"
#     if (is_ios) { target_type = "ios_framework_bundle" }
#
#   So an iOS build produces `libEGL.framework`, not `libEGL.dylib`. The first
#   run of this recipe assumed the dylib and failed AFTER a successful 1554-step
#   ninja build, which is the cheapest possible way to be told -- and only
#   because the failure printed the directory instead of just the missing name.
#   The fix is not the other guess: the product is now located by asking the
#   output directory what it holds named after the ninja target, and a target
#   that resolves to nothing, or to more than one thing, fails with the listing.
#   That rule is the same on every platform and stays right when a fourth one
#   behaves like neither of the first three.
#
#   iOS wanting a framework is not an inconvenience, it is Apple's rule: an app
#   may embed a framework bundle and may not embed a bare dylib.
#
# WHAT THE SLICES ARE, AND WHY THEY ARE NOT LISTED HERE.
#   They are asked of `scripts/build-apple-sdk.sh --print-slices`. The ANGLE
#   xcframework and the engine xcframework are linked into the same application,
#   so their slice sets have to match; if this script kept its own copy of the
#   list, the two could drift and the failure would land in a consumer's link
#   step naming neither script. Five Rust triples across the three platform
#   groups, so five gn configurations -- the probe measured three because three
#   was enough to answer the question it was asking.
#
# WHAT THE REVISION AND THE gn ARGUMENTS ARE, AND WHY THEY ARE NOT HERE EITHER.
#   contracts/artifact-manifest/apple-angle.lock.json. The pin is the identity of
#   what gets built; a copy in this file would be the one that silently wins on
#   the build machine.
#
# Usage:
#   scripts/build-angle-apple.sh --fetch [--source DIR] [--revision SHA]
#   scripts/build-angle-apple.sh --platform <ios|ios-simulator|macos> [--source DIR]
#                                            [--keep-going]
#   scripts/build-angle-apple.sh --archive <ios|ios-simulator|macos>
#   scripts/build-angle-apple.sh --xcframework
#   scripts/build-angle-apple.sh --print-gn-args <rust-target-triple>
#   scripts/build-angle-apple.sh --print-loader-layout <ios|ios-simulator|macos>
#   scripts/build-angle-apple.sh --print-xcframeworks
#   scripts/build-angle-apple.sh --check
#
#   --fetch          Create or update the ANGLE checkout at --source and put it
#                    on the pinned revision. Needs depot_tools on PATH.
#   --platform       gn gen + ninja for every slice of that platform group, then
#                    lipo them into engine/third_party/angle-apple-<platform>/.
#                    macOS only.
#   --archive        Pack one installed platform into a single .tar.gz for
#                    publication, beside a .sha256 of it. One archive per
#                    platform rather than one asset per file, because an iOS
#                    product is a DIRECTORY (a framework bundle) and a directory
#                    is not a release asset. Made on the machine that built the
#                    bytes, so what gets published is what the build produced
#                    rather than a repackaging of a CI artifact download.
#   --keep-going     Diagnostic only: pass `-k 0` to ninja so a failing build
#                    reports EVERY failure instead of stopping at the first.
#                    It exists because "is this one broken file or twenty" is
#                    the question that decides whether a problem is patched or
#                    designed around, and answering it one build at a time costs
#                    a checkout each time. Never for a shipping build -- it makes
#                    a partial build look like a finished one, and the install
#                    step below is what then fails.
#   --xcframework    Assemble the installed platforms into xcframeworks in
#                    platforms/apple/Frameworks/. macOS only. One per shipped
#                    library per platform family -- four -- because
#                    `xcodebuild -create-xcframework` holds one library per
#                    platform AND refuses to mix framework bundles with plain
#                    libraries, which is the split ANGLE itself makes on
#                    `is_ios`. The alternative, repackaging macOS to match iOS,
#                    breaks ANGLE's own dispatch-library lookup; see "The layout
#                    ANGLE's own loader will search".
#   --print-gn-args  Print the full gn argument string for one Rust target
#                    triple and exit, on any host. It is how the contract gate
#                    checks this script's real behaviour instead of grepping it,
#                    and it fails closed on a triple with no mapping.
#   --print-loader-layout
#                    Print `<ninja target> <path>` per line: where ANGLE's own
#                    loader will look for each library, relative to the
#                    directory they are installed into. Any host. The build step
#                    asserts against it and the contract gate checks it, so the
#                    rule that decides how these products may be packaged is one
#                    the recipe can be ASKED about rather than one someone has
#                    to know.
#   --print-xcframeworks
#                    Print the basenames --xcframework would assemble. Any host.
#                    One per shipped library per platform family; a consumer's
#                    Package.swift needs these names, and the contract gate uses
#                    them to check the split survives.
#   --check          Report readiness and build nothing.
#
# bash 3.2: macOS ships it as /bin/bash and this script runs there. No mapfile,
# no associative arrays, no ${v,,}, and every array expansion guarded --
# scripts/test-macos-bash32-contract.sh derives its scope from the workflows, so
# the lane that runs this script puts it in that gate's scope automatically.
# =============================================================================
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
LOCK="$REPO_ROOT/contracts/artifact-manifest/apple-angle.lock.json"
SDK_SCRIPT="$REPO_ROOT/scripts/build-apple-sdk.sh"
PATCH_DIR="$REPO_ROOT/engine/third_party/angle-patches/apple"
INSTALL_ROOT="$REPO_ROOT/engine/third_party"
FRAMEWORKS_DIR="$REPO_ROOT/platforms/apple/Frameworks"
# The SDK assembler builds the dependency set in a temporary directory before
# replacing a previously usable package. Standalone ANGLE builds keep the default.
FRAMEWORKS_DIR="${MIGO_APPLE_FRAMEWORKS_DIR:-$FRAMEWORKS_DIR}"

BUILD_ROOT="${MIGO_ANGLE_APPLE_BUILD_ROOT:-/tmp/migo-angle-apple}"
SOURCE="${MIGO_ANGLE_APPLE_SRC:-$BUILD_ROOT/src}"

MODE=""
PLATFORM=""
PRINT_TRIPLE=""
REVISION=""
KEEP_GOING=0

err()  { printf '\033[0;31m[angle-apple] %s\033[0m\n' "$*" >&2; }
ok()   { printf '\033[0;32m[angle-apple] %s\033[0m\n' "$*"; }
info() { printf '\033[0;36m[angle-apple] %s\033[0m\n' "$*"; }

usage() {
    # The header IS the usage text -- one copy, so `--help` cannot drift from
    # what the file says. The closing banner line is excluded rather than
    # trimmed afterwards, so the range stays readable.
    sed -n '/^# Usage:/,/^# ====/{/^# ====/!p;}' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
}

while [ $# -gt 0 ]; do
    case "$1" in
        --fetch)          MODE="fetch"; shift ;;
        --platform)       MODE="build"; PLATFORM="${2:-}"; shift 2 ;;
        --archive)        MODE="archive"; PLATFORM="${2:-}"; shift 2 ;;
        --xcframework)    MODE="xcframework"; shift ;;
        --print-gn-args)  MODE="print-gn-args"; PRINT_TRIPLE="${2:-}"; shift 2 ;;
        --print-loader-layout)
                          MODE="print-loader-layout"; PLATFORM="${2:-}"; shift 2 ;;
        --print-xcframeworks)
                          MODE="print-xcframeworks"; shift ;;
        --check)          MODE="check"; shift ;;
        --source)         SOURCE="${2:-}"; shift 2 ;;
        --revision)       REVISION="${2:-}"; shift 2 ;;
        --keep-going)     KEEP_GOING=1; shift ;;
        -h|--help)        usage; exit 0 ;;
        *)                err "unknown argument: $1"; usage >&2; exit 2 ;;
    esac
done

[ -n "$MODE" ] || { err "one of --fetch, --platform, --archive, --xcframework, --print-gn-args, --print-loader-layout, --print-xcframeworks or --check is required"; usage >&2; exit 2; }
[ -f "$LOCK" ] || { err "pin not found: $LOCK"; exit 1; }

# ---------------------------------------------------------------------------
# What the pin says
# ---------------------------------------------------------------------------

# One reader, one place a key name is spelled. `python3 - "$LOCK" key` rather
# than a grep: a JSON file read with a regular expression is a file read
# incorrectly on the day somebody reformats it.
lock_value() {
    python3 - "$LOCK" "$1" <<'PY'
import json
import sys

path, dotted = sys.argv[1], sys.argv[2]
with open(path, encoding="utf-8") as handle:
    node = json.load(handle)
for key in dotted.split("."):
    if not isinstance(node, dict) or key not in node:
        print(f"{path}: no such key: {dotted}", file=sys.stderr)
        raise SystemExit(1)
    node = node[key]
if isinstance(node, list):
    print(" ".join(str(item) for item in node))
else:
    print(node)
PY
}

PINNED_REVISION="$(lock_value source.angle_revision)"
GCLIENT_TARGET_OS="$(lock_value source.gclient_target_os)"
GN_ARGS_COMMON="$(lock_value source.gn_args_common)"
NINJA_TARGETS="$(lock_value source.ninja_targets)"
ANGLE_REPOSITORY="$(lock_value source.repository)"
[ -n "$REVISION" ] || REVISION="$PINNED_REVISION"

# ---------------------------------------------------------------------------
# Rust target triple -> gn configuration
# ---------------------------------------------------------------------------

# The one thing in this file that is a mapping rather than a lookup, because
# there is nowhere else it exists: it is the correspondence between Rust's Apple
# triples and Chromium's build configuration, and neither project publishes it.
#
# Total over the triples build-apple-sdk.sh reports and fail-closed outside them
# -- scripts/test-apple-angle-recipe-contract.sh checks both halves. A default
# branch that guessed would produce a build for the wrong environment, and the
# artifact would be indistinguishable from a right one until an app failed to
# launch.
#
# `x86_64-apple-ios` is a simulator target: there is no Intel iOS device, which
# is why the pair (simulator, x64) and not (device, x64) is the correct reading
# of it.
gn_args_for_triple() {
    triple="$1"
    ios_target=""
    mac_target=""
    case "$triple" in
        aarch64-apple-ios|aarch64-apple-ios-sim|x86_64-apple-ios)
            ios_target="$(deployment_target ios)" || return 1 ;;
        aarch64-apple-darwin|x86_64-apple-darwin)
            mac_target="$(deployment_target macos)" || return 1 ;;
    esac
    case "$triple" in
        aarch64-apple-ios)
            printf '%s target_os="ios" target_environment="device" target_cpu="arm64" ios_enable_code_signing=false ios_deployment_target="%s"' \
                "$GN_ARGS_COMMON" "$ios_target" ;;
        aarch64-apple-ios-sim)
            printf '%s target_os="ios" target_environment="simulator" target_cpu="arm64" ios_enable_code_signing=false ios_deployment_target="%s"' \
                "$GN_ARGS_COMMON" "$ios_target" ;;
        x86_64-apple-ios)
            printf '%s target_os="ios" target_environment="simulator" target_cpu="x64" ios_enable_code_signing=false ios_deployment_target="%s"' \
                "$GN_ARGS_COMMON" "$ios_target" ;;
        aarch64-apple-darwin)
            printf '%s target_os="mac" target_cpu="arm64" mac_deployment_target="%s"' \
                "$GN_ARGS_COMMON" "$mac_target" ;;
        x86_64-apple-darwin)
            printf '%s target_os="mac" target_cpu="x64" mac_deployment_target="%s"' \
                "$GN_ARGS_COMMON" "$mac_target" ;;
        *)
            err "no ANGLE configuration for Rust target: $triple"
            err "Every slice scripts/build-apple-sdk.sh --print-slices reports needs one"
            err "here, or ANGLE would be built for a different set of architectures than"
            err "the engine and the two xcframeworks could not be linked into one app."
            return 1 ;;
    esac
}

# The deployment target, asked of the script that owns
# contracts/apple/deployment-floor.json rather than read from the contract
# again here. Two readers of one JSON file is two places a key name can be
# spelled wrong; one reader with two callers is one.
deployment_target() {
    bash "$SDK_SCRIPT" --print-deployment-target "$1"
}

slices_for_platform() {
    bash "$SDK_SCRIPT" --print-slices "$1"
}

# Every Apple slice group, from the script that defines them. Iterating a
# written-out list here would be a second copy of a set that already exists, and
# the copy that quietly covers two of three is the one nobody notices.
apple_platforms() {
    bash "$SDK_SCRIPT" --print-platforms
}

# ---------------------------------------------------------------------------
# The layout ANGLE's own loader will search
# ---------------------------------------------------------------------------

# libEGL DOES NOT LINK libGLESv2. `otool -L` on the built library names only
# system frameworks; libEGL opens its dispatch library at run time, under a name
# it composes itself. Read at the pinned revision rather than remembered:
#
#   src/libEGL/libEGL_autogen.cpp:41     OpenSystemLibraryAndGetError(
#                                          ANGLE_DISPATCH_LIBRARY,
#                                          SearchType::ModuleDir)
#   src/common/system_utils.cpp:221      name + "." + GetSharedLibraryExtension()
#                             :229-234   ... and on the iOS family, + "/" + name
#   src/common/system_utils_ios.cpp:27   that extension is "framework"
#   src/common/system_utils_mac.cpp:26   that extension is "dylib"
#   src/common/system_utils_posix.cpp:198
#                                        ModuleDir is GetExecutableDirectory() +
#                                        "/Frameworks/" on the iOS family, and on
#                                        macOS the directory libEGL itself was
#                                        loaded from, via dladdr.
#
# So the two products have to be siblings, and their names are load-bearing:
#
#   macOS       <the directory libEGL was loaded from>/libGLESv2.dylib
#   iOS family  <the app bundle>/Frameworks/libGLESv2.framework/libGLESv2
#
# Both are what ANGLE's own build already emits, WHICH IS WHY THIS IS ASSERTED
# RATHER THAN ASSUMED. `xcodebuild -create-xcframework` refuses to hold
# frameworks and libraries in one bundle, and the tidy-looking answer to that --
# wrap the macOS libraries in framework bundles so all three platforms match --
# moves libEGL to libEGL.framework/Versions/A/libEGL, whereupon ANGLE searches
# libEGL.framework/Versions/A/libGLESv2.dylib and finds nothing. That failure has
# no build step to land in. It appears the first time an application asks EGL for
# a display, and nothing in this repository is on the stack to say why. The
# xcframeworks are split by family instead; see the assembly step at the end.
#
# What is owned here is the RELATIVE layout. Where the directory itself ends up
# -- an app's Frameworks folder, a test bundle, a staging tree -- belongs to
# whoever embeds it.
loader_path_for() {
    case "$1" in
        ios) printf '%s.framework/%s' "$2" "$2" ;;
        mac) printf '%s.dylib' "$2" ;;
        *)   err "no ANGLE loader layout for platform family '$1'"; return 1 ;;
    esac
}

loader_layout_for_family() {
    for layout_target in $NINJA_TARGETS; do
        layout_path="$(loader_path_for "$1" "$layout_target")" || return 1
        printf '%s %s\n' "$layout_target" "$layout_path"
    done
}

# Which family a platform belongs to, read out of the gn arguments this script
# already generates for it rather than from a second list of platform names.
# ANGLE's `angle_shared_library` template switches product shape on `is_ios` and
# on nothing finer, so `target_os` IS the axis that both the loader layout above
# and the xcframework split below have to follow.
family_for_platform() {
    family_platform="$1"
    family_slices="$(slices_for_platform "$family_platform")" || return 1
    family_seen=""
    for family_triple in $family_slices; do
        family_args="$(gn_args_for_triple "$family_triple")" || return 1
        case "$family_args" in
            *'target_os="ios"'*) family_this=ios ;;
            *'target_os="mac"'*) family_this=mac ;;
            *)  err "the gn arguments for $family_triple name no target_os this script knows"
                return 1 ;;
        esac
        if [ -n "$family_seen" ] && [ "$family_seen" != "$family_this" ]; then
            err "platform '$family_platform' mixes ANGLE families ($family_seen and $family_this):"
            err "one platform group is one gn target_os, or its products have two shapes and"
            err "cannot go into one xcframework."
            return 1
        fi
        family_seen="$family_this"
    done
    [ -n "$family_seen" ] || { err "platform '$family_platform' reports no slices"; return 1; }
    printf '%s' "$family_seen"
}

# The families present, collected from the platforms rather than written out.
apple_families() {
    families_seen=""
    for families_platform in $(apple_platforms); do
        families_one="$(family_for_platform "$families_platform")" || return 1
        case " $families_seen " in
            *" $families_one "*) ;;
            *)  families_seen="$families_seen $families_one"
                printf '%s\n' "$families_one" ;;
        esac
    done
    [ -n "$families_seen" ] || { err "build-apple-sdk.sh reports no Apple platforms"; return 1; }
}

# gn's word for a family -> the word every artifact name in this repository uses.
# Two vocabularies exist already: gn says "mac", the archives say "macos". This
# translates rather than letting a third spelling reach a published file, and it
# fails closed, because a family with no name would otherwise be published under
# an empty one.
family_artifact_token() {
    case "$1" in
        ios) printf 'ios' ;;
        mac) printf 'macos' ;;
        *)   err "no artifact name for ANGLE platform family '$1'"; return 1 ;;
    esac
}

# One name, used by the assembly step and by --print-xcframeworks, so what the
# lane builds and what a consumer is told to expect cannot drift apart.
# `ANGLELib...` because a SwiftPM binaryTarget is named after the xcframework's
# basename.
xcframework_basename() {
    xcf_token="$(family_artifact_token "$2")" || return 1
    printf 'ANGLELib%s-%s' "${1#lib}" "$xcf_token"
}

if [ "$MODE" = "print-gn-args" ]; then
    [ -n "$PRINT_TRIPLE" ] || { err "--print-gn-args needs a Rust target triple"; exit 2; }
    gn_args_for_triple "$PRINT_TRIPLE" || exit 1
    printf '\n'
    exit 0
fi

# Beside --print-gn-args and for the same reason: the properties worth checking
# on every pull request are the ones a host with no Xcode can ask about, and a
# question answered by RUNNING the recipe cannot drift from what it does the way
# a grep of its source can.
if [ "$MODE" = "print-loader-layout" ]; then
    [ -n "$PLATFORM" ] || { err "--print-loader-layout needs a platform"; exit 2; }
    layout_family="$(family_for_platform "$PLATFORM")" || exit 1
    loader_layout_for_family "$layout_family" || exit 1
    exit 0
fi

if [ "$MODE" = "print-xcframeworks" ]; then
    xcf_families="$(apple_families)" || exit 1
    for xcf_target in $NINJA_TARGETS; do
        for xcf_family in $xcf_families; do
            xcf_name="$(xcframework_basename "$xcf_target" "$xcf_family")" || exit 1
            printf '%s.xcframework\n' "$xcf_name"
        done
    done
    exit 0
fi

# ---------------------------------------------------------------------------
# Where the checkout is
# ---------------------------------------------------------------------------

# Read from .gclient, never guessed. See the header: `fetch angle` names its
# solution `.`, and the probe's first run lost a four-minute fetch to assuming
# otherwise. Reading the file also means a shape change upstream keeps working.
angle_root() {
    [ -f "$SOURCE/.gclient" ] || { err "not a gclient checkout: $SOURCE (no .gclient)"; return 1; }
    root="$(python3 - "$SOURCE/.gclient" <<'PY'
import pathlib
import sys

source = pathlib.Path(sys.argv[1]).read_text()
namespace = {}
exec(compile(source, ".gclient", "exec"), namespace)
solutions = namespace["solutions"]
if len(solutions) != 1:
    raise SystemExit(
        f".gclient declares {len(solutions)} solutions; this recipe knows how to locate one"
    )
print(solutions[0]["name"])
PY
)" || return 1
    (cd "$SOURCE/$root" && pwd)
}

# ---------------------------------------------------------------------------
# Readiness
# ---------------------------------------------------------------------------

check_ready() {
    failures=0
    for tool in python3 git; do
        command -v "$tool" >/dev/null 2>&1 || { err "not on PATH: $tool"; failures=$((failures + 1)); }
    done
    # `--check` answers for the WHOLE lane, not for itself. A readiness report
    # that only checks what the reporting mode needs is the shape this
    # repository keeps removing: it passes on a machine that cannot do the work,
    # and the first thing anyone learns is a failure four minutes into a 12 GB
    # fetch. On a host that cannot build ANGLE at all this is expected to fail,
    # which is the same position scripts/build-apple-sdk.sh takes.
    checking="$MODE"
    [ "$MODE" = "check" ] && checking="fetch build archive xcframework"
    for mode in $checking; do
    case "$mode" in
        fetch)
            for tool in fetch gclient; do
                command -v "$tool" >/dev/null 2>&1 || {
                    err "not on PATH: $tool -- clone depot_tools and put it on PATH:"
                    err "  git clone --depth 1 https://chromium.googlesource.com/chromium/tools/depot_tools.git"
                    failures=$((failures + 1))
                }
            done
            ;;
        build)
            for tool in gn ninja lipo xcrun patch install_name_tool; do
                command -v "$tool" >/dev/null 2>&1 || { err "not on PATH: $tool"; failures=$((failures + 1)); }
            done
            ;;
        archive)
            command -v tar >/dev/null 2>&1 || { err "not on PATH: tar"; failures=$((failures + 1)); }
            # Either digest tool, matching what the archive step actually calls.
            # A readiness check stricter than the code it guards fails for a
            # reason that is not true, which is worse than not checking.
            command -v shasum >/dev/null 2>&1 || command -v sha256sum >/dev/null 2>&1 || {
                err "no sha256 tool on PATH: neither shasum nor sha256sum"
                failures=$((failures + 1))
            }
            ;;
        xcframework)
            command -v xcodebuild >/dev/null 2>&1 || { err "not on PATH: xcodebuild"; failures=$((failures + 1)); }
            ;;
    esac
    done
    # `case` rather than `echo ... | grep -q`: pipefail plus grep's early exit
    # reports the writer's SIGPIPE as the pipeline's status, so the test can
    # silently stop testing. See the head of
    # scripts/test-apple-angle-recipe-contract.sh, where that cost a live check.
    case "$PINNED_REVISION" in
        *[!0-9a-f]* | "")
            err "source.angle_revision is not a lowercase hex commit hash: $PINNED_REVISION"
            failures=$((failures + 1)) ;;
        ????????????????????????????????????????) ;;
        *)
            err "source.angle_revision is not 40 characters long: $PINNED_REVISION"
            failures=$((failures + 1)) ;;
    esac
    return "$failures"
}

if [ "$MODE" = "check" ]; then
    info "pin        $PINNED_REVISION"
    info "repository $ANGLE_REPOSITORY"
    info "source     $SOURCE"
    if check_ready; then ok "ready"; exit 0; fi
    err "not ready"
    exit 1
fi

check_ready || { err "prerequisites unmet; run --check for the list"; exit 1; }

# ---------------------------------------------------------------------------
# Fetch
# ---------------------------------------------------------------------------

if [ "$MODE" = "fetch" ]; then
    mkdir -p "$SOURCE"
    if [ ! -f "$SOURCE/.gclient" ]; then
        info "fetch --no-history angle into $SOURCE (about 12 GB and four minutes)"
        ( cd "$SOURCE" && fetch --no-history angle )
        # target_os limited to the two Apple platforms so gclient does not sync
        # the Android and Windows dependency sets, which are most of the tree
        # and none of the product. Written from the pin so the checkout's shape
        # is part of what the lock file describes.
        python3 - "$SOURCE/.gclient" "$GCLIENT_TARGET_OS" <<'PY'
import pathlib
import sys

path = pathlib.Path(sys.argv[1])
platforms = sys.argv[2].split()
with path.open("a", encoding="utf-8") as handle:
    handle.write("target_os = %r\n" % (platforms,))
    handle.write("target_os_only = True\n")
PY
    fi

    ANGLE_ROOT="$(angle_root)"
    info "solution root $ANGLE_ROOT (read from .gclient)"

    # `--revision` and not a bare sync: the whole point of a pin is that the
    # tree is the one at that hash rather than whatever tip happens to be. A
    # `--no-history` checkout is shallow, so reaching an older commit can need
    # the history the fetch deliberately skipped; that is a diagnosable failure
    # with a named recovery rather than a mystery, so it gets one.
    info "gclient sync to $REVISION"
    if ! ( cd "$SOURCE" && gclient sync --no-history --revision "$REVISION" ); then
        err "sync to $REVISION failed against a --no-history checkout; unshallowing and retrying"
        ( cd "$ANGLE_ROOT" && git fetch --unshallow origin ) || true
        ( cd "$SOURCE" && gclient sync --revision "$REVISION" )
    fi

    head="$(cd "$ANGLE_ROOT" && git rev-parse HEAD)"
    if [ "$head" != "$REVISION" ]; then
        err "checkout is at $head, the pin says $REVISION"
        exit 1
    fi
    ok "ANGLE at $REVISION in $ANGLE_ROOT"
    exit 0
fi

# ---------------------------------------------------------------------------
# Archive for publication
# ---------------------------------------------------------------------------

# Deliberately BEFORE the macOS gate and before the checkout check: packing bytes
# that already exist needs neither Xcode nor a 12 GB source tree, and a mode that
# refuses to run on a machine where it would work is a mode nobody can test.
if [ "$MODE" = "archive" ]; then
    if ! slices_for_platform "$PLATFORM" >/dev/null 2>&1; then
        err "--archive needs a platform scripts/build-apple-sdk.sh knows:"
        apple_platforms | sed 's/^/  /' >&2
        exit 2
    fi
    src="$INSTALL_ROOT/angle-apple-$PLATFORM"
    [ -d "$src" ] || { err "nothing installed for $PLATFORM: $src"; exit 1; }

    mkdir -p "$BUILD_ROOT"
    archive="$BUILD_ROOT/angle-apple-$PLATFORM.tar.gz"
    rm -f "$archive" "$archive.sha256"

    # `COPYFILE_DISABLE=1` so bsdtar does not write AppleDouble `._` members for
    # extended attributes. Those are metadata about the build machine, they
    # differ between runs, and one of them inside a framework bundle is a file a
    # consumer would have to explain.
    #
    # This archive is NOT claimed to be byte-reproducible -- gzip stores an
    # mtime, and nothing here strips it. The pin is trust-on-first-use, the same
    # model contracts/artifact-manifest/ohos-sdk.lock.json states plainly: the
    # hash proves the bytes have not changed since they were hashed, not that a
    # second build would produce them.
    COPYFILE_DISABLE=1 tar -czf "$archive" -C "$INSTALL_ROOT" "angle-apple-$PLATFORM"

    if command -v shasum >/dev/null 2>&1; then
        digest="$(shasum -a 256 "$archive" | cut -d' ' -f1)"
    else
        digest="$(sha256sum "$archive" | cut -d' ' -f1)"
    fi
    bytes="$(wc -c < "$archive" | tr -d ' ')"
    printf '%s  %s\n' "$digest" "$(basename "$archive")" > "$archive.sha256"

    ok "$archive"
    ok "  sha256      $digest"
    ok "  size_bytes  $bytes"
    ok "  contents    $(tar -tzf "$archive" | sed "s|^angle-apple-$PLATFORM/||" | grep -v '^$' | cut -d/ -f1 | sort -u | tr '\n' ' ')"
    exit 0
fi

# ---------------------------------------------------------------------------
# Build
# ---------------------------------------------------------------------------

if [ "$(uname -s)" != "Darwin" ]; then
    err "Building ANGLE for an Apple platform needs macOS: gn takes the SDK from"
    err "xcrun and the Metal backend is compiled by Apple's clang."
    err ""
    err "This host is $(uname -s). Refusing to report a build that did not happen."
    err "What is checkable here:"
    err "  bash scripts/build-angle-apple.sh --print-gn-args aarch64-apple-ios"
    err "  bash scripts/test-apple-angle-recipe-contract.sh"
    exit 1
fi

if [ "$MODE" = "build" ]; then
    ANGLE_ROOT="$(angle_root)"
    head="$(cd "$ANGLE_ROOT" && git rev-parse HEAD)"
    if [ "$head" != "$REVISION" ]; then
        err "the checkout at $ANGLE_ROOT is at $head, not the pinned $REVISION"
        err "run: scripts/build-angle-apple.sh --fetch --source $SOURCE"
        exit 1
    fi
fi

# What a ninja target actually produced, asked of the output directory.
#
# Not a table of extensions per platform: ANGLE emits `libEGL.framework` for iOS
# and a plain shared library for macOS, and a table would have to be right about
# a third platform before anyone had built one. The rule here is the same
# everywhere -- exactly one thing in the output directory is named after the
# target -- and it fails with the listing when that is not true, which is how
# the framework was found in the first place.
#
# `.TOC` is ninja's link-order side file, not a product; `.dSYM` is a debug
# bundle these builds do not produce (symbol_level=0) but would be a second
# match if they ever did.
locate_product() {
    out_dir="$1"
    target="$2"
    found=""
    found_count=0
    for candidate in "$out_dir/$target" "$out_dir/$target".*; do
        [ -e "$candidate" ] || continue
        case "$candidate" in
            *.TOC|*.dSYM) continue ;;
        esac
        found="$candidate"
        found_count=$((found_count + 1))
    done
    if [ "$found_count" -ne 1 ]; then
        err "ninja target '$target' resolved to $found_count products in $out_dir"
        err "what that directory holds at the top level:"
        ls -1 "$out_dir" >&2 2>/dev/null || true
        return 1
    fi
    printf '%s' "$found"
}

# ---------------------------------------------------------------------------
# Patches
# ---------------------------------------------------------------------------

# ANGLE does not compile for this project's macOS deployment floor unpatched.
# `src/common/apple_platform_utils.mm` uses `kIOMainPortDefault`, which Apple
# introduced in macOS 12.0, with no `@available` guard -- and Chromium's own
# `mac_deployment_target` defaults to 13.0, so upstream never compiles that line
# below 12 and has no reason to notice. Measured, not guessed: the macOS leg of
# .github/workflows/apple-angle.yml failed on exactly that line with
# `-Werror,-Wunguarded-availability-new` while both iOS legs, whose floor is
# 15.0, built clean.
#
# Suppressing the diagnostic was rejected rather than untried: `kIOMainPortDefault`
# is a real IOKit symbol macOS 11 does not export, so the library would compile
# and then fail to load on exactly the floor it claims to support. Raising the
# floor was the other candidate and is a DECISION -- contracts/apple/deployment-floor.json
# says so in as many words -- so it is not something a build script gets to make
# by being easier to write.
#
# APPLIED-NESS IS DECIDED BY ASKING `patch` TO REVERSE THE PATCH, never by a
# sentinel string in the target file. scripts/lib/v8-patch-apply.sh's header
# records why in detail -- a sentinel restates what a patch does, so it can
# drift from it, and it drifted four separate ways in this repository, including
# one that silenced a sysroot patch for every Android build ever run.
#
# That library is deliberately NOT sourced here, and the reason is the gate next
# door: it uses `mapfile` and `local -A`, both bash 4.0, in its tree-audit half.
# macOS ships bash 3.2, and scripts/test-macos-bash32-contract.sh derives its
# scope from the scripts a macOS job NAMES -- so a bash-4 construct reached
# through a `source` would have been outside what it inspects. The three `patch`
# invocations below are the whole of the method that half of the library
# provides; the reasoning stays single-sourced in its header rather than copied.
#
# `--forward` on the reverse probe is load-bearing: with `--reverse` alone, GNU
# patch hits its "Unreversed patch detected!  Ignoring -R." heuristic, decides
# we meant to apply the patch, applies it, and exits 0 -- so an unapplied patch
# would be indistinguishable from an applied one. `--fuzz=0` stops loose context
# matching from calling a hunk reversible against code it does not match.
# `patch(1)` DISAGREES WITH ITSELF ACROSS PLATFORMS, and this script runs on the
# one that differs. GNU patch -- every Linux machine here, and git-bash on
# Windows -- spells a dry run `--dry-run` and has no `-C`. BSD patch, which is
# what macOS ships, spells it `-C` and has no `--dry-run`. There is no common
# spelling, so the tool is asked rather than guessed; this is the same family as
# macOS having no `timeout(1)`, which cost the probe lane a run to discover.
#
# `--no-backup-if-mismatch` is GNU-only as well, and on BSD it is not needed:
# BSD patch writes a backup only when asked with `-b`, while GNU writes a
# `.orig` beside any file whose hunks applied at an offset. So it is passed
# where it exists and omitted where it does not, rather than emulated.
#
# The probe is `patch <option> --version`, which discriminates: an unrecognised
# option makes patch fail before it prints anything. Verified against a nonsense
# option so that "accepted" is not simply what this probe always says.
patch_capability() {
    if patch "$1" --version >/dev/null 2>&1; then
        printf '%s' "$1"
    fi
}

PATCH_DRY_RUN="$(patch_capability --dry-run)"
[ -n "$PATCH_DRY_RUN" ] || PATCH_DRY_RUN="$(patch_capability -C)"
PATCH_NO_BACKUP="$(patch_capability --no-backup-if-mismatch)"
if [ -z "$PATCH_DRY_RUN" ]; then
    err "this patch(1) accepts neither --dry-run nor -C, so a patch cannot be"
    err "checked before it is written. Refusing to modify the checkout blind."
    exit 1
fi
# Printed rather than merely used, because which patch macOS actually ships is a
# fact this project did not have and will want the next time something differs.
info "patch(1)  $(patch --version 2>/dev/null | head -1) [dry-run=$PATCH_DRY_RUN backup-flag=${PATCH_NO_BACKUP:-none}]"

# Short forms for the rest -- `-t` batch, `-N` forward, `-R` reverse, `-F 0`
# fuzz -- because those four ARE common to both implementations.
apply_one_patch() {
    tree="$1"
    patch_file="$2"
    name="$(basename "$patch_file")"

    if patch -p1 -d "$tree" -t -N -R -F 0 $PATCH_DRY_RUN \
            < "$patch_file" >/dev/null 2>&1; then
        info "  = already in effect: $name"
        return 0
    fi
    # Decide on a dry run before writing anything. `--forward` is not
    # transactional: given a tree where an earlier hunk is unapplied and a later
    # one is already applied, patch writes the earlier hunk, skips the later one
    # and exits non-zero -- leaving the tree more modified than it found it, and
    # the next run starting from that new state.
    if ! patch -p1 -d "$tree" -t -N -F 0 $PATCH_DRY_RUN \
            < "$patch_file" >/dev/null 2>&1; then
        err "$name neither applies cleanly nor is already applied"
        err "the checkout is partly patched or has drifted; leaving it untouched"
        return 1
    fi
    # A stray `.orig` inside the source tree is a file the next `gclient sync`
    # has an opinion about, which is what $PATCH_NO_BACKUP suppresses where the
    # implementation can create one at all.
    if ! patch -p1 -d "$tree" -t -N -F 0 $PATCH_NO_BACKUP \
            < "$patch_file" >/dev/null 2>&1; then
        err "$name failed to apply after its own dry run succeeded"
        return 1
    fi
    info "  + applied: $name"
}

apply_apple_patches() {
    tree="$1"
    [ -d "$PATCH_DIR" ] || { err "no patch directory: $PATCH_DIR"; return 1; }
    applied=0
    for patch_file in "$PATCH_DIR"/*.patch; do
        [ -f "$patch_file" ] || continue
        apply_one_patch "$tree" "$patch_file" || return 1
        applied=$((applied + 1))
    done
    # An empty patch stage is indistinguishable from a clean one, and this
    # repository has been bitten by that shape more than once. There is at least
    # one patch; if there is ever none, that is a deletion somebody should have
    # to justify rather than a silent no-op.
    if [ "$applied" -eq 0 ]; then
        err "$PATCH_DIR contains no patches, so this stage checked nothing"
        return 1
    fi
    info "$applied ANGLE patch(es) in effect"
}

if [ "$MODE" = "build" ]; then
    SLICES="$(slices_for_platform "$PLATFORM")" || exit 1
    OUT_DIR="$INSTALL_ROOT/angle-apple-$PLATFORM"

    info "platform  $PLATFORM"
    info "revision  $REVISION"
    info "slices    $(echo $SLICES | tr '\n' ' ')"
    info "targets   $NINJA_TARGETS"

    apply_apple_patches "$ANGLE_ROOT" || { err "patch stage failed"; exit 1; }

    for triple in $SLICES; do
        args="$(gn_args_for_triple "$triple")" || exit 1
        out="out/migo-$triple"
        info "gn gen $out"
        ( cd "$ANGLE_ROOT" && gn gen "$out" --args="$args" )
        ( cd "$ANGLE_ROOT" && gn args "$out" --list --short --overrides-only )
        if [ "$KEEP_GOING" = "1" ]; then
            info "ninja -k 0 -C $out $NINJA_TARGETS  (diagnostic: reports every failure)"
            ( cd "$ANGLE_ROOT" && ninja -k 0 -C "$out" $NINJA_TARGETS )
        else
            info "ninja -C $out $NINJA_TARGETS"
            ( cd "$ANGLE_ROOT" && ninja -C "$out" $NINJA_TARGETS )
        fi
    done

    rm -rf "$OUT_DIR"
    mkdir -p "$OUT_DIR"

    for target in $NINJA_TARGETS; do
        # An array, not a space-joined string. A developer's Mac has a home
        # directory named after them and "/Users/Jimmy McGill/..." is an
        # ordinary path there; a joined string would split it into two
        # arguments and lipo would report a missing file that exists. CI never
        # meets a space, so the string form would have passed every run and
        # failed on the first machine that mattered.
        inputs=()
        # A plain counter beside the array, and not `${#inputs[@]}`. bash 3.2's
        # treatment of an EMPTY array under `set -u` is the exact hazard
        # scripts/test-macos-bash32-contract.sh exists for, and whether the
        # length form is safe there is a thing to look up rather than assume --
        # so it is not used. An integer is unambiguous in every shell.
        input_count=0
        for triple in $SLICES; do
            candidate="$(locate_product "$ANGLE_ROOT/out/migo-$triple" "$target")" || exit 1
            inputs[$input_count]="$candidate"
            input_count=$((input_count + 1))
        done

        product="$(basename "${inputs[0]}")"
        info "$target -> $product"

        # lipo even for one slice would work, but `cp` keeps a single-slice
        # platform a thin Mach-O rather than a one-architecture fat file --
        # which is what build-apple-sdk.sh produces for the same platform, and
        # matching it keeps the two archives comparable.
        if [ "$input_count" -eq 1 ]; then
            cp -R ${inputs[@]+"${inputs[@]}"} "$OUT_DIR/$product"
        elif [ -d "${inputs[0]}" ]; then
            # A framework bundle. lipo joins Mach-O files, not directories, so
            # one slice's bundle is taken whole and its executable replaced by
            # the fat one. Taking the bundle whole rather than rebuilding it is
            # deliberate: everything else inside it -- the Info.plist ninja
            # generated, the bundle layout -- is architecture-independent and
            # produced by the build system, and a hand-written copy would be a
            # second, worse source for it.
            info "lipo $input_count slices inside $product"
            cp -R "${inputs[0]}" "$OUT_DIR/$product"
            binaries=()
            binary_count=0
            for bundle in ${inputs[@]+"${inputs[@]}"}; do
                binaries[$binary_count]="$bundle/$target"
                binary_count=$((binary_count + 1))
            done
            lipo -create ${binaries[@]+"${binaries[@]}"} -output "$OUT_DIR/$product/$target"
        else
            info "lipo $input_count slices into $product"
            lipo -create ${inputs[@]+"${inputs[@]}"} -output "$OUT_DIR/$product"
        fi
    done

    # A PLAIN LIBRARY LEAVES THE LINKER WITH A RELATIVE INSTALL NAME, and that
    # is measured: the first macOS build of this recipe produced
    # `LC_ID_DYLIB = ./libEGL.dylib`, while the iOS framework bundles correctly
    # carried `@rpath/libEGL.framework/libEGL` because `ios_framework_bundle`
    # sets one. A consumer that LINKS `./libEGL.dylib` records a dependency dyld
    # resolves against the process's working directory, which is wrong anywhere
    # and silently wrong inside an .app.
    #
    # It would not have bitten this engine, which opens ANGLE by path rather
    # than linking it -- which is precisely why it would have shipped. `@rpath`
    # is what an embedded library in `Contents/Frameworks` needs, and it is what
    # Xcode's own embed step assumes.
    #
    # Bundles are skipped: theirs is already correct and set by the build.
    for target in $NINJA_TARGETS; do
        installed="$(locate_product "$OUT_DIR" "$target")" || exit 1
        [ -d "$installed" ] && continue
        want="@rpath/$(basename "$installed")"
        if ! install_name_tool -id "$want" "$installed"; then
            err "could not set the install name of $(basename "$installed") to $want"
            err "A relative LC_ID_DYLIB is not shippable, so this is fatal rather"
            err "than a warning."
            exit 1
        fi
        info "install name  $(basename "$installed") -> $want"
    done

    # THE LAYOUT ANGLE'S OWN LOADER WILL SEARCH, checked against what was just
    # installed. The source lines this comes from, and the repackaging it exists
    # to stop, are under "The layout ANGLE's own loader will search" above.
    #
    # It is deliberately not a restatement of what locate_product already found:
    # that function finds whatever is named after the ninja target, and this
    # pins the exact name and shape ANGLE will compose. A product located as
    # `libEGL.dylib` on a platform whose loader appends `.framework` passes the
    # first and fails this one.
    build_family="$(family_for_platform "$PLATFORM")" || exit 1
    for target in $NINJA_TARGETS; do
        want_rel="$(loader_path_for "$build_family" "$target")" || exit 1
        installed="$(locate_product "$OUT_DIR" "$target")" || exit 1
        if [ ! -e "$OUT_DIR/$want_rel" ]; then
            err "ANGLE loads $target from '$want_rel', relative to the directory it"
            err "finds itself in, and there is nothing there. What was installed:"
            ls -1 "$OUT_DIR" >&2 2>/dev/null || true
            exit 1
        fi
        # The located product and the searched path have to be the SAME product.
        # A second copy under the searched name would satisfy the check above
        # while the build shipped the other one.
        if [ "$installed" != "$OUT_DIR/${want_rel%%/*}" ]; then
            err "$target was built as '$(basename "$installed")' but ANGLE will load"
            err "'${want_rel%%/*}'. Those are two different files, and the one that"
            err "ships is not the one that gets opened."
            exit 1
        fi
        info "loader path   $target -> $want_rel"
    done

    # Facts the presenter will need and nobody has: what dyld will call these
    # libraries, what they expect to find beside them, and which architectures
    # actually landed. Printed rather than asserted -- there is no presenter yet
    # to have an opinion, and a log is where the next step reads them from.
    for target in $NINJA_TARGETS; do
        installed="$(locate_product "$OUT_DIR" "$target")" || exit 1
        macho="$installed"
        [ -d "$installed" ] && macho="$installed/$target"
        info "--- $(basename "$installed") ---"
        ls -ld "$installed"
        lipo -info "$macho" || true
        otool -D "$macho" || true
        otool -L "$macho" || true
    done

    ok "installed $NINJA_TARGETS into $OUT_DIR"
    exit 0
fi

# ---------------------------------------------------------------------------
# xcframework assembly
# ---------------------------------------------------------------------------

if [ "$MODE" = "xcframework" ]; then
    mkdir -p "$FRAMEWORKS_DIR"

    # ONE xcframework PER SHIPPED LIBRARY PER PLATFORM FAMILY -- four, not two,
    # and the split is forced from both ends.
    #
    # From the tool: `xcodebuild -create-xcframework` was asked, on run
    # 33958391676, to hold ANGLE's iOS framework bundles and its macOS shared
    # libraries together, and answered
    #
    #     error: an xcframework cannot contain both frameworks and libraries.
    #
    # From ANGLE: the obvious way to make that one bundle possible is to wrap
    # the macOS libraries in framework bundles so every platform matches. That
    # would break loading. libEGL finds libGLESv2 by composing a name against
    # the directory it was itself loaded from, and on macOS that name ends
    # `.dylib` and carries no bundle -- see "The layout ANGLE's own loader will
    # search" above for the source lines. Wrapping moves libEGL two directories
    # down and the search follows it. Nothing would fail until an application
    # asked for a display.
    #
    # So each platform keeps exactly the shape ANGLE builds for it, and the
    # xcframeworks are cut along ANGLE's own axis -- `is_ios`, read here as the
    # gn target_os each platform's slices carry. A consumer's Package.swift then
    # names the iOS pair and the macOS pair separately, which is what a package
    # that supports both platforms has to do anyway.
    xcf_families="$(apple_families)" || exit 1
    assembled=0

    for target in $NINJA_TARGETS; do
        for family in $xcf_families; do
            name="$(xcframework_basename "$target" "$family")" || exit 1
            xcframework="$FRAMEWORKS_DIR/$name.xcframework"
            libraries=()
            library_count=0
            flavour=""
            present=""
            for platform in $(apple_platforms); do
                platform_family="$(family_for_platform "$platform")" || exit 1
                [ "$platform_family" = "$family" ] || continue
                install_dir="$INSTALL_ROOT/angle-apple-$platform"
                [ -d "$install_dir" ] || continue
                candidate="$(locate_product "$install_dir" "$target")" || exit 1
                if [ -d "$candidate" ]; then
                    this_flavour="framework"
                else
                    this_flavour="library"
                fi
                # Within one family the shape has to be uniform, or the split
                # above is cut along the wrong axis. Asserted rather than left
                # to xcodebuild: the same refusal arrives either way, but only
                # one of the two says which platform disagreed with which.
                if [ -n "$flavour" ] && [ "$flavour" != "$this_flavour" ]; then
                    err "$target is a $flavour on[$present ] and a $this_flavour on"
                    err "$platform, and both are in the '$family' family. An xcframework"
                    err "cannot hold both, so the family is no longer ANGLE's product axis."
                    exit 1
                fi
                flavour="$this_flavour"
                case "$this_flavour" in
                    framework) libraries[$library_count]="-framework" ;;
                    library)   libraries[$library_count]="-library" ;;
                esac
                library_count=$((library_count + 1))
                libraries[$library_count]="$candidate"
                library_count=$((library_count + 1))
                present="$present $platform"
            done
            # A family with nothing installed is skipped rather than fatal: a
            # narrowed build (--platform macos alone) is a legitimate way to run
            # this lane, and it produces the macOS pair only.
            [ "$library_count" -ne 0 ] || continue
            rm -rf "$xcframework"
            info "xcodebuild -create-xcframework ($flavour:$present ) -> $name.xcframework"
            xcodebuild -create-xcframework ${libraries[@]+"${libraries[@]}"} -output "$xcframework"
            assembled=$((assembled + 1))
        done
    done

    if [ "$assembled" -eq 0 ]; then
        err "no installed platform carries any of: $NINJA_TARGETS"
        err "build at least one first:"
        err "  scripts/build-angle-apple.sh --platform ios"
        exit 1
    fi
    ok "assembled $assembled xcframework(s) into $FRAMEWORKS_DIR"
    exit 0
fi

err "unreachable mode: $MODE"
exit 1
