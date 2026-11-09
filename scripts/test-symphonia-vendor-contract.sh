#!/usr/bin/env bash
# scripts/test-symphonia-vendor-contract.sh
#
# Assert that every engine/third_party/symphonia-<crate>-<version> is the published crate plus the
# patches in engine/third_party/symphonia-<crate>-patches, applied in order, and nothing else; and
# that Cargo.lock builds that copy rather than the registry crate.
#
# WHY THIS EXISTS:
# The audio readers and decoders are vendored Symphonia with a few bug, bound and allocation fixes.
# A vendored copy rots without failing a build: an edit lands in the copy and not in a patch (the
# next upgrade re-extracts and silently drops it, or a reviewer reads a patch that is not what
# ships), or a patch is deleted while its change stays in the copy. The checker that used to replay
# these patches went with the step-driven decoders, which left the copies unverified.
#
# WHY IT CHECKS WHAT IT CHECKS:
# Each published .crate is pinned by sha256 in UPSTREAM, extracted, patched with `patch -p0`, and
# compared with the copy byte for byte. Then the comparison runs against two broken copies (one byte
# changed, one patch withheld) and must fail on both: a comparison that passes a tampered copy is not
# a comparison.
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
THIRD_PARTY="$REPO_ROOT/engine/third_party"
TAG='[symphonia-vendor]'

pass() { echo -e "\033[0;32m$TAG PASS $*\033[0m"; }
fail() { echo -e "\033[0;31m$TAG FAIL $*\033[0m" >&2; failures=$((failures + 1)); }
failures=0

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

# The published archive for "<name> <version> <sha256>", from Cargo's cache when present.
fetch_crate() {
    local name=$1 version=$2 sha=$3 out=$4 cached
    cached="$(ls "${CARGO_HOME:-$HOME/.cargo}"/registry/cache/*/"$name-$version.crate" 2>/dev/null | head -1)"
    if [[ -n "$cached" ]]; then
        cp "$cached" "$out"
    else
        curl --fail --silent --show-error --location --retry 3 \
            "https://static.crates.io/crates/$name/$name-$version.crate" -o "$out" || return 1
    fi
    [[ "$(sha256sum "$out" | cut -d' ' -f1)" == "$sha" ]]
}

# Rebuild <name>-<version> from its archive and patches under $2; patches withheld by name in $3.
rebuild() {
    local patches=$1 into=$2 withhold=${3:-} name version sha patch
    read -r name version sha < "$patches/UPSTREAM"
    mkdir -p "$into"
    fetch_crate "$name" "$version" "$sha" "$into/$name.crate" || { echo "archive $name $version"; return 1; }
    tar -xzf "$into/$name.crate" -C "$into" && rm "$into/$name.crate"
    for patch in "$patches"/*.patch; do
        [[ -n "$withhold" && "$(basename "$patch")" == "$withhold" ]] && continue
        (cd "$into" && patch -p0 --quiet --no-backup-if-mismatch < "$patch") || { echo "patch $(basename "$patch")"; return 1; }
    done
}

crates=()
for patches in "$THIRD_PARTY"/symphonia-*-patches; do
    [[ -f "$patches/UPSTREAM" ]] || continue
    read -r name version sha < "$patches/UPSTREAM"
    copy="$THIRD_PARTY/$name-$version"
    crates+=("$patches")
    if ! reason="$(rebuild "$patches" "$WORK/$name")"; then
        fail "$name $version: cannot rebuild from the published crate ($reason)"
        continue
    fi
    if diff -r "$WORK/$name/$name-$version" "$copy" > "$WORK/$name.diff"; then
        pass "$name $version is the published crate plus $(ls "$patches"/*.patch 2>/dev/null | wc -l) patch(es)"
    else
        fail "$name $version differs from its published crate plus patches:"
        head -20 "$WORK/$name.diff" >&2
    fi
    lock="$(awk -v n="$name" -v v="$version" '
        $0 == "[[package]]" { inpkg = 0 }
        $0 == "name = \"" n "\"" { inpkg = 1 }
        inpkg && $0 == "version = \"" v "\"" { found = 1 }
        inpkg && /^source = / { registry = 1 }
        END { print (found ? (registry ? "registry" : "path") : "missing") }' "$REPO_ROOT/engine/Cargo.lock")"
    [[ "$lock" == path ]] && pass "Cargo.lock builds the vendored $name" \
        || fail "Cargo.lock does not build the vendored $name ($lock)"
done
[[ ${#crates[@]} -gt 0 ]] || fail "no Symphonia patch sets found"

# The comparison must reject a tampered copy and a withheld patch.
probe="${crates[0]}"
read -r name version sha < "$probe/UPSTREAM"
rebuild "$probe" "$WORK/tampered" >/dev/null || fail "self-test rebuild"
file="$(find "$WORK/tampered/$name-$version/src" -name '*.rs' | sort | head -1)"
printf ' ' >> "$file"
diff -rq "$WORK/tampered/$name-$version" "$THIRD_PARTY/$name-$version" >/dev/null \
    && fail "self-test: a changed byte went unnoticed" || pass "self-test: a changed byte is caught"
first="$(basename "$(ls "$probe"/*.patch | head -1)")"
if rebuild "$probe" "$WORK/withheld" "$first" >/dev/null 2>&1; then
    diff -rq "$WORK/withheld/$name-$version" "$THIRD_PARTY/$name-$version" >/dev/null \
        && fail "self-test: a withheld patch went unnoticed" || pass "self-test: a withheld patch is caught"
else
    pass "self-test: a withheld patch is caught (later patches no longer apply)"
fi

if [[ $failures -ne 0 ]]; then
    echo -e "\033[0;31m$TAG $failures failure(s)\033[0m" >&2
    exit 1
fi
echo "Symphonia vendor contract: PASS (${#crates[@]} crates)"
