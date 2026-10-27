#!/usr/bin/env bash
# scripts/test-v8-gn-pin-contract.sh
#
# Assert that the V8 build refuses a gn that is not the pinned one.
#
# WHY THIS EXISTS (observed, not hypothetical):
# gn generates the entire build graph, so it is an input to every byte of
# librusty_v8.a. The Android build script used to resolve gn from V8_GN_PATH, then
# a prefetched path, then the system PATH, and merely *log* whichever it found.
# The generated component manifest recorded rustc, the compiler, the SDK and the
# linker, and omitted gn entirely — so two archives built with different gn
# revisions were indistinguishable in their own provenance. The gn in use here was
# additionally built from source with a local one-line change that existed nowhere
# in the repository.
#
# WHY IT CHECKS WHAT IT CHECKS:
# `gn --version` prints `<version> (<short-revision>)`, and neither half is an
# identity. Both come from the same `git describe HEAD` at gn build time, with no
# dirty marker, so a gn built from the pinned commit *without* the required patch
# reports exactly the same string as the intended binary. Checking the revision
# looks like it closes that and does not. The version string is only a cheap first
# filter; what enforces the patch set is the receipt scripts/build-gn.sh writes
# beside the installed binary, naming the patches it applied and binding them to
# that binary's own hash. The local change is a committed patch, and the gn
# checkout is additionally proved to be HEAD plus exactly the declared patches, so
# the pin describes a gn that can be reproduced rather than one that merely happens
# to be on this machine.
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
LIB="$SCRIPT_DIR/lib/gn-pin.sh"
LOCK="$REPO_ROOT/contracts/artifact-manifest/android-v8.lock.json"
GN_PATCH_DIR="$REPO_ROOT/engine/third_party/gn-patches"
TAG='[gn-pin]'

pass() { echo -e "\033[0;32m$TAG PASS $*\033[0m"; }
fail() { echo -e "\033[0;31m$TAG FAIL $*\033[0m" >&2; failures=$((failures + 1)); }
info() { echo -e "\033[0;36m$TAG $*\033[0m"; }
failures=0

[[ -f "$LIB" ]]  || { echo "$TAG missing library: $LIB" >&2; exit 1; }
[[ -f "$LOCK" ]] || { echo "$TAG missing lock: $LOCK" >&2; exit 1; }

# shellcheck source=scripts/lib/gn-pin.sh
source "$LIB"

info "the lock declares a complete gn pin"
if gn_pin_read "$LOCK"; then
    pass "gn pin reads: version $GN_PIN_VERSION revision $GN_PIN_REVISION"
else
    fail "the lock does not declare a readable gn pin"
    exit 1
fi
if (( ${#GN_PIN_REVISION} == 40 )); then
    pass "the pinned revision is a full 40-character sha"
else
    fail "the pinned revision is not a full sha: $GN_PIN_REVISION"
fi
if (( ${#GN_PIN_PATCHES[@]} > 0 )); then
    pass "the pin declares ${#GN_PIN_PATCHES[@]} required gn patch(es)"
else
    fail "the pin declares no required gn patches"
fi

info "every declared gn patch is committed"
for patch_id in "${GN_PIN_PATCHES[@]}"; do
    if [[ -f "$GN_PATCH_DIR/$patch_id.patch" ]]; then
        pass "committed: $patch_id.patch"
    else
        fail "declared but absent: $GN_PATCH_DIR/$patch_id.patch"
    fi
done

info "the version assertion accepts the pin and rejects everything else"
short="${GN_PIN_REVISION:0:12}"
check() { # description, expected(pass|fail), version-string
    local desc="$1" expect="$2" reported="$3" rc
    gn_pin_assert_version "$reported" >/dev/null 2>&1; rc=$?
    if { [[ "$expect" == pass && $rc -eq 0 ]] || [[ "$expect" == fail && $rc -ne 0 ]]; }; then
        pass "$desc"
    else
        fail "$desc (rc=$rc, wanted $expect) for '$reported'"
    fi
}
check "the pinned version and revision are accepted" pass "$GN_PIN_VERSION ($short)"
check "an older version is refused"                 fail "2175 ($short)"
check "a newer version is refused"                  fail "2600 ($short)"
check "a matching version with a foreign revision is refused" \
      fail "$GN_PIN_VERSION (deadbeefcafe)"
check "a truncated revision that is not a prefix is refused" \
      fail "$GN_PIN_VERSION (${short:0:6}999999)"
check "a version with no revision is refused"       fail "$GN_PIN_VERSION"
check "an empty revision is refused"                fail "$GN_PIN_VERSION ()"
check "a one-character revision is refused"         fail "$GN_PIN_VERSION (1)"
check "a non-hexadecimal revision is refused"       fail "$GN_PIN_VERSION (zzzzzzzzzzzz)"
check "an empty version string is refused"          fail ""
check "a non-numeric version is refused"            fail "banana ($short)"

info "the gn the build would use satisfies the pin"
prefetched="${RUSTY_V8_SRC:-$(cd "$REPO_ROOT/.." && pwd)/rusty_v8_src}/third_party/v8_correct_gn/gn"
if [[ -x "$prefetched" ]]; then
    reported="$($prefetched --version 2>/dev/null || true)"
    if gn_pin_assert_version "$reported"; then
        pass "the prefetched gn reports '$reported'"
    else
        fail "the prefetched gn reports '$reported', which the pin refuses"
    fi
    if gn_pin_assert_binary "$prefetched" "$GN_PATCH_DIR" >/dev/null 2>&1; then
        pass "the prefetched gn carries a receipt matching the pin"
    else
        fail "the prefetched gn has no receipt matching the pin"
    fi
else
    info "SKIP no prefetched gn at $prefetched (run scripts/build-gn.sh)"
fi

info "the receipt is what makes the patch set enforceable"
# gn --version cannot distinguish a gn built with the pinned patches from one
# built without them, so these fixtures work on the receipt, not the version.
receipt_dir="$(mktemp -d)"
printf '#!/bin/sh\necho "%s (%s)"\n' "$GN_PIN_VERSION" "$short" > "$receipt_dir/gn"
chmod +x "$receipt_dir/gn"
receipt_check() { # description, expected(pass|fail)
    local desc="$1" expect="$2" rc
    gn_pin_assert_binary "$receipt_dir/gn" "$GN_PATCH_DIR" >/dev/null 2>&1; rc=$?
    if { [[ "$expect" == pass && $rc -eq 0 ]] || [[ "$expect" == fail && $rc -ne 0 ]]; }; then
        pass "$desc"
    else
        fail "$desc (rc=$rc, wanted $expect)"
    fi
}
receipt_check "a gn reporting the pinned version but carrying no receipt is refused" fail
gn_pin_write_receipt "$receipt_dir/gn" "$GN_PATCH_DIR"
receipt_check "a gn with a receipt the builder wrote is accepted" pass

python3 - "$receipt_dir/gn-receipt.json" <<'PY'
import json, pathlib, sys
path = pathlib.Path(sys.argv[1])
receipt = json.loads(path.read_text())
receipt["patches"] = []
path.write_text(json.dumps(receipt, indent=2, sort_keys=True) + "\n")
PY
receipt_check "a receipt claiming no patches were applied is refused" fail

gn_pin_write_receipt "$receipt_dir/gn" "$GN_PATCH_DIR"
printf '#!/bin/sh\necho "%s (%s)"\n# tampered\n' "$GN_PIN_VERSION" "$short" > "$receipt_dir/gn"
chmod +x "$receipt_dir/gn"
receipt_check "a binary changed after its receipt was written is refused" fail

gn_pin_write_receipt "$receipt_dir/gn" "$GN_PATCH_DIR"
python3 - "$receipt_dir/gn-receipt.json" <<'PY'
import json, pathlib, sys
path = pathlib.Path(sys.argv[1])
receipt = json.loads(path.read_text())
receipt["gn_revision"] = "0" * 40
path.write_text(json.dumps(receipt, indent=2, sort_keys=True) + "\n")
PY
receipt_check "a receipt naming a different revision is refused" fail
rm -rf "$receipt_dir"

info "the gn checkout is HEAD plus exactly the declared gn patches"
# The synthetic fixtures for this live in test-v8-patch-application-contract.sh,
# alongside the function itself. What matters here is the real checkout: a gn
# built from a tree carrying an undeclared edit reports the pinned version, so
# this is the only thing that rules that out at the source.
# shellcheck source=scripts/lib/v8-patch-apply.sh
source "$SCRIPT_DIR/lib/v8-patch-apply.sh"
gn_src="${GN_SRC:-$(cd "$REPO_ROOT/.." && pwd)/gn}"
if [[ -d "$gn_src/.git" ]]; then
    declare -a gn_globs=()
    for patch_id in "${GN_PIN_PATCHES[@]}"; do gn_globs+=("$patch_id.patch"); done
    if v8_assert_tree_is_exactly_patched "$gn_src" "$GN_PATCH_DIR" "${gn_globs[@]}"; then
        pass "$gn_src is HEAD plus exactly the ${#gn_globs[@]} declared gn patch(es)"
    else
        fail "$gn_src carries changes the declared gn patches do not explain"
    fi
else
    info "SKIP no gn checkout at $gn_src"
fi

info "the build scripts enforce the pin rather than logging it"
for s in "$SCRIPT_DIR"/build-v8-android.sh; do
    name="$(basename "$s")"
    assert_line="$(grep -n 'gn_pin_assert_binary' "$s" | head -1 | cut -d: -f1)"
    # The invocation, whatever order its flags come in: `--target-dir` moved
    # ahead of `--release` and a pattern that spelled the old order found none.
    build_line="$(grep -nE '^cargo build( |$)' "$s" | head -1 | cut -d: -f1)"
    if [[ -n "$assert_line" ]]; then
        pass "$name asserts the gn pin"
    else
        fail "$name does not assert the gn pin"
    fi
    # Ordering, not just presence: an assertion that runs after the build has
    # already consumed gn cannot keep an unpinned gn out of the artifact.
    if [[ -z "$build_line" ]]; then
        fail "$name has no recognisable cargo build invocation to order against"
    elif [[ -n "$assert_line" ]] && (( assert_line < build_line )); then
        pass "$name asserts the pin before building (line $assert_line < $build_line)"
    else
        fail "$name asserts the pin at line $assert_line, after building at $build_line"
    fi
done

if (( failures == 0 )); then
    echo -e "\033[0;32m$TAG all checks passed\033[0m"
else
    echo -e "\033[0;31m$TAG $failures check(s) failed\033[0m" >&2
fi
exit $(( failures > 0 ))
