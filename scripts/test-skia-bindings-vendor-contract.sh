#!/usr/bin/env bash
# scripts/test-skia-bindings-vendor-contract.sh
#
# Assert that engine/third_party/skia-bindings-<version> is the published crate plus the patches in
# engine/third_party/skia-bindings-patches, nothing else, and that Cargo actually builds it.
#
# WHY THIS EXISTS:
# skia-bindings is vendored so Skia can link V8's ICU instead of bundling a second one (see
# scripts/lib/skia-external-icu.sh). A vendored crate rots two quiet ways, and neither fails a build:
#
#   - an edit lands in the copy and not in a patch, so the next upgrade -- re-extract, re-apply --
#     drops it without a word, or a reviewer reads a patch that is not what ships;
#   - skia-safe moves to a skia-bindings version the copy is not, Cargo answers `[patch]` with a
#     "patch was not used" warning, and the registry crate builds with its bundled ICU again: the
#     binary grows 1.36 MB and every test still passes.
#
# WHY IT CHECKS WHAT IT CHECKS:
# The published .crate is pinned by sha256, extracted, patched, and compared with the copy byte for
# byte; Cargo.lock must name skia-bindings at that version with no registry source, which is the
# lockfile's own way of saying the patch is in effect. Then the comparison is run again against two
# broken copies (one byte changed, one patch withheld) and must fail on both: a comparison that
# passes a tampered copy is not a comparison.
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
PATCH_DIR="$REPO_ROOT/engine/third_party/skia-bindings-patches"
TAG='[skia-bindings-vendor]'

pass() { echo -e "\033[0;32m$TAG PASS $*\033[0m"; }
fail() { echo -e "\033[0;31m$TAG FAIL $*\033[0m" >&2; failures=$((failures + 1)); }
failures=0

read -r name version sha256 < "$PATCH_DIR/UPSTREAM" || { echo "$TAG cannot read $PATCH_DIR/UPSTREAM" >&2; exit 1; }
VENDORED="$REPO_ROOT/engine/third_party/$name-$version"
[[ -d "$VENDORED" ]] || { echo "$TAG no vendored crate at $VENDORED" >&2; exit 1; }

scratch="$(mktemp -d)" || { echo "$TAG no writable temp directory" >&2; exit 1; }
trap 'rm -rf "$scratch"' EXIT

crate="$(find "${CARGO_HOME:-$HOME/.cargo}/registry/cache" -maxdepth 2 -name "$name-$version.crate" 2>/dev/null | head -1)"
if [[ -z "$crate" ]]; then
    crate="$scratch/$name-$version.crate"
    curl -fsSL "https://static.crates.io/crates/$name/$name-$version.crate" -o "$crate" \
        || { echo "$TAG cannot fetch $name-$version.crate" >&2; exit 1; }
fi
actual="$(sha256sum "$crate" | cut -d' ' -f1)"
if [[ "$actual" == "$sha256" ]]; then
    pass "$name-$version.crate matches the pinned sha256"
else
    fail "$name-$version.crate is $actual, UPSTREAM pins $sha256"
fi

# <tree to compare> <patch list...>: 0 when pristine + patches equals the tree.
pristine_plus_patches_is() {
    local tree="$1" work
    shift
    work="$(mktemp -d -p "$scratch")"
    tar xzf "$crate" -C "$work" || return 1
    local p
    for p in "$@"; do
        (cd "$work" && patch -s -p0 --forward < "$p") >/dev/null || return 1
    done
    # Only the top-level skia/ and depot_tools/ are excluded: they are what the build script downloads
    # into the crate. `diff --exclude=skia` would also skip build_support/skia/, which is where the
    # patch lands -- the comparison would have passed any edit to the patched files.
    diff <(tree_manifest "$work/$name-$version") <(tree_manifest "$tree") >/dev/null
}

tree_manifest() { # <tree>: one "sha256  path" line per file, sorted.
    (cd "$1" && find . \( -path ./skia -o -path ./depot_tools \) -prune -o -type f -print0 \
        | sort -z | xargs -0 sha256sum)
}

mapfile -t patches < <(find "$PATCH_DIR" -maxdepth 1 -name '*.patch' | sort)
if ((${#patches[@]} == 0)); then
    fail "no patches in $PATCH_DIR"
elif pristine_plus_patches_is "$VENDORED" "${patches[@]}"; then
    pass "$name-$version + ${#patches[@]} patch(es) == the vendored copy"
else
    fail "the vendored copy is not $name-$version + the patches in $PATCH_DIR"
fi

lock_entry="$(awk -v n="$name" '/^\[\[package\]\]/{p=0} $0=="name = \""n"\""{p=1} p' "$REPO_ROOT/engine/Cargo.lock")"
if grep -q "^version = \"$version\"" <<<"$lock_entry" && ! grep -q '^source = ' <<<"$lock_entry"; then
    pass "Cargo.lock builds $name $version from the vendored path"
else
    fail "Cargo.lock does not build $name $version from the vendored path (is the [patch] unused?)"
fi

# The lockfile says a path crate is built; the manifest says which path. Pointing the [patch] at another
# copy of the same version would leave Cargo.lock as it is and this gate checking a directory nobody builds.
patched="$(python3 -c 'import sys, tomllib
m = tomllib.load(open(sys.argv[1], "rb"))
print(m.get("patch", {}).get("crates-io", {}).get(sys.argv[2], {}).get("path", ""))' "$REPO_ROOT/engine/Cargo.toml" "$name")"
if [[ -n "$patched" && "$(realpath -m "$REPO_ROOT/engine/$patched")" == "$(realpath -m "$VENDORED")" ]]; then
    pass "engine/Cargo.toml patches $name to the verified copy"
else
    fail "engine/Cargo.toml patches $name to '${patched:-nothing}', not $VENDORED"
fi

# The comparison must be able to fail.
tampered="$scratch/tampered"
# The edit goes into the file the patch itself changes, the one place an exclusion could hide it.
# The copy takes tree_manifest's file list: bsdtar's `--exclude=./skia` also drops
# build_support/skia/, and a copy missing the file under test is refused for the wrong reason.
mkdir -p "$tampered" && (cd "$VENDORED" && find . \( -path ./skia -o -path ./depot_tools \) -prune \
    -o -type f -print | tar cf - -T -) | (cd "$tampered" && tar xf -) \
    && printf '\n' >> "$tampered/build_support/skia/env.rs" \
    || { echo "$TAG cannot build the tampered copy" >&2; exit 1; }
if pristine_plus_patches_is "$tampered" "${patches[@]}"; then
    fail "self-test: a copy with an unpatched edit compared equal"
else
    pass "self-test: a copy with an unpatched edit is refused"
fi
if ((${#patches[@]} > 0)) && pristine_plus_patches_is "$VENDORED" "${patches[@]:1}"; then
    fail "self-test: the copy compared equal with a patch withheld"
else
    pass "self-test: the copy is refused with a patch withheld"
fi

((failures == 0)) || { echo "$TAG $failures failure(s)" >&2; exit 1; }
