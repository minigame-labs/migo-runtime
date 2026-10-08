#!/usr/bin/env bash
# ============================================================
# Turn a staged SDK prefix into the release asset pair a tag publishes.
# Location: scripts/package-sdk.sh
#
# All four platform build scripts stage a prefix directory and stop there, so the
# `migo-sdk-<os>-<arch>.tar.gz` on every release so far was produced by a `tar`
# typed on the release machine. That is why the published Linux archive records
# the release user's own user/group as owner and the build machine's wall clock as every mtime: nobody can
# rebuild those bytes, and the `package_sha256` its attestation swears to is
# therefore unverifiable by the person receiving it.
#
# scripts/test-reproducible-timestamp-contract.sh exists to stop exactly this, and
# could not see it -- a hand-typed command is in no script for it to scan.
#
# The archive produced here is byte-identical for one commit: entries sorted, owner
# and group normalised to numeric 0, every mtime set from SOURCE_DATE_EPOCH, and
# gzip told not to store its own timestamp. SOURCE_DATE_EPOCH defaults to HEAD's
# commit time, which is derived from the source rather than read from a clock, so
# the default is already reproducible and an explicit value only pins it harder.
#
# Usage: bash scripts/package-sdk.sh <staged-prefix> [--output-dir DIR]
#   <staged-prefix>   e.g. dist/migo-linux-x86_64 (as staged by build-<os>-sdk.sh)
#   --output-dir DIR  where the asset pair goes (default: the prefix's parent)
# ============================================================
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
# shellcheck source=scripts/lib/windows-native-toolchain.sh
source "$SCRIPT_DIR/lib/windows-native-toolchain.sh"
# This script's own `cargo build` below (for tools/artifact-manifest) is
# subject to the same Git-Bash/MSVC link.exe shadowing on a native Windows
# runner as every other cargo invocation in this repo's Windows CI job --
# guarded on cl.exe being present so this is a no-op on every other platform,
# where it is neither needed nor safe to assume.
command -v cl.exe >/dev/null 2>&1 && windows_native_ensure_msvc_link_wins

info() { printf '\033[0;36m[package-sdk] %s\033[0m\n' "$*"; }
ok()   { printf '\033[0;32m[package-sdk] %s\033[0m\n' "$*"; }
err()  { printf '\033[0;31m[package-sdk] %s\033[0m\n' "$*" >&2; }

PREFIX=""
OUTPUT_DIR=""
while [[ $# -gt 0 ]]; do
    case "$1" in
        --output-dir) OUTPUT_DIR="$2"; shift 2 ;;
        -*)
            err "unknown option: $1"
            err "usage: package-sdk.sh <staged-prefix> [--output-dir DIR]"
            exit 2
            ;;
        *)
            if [[ -n "$PREFIX" ]]; then
                err "expected one staged prefix, got a second: $1"
                exit 2
            fi
            PREFIX="$1"; shift
            ;;
    esac
done

if [[ -z "$PREFIX" ]]; then
    err "usage: package-sdk.sh <staged-prefix> [--output-dir DIR]"
    exit 2
fi
if [[ ! -d "$PREFIX" ]]; then
    err "staged prefix is not a directory: $PREFIX"
    exit 1
fi

PREFIX="$(cd "$PREFIX" && pwd)"
PREFIX_PARENT="$(dirname "$PREFIX")"
PREFIX_NAME="$(basename "$PREFIX")"
: "${OUTPUT_DIR:=$PREFIX_PARENT}"

# shellcheck source=scripts/lib/release-version.sh
source "$ROOT/scripts/lib/release-version.sh"
VERSION="$(read_release_version "$ROOT")"

# The asset name is the staged prefix's own name with the product prefix replaced and
# the release version inserted (dist/migo-linux-x86_64 ->
# migo-0.9.1-capi-linux-x86_64.tar.gz). Deriving it means there is no second naming
# scheme to keep in step with the directory the build script chose.
#
# `capi` distinguishes these from the Android AAR, whose `.aar` extension already says
# "Android, Java/Kotlin" -- so that artifact carries no api segment while these must.
# The version is in the name because a downloaded file that has been renamed or moved
# off the release page is otherwise unidentifiable.
if [[ "$PREFIX_NAME" != migo-* ]]; then
    err "staged prefix is not named migo-<os>-<arch>: $PREFIX_NAME"
    err "the published asset name is derived from it, so an unrecognised name would"
    err "produce an asset no consumer is looking for"
    exit 1
fi
ASSET="migo-${VERSION}-capi-${PREFIX_NAME#migo-}.tar.gz"

# The index is the package manifest the platform's build script wrote. Exactly one,
# because the attestation names a single index and a tree carrying two would make
# the choice silent.
mapfile -t INDEX_CANDIDATES < <(find "$PREFIX/share/migo" -maxdepth 1 -name '*-manifest.json' -type f 2>/dev/null | sort)
if (( ${#INDEX_CANDIDATES[@]} == 0 )); then
    err "no package manifest under $PREFIX/share/migo"
    err "Every platform's build script writes exactly one there (Android/Linux/"
    err "OpenHarmony in build-<os>-sdk.sh, Windows via windows_sdk_write_manifest"
    err "in scripts/lib/windows-sdk-package.sh). A prefix without one was staged"
    err "by hand or by a build predating that step."
    exit 1
fi
if (( ${#INDEX_CANDIDATES[@]} > 1 )); then
    err "more than one package manifest under $PREFIX/share/migo:"
    printf '[package-sdk]   %s\n' "${INDEX_CANDIDATES[@]}" >&2
    exit 1
fi
INDEX="${INDEX_CANDIDATES[0]}"

EPOCH="${SOURCE_DATE_EPOCH:-$(git -C "$ROOT" log -1 --format=%ct)}"
if [[ ! "$EPOCH" =~ ^[0-9]+$ ]]; then
    err "SOURCE_DATE_EPOCH must be non-negative Unix seconds, got: $EPOCH"
    exit 1
fi

MANIFEST_TOOL="${MIGO_ARTIFACT_MANIFEST_TOOL:-}"
if [[ -z "$MANIFEST_TOOL" ]]; then
    MANIFEST_TOOL_TARGET="${MIGO_ARTIFACT_MANIFEST_TARGET_DIR:-$ROOT/tools/artifact-manifest/target}"
    CARGO_TARGET_DIR="$MANIFEST_TOOL_TARGET" cargo build \
        --manifest-path "$ROOT/tools/artifact-manifest/Cargo.toml" \
        --locked --release
    MANIFEST_TOOL="$MANIFEST_TOOL_TARGET/release/migo-artifact-manifest"
fi
[[ -x "$MANIFEST_TOOL" ]] || {
    err "artifact manifest tool is not executable: $MANIFEST_TOOL"
    exit 1
}

mkdir -p "$OUTPUT_DIR"
OUTPUT_DIR="$(cd "$OUTPUT_DIR" && pwd)"
PACKAGE="$OUTPUT_DIR/$ASSET"
ATTESTATION="$PACKAGE.attestation.json"

info "packaging $PREFIX_NAME -> $ASSET (SOURCE_DATE_EPOCH=$EPOCH)"
# --format=gnu rather than the default pax: pax writes extended headers carrying a
# second copy of the mtime at nanosecond precision, which --mtime does not flatten.
# gzip -n withholds the timestamp and original name gzip would otherwise store in
# its own header, where they are invisible to tar and still change the bytes.
#
# --mode normalises permissions, which --owner and --mtime do not reach. The staged
# tree is assembled with plain `cp` and `mkdir`, so its group and other bits carry
# the builder's umask: the same commit packaged under umask 002 and 022 would
# otherwise differ. `go=u,go-w` derives them from the owner bits instead, which
# keeps the one distinction that matters -- 0644 for a header, 0755 for a library
# or a directory -- while erasing the one that does not.
tar --sort=name \
    --format=gnu \
    --numeric-owner --owner=0 --group=0 \
    --mode='u+rw,go=u,go-w' \
    --mtime="@$EPOCH" \
    -C "$PREFIX_PARENT" -cf - "$PREFIX_NAME" \
    | gzip -9 -n > "$PACKAGE"

info "attesting $ASSET against $(basename "$INDEX")"
"$MANIFEST_TOOL" attest "$PACKAGE" "$INDEX" "$ATTESTATION" >/dev/null
"$MANIFEST_TOOL" verify-attestation "$ATTESTATION" "$PACKAGE" "$INDEX" >/dev/null

ok "$ASSET  $(stat -c %s "$PACKAGE") bytes  sha256=$(sha256sum "$PACKAGE" | cut -d' ' -f1)"
ok "$(basename "$ATTESTATION")"
