#!/usr/bin/env bash
# Turn a source-built Skia into the archive skia-bindings knows how to download.
#
# ## Why
#
# `scripts/apple-skia-gl-env.sh` makes every Apple build compile Skia from source,
# because the GN argument that makes Skia speak ES is not part of the binary-cache
# key and a successful download would silently discard it. That costs 8-9 minutes
# per (target x feature set) per job per RUN -- measured, and not amortised by
# `Swatinem/rust-cache`, which prunes large files out of build-script output
# directories.
#
# The way to get the speed back without giving up the correction is to publish
# archives built WITH the argument and point `SKIA_BINARIES_URL` at them. This
# produces one. It is the same shape as the pinned ANGLE and V8 archives: built
# once, published, verified on fetch.
#
# ## What it will not do
#
# It refuses to package a downloaded archive: packaging that would republish
# upstream's bytes under our name, which is a supply chain lie even when the
# bytes are fine.
#
# The discriminator is ninja's object tree, `out/skia/obj`, which a download
# never creates -- NOT the presence of `key.txt`, which was the obvious choice
# and is wrong. Cargo reuses one OUT_DIR per fingerprint, so a directory that
# was downloaded into and later built from source holds both, and a real one on
# this machine did. The libraries are also required to be no older than
# `build.ninja.stamp`, which catches the reverse order: a source build that a
# later download overwrote the libraries of.
#
# And it refuses a build whose `args.gn` does not carry `skia_gl_standard = ""`,
# because an archive without it is the defect this whole exercise exists to stop
# shipping.
#
# ## The key
#
# `skia-binaries-<key>.tar.gz`, where the key is
#
#     <rust-skia commit, first 20 hex>-<target triple>-<feature ids>
#
# The first two are derived here. The feature ids are NOT: skia-bindings maps
# cargo feature names through its own replacement table (`jpeg-decode` becomes
# `jpegd`), and a shell reimplementation of that table would be a third copy of
# a naming rule. Pass them with --features, taking them from the `key.txt` of
# any downloaded archive with the same cargo features, or from the build's own
#
#     TRYING TO DOWNLOAD AND INSTALL SKIA BINARIES: <tag>/<key>
#
# line. What is checked is that the key you supply begins with the prefix this
# script derives, which catches the mistakes that actually happen: the wrong
# triple, and a skia-bindings version that has moved.
#
# ## Verified end to end, with no network and nothing published
#
# 2026-09-11 on an Intel Mac, x86_64-apple-darwin: this script packaged a
# source build into a 17 MB archive; `SKIA_BINARIES_URL` was pointed at it with
# `file://`, `FORCE_SKIA_BUILD` and `SKIA_GN_ARGS` unset, and every
# skia-bindings output directory deleted. The rebuild took about three minutes
# against 8m42s for a source build, and the resulting directory holds `key.txt`
# with this script's key and NO `obj/` -- unpacked, not built. On that Skia,
# `skia_builds_a_gl_context_on_this_platforms_angle` passes, which is what says
# the GN correction survived into the archive.
#
# Publishing is then the same thing with an https URL.
#
# Host-only, macOS or Linux: reads a build tree and writes a tarball.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

usage() {
    cat >&2 <<'USAGE'
usage: package-apple-skia-binaries.sh --target <triple> --features <ids> [options]

  --target <triple>     e.g. aarch64-apple-darwin
  --features <ids>      the feature part of the key, e.g. gl-jpegd-jpege-pdf-textlayout
  --profile <name>      cargo profile directory (default: debug)
  --out <dir>           where to write the archive (default: engine/target/skia-binaries)
  --target-dir <dir>    cargo target directory (default: engine/target)
USAGE
    exit 2
}

TARGET=""
FEATURES=""
PROFILE="debug"
OUT_DIR=""
TARGET_DIR=""

while [[ $# -gt 0 ]]; do
    case "$1" in
        --target) TARGET="${2:-}"; shift 2 ;;
        --features) FEATURES="${2:-}"; shift 2 ;;
        --profile) PROFILE="${2:-}"; shift 2 ;;
        --out) OUT_DIR="${2:-}"; shift 2 ;;
        --target-dir) TARGET_DIR="${2:-}"; shift 2 ;;
        -h|--help) usage ;;
        *) echo "unknown argument: $1" >&2; usage ;;
    esac
done

[[ -n "$TARGET" ]] || { echo "--target is required" >&2; usage; }
[[ -n "$FEATURES" ]] || { echo "--features is required" >&2; usage; }
TARGET_DIR="${TARGET_DIR:-$ROOT/engine/target}"
OUT_DIR="${OUT_DIR:-$TARGET_DIR/skia-binaries}"

fail() { echo "[skia-package] FAIL $*" >&2; exit 1; }
info() { echo "[skia-package] $*"; }

# --- the rust-skia commit the crate was published from -----------------------
#
# `.cargo_vcs_info.json` is written by `cargo package` and is what skia-bindings
# itself reads (`cargo::crate_repository_hash`) to build the key it looks for.
# Deriving it the same way is what makes the archive findable.
VCS_INFO="$(find "${CARGO_HOME:-$HOME/.cargo}/registry/src" -maxdepth 2 \
    -type d -name 'skia-bindings-*' -print 2>/dev/null | sort | tail -1)/.cargo_vcs_info.json"
[[ -f "$VCS_INFO" ]] || fail "no .cargo_vcs_info.json under the cargo registry; is skia-bindings vendored differently?"
SHA="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["git"]["sha1"])' "$VCS_INFO")"
SHORT="${SHA:0:20}"
info "rust-skia commit      $SHORT (from $(basename "$(dirname "$VCS_INFO")"))"

EXPECTED_PREFIX="$SHORT-$TARGET-"
KEY="$EXPECTED_PREFIX$FEATURES"
info "archive key           $KEY"

# --- find the build, and insist it is a source build -------------------------
BUILD_ROOT="$TARGET_DIR/$TARGET/$PROFILE/build"
[[ -d "$BUILD_ROOT" ]] || fail "no build directory at $BUILD_ROOT; build for $TARGET first"

SOURCE_BUILT=""
DOWNLOADED=""
while IFS= read -r candidate; do
    if [[ -d "$candidate/obj" && -f "$candidate/args.gn" ]]; then
        SOURCE_BUILT="$candidate"
    elif [[ -f "$candidate/key.txt" ]]; then
        DOWNLOADED="$candidate"
    fi
done < <(find "$BUILD_ROOT" -maxdepth 3 -type d -name skia -path '*/skia-bindings-*/out/skia' 2>/dev/null | sort)

if [[ -z "$SOURCE_BUILT" ]]; then
    if [[ -n "$DOWNLOADED" ]]; then
        fail "the only Skia under $BUILD_ROOT was DOWNLOADED ($DOWNLOADED has no ninja object tree).
      Packaging that would republish upstream's bytes under our name. Build from source
      first: dot-source scripts/apple-skia-gl-env.sh, then cargo build --target $TARGET."
    fi
    fail "no Skia build under $BUILD_ROOT"
fi
info "source build          $SOURCE_BUILT"

# --- and insist it carries the correction -----------------------------------
if ! grep -qE 'skia_gl_standard *= *""' "$SOURCE_BUILT/args.gn"; then
    fail "$SOURCE_BUILT/args.gn does not set skia_gl_standard = \"\".
      An archive without it is the defect this exists to stop shipping: Skia would
      assume desktop GL and refuse every ANGLE context. See scripts/apple-skia-gl-env.sh."
fi
info "args.gn               carries skia_gl_standard=\"\""

# --- assemble ---------------------------------------------------------------
#
# The file list is what `BinariesConfiguration::export` writes and what
# `binaries::unpack` flattens back out: the built libraries, the generated
# bindings, and the licence. Named explicitly rather than copied wholesale so an
# archive cannot quietly grow a build artifact nobody meant to publish.
FILES=(
    LICENSE_SKIA
    bindings.rs
    libskia-bindings.a
    libskia.a
    libskparagraph.a
    libskshaper.a
    libskunicode_core.a
    libskunicode_icu.a
)

# Copied when the build produced it, required when it did not.
#
# `embed-icudtl` adds `icudtl.dat` to what skia-bindings exports, and it is NOT
# part of the binary-cache key -- the profile-full and engine-free builds in this
# workspace differ by that feature and ask for the same key, which is checkable:
# every downloaded archive on this machine carries the same
# `...-gl-jpegd-jpege-pdf-textlayout` suffix across both. So one archive serves
# both profiles, and the ones upstream publishes contain no `icudtl.dat` at all.
# A profile-full build links against them today and this repository ships that.
#
# Listed as optional rather than omitted because "the upstream archive does not
# have it" is an observation about upstream, not a rule about us: if a build here
# ever produces one, an archive that silently dropped it would fail at link time
# somewhere else entirely.
OPTIONAL_FILES=(icudtl.dat)

STAGE="$(mktemp -d)"
trap 'rm -rf "$STAGE"' EXIT
mkdir -p "$STAGE/skia-binaries"

# The staleness check applies to the ninja-built libraries and nothing else.
#
# It was written against every file in the list and turned red immediately -- on
# `LICENSE_SKIA`, which ninja does not produce: the build script copies it, so
# its mtime legitimately predates the last ninja run, as does `bindings.rs`,
# which bindgen writes. A guard that fires on the wrong file is not a strict
# guard, it is a broken one, and it would have been "fixed" by deleting it.
STAMP="$SOURCE_BUILT/build.ninja.stamp"
# `${FILES[@]+...}` rather than `"${FILES[@]}"`: under `set -u`, bash 3.2 --
# which is what macOS ships, and macOS is the only OS that can build an Apple
# product -- treats an empty array expansion as an unbound variable. These two
# are never empty today, and the guard costs nothing; the rule is what keeps a
# later edit from being a Mac-only failure.
for name in ${FILES[@]+"${FILES[@]}"}; do
    [[ -f "$SOURCE_BUILT/$name" ]] || fail "$SOURCE_BUILT has no $name; the build did not finish, or this skia-bindings emits a different set"
    # Older than the ninja stamp means something replaced it after the build --
    # in practice a download into the same reused OUT_DIR. The object-tree check
    # above catches a directory that was only ever downloaded into; this catches
    # the opposite order.
    if [[ "$name" == lib*.a && -f "$STAMP" && "$SOURCE_BUILT/$name" -ot "$STAMP" ]]; then
        fail "$SOURCE_BUILT/$name is older than build.ninja.stamp, so it is not what this ninja run produced.
      Wipe $(dirname "$(dirname "$SOURCE_BUILT")") and build from source again."
    fi
    cp "$SOURCE_BUILT/$name" "$STAGE/skia-binaries/$name"
done

for name in ${OPTIONAL_FILES[@]+"${OPTIONAL_FILES[@]}"}; do
    if [[ -f "$SOURCE_BUILT/$name" ]]; then
        cp "$SOURCE_BUILT/$name" "$STAGE/skia-binaries/$name"
        info "also packaged        $name"
    fi
done

# `tag.txt` is the skia-bindings crate version and `key.txt` the key above --
# both live inside the archive because that is where upstream puts them, and a
# consumer that ever learns to verify the key will find one.
CRATE_VERSION="$(basename "$(dirname "$VCS_INFO")" | sed 's/^skia-bindings-//')"
printf '%s' "$CRATE_VERSION" > "$STAGE/skia-binaries/tag.txt"
printf '%s' "$KEY" > "$STAGE/skia-binaries/key.txt"

mkdir -p "$OUT_DIR"
ARCHIVE="$OUT_DIR/skia-binaries-$KEY.tar.gz"
tar -czf "$ARCHIVE" -C "$STAGE" skia-binaries

info "tag                   $CRATE_VERSION"
info "wrote                 $ARCHIVE ($(du -h "$ARCHIVE" | cut -f1))"
if command -v shasum >/dev/null 2>&1; then
    info "sha256                $(shasum -a 256 "$ARCHIVE" | cut -d' ' -f1)"
elif command -v sha256sum >/dev/null 2>&1; then
    info "sha256                $(sha256sum "$ARCHIVE" | cut -d' ' -f1)"
fi
echo
echo "Verify it before publishing, with no network involved:"
echo "  export SKIA_BINARIES_URL='file://$OUT_DIR/skia-binaries-{key}.tar.gz'"
echo "  unset FORCE_SKIA_BUILD"
echo "  rm -rf $(dirname "$(dirname "$SOURCE_BUILT")")"
echo "  cargo test -p migo-platform --target $TARGET ... skia_builds_a_gl_context"
echo "The build must print UNPACKING ARCHIVE INTO and must NOT print 'ninja: Entering directory'."
