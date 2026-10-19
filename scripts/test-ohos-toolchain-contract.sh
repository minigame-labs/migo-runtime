#!/usr/bin/env bash
# scripts/test-ohos-toolchain-contract.sh
#
# Assert that every C/C++ object in an OpenHarmony build was produced by the
# OpenHarmony SDK's own clang -- not by whatever compiler happened to be on
# PATH.
#
# WHY THIS EXISTS (observed 2026-07-30, not hypothetical):
# The first ohos build on this machine compiled zstd and sqlite3 with the
# Android NDK r23c's clang 12 and its bionic headers, for a musl target. It
# SUCCEEDED: cc-rs passes --target, so the object files carried the correct
# triple and the link went through. The damage is invisible until runtime,
# where bionic-shaped struct layouts meet musl ones. An ambient CC beats
# .cargo/config.toml's non-forcing [env] block, and skia-bindings does not even
# consult cc-rs's target prefixes -- it reads CLANGCC, then plain CC.
#
# WHY IT CHECKS WHAT IT CHECKS:
# The obvious probe -- readelf -p .comment on the objects -- is NOT usable.
# The OHOS clang emits no .comment section at all, so "no object says Android"
# is true both when the toolchain is correct and when the object carries no
# identity whatsoever. A guard whose green state is indistinguishable from its
# blind state is worse than none. Instead this reads the compiler path cc-rs
# itself recorded in each build script's `output` file: the build reports its
# own toolchain, and that report is what gets checked.
#
# Usage:
#   scripts/test-ohos-toolchain-contract.sh [target-triple]
#     default target: x86_64-unknown-linux-ohos
#
# Env:
#   OHOS_SDK_NATIVE  the SDK's native/ directory. If unset, dev-setup-ohos.sh
#                    is consulted.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
TARGET="${1:-x86_64-unknown-linux-ohos}"
# shellcheck source=scripts/lib/cargo-target-dir.sh
source "$SCRIPT_DIR/lib/cargo-target-dir.sh"
BUILD_DIR="$(cargo_target_dir "$REPO_ROOT/engine")/$TARGET"

pass() { echo -e "\033[0;32m[ohos-toolchain] PASS $*\033[0m"; }
fail() { echo -e "\033[0;31m[ohos-toolchain] FAIL $*\033[0m" >&2; }
info() { echo -e "\033[0;36m[ohos-toolchain] $*\033[0m"; }

if [[ -z "${OHOS_SDK_NATIVE:-}" ]]; then
    eval "$(bash "$SCRIPT_DIR/dev-setup-ohos.sh" | grep '^export OHOS_SDK_NATIVE')"
fi
if [[ -z "${OHOS_SDK_NATIVE:-}" || ! -d "$OHOS_SDK_NATIVE" ]]; then
    fail "OHOS_SDK_NATIVE is not set to an existing directory"
    exit 1
fi

if [[ ! -d "$BUILD_DIR" ]]; then
    fail "no build output for $TARGET at $BUILD_DIR"
    fail "build something for that target first, e.g."
    fail "  cargo build --target $TARGET -p migo-io"
    exit 1
fi

info "target:  $TARGET"
info "sdk:     $OHOS_SDK_NATIVE"

# ---- 1. every recorded compiler invocation must live inside the SDK ---------
# cc-rs writes the resolved compiler into the build script's `output` file.
FOREIGN=0
SEEN=0
declare -A COMPILERS=()

while IFS= read -r output_file; do
    while IFS= read -r compiler; do
        [[ -n "$compiler" ]] || continue
        # These `output` files also carry skia's ninja log, where a line such as
        #   ACTION //third_party/icu:make_data_assembly(//gn/toolchain:gcc_like)
        # matches the path-shaped pattern below. That is a GN label, not a
        # compiler, and it made this gate report a foreign compiler for a build
        # whose every real compiler was correct.
        #
        # A GN label is excluded by its syntax -- `//target:name` -- rather than
        # by its text: a colon does not appear in a compiler path any build
        # produces. Existence on disk is deliberately NOT the test. The case
        # this gate exists for is a poisoned object recorded by a compiler that
        # may since have been uninstalled, and skipping what no longer exists
        # would make the guard quietly stop guarding exactly then.
        case "$compiler" in
            *:*) continue ;;
        esac
        SEEN=$((SEEN + 1))
        COMPILERS["$compiler"]=1
        if [[ "$compiler" != "$OHOS_SDK_NATIVE"/* ]]; then
            fail "foreign compiler: $compiler"
            fail "  recorded in: ${output_file#"$REPO_ROOT"/}"
            FOREIGN=$((FOREIGN + 1))
        fi
    # The trailing component is NOT bare "clang": the SDK's drivers are named
    # after the Rust triple (x86_64-unknown-linux-ohos-clang), so a pattern
    # anchoring `clang` right after a slash matches nothing. That exact
    # mistake made an earlier revision of this guard report zero foreign
    # compilers while inspecting zero compilers -- which is why the
    # anti-vacuity check below is not optional.
    # /usr/bin/grep explicitly: the system `grep` here is ugrep, whose flag
    # behaviour differs.
    done < <(/usr/bin/grep -oE '/[^ )"]*(clang|gcc|g\+\+)[^ )"]*' "$output_file" 2>/dev/null | sort -u)
done < <(find "$BUILD_DIR" -name output -type f 2>/dev/null)

# ---- 2. anti-vacuity: a guard that found nothing has not passed -------------
# If no compiler was recorded at all, this script proves nothing. That happens
# when the build had no C dependencies, when cc-rs changes its output format,
# or when the path layout moves -- all of which must be loud, because each one
# silently disables the check above.
if [[ $SEEN -eq 0 ]]; then
    fail "no compiler invocation found in any build output under $BUILD_DIR"
    fail "this guard cannot pass without evidence; build a crate with a C"
    fail "dependency (migo-io pulls sqlite3, migo-shared pulls zstd) and retry"
    exit 1
fi

info "compilers recorded ($SEEN invocation(s), ${#COMPILERS[@]} distinct):"
for c in "${!COMPILERS[@]}"; do
    info "  $c"
done

if [[ $FOREIGN -ne 0 ]]; then
    fail "$FOREIGN invocation(s) used a compiler outside the OpenHarmony SDK"
    fail "run: source <(bash scripts/dev-setup-ohos.sh | grep '^export')"
    fail "then remove $BUILD_DIR and rebuild -- cargo will not"
    fail "recompile C objects that are already present"
    exit 1
fi
pass "every recorded compiler is inside the OpenHarmony SDK"

# ---- 3. ABI cross-check: objects must reference musl, never bionic ----------
# Independent of who the build says it ran: musl and glibc expose
# __errno_location, bionic exposes __errno. An object referencing __errno
# was compiled against bionic headers no matter what the build log claims.
BIONIC=0
MUSL=0
while IFS= read -r obj; do
    if nm -u "$obj" 2>/dev/null | /usr/bin/grep -qw "__errno"; then
        fail "bionic errno ABI in ${obj#"$BUILD_DIR"/}"
        BIONIC=$((BIONIC + 1))
    fi
    if nm -u "$obj" 2>/dev/null | /usr/bin/grep -qw "__errno_location"; then
        MUSL=$((MUSL + 1))
    fi
done < <(find "$BUILD_DIR" -name '*.o' -type f 2>/dev/null)

if [[ $BIONIC -ne 0 ]]; then
    fail "$BIONIC object(s) carry the bionic errno ABI"
    exit 1
fi
if [[ $MUSL -eq 0 ]]; then
    info "note: no object referenced __errno_location; the ABI cross-check had"
    info "      nothing to look at (harmless, but it did not corroborate)"
else
    pass "$MUSL object(s) reference the musl/glibc errno ABI, none reference bionic's"
fi

pass "OpenHarmony toolchain contract holds for $TARGET"
