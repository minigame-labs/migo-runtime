#!/usr/bin/env bash
# ============================================================
# Fetch prebuilt V8 archives for one or more targets and verify them.
# Location: scripts/fetch-v8-archives.sh
#
# These are build products of scripts/build-v8-{android,linux}.sh. They used to
# be tracked in Git LFS, which made every CI checkout spend LFS bandwidth on
# ~247 MB and eventually exhausted the repository's quota -- at which point
# checkout failed and nothing could build, for a reason that had nothing to do
# with the code. Release assets do not count against that quota, so the archives
# live on a release and are pulled here instead.
#
# Integrity does not depend on the transport. Each archive is checked against the
# `hashes.archive` sha256 in the committed component-manifest.json, which the
# release gate already treats as the authority on what a valid archive is. That
# is a stronger statement than LFS made: LFS guaranteed the bytes matched an
# object id, but nothing tied that object to the manifest.
#
# A target with no committed manifest cannot be fetched, by design: without one
# there is nothing to say what a valid archive is, and downloading bytes that
# cannot be checked would make this a plain transport wearing a verification
# comment.
#
# `aarch64-linux-ohos` was absent for that same reason and no longer is: it was
# built and sealed on 2026-08-10 (component_id 2a85cc63…), so it now has
# something to be verified against and is listed. Note what it means for the two
# modes, which differ here: `--check` verifies the local archive against its
# committed manifest and passes, while a *download* of this target has nowhere to
# come from until the archive is published as a release asset. That is a true
# report of the state rather than a gap -- the fetch fails naming the missing
# asset instead of the target being silently unknown.
#
# `x86_64-pc-windows-msvc` is the same shape with one difference: the build
# yields a DLL plus its import library, not a single .a, so it is not
# `librusty_v8-<target>.a`. Only the primary link-time artifact (the import
# library, hashes.archive in the component manifest -- see
# write-windows-v8-component-manifest.py's comment on why the schema does not
# widen to cover the DLL) is hash-verified here; the DLL is fetched alongside it
# because a Windows build cannot link or ship without it, verified in turn when
# scripts/build-windows-sdk.sh stages it into the package manifest.
#
# Usage: bash scripts/fetch-v8-archives.sh [--check] [--all] [target...]
#   --check      verify what is already present; download nothing
#   --all        every target known to this script
#   target...    one or more of: aarch64-linux-android, x86_64-linux-android,
#                x86_64-linux-gnu, x86_64-linux-ohos, aarch64-linux-ohos,
#                x86_64-pc-windows-msvc
#
# With no target given, the two Android targets are fetched. That is what the
# Android build and CI need, and keeping it the default means adding a target
# here never silently makes those builds download more than they use.
# ============================================================
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
# shellcheck source=scripts/lib/python-cmd.sh
source "$SCRIPT_DIR/lib/python-cmd.sh"
V8_DIR="$ROOT/engine/third_party/rusty_v8"
# The release is named after the rusty_v8 revision the native locks pin -- the
# four agree, and scripts/lib/v8_cohort.py holds them to it -- so moving the
# cohort moves the release with it. A tag written here instead stayed on 145's
# release when the locks moved to 150.4, and every archive fetched was a 145 one
# failing its 150 manifest's hash.
cohort_release() {
    "$(python_cmd)" -c '
import json, sys
print("v8-archives-" + json.load(open(sys.argv[1]))["rusty_v8_revision"][:7])
' "$ROOT/contracts/artifact-manifest/android-v8.lock.json"
}
TAG="${MIGO_V8_ARCHIVE_TAG:-$(cohort_release)}"
# Overridable so the archives can be served from somewhere else (a mirror, or a
# fork's own release) without editing this script.
BASE_URL="${MIGO_V8_ARCHIVE_BASE_URL:-https://github.com/minigame-labs/migo/releases/download/$TAG}"

# Targets this script knows how to fetch and verify. The directory name doubles
# as the asset suffix for every target but Windows (see above), so a release
# asset is otherwise always librusty_v8-<target>.a and there is no second naming
# scheme to keep in sync.
#
# The name is the Rust triple with `unknown-` removed, which is why
# `x86_64-linux-gnu` and `aarch64-apple-darwin` are both spelled the way they
# are: the first has an `unknown` vendor to drop and the second does not.
#
# The two darwin entries have no committed manifest yet, and that is a working
# state rather than a half-wire: this script refuses a target whose
# component-manifest.json is absent, which is exactly the fail-closed behaviour
# the header above argues for. They are listed so that the moment the macOS lane
# publishes archives there is one place already naming them, and
# scripts/test-apple-v8-pin-contract.sh asserts this list and
# contracts/artifact-manifest/apple-v8.lock.json agree about which triples exist.
KNOWN_TARGETS=(aarch64-linux-android x86_64-linux-android x86_64-linux-gnu aarch64-linux-gnu x86_64-linux-ohos aarch64-linux-ohos aarch64-apple-darwin x86_64-apple-darwin x86_64-pc-windows-msvc aarch64-pc-windows-msvc)
DEFAULT_TARGETS=(aarch64-linux-android x86_64-linux-android)

info() { printf '\033[0;36m[v8-fetch] %s\033[0m\n' "$*"; }
ok()   { printf '\033[0;32m[v8-fetch] %s\033[0m\n' "$*"; }
err()  { printf '\033[0;31m[v8-fetch] %s\033[0m\n' "$*" >&2; }

CHECK_ONLY=0
requested=()
for arg in "$@"; do
    case "$arg" in
        --check) CHECK_ONLY=1 ;;
        --all)   requested+=("${KNOWN_TARGETS[@]}") ;;
        -*)
            err "unknown option: $arg"
            err "usage: fetch-v8-archives.sh [--check] [--all] [target...]"
            exit 2
            ;;
        *)
            match=0
            for known in "${KNOWN_TARGETS[@]}"; do
                [[ "$arg" == "$known" ]] && match=1 && break
            done
            if (( match == 0 )); then
                err "unknown target: $arg"
                err "known targets: ${KNOWN_TARGETS[*]}"
                exit 2
            fi
            requested+=("$arg")
            ;;
    esac
done
(( ${#requested[@]} == 0 )) && requested=("${DEFAULT_TARGETS[@]}")

# De-duplicate while preserving order, so `--all x86_64-linux-android` does not fetch twice.
targets=()
for t in "${requested[@]}"; do
    seen=0
    for u in ${targets[@]+"${targets[@]}"}; do
        [[ "$t" == "$u" ]] && seen=1 && break
    done
    (( seen == 0 )) && targets+=("$t")
done

expected_sha() {
    "$(python_cmd)" -c "
import json, sys
with open(sys.argv[1]) as f:
    print(json.load(f).get('hashes', {}).get('archive', ''))
" "$1"
}

# (target) -> primary archive's local filename. Every target but Windows uses
# the same "librusty_v8.a" name the directory-derived asset name is built from;
# every *-pc-windows-msvc target's primary build product is the static
# rusty_v8.lib -- hashes.archive in its component manifest is computed from
# exactly that file (see write-windows-v8-component-manifest.py). Matched by
# pattern, not the one literal x86_64-pc-windows-msvc: a per-arch case arm here
# is exactly the bug that shipped aarch64-pc-windows-msvc with Unix naming and
# a 404 (fetch-v8-archives.sh tried librusty_v8-aarch64-pc-windows-msvc.a,
# which was never a name this project's Windows archives used).
primary_filename() {
    case "$1" in
        *-pc-windows-msvc) echo "rusty_v8.lib" ;;
        *) echo "librusty_v8.a" ;;
    esac
}
primary_asset_name() {
    case "$1" in
        *-pc-windows-msvc) echo "rusty_v8-$1.lib" ;;
        *) echo "librusty_v8-$1.a" ;;
    esac
}
# Windows cannot link or ship without its DLL and the import library the
# linker resolves against it -- neither is covered by hashes.archive (a
# deliberate schema choice: see write-windows-v8-component-manifest.py's
# comment). They are fetched here because there is nowhere else a machine
# without an MSVC host could get them, and verified in turn when
# scripts/build-windows-sdk.sh stages them into the package manifest that
# records every shipped file's hash.
companion_filenames() {
    case "$1" in
        *-pc-windows-msvc) printf '%s\n' rusty_v8.dll rusty_v8.dll.lib ;;
    esac
}
companion_asset_name() {
    local target="$1" filename="$2"
    # "rusty_v8.dll" -> "rusty_v8-<target>.dll", "rusty_v8.dll.lib" ->
    # "rusty_v8-<target>.dll.lib": the target goes between the "rusty_v8" stem
    # and its extension, matching the primary asset's own "rusty_v8-<target>.lib"
    # naming rather than appending the local filename whole (which would
    # duplicate the "rusty_v8" stem).
    echo "${filename/rusty_v8./rusty_v8-$target.}"
}

fetch_verified() {
    local target="$1" path="$2" asset="$3" want="$4"
    if [[ -f "$path" ]]; then
        local have
        have="$(sha256sum "$path" | cut -d' ' -f1)"
        if [[ "$have" == "$want" ]]; then
            ok "$target: $(basename "$path") already present and matches the manifest"
            return 0
        fi
        # A wrong archive is worse than a missing one: it links and produces a
        # binary whose provenance chain is a lie. Replace it, never keep it.
        err "$target: $(basename "$path") present but does NOT match the manifest -- refetching"
        rm -f "$path"
    fi

    if [[ "$CHECK_ONLY" == "1" ]]; then
        err "$target: $(basename "$path") missing or mismatched (checking only, nothing downloaded)"
        return 1
    fi

    mkdir -p "$(dirname "$path")"
    info "$target: downloading $asset"
    if ! curl -fsSL --retry 3 --retry-delay 2 -o "$path" "$BASE_URL/$asset"; then
        err "$target: download failed from $BASE_URL/$asset"
        rm -f "$path"
        return 1
    fi

    local have
    have="$(sha256sum "$path" | cut -d' ' -f1)"
    if [[ "$have" != "$want" ]]; then
        err "$target: $(basename "$path") sha256 mismatch after download"
        err "  expected $want"
        err "  got      $have"
        # Leaving it on disk would let a later step pick it up.
        rm -f "$path"
        return 1
    fi
    ok "$target: $(basename "$path") downloaded and verified against the manifest"
}

fetch_unverified_companion() {
    local target="$1" path="$2" asset="$3"
    if [[ -f "$path" ]]; then
        ok "$target: $(basename "$path") already present"
        return 0
    fi
    if [[ "$CHECK_ONLY" == "1" ]]; then
        err "$target: $(basename "$path") missing (checking only, nothing downloaded)"
        return 1
    fi
    mkdir -p "$(dirname "$path")"
    info "$target: downloading $asset"
    if ! curl -fsSL --retry 3 --retry-delay 2 -o "$path" "$BASE_URL/$asset"; then
        err "$target: download failed from $BASE_URL/$asset"
        rm -f "$path"
        return 1
    fi
    ok "$target: $(basename "$path") downloaded"
}

failures=0
for target in "${targets[@]}"; do
    manifest="$V8_DIR/$target/component-manifest.json"

    if [[ ! -f "$manifest" ]]; then
        err "no component manifest for $target -- cannot say what a valid archive is"
        failures=$((failures + 1)); continue
    fi
    want="$(expected_sha "$manifest")"
    if [[ -z "$want" ]]; then
        err "$target manifest records no archive hash"
        failures=$((failures + 1)); continue
    fi

    primary="$V8_DIR/$target/$(primary_filename "$target")"
    if ! fetch_verified "$target" "$primary" "$(primary_asset_name "$target")" "$want"; then
        failures=$((failures + 1)); continue
    fi

    companion_failed=0
    while IFS= read -r companion; do
        [[ -n "$companion" ]] || continue
        path="$V8_DIR/$target/$companion"
        asset="$(companion_asset_name "$target" "$companion")"
        fetch_unverified_companion "$target" "$path" "$asset" || companion_failed=1
    done < <(companion_filenames "$target")
    (( companion_failed == 0 )) || failures=$((failures + 1))
done

if (( failures > 0 )); then
    err "$failures archive(s) unavailable or unverified"
    exit 1
fi
ok "${#targets[@]} V8 archive(s) verified: ${targets[*]}"
