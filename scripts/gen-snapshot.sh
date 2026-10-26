#!/usr/bin/env bash
# =============================================================================
# Generate a V8 startup snapshot for one (os, arch) platform.
# =============================================================================
#
# V8 startup snapshots are PLATFORM-bound (OS + CPU arch): a snapshot serializes
# a live V8 heap, so it MUST be produced by the same os-<arch> V8 the shipping
# binary links. We therefore build `migo-snapshot-gen` for the target platform
# (linking that platform's committed V8 archive) and RUN it there — natively
# for Linux/Windows, on an emulator/device for Android/OpenHarmony (the Deno
# issue #27496 approach) — then write the result into
#   engine/crates/runtime-v8/snapshots/SNAPSHOT-<profile>-<os>-<arch>.bin
# which runtime-v8/build.rs embeds at .so/.a build time.
#
# Usage:
#   scripts/gen-snapshot.sh <x86_64|arm64|aarch64> [--os android|linux]
#                           [--product-profile full|slim]
#                           [--snapshot-kind host|worker]
#                           [--device SERIAL] [--keep]
#
#   --os android (default)  cross-compile and run on an emulator/real device
#                            (arm64 needs a real device: hosted CI has no arm64
#                            KVM; x86_64 can use a hardware-accelerated emulator)
#   --os linux               build and run natively (x86_64 only: no arm64
#                            Linux SDK exists in this repo)
#
#   --device SERIAL   adb serial to use for --os android (else: $ANDROID_SERIAL,
#                     else the only connected device, else error).
#   --keep            leave the pushed binary/lib on the device afterwards
#                     (--os android only).
#
# --os ohos and --os windows are not implemented here. Both now have a committed
# V8 archive under engine/third_party/rusty_v8/, so what is missing is a way to
# *run* the generator on them: a snapshot is platform-bound, so it has to be
# produced by the same platform's V8 -- OpenHarmony would need an hdc-reachable
# device or emulator, and Windows would need the generator run on a Windows host.
# Until then those targets ship no snapshot and build.rs falls back to loading
# extension JS from source, which is correct but a slower cold start. Both fail
# closed with a clear message rather than half-working.
#
# Environment:
#   ANDROID_NDK_HOME  an NDK to prefer; checked against the pin like any other
#                     candidate, and found in the standard SDK layouts when unset
#   ADB               adb path (default: ~/Android/Sdk/platform-tools/adb, then PATH)
#
# Output:
#   engine/crates/runtime-v8/snapshots/SNAPSHOT-<profile>-<os>-<arch>.bin
#   engine/crates/runtime-v8/snapshots/SNAPSHOT-<profile>-<os>-<arch>.bin.manifest.json
#
# After generating every ABI for a platform, build with embedding, e.g.:
#   bash scripts/build-aar.sh release arm64-v8a x86_64
# =============================================================================
set -euo pipefail

c_info()  { echo -e "\033[0;36m[INFO] $*\033[0m"; }
c_ok()    { echo -e "\033[0;32m[OK]   $*\033[0m"; }
c_err()   { echo -e "\033[0;31m[ERR]  $*\033[0m" >&2; }
die()     { c_err "$*"; exit 1; }

# ---- args -------------------------------------------------------------------
if [[ "${1:-}" == "-h" || "${1:-}" == "--help" ]]; then sed -n '2,45p' "$0"; exit 0; fi
ABI="${1:-}"; shift || true
SERIAL="${ANDROID_SERIAL:-}"
KEEP=0
PRODUCT_PROFILE="full"
SNAPSHOT_KIND="host"
OS="android"
while [[ $# -gt 0 ]]; do
  case "$1" in
    --device) SERIAL="${2:-}"; shift 2 ;;
    --product-profile) PRODUCT_PROFILE="${2:-}"; shift 2 ;;
    --product-profile=*) PRODUCT_PROFILE="${1#*=}"; shift ;;
    --snapshot-kind) SNAPSHOT_KIND="${2:-}"; shift 2 ;;
    --snapshot-kind=*) SNAPSHOT_KIND="${1#*=}"; shift ;;
    --os) OS="${2:-}"; shift 2 ;;
    --os=*) OS="${1#*=}"; shift ;;
    --keep)   KEEP=1; shift ;;
    -h|--help) sed -n '2,45p' "$0"; exit 0 ;;
    *) die "unknown arg: $1 (try --help)" ;;
  esac
done

case "$PRODUCT_PROFILE" in
  full|slim) ;;
  *) die "invalid --product-profile '$PRODUCT_PROFILE' (expected full|slim)" ;;
esac
case "$SNAPSHOT_KIND" in
  host|worker) ;;
  *) die "invalid snapshot kind '$SNAPSHOT_KIND' (expected host|worker)" ;;
esac
if [[ "$SNAPSHOT_KIND" == "worker" && "$PRODUCT_PROFILE" != "full" ]]; then
  die "Worker snapshot requires product profile full"
fi
# The slim profile ships on Android only -- `snapshots/README.md` says so, and
# `check-snapshot-freshness.sh` reports the Linux slim lane as legitimately
# absent. Refused here rather than left to the operator, because a stray
# `SNAPSHOT-slim-linux-x86_64.bin` is not an unused file: `build.rs` picks up
# whatever matches the target, so it would be embedded in the Linux slim product
# silently. Written after generating one twice by accident in one session.
if [[ "$OS" == "linux" && "$PRODUCT_PROFILE" == "slim" ]]; then
  die "there is no Linux slim snapshot: the slim product profile ships on Android only (snapshots/README.md)"
fi

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ENGINE="$ROOT/engine"
# shellcheck source=scripts/lib/cargo-target-dir.sh
source "$ROOT/scripts/lib/cargo-target-dir.sh"
OUT_DIR="$ENGINE/crates/runtime-v8/snapshots"
mkdir -p "$OUT_DIR"
# shellcheck source=scripts/lib/snapshot-fingerprint.sh
source "$ROOT/scripts/lib/snapshot-fingerprint.sh"
snapshot_valid_os "$OS" || die "invalid --os '$OS' (expected android|linux|ohos|windows)"

case "$OS" in
  ohos)    die "generation for --os ohos is not implemented in this script yet (needs an hdc-reachable device/emulator bridge)" ;;
  windows) die "generation for --os windows is not implemented yet: no V8 archive exists until Task 2 seals a component manifest" ;;
esac

case "$ABI" in
  x86_64) RUST_ARCH="x86_64" ;;
  arm64|arm64-v8a|aarch64) RUST_ARCH="aarch64" ;;
  *) die "usage: gen-snapshot.sh <x86_64|arm64> [--os android|linux] [--product-profile full|slim] [--device SERIAL] [--keep]" ;;
esac
V8_DIR="$(snapshot_v8_target_dir "$OS" "$RUST_ARCH")" || die "unsupported os/arch: $OS/$RUST_ARCH"

if [[ "$SNAPSHOT_KIND" == "host" ]]; then
  OUT="$OUT_DIR/SNAPSHOT-$PRODUCT_PROFILE-$OS-$RUST_ARCH.bin"
else
  OUT="$OUT_DIR/SNAPSHOT-worker-$PRODUCT_PROFILE-$OS-$RUST_ARCH.bin"
fi
TARGET_DIR="$(cargo_target_dir "$ENGINE")"

if [[ "$OS" == "linux" ]]; then
  [[ "$RUST_ARCH" == "x86_64" ]] || die "no linux V8 archive exists for arch '$RUST_ARCH' (x86_64 only)"

  # ---- native path: build and run in place, no device involved -------------
  # Materialised (hash-verified against component-manifest.json) rather than
  # scripts/lib/host-v8.sh's looser existence-based resolution: this path
  # produces the committed, shipped snapshot, not a local dev/test run, so it
  # is held to the same "verify before you link" rule as the Android path
  # below (see scripts/lib/v8-materialise.sh and
  # test-artifact-manifest-contract.sh's shipping-consumer enumeration).
  # shellcheck source=scripts/lib/v8-materialise.sh
  source "$(dirname "${BASH_SOURCE[0]}")/lib/v8-materialise.sh"
  if ! v8_materialise "$ENGINE/third_party/rusty_v8/$V8_DIR" "$TARGET_DIR/v8-materialised"; then
    die "cannot use the linux V8 archive for $V8_DIR (run: bash scripts/fetch-v8-archives.sh x86_64-linux-gnu)"
  fi
  c_info "V8 archive verified: ${V8_MATERIALISED_ARCHIVE#"$ROOT"/}"
  c_info "kind=$SNAPSHOT_KIND  profile=$PRODUCT_PROFILE  os=$OS  arch=$RUST_ARCH (native)"

  ( cd "$ENGINE"
    RUSTY_V8_ARCHIVE="$V8_MATERIALISED_ARCHIVE" \
    RUSTY_V8_SRC_BINDING_PATH="$V8_MATERIALISED_BINDING" \
    cargo build -p migo-snapshot-gen \
      --no-default-features --features "profile-$PRODUCT_PROFILE" --locked )

  BIN="$TARGET_DIR/debug/migo-snapshot-gen"
  [[ -f "$BIN" ]] || die "build produced no binary at $BIN"

  c_info "generating snapshot natively ..."
  MIGO_SNAPSHOT_KIND="$SNAPSHOT_KIND" MIGO_SNAPSHOT_OUT="$OUT" "$BIN"
  [[ -s "$OUT" ]] || die "generated snapshot is empty"

  bash "$ROOT/scripts/write-snapshot-manifest.sh" \
    "$PRODUCT_PROFILE" "$OS" "$RUST_ARCH" "$OUT" "$SNAPSHOT_KIND"

  c_ok "snapshot -> $OUT  ($(stat -c %s "$OUT") bytes)"
  c_ok "manifest -> $OUT.manifest.json"
  exit 0
fi

# ---- android path: cross-compile and run on device/emulator ----------------
case "$RUST_ARCH" in
  x86_64)
    NDK_TARGET="x86_64"; TRIPLE="x86_64-linux-android"
    LIBCXX_TRIPLE="x86_64-linux-android"; NEEDS_BUILTINS=0 ;;
  aarch64)
    NDK_TARGET="arm64-v8a"; TRIPLE="aarch64-linux-android"
    LIBCXX_TRIPLE="aarch64-linux-android"; NEEDS_BUILTINS=1 ;;
esac

# shellcheck source=scripts/lib/android-ndk.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib/android-ndk.sh"
# shellcheck source=scripts/lib/v8-materialise.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib/v8-materialise.sh"
android_ndk_read_pin "$ROOT/contracts/artifact-manifest/android-v8.lock.json" || exit 1
android_ndk_resolve || die "cannot resolve the pinned Android NDK"
NDK="$ANDROID_NDK_HOME"

# The archive is verified before adb is required, and that ordering is deliberate: this
# check is local and deterministic, while the device is neither, so failing on the cheap
# one first is what makes the archive handling observable without hardware.
#
# What a snapshot is makes this the strictest case in the tree rather than the loosest.
# A startup snapshot serialises a live V8 heap, so it is only valid for the exact V8 that
# produced it -- and the result is *committed* under engine/crates/runtime-v8/snapshots/
# and embedded by build.rs into every shipping Android .so. This used to check existence
# plus "larger than a megabyte", a heuristic for an unresolved LFS pointer, which cannot
# tell one real archive from another. The hash subsumes it: a stub fails the comparison
# for the same reason a wrong archive does.
if ! v8_materialise "$ENGINE/third_party/rusty_v8/$V8_DIR" "$TARGET_DIR/v8-materialised"; then
    die "cannot use the android V8 archive for $V8_DIR (run: bash scripts/fetch-v8-archives.sh)"
fi
c_info "V8 archive verified: ${V8_MATERIALISED_ARCHIVE#"$ROOT"/}"

ADB="${ADB:-$HOME/Android/Sdk/platform-tools/adb}"
[[ -x "$ADB" ]] || ADB="$(command -v adb || true)"
[[ -n "$ADB" && -x "$ADB" ]] || die "adb not found (set ADB=/path/to/adb)"

LIBCXX="$NDK/toolchains/llvm/prebuilt/linux-x86_64/sysroot/usr/lib/$LIBCXX_TRIPLE/libc++_shared.so"
[[ -f "$LIBCXX" ]] || die "libc++_shared.so not found: $LIBCXX"

# ---- device -----------------------------------------------------------------
if [[ -z "$SERIAL" ]]; then
  mapfile -t DEVS < <("$ADB" devices | awk 'NR>1 && $2=="device"{print $1}')
  [[ "${#DEVS[@]}" -eq 1 ]] || die "specify --device SERIAL (connected: ${DEVS[*]:-none})"
  SERIAL="${DEVS[0]}"
fi
ADBD=("$ADB" -s "$SERIAL")
c_info "kind=$SNAPSHOT_KIND  profile=$PRODUCT_PROFILE  os=$OS  arch=$RUST_ARCH  device=$SERIAL  NDK=$NDK"

# ---- 1. cross-compile snapshot-gen -----------------------------------------
# RUSTFLAGS here intentionally overrides .cargo/config.toml (snapshot-gen is a
# standalone binary, not the .so). arm64 additionally needs libclang_rt.builtins
# for __clear_cache (V8 cpu-arm64.cc JIT i-cache flush); x86_64 does not.
RUSTFLAGS_COMMON="-C link-arg=-Wl,--allow-multiple-definition -C link-arg=-landroid -C embed-bitcode=no"
if [[ "$NEEDS_BUILTINS" -eq 1 ]]; then
  BUILTINS="$(find "$NDK" -name 'libclang_rt.builtins-aarch64-android.a' 2>/dev/null | head -1)"
  [[ -n "$BUILTINS" ]] || die "libclang_rt.builtins-aarch64-android.a not found under $NDK"
  RUSTFLAGS_COMMON+=" -L $(dirname "$BUILTINS") -l static=clang_rt.builtins-aarch64-android"
fi

c_info "cross-compiling migo-snapshot-gen for $TRIPLE ..."
( cd "$ENGINE"
  RUSTY_V8_ARCHIVE="$V8_MATERIALISED_ARCHIVE" \
  RUSTY_V8_SRC_BINDING_PATH="$V8_MATERIALISED_BINDING" \
  RUSTFLAGS="$RUSTFLAGS_COMMON" \
  cargo ndk -t "$NDK_TARGET" --platform 26 build -p migo-snapshot-gen \
    --no-default-features --features "profile-$PRODUCT_PROFILE" --locked )

BIN="$TARGET_DIR/$TRIPLE/debug/migo-snapshot-gen"
[[ -f "$BIN" ]] || die "build produced no binary at $BIN"

# ---- 2. run on device -------------------------------------------------------
c_info "pushing to device ..."
"${ADBD[@]}" push "$LIBCXX" /data/local/tmp/libc++_shared.so >/dev/null
"${ADBD[@]}" push "$BIN" /data/local/tmp/migo-snapshot-gen >/dev/null
"${ADBD[@]}" shell chmod 755 /data/local/tmp/migo-snapshot-gen

c_info "generating snapshot on device ..."
"${ADBD[@]}" shell "cd /data/local/tmp && LD_LIBRARY_PATH=/data/local/tmp MIGO_SNAPSHOT_KIND=$SNAPSHOT_KIND MIGO_SNAPSHOT_OUT=/data/local/tmp/SNAPSHOT.bin ./migo-snapshot-gen"

# ---- 3. pull + manifest -----------------------------------------------------
"${ADBD[@]}" pull /data/local/tmp/SNAPSHOT.bin "$OUT" >/dev/null
[[ -s "$OUT" ]] || die "pulled snapshot is empty"

# Fingerprint manifest (js_sources_sha256 + deno_core_version). Written by the
# shared helper so this on-device arm64 path and CI's x86_64 path emit identical
# manifests — see scripts/write-snapshot-manifest.sh + check-snapshot-freshness.sh.
MANIFEST="$OUT.manifest.json"
bash "$ROOT/scripts/write-snapshot-manifest.sh" \
  "$PRODUCT_PROFILE" "$OS" "$RUST_ARCH" "$OUT" "$SNAPSHOT_KIND"

# ---- 4. cleanup -------------------------------------------------------------
if [[ "$KEEP" -eq 0 ]]; then
  "${ADBD[@]}" shell rm -f /data/local/tmp/migo-snapshot-gen /data/local/tmp/SNAPSHOT.bin /data/local/tmp/libc++_shared.so || true
fi

c_ok "snapshot -> $OUT  ($(stat -c %s "$OUT") bytes)"
c_ok "manifest -> $MANIFEST"
echo
if [[ "$SNAPSHOT_KIND" == "worker" ]]; then
  c_info "Build candidate: bash scripts/build-aar.sh --product-profile full --worker-snapshot release $NDK_TARGET <other abis...>"
else
  c_info "Build with embedding: bash scripts/build-aar.sh --product-profile $PRODUCT_PROFILE release $NDK_TARGET <other abis...>"
fi
