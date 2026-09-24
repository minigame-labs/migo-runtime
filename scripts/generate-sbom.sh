#!/usr/bin/env bash
# Generate a CycloneDX SBOM for one concrete release artifact.
#
# The artifact already exists when this runs. Its SHA-256, source revision,
# target matrix and product profile become part of the SBOM identity, while
# `cargo metadata --locked --offline --filter-platform` limits components to
# the graph that can reach the selected shipping crate.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT=""
ARTIFACT=""
ARTIFACT_KIND=""
TARGET_LABEL=""
CARGO_TARGET=""
PROFILE=""
ROOT_PACKAGE=""
MANIFEST=""
GRAPHS=()

usage() {
    cat <<'EOF'
usage: scripts/generate-sbom.sh \
  --artifact FILE --artifact-kind KIND \
  --target LABEL --cargo-target RUST-TRIPLE \
  --profile full|slim --root-package CARGO-PACKAGE \
  --manifest CARGO-TOML --out FILE

An artifact that carries more than one build of the root replaces
--cargo-target with one --graph per build:

  --graph RUST-TRIPLE=CARGO-FEATURES   (repeatable; features comma-separated,
                                        default features always off)

The SBOM then lists the union of those graphs. --profile is still the
artifact's product profile and is recorded, not used to pick features.
EOF
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --artifact) ARTIFACT="${2:?--artifact requires a path}"; shift 2 ;;
        --artifact=*) ARTIFACT="${1#*=}"; shift ;;
        --artifact-kind) ARTIFACT_KIND="${2:?--artifact-kind requires a value}"; shift 2 ;;
        --artifact-kind=*) ARTIFACT_KIND="${1#*=}"; shift ;;
        --target) TARGET_LABEL="${2:?--target requires a label}"; shift 2 ;;
        --target=*) TARGET_LABEL="${1#*=}"; shift ;;
        --cargo-target) CARGO_TARGET="${2:?--cargo-target requires a triple}"; shift 2 ;;
        --cargo-target=*) CARGO_TARGET="${1#*=}"; shift ;;
        --profile) PROFILE="${2:?--profile requires full or slim}"; shift 2 ;;
        --profile=*) PROFILE="${1#*=}"; shift ;;
        --root-package) ROOT_PACKAGE="${2:?--root-package requires a name}"; shift 2 ;;
        --root-package=*) ROOT_PACKAGE="${1#*=}"; shift ;;
        --graph) GRAPHS+=("${2:?--graph requires TRIPLE=FEATURES}"); shift 2 ;;
        --graph=*) GRAPHS+=("${1#*=}"); shift ;;
        --manifest) MANIFEST="${2:?--manifest requires a path}"; shift 2 ;;
        --manifest=*) MANIFEST="${1#*=}"; shift ;;
        --out) OUT="${2:?--out requires a path}"; shift 2 ;;
        --out=*) OUT="${1#*=}"; shift ;;
        -h|--help) usage; exit 0 ;;
        *) echo "ERROR: unknown argument: $1" >&2; usage >&2; exit 2 ;;
    esac
done

if [[ ${#GRAPHS[@]} -gt 0 && -n "$CARGO_TARGET" ]]; then
    echo "ERROR: --graph replaces --cargo-target; pass one or the other" >&2
    exit 2
fi
if [[ ${#GRAPHS[@]} -eq 0 ]]; then
    [[ -n "$CARGO_TARGET" ]] || { echo "ERROR: missing required cargo_target" >&2; usage >&2; exit 2; }
    GRAPHS=("$CARGO_TARGET=profile-$PROFILE")
fi
for value in ARTIFACT ARTIFACT_KIND TARGET_LABEL PROFILE ROOT_PACKAGE MANIFEST OUT; do
    if [[ -z "${!value}" ]]; then
        echo "ERROR: missing required ${value,,}" >&2
        usage >&2
        exit 2
    fi
done
if [[ "$PROFILE" != "full" && "$PROFILE" != "slim" ]]; then
    echo "ERROR: --profile must be full or slim" >&2
    exit 2
fi

case "$ARTIFACT" in /*) ;; *) ARTIFACT="$ROOT/$ARTIFACT" ;; esac
case "$MANIFEST" in /*) ;; *) MANIFEST="$ROOT/$MANIFEST" ;; esac
case "$OUT" in /*) ;; *) OUT="$ROOT/$OUT" ;; esac

if [[ ! -f "$ARTIFACT" ]]; then
    echo "ERROR: artifact does not exist: $ARTIFACT" >&2
    exit 1
fi
if [[ ! -f "$MANIFEST" ]]; then
    echo "ERROR: Cargo manifest does not exist: $MANIFEST" >&2
    exit 1
fi

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

metadata_args=()
index=0
for graph in "${GRAPHS[@]}"; do
    triple="${graph%%=*}"
    features="${graph#*=}"
    if [[ "$graph" != *=* || -z "$triple" || -z "$features" ]]; then
        echo "ERROR: --graph takes RUST-TRIPLE=FEATURES; got '$graph'" >&2
        exit 2
    fi
    index=$((index + 1))
    cargo metadata \
        --manifest-path "$MANIFEST" \
        --format-version 1 \
        --locked \
        --offline \
        --filter-platform "$triple" \
        --no-default-features \
        --features "$features" \
        > "$work/metadata-$index.json"
    metadata_args+=(--metadata "$work/metadata-$index.json")
    # What this build compiles, with its own features: the metadata above is
    # feature-unified across the workspace and over-reports. See compiled_subset.
    cargo tree \
        --manifest-path "$MANIFEST" \
        --locked \
        --offline \
        -p "$ROOT_PACKAGE" \
        --target "$triple" \
        --no-default-features \
        --features "$features" \
        -e normal,build \
        --prefix none \
        --format '{p}' \
        --color never \
        > "$work/tree-$index.txt"
    metadata_args+=(--compiled "$work/tree-$index.txt")
done

python3 "$ROOT/scripts/generate-sbom.py" \
    "${metadata_args[@]}" \
    --artifact "$ARTIFACT" \
    --artifact-kind "$ARTIFACT_KIND" \
    --target "$TARGET_LABEL" \
    --profile "$PROFILE" \
    --root-package "$ROOT_PACKAGE" \
    --policy "$ROOT/supply-chain.toml" \
    --workspace-root "$ROOT" \
    --out "$OUT"
