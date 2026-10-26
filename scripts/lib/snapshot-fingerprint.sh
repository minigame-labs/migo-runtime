#!/usr/bin/env bash
# Shared "snapshot input fingerprint" helpers — sourced by gen-snapshot.sh
# (via write-snapshot-manifest.sh) and check-snapshot-freshness.sh so both
# compute identical values.
#
# The fingerprint captures the inputs that make a snapshot valid/stale:
#   * js_sources_sha256  — every extension *.js on disk under runtime-v8. A
#       snapshot bakes the post-execution heap of these, and with a snapshot the
#       extension JS is NOT re-loaded from source at runtime, so changing JS
#       without regenerating silently runs the OLD code.
#   * deno_core_version  — deno_core (hence V8) version; a bump changes builtins
#       + the external-reference table, breaking the snapshot's V8 magic number.
#
# Schema v3 also binds the runtime kind (host versus Worker), fingerprints the
# named product feature set, every Rust source
# in runtime-v8 + snapshot-gen, both feature manifests, and Cargo.lock,
# conservatively covering op signatures/order, generator options, dependencies,
# and V8 flags.

SNAPSHOT_SCHEMA_VERSION=3

snapshot_valid_os() {
  case "$1" in
    android|linux|ohos|windows) return 0 ;;
    *) return 1 ;;
  esac
}

# (os, arch) -> Rust target triple, recorded in the manifest's target_triple
# field. Mirrors the match in engine/crates/runtime-v8/build.rs exactly: both
# must agree on which V8 archive a given (os, arch) snapshot is bound to, since
# a snapshot serializes a live V8 heap and only loads in the exact V8 that
# produced it.
snapshot_target_triple() {
  local os="$1" arch="$2"
  case "$os-$arch" in
    android-aarch64) echo "aarch64-linux-android" ;;
    android-x86_64)  echo "x86_64-linux-android" ;;
    linux-x86_64)    echo "x86_64-unknown-linux-gnu" ;;
    ohos-aarch64)    echo "aarch64-unknown-linux-ohos" ;;
    ohos-x86_64)     echo "x86_64-unknown-linux-ohos" ;;
    windows-x86_64)  echo "x86_64-pc-windows-msvc" ;;
    *) echo "unsupported os/arch combination: $os/$arch" >&2; return 1 ;;
  esac
}

# (os, arch) -> directory name under engine/third_party/rusty_v8/ holding that
# platform's V8 archive. Kept as its own function (rather than reusing the
# target triple) because the directory is a repo-local shorthand, not the
# literal triple: linux/ohos drop "unknown", android has no "unknown" to drop.
snapshot_v8_target_dir() {
  local os="$1" arch="$2"
  case "$os-$arch" in
    android-aarch64) echo "aarch64-linux-android" ;;
    android-x86_64)  echo "x86_64-linux-android" ;;
    linux-x86_64)    echo "x86_64-linux-gnu" ;;
    ohos-aarch64)    echo "aarch64-linux-ohos" ;;
    ohos-x86_64)     echo "x86_64-linux-ohos" ;;
    windows-x86_64)  echo "x86_64-pc-windows-msvc" ;;
    *) echo "unsupported os/arch combination: $os/$arch" >&2; return 1 ;;
  esac
}

snapshot_validate_kind_profile() {
  local snapshot_kind="$1"
  local product_profile="$2"
  case "$snapshot_kind" in
    host|worker) ;;
    *)
      echo "invalid snapshot kind '$snapshot_kind' (expected host|worker)" >&2
      return 1
      ;;
  esac
  if [[ "$snapshot_kind" == "worker" && "$product_profile" != "full" ]]; then
    echo "Worker snapshot requires product profile full" >&2
    return 1
  fi
}

# Each kind/profile/OS snapshot set is a release unit spanning every arch that
# OS ships. An absent set stays optional, but once any arch of that OS exists,
# require freshness for the full set so a partial release candidate cannot
# pass a default scan. Android and OpenHarmony ship two arches; Linux and
# Windows ship x86_64 only (no arm64 Linux/Windows SDK exists).
snapshot_default_arches() {
  local _snapshot_kind="$1" os="$2"
  shift 2
  if [[ "$#" -gt 0 ]]; then
    case "$os" in
      android|ohos) printf '%s\n' aarch64 x86_64 ;;
      linux|windows) printf '%s\n' x86_64 ;;
      *) echo "unsupported os for default arch set: $os" >&2; return 1 ;;
    esac
  fi
}

# Hash identity of a materialized artifact or its Git LFS pointer. Freshness-only
# CI intentionally checks out without LFS; a valid pointer's oid and size are
# the identity of the materialized content, not of the pointer text.
snapshot_artifact_hash() {
  local artifact="$1"
  local size oid declared_size
  [[ -s "$artifact" ]] || {
    echo "snapshot artifact missing or empty: $artifact" >&2
    return 1
  }
  size="$(stat -c %s "$artifact")" || return 1
  if (( size >= 100000 )); then
    sha256sum "$artifact" | awk '{print $1}'
    return
  fi

  if [[ "$(sed -n '1p' "$artifact")" != \
        "version https://git-lfs.github.com/spec/v1" ]]; then
    echo "artifact is too small and is not a valid Git LFS pointer: $artifact" >&2
    return 1
  fi
  oid="$(sed -n 's/^oid sha256:\([0-9a-f]\{64\}\)$/\1/p' "$artifact")"
  declared_size="$(sed -n 's/^size \([0-9][0-9]*\)$/\1/p' "$artifact")"
  if [[ ! "$oid" =~ ^[0-9a-f]{64}$ || ! "$declared_size" =~ ^[0-9]+$ ]]; then
    echo "malformed Git LFS pointer: $artifact" >&2
    return 1
  fi
  printf '%s\n' "$oid"
}

snapshot_artifact_size() {
  local artifact="$1"
  local size declared_size
  [[ -s "$artifact" ]] || return 1
  size="$(stat -c %s "$artifact")" || return 1
  if (( size >= 100000 )); then
    printf '%s\n' "$size"
    return
  fi
  [[ "$(sed -n '1p' "$artifact")" == \
     "version https://git-lfs.github.com/spec/v1" ]] || return 1
  declared_size="$(sed -n 's/^size \([0-9][0-9]*\)$/\1/p' "$artifact")"
  [[ "$declared_size" =~ ^[0-9]+$ ]] || return 1
  printf '%s\n' "$declared_size"
}

snapshot_v8_archive_hash() {
  local artifact="$1"

  # The archive is a git-ignored build product fetched from a release asset, so
  # it is usually absent in a checkout. Its sha256 is recorded in the sibling
  # component-manifest.json -- the same value the release gate treats as the
  # authority on a valid archive -- so identity does not need the bytes. This
  # keeps the freshness gate a zero-download check, which is what it was when the
  # archive was an LFS pointer it could read the oid from.
  if [[ ! -s "$artifact" ]]; then
    local manifest="$(dirname "$artifact")/component-manifest.json"
    if [[ -f "$manifest" ]]; then
      local recorded
      recorded="$(python3 -c "
import json, sys
with open(sys.argv[1]) as f:
    print(json.load(f).get('hashes', {}).get('archive', ''))
" "$manifest" 2>/dev/null)"
      if [[ "$recorded" =~ ^[0-9a-f]{64}$ ]]; then
        printf '%s\n' "$recorded"
        return 0
      fi
    fi
    echo "V8 archive absent and no usable hash in $manifest" >&2
    return 1
  fi

  snapshot_artifact_hash "$artifact"
}

# Generation and native linking require real bytes. Unlike freshness, these
# paths must never accept an LFS pointer and write a manifest from it.
snapshot_require_materialized_artifact() {
  local artifact="$1"
  local kind="$2"
  local size
  [[ -s "$artifact" ]] || {
    echo "$kind missing or empty: $artifact" >&2
    return 1
  }
  size="$(stat -c %s "$artifact")" || return 1
  if (( size < 100000 )); then
    echo "$kind is only $size bytes (likely a Git LFS pointer): $artifact" >&2
    return 1
  fi
}

snapshot_require_materialized_v8_archive() {
  snapshot_require_materialized_artifact "$1" "V8 archive"
}

snapshot_require_materialized_snapshot() {
  snapshot_require_materialized_artifact "$1" "snapshot"
}

# sha256 of every extension *.js on disk under runtime-v8.
#
# This MUST stay byte-identical to the Rust helper
# engine/crates/runtime-v8/build_snapshot.rs (shared by build.rs and its
# regression test). Both:
#   * enumerate via a filesystem walk (NOT `git ls-files`), so it works in a
#     worktree without a .git directory and untracked/generated embedded JS also
#     invalidates the snapshot;
#   * order by the raw bytes of the repo-root-relative path (LC_ALL=C) — NOT the
#     component-wise order of git pathspecs or Rust's `Path` `Ord`, which differ
#     whenever a filename byte is below `/` (0x2f), e.g. `worker.js` vs
#     `worker/x.js`; and
#   * hash `"<sha256hex>  <relpath>\n"` lines (the sha256sum column format) with
#     an outer sha256.
# $1 = repo root
snapshot_js_hash() {
  local root="$1"
  ( set -o pipefail; cd "$root" && \
    { find engine/crates/runtime-v8 -type f -name '*.js' || exit 1; \
      if [[ -f engine/Cargo.toml ]]; then \
        local core_name core_version core_sha; \
        read -r core_name core_version core_sha < engine/third_party/deno-core-patches/UPSTREAM || exit 1; \
        [[ "$core_name" == deno_core && "$core_version" =~ ^[0-9.]+$ ]] || exit 1; \
        find "engine/third_party/$core_name-$core_version" -type f -name '*.js' || exit 1; \
      fi; } \
      | LC_ALL=C sort \
      | xargs -r -d '\n' sha256sum \
      | sha256sum | awk '{print $1}' )
}

snapshot_runtime_hash() {
  local root="$1"
  ( set -o pipefail; cd "$root" && \
    { find engine/crates/runtime-v8 engine/tools/snapshot-gen \
        -type f -name '*.rs' || exit 1; \
      printf '%s\n' \
        engine/crates/runtime-v8/Cargo.toml \
        engine/tools/snapshot-gen/Cargo.toml \
        engine/Cargo.lock || exit 1; \
      if [[ -f engine/Cargo.toml ]]; then \
        local core_name core_version core_sha; \
        read -r core_name core_version core_sha < engine/third_party/deno-core-patches/UPSTREAM || exit 1; \
        [[ "$core_name" == deno_core && "$core_version" =~ ^[0-9.]+$ ]] || exit 1; \
        find "engine/third_party/$core_name-$core_version" -type f -name '*.rs' || exit 1; \
        find engine/third_party/deno-core-patches -type f -name '*.patch' || exit 1; \
        printf '%s\n' engine/Cargo.toml "engine/third_party/$core_name-$core_version/Cargo.toml" \
          engine/third_party/deno-core-patches/UPSTREAM || exit 1; \
        local v8_name v8_version v8_sha; \
        read -r v8_name v8_version v8_sha < engine/third_party/rusty-v8-rust-patches/UPSTREAM || exit 1; \
        [[ "$v8_name" == v8 && "$v8_version" =~ ^[0-9.]+$ ]] || exit 1; \
        find "engine/third_party/$v8_name-$v8_version/src" \
          "engine/third_party/$v8_name-$v8_version/gen" -type f -name '*.rs' || exit 1; \
        find engine/third_party/rusty-v8-rust-patches -type f -name '*.patch' || exit 1; \
        printf '%s\n' "engine/third_party/$v8_name-$v8_version/Cargo.toml" \
          "engine/third_party/$v8_name-$v8_version/build.rs" \
          engine/third_party/rusty-v8-rust-patches/UPSTREAM || exit 1; \
      fi; } \
      | LC_ALL=C sort \
      | xargs -r -d '\n' sha256sum \
      | sha256sum | awk '{print $1}' )
}

snapshot_profile_features() {
  case "$1" in
    full)
      printf '%s\n' api-commerce api-connectivity api-media api-sensors \
        api-system code-signing profile-full v8-limits
      ;;
    slim)
      printf '%s\n' code-signing profile-slim v8-limits
      ;;
    *)
      echo "unsupported snapshot product profile: $1" >&2
      return 1
      ;;
  esac
}

snapshot_feature_hash() {
  snapshot_profile_features "$1" | LC_ALL=C sort | sha256sum | awk '{print $1}'
}

snapshot_profile_features_json() {
  case "$1" in
    full)
      printf '%s' '["api-commerce","api-connectivity","api-media","api-sensors","api-system","code-signing","profile-full","v8-limits"]'
      ;;
    slim)
      printf '%s' '["code-signing","profile-slim","v8-limits"]'
      ;;
    *)
      echo "unsupported snapshot product profile: $1" >&2
      return 1
      ;;
  esac
}

# deno_core version from Cargo.lock. $1 = engine dir
snapshot_deno_core_version() {
  local engine="$1"
  awk '/^name = "deno_core"$/{getline; gsub(/[",]/,"",$3); print $3; exit}' "$engine/Cargo.lock"
}
