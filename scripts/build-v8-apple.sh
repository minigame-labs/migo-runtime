#!/usr/bin/env bash
# ============================================================
# Build the macOS librusty_v8.a that migo's `macos-v8` product links against.
# Location: scripts/build-v8-apple.sh
#
# Counterpart to scripts/build-v8-{android,linux,ohos,windows}.sh, and the
# shortest of the five, because darwin needed none of what the other four had to
# work around. That is not a guess: .github/workflows/apple-v8-probe.yml ran on
# 2026-09-09 (run 34299937361) before a line of this existed, and
# contracts/artifact-manifest/apple-v8.lock.json's `measured` block records what
# it answered. Three of those answers are why this file is short:
#
#   * `V8_FROM_SOURCE=1` plus `cargo build -p v8` works on a hosted macOS runner.
#     No self-hosted machine, no prebuilt fallback.
#   * `x86_64-apple-darwin` cross-builds with NO extra arguments. OpenHarmony
#     needed a pinned `v8_snapshot_toolchain` and carries two code paths for it;
#     Apple's own toolchain treats x86_64 as a first-class target, so this script
#     has one path and the triple is a parameter.
#   * `src_binding.rs` is generated here (870 lines). ohos-v8.lock.json records a
#     pre-committed binding as structural because that SDK's clang is too old for
#     bindgen; Xcode's is not.
#
# THE ONE THING THE PROBE FOUND THAT THIS FILE EXISTS TO FIX. Left alone, the
# archive carries `minos 12.0` -- inherited from the runner's SDK -- while
# contracts/apple/deployment-floor.json declares the macOS floor as 11.0. A
# product linking that archive would fail to load on the oldest macOS the project
# says it supports, and the floor would be a claim the bytes do not meet. It is
# the same shape as the ANGLE loader-path trap: right on the machine that built
# it, wrong on a consumer's.
#
# So the deployment target is passed explicitly, read from the contract rather
# than typed here -- AND THEN VERIFIED AGAINST THE BYTES. The verification is the
# point. `mac_deployment_target` is the argument Chromium's build files define
# for this, but the probe's accepted `args.gn` did not contain it, so this script
# does not get to assume the name did what its name suggests. It asks `vtool`
# what the object actually says and fails closed if it disagrees, which turns a
# wrong argument name into a build failure naming the number instead of an
# archive that loads here and not on a customer's Mac.
#
# ONE TRIPLE PER INVOCATION, and that is a measured decision too: the probe spent
# 57 minutes on the host triple and 56 on the cross, 115 of a 120-minute job cap
# for the two builds alone -- no archiving, no hashing, no upload. A lane that
# copies the probe's shape discovers the cap as a cancellation that reads like a
# flake. Call this once per triple, in its own job, and join the results in a
# third.
# ============================================================
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
LOCK="$PROJECT_ROOT/contracts/artifact-manifest/apple-v8.lock.json"
FLOOR="$PROJECT_ROOT/contracts/apple/deployment-floor.json"

info() { printf '\033[0;36m[v8-apple]\033[0m %s\n' "$*"; }
ok() { printf '\033[0;32m[v8-apple]\033[0m %s\n' "$*"; }
err() {
    printf '\033[0;31m[v8-apple] %s\033[0m\n' "$*" >&2
    exit 1
}

usage() {
    cat <<'USAGE'
usage: build-v8-apple.sh --arch <aarch64|x86_64> [--out <dir>] [--src <dir>]

  --arch <aarch64|x86_64>  Which darwin triple to build. One per invocation:
                           the two builds together take about 113 minutes, which
                           does not fit a 120-minute job with anything after it.
  --out <dir>              Where librusty_v8.a, src_binding.rs and
                           build-metadata.json land.
                           Default: build/apple/v8/<arch>.
  --src <dir>              An existing rusty_v8 checkout to reuse. Default: a
                           fresh clone at the pinned revision under the out
                           directory's parent.
  --keep-src               Do not delete a checkout this script created.

  --print-deployment-target
                           Print the macOS deployment target this build would
                           use and exit. Exists so
                           scripts/test-apple-deployment-floor-contract.sh can
                           ASK rather than read: grepping for the literal would
                           pass a script that holds the number in a comment and
                           computes something else, and fail the script that
                           does the right thing and holds no copy at all. It
                           runs before the macOS check, so the gate can ask it
                           anywhere.

Environment:
  MIGO_V8_JOBS             Passed to cargo as --jobs.

What it does not do: publish anything, or write hashes into the lock. Those
belong to the lane that runs this, after real bytes exist -- the same order
A2.1's ANGLE round used, and the same reason.
USAGE
}

ARCH=""
OUT_DIR=""
SRC_DIR=""
KEEP_SRC=0
PRINT_DEPLOYMENT_TARGET=0

while [[ $# -gt 0 ]]; do
    case "$1" in
        --arch)
            [[ $# -ge 2 ]] || err "--arch needs aarch64 or x86_64"
            ARCH="$2"
            shift 2
            ;;
        --out)
            [[ $# -ge 2 ]] || err "--out needs a directory"
            OUT_DIR="$2"
            shift 2
            ;;
        --src)
            [[ $# -ge 2 ]] || err "--src needs a directory"
            SRC_DIR="$2"
            shift 2
            ;;
        --keep-src)
            KEEP_SRC=1
            shift
            ;;
        --print-deployment-target)
            PRINT_DEPLOYMENT_TARGET=1
            shift
            ;;
        -h | --help)
            usage
            exit 0
            ;;
        *) err "$1 is not an option this script takes; --help lists them" ;;
    esac
done

read_json() {
    python3 -c "
import json,sys
with open(sys.argv[1]) as handle:
    value = json.load(handle)
for key in sys.argv[2].split('.'):
    value = value[key]
print(value)
" "$1" "$2"
}

if ((PRINT_DEPLOYMENT_TARGET)); then
    [[ -f "$FLOOR" ]] || err "$FLOOR is missing"
    read_json "$FLOOR" "platforms.macos.deployment_target"
    exit 0
fi

case "$ARCH" in
    aarch64 | x86_64) ;;
    "") err "pass --arch aarch64 or --arch x86_64" ;;
    *) err "--arch $ARCH is not a darwin architecture this project builds" ;;
esac

TRIPLE="$ARCH-apple-darwin"
[[ "$(uname -s)" == "Darwin" ]] || err "this builds with Apple's own toolchain and needs macOS"
command -v xcrun >/dev/null || err "xcrun is not on PATH"
command -v cargo >/dev/null || err "cargo is not on PATH"
command -v python3 >/dev/null || err "python3 is not on PATH"
command -v vtool >/dev/null || err "vtool is not on PATH; the floor could not be verified"

[[ -f "$LOCK" ]] || err "$LOCK is missing"
[[ -f "$FLOOR" ]] || err "$FLOOR is missing"

# ---------------------------------------------------------------------------
# Everything numeric comes from a contract. A number typed here is a number that
# drifts from the file that decides it, silently, in the direction of whatever
# the machine happened to default to.
# ---------------------------------------------------------------------------

RUSTY_V8_REVISION="$(read_json "$LOCK" "rusty_v8_revision")"
RUSTY_V8_URL="https://github.com/denoland/rusty_v8"
DEPLOYMENT_TARGET="$(read_json "$FLOOR" "platforms.macos.deployment_target")"
LOCK_TARGET="$(read_json "$LOCK" "macos.deployment_target")"

# Two files state the floor and one gate already checks that they agree; this
# refuses rather than picking, because a build that picked would be shipping a
# floor nobody chose.
[[ "$DEPLOYMENT_TARGET" == "$LOCK_TARGET" ]] \
    || err "the deployment floor says $DEPLOYMENT_TARGET and the V8 lock says $LOCK_TARGET"

[[ -n "$OUT_DIR" ]] || OUT_DIR="$PROJECT_ROOT/build/apple/v8/$ARCH"
mkdir -p "$OUT_DIR"
OUT_DIR="$(cd "$OUT_DIR" && pwd)"

CREATED_SRC=0
if [[ -z "$SRC_DIR" ]]; then
    SRC_DIR="$PROJECT_ROOT/build/apple/v8/rusty_v8_src"
    CREATED_SRC=1
fi

cleanup() {
    if ((CREATED_SRC == 1 && KEEP_SRC == 0)) && [[ -d "$SRC_DIR" ]]; then
        rm -rf "$SRC_DIR"
    fi
    # Not a conditional as the last statement: a trap that ends on a false test
    # makes a successful script exit 1, which this project has debugged once.
    return 0
}
trap cleanup EXIT

info "arch          $ARCH ($TRIPLE)"
info "rusty_v8      $RUSTY_V8_REVISION"
info "macOS floor   $DEPLOYMENT_TARGET (from contracts/apple/deployment-floor.json)"
info "out           $OUT_DIR"

# ---------------------------------------------------------------------------
# Source
# ---------------------------------------------------------------------------

if [[ ! -d "$SRC_DIR/.git" ]]; then
    info "cloning rusty_v8 at the pinned revision"
    mkdir -p "$(dirname "$SRC_DIR")"
    # `--filter=blob:none` and a shallow submodule update: the probe measured this
    # combination, and a full clone of V8's history is tens of gigabytes for
    # history nothing here reads.
    git clone --filter=blob:none "$RUSTY_V8_URL" "$SRC_DIR"
    git -C "$SRC_DIR" checkout --quiet "$RUSTY_V8_REVISION"
    git -C "$SRC_DIR" submodule update --init --recursive --depth 1
else
    info "reusing the checkout at $SRC_DIR"
    ACTUAL="$(git -C "$SRC_DIR" rev-parse HEAD)"
    [[ "$ACTUAL" == "$RUSTY_V8_REVISION" ]] \
        || err "the checkout at $SRC_DIR is $ACTUAL and the lock pins $RUSTY_V8_REVISION"
fi

rustup target add "$TRIPLE" >/dev/null 2>&1 || true

# ---------------------------------------------------------------------------
# Build
# ---------------------------------------------------------------------------
#
# `EXTRA_GN_ARGS` and not `GN_ARGS`: rusty_v8's build.rs appends GN_ARGS early
# and EXTRA_GN_ARGS last, and gn takes the last assignment of a key. The floor
# has to win over anything the crate or the SDK sets, so it goes last.
#
# MACOSX_DEPLOYMENT_TARGET as well, for the parts of the build that read the
# environment rather than the gn arg. Belt and braces on purpose: which of the
# two the object file ends up honouring is exactly what the check below asks,
# rather than something this comment gets to assert.
export V8_FROM_SOURCE=1
export EXTRA_GN_ARGS="mac_deployment_target=\"$DEPLOYMENT_TARGET\""
export MACOSX_DEPLOYMENT_TARGET="$DEPLOYMENT_TARGET"

CARGO_ARGS=(build --release --target "$TRIPLE" -p v8)
[[ -n "${MIGO_V8_JOBS:-}" ]] && CARGO_ARGS+=(--jobs "$MIGO_V8_JOBS")

info "building (the probe measured about 56 minutes per triple)"
# A stopwatch, and it stays out of build-metadata.json on purpose. How long a
# build took is a fact about the build, not about its output: putting it in the
# metadata would make two builds of one commit differ, and that file is the one a
# reproducibility check would compare. Emitted the way apple-v8-probe.yml emits
# its own timings, as a line for a log rather than a field in an artifact.
BUILD_START=$(date +%s)
(cd "$SRC_DIR" && cargo "${CARGO_ARGS[@]}")
BUILD_SECONDS=$(($(date +%s) - BUILD_START))
echo "build_seconds=$BUILD_SECONDS"
ok "built in ${BUILD_SECONDS}s"

GN_OUT="$SRC_DIR/target/$TRIPLE/release/gn_out"
ARCHIVE="$GN_OUT/obj/librusty_v8.a"
BINDING="$GN_OUT/src_binding.rs"
ARGS_GN="$GN_OUT/args.gn"

[[ -f "$ARCHIVE" ]] || err "the build produced no archive at $ARCHIVE"
[[ -f "$ARGS_GN" ]] || err "no args.gn at $ARGS_GN, so nothing can record what this build used"
# The probe found this generated. If it ever is not, that is a structural fact
# about the toolchain and belongs in the lock's notes, not in a silent skip.
[[ -f "$BINDING" ]] \
    || err "src_binding.rs was not generated. The probe measured it as generated on this platform; if that has changed, record it in the lock the way ohos-v8.lock.json records its own"

# ---------------------------------------------------------------------------
# The floor, checked against the bytes
# ---------------------------------------------------------------------------
#
# An `ar` archive is not a Mach-O, so vtool is asked about a member. Extracting
# into a scratch directory rather than in place: `ar x` writes into the working
# directory and would otherwise litter the checkout with object files that the
# next build's globs would find.

info "asking the object what floor it actually carries"
PROBE_DIR="$(mktemp -d)"
# One member, named first and extracted second. A bare `ar x` unpacks the whole
# archive -- about 126 MiB of object files, per the probe's measurement -- to read
# a single load command out of one of them.
MEMBER_NAME="$(ar t "$ARCHIVE" | grep -m1 '\.o$' || true)"
[[ -n "$MEMBER_NAME" ]] || err "no object member in $ARCHIVE to inspect"
(cd "$PROBE_DIR" && ar x "$ARCHIVE" "$MEMBER_NAME")
MEMBER="$PROBE_DIR/$MEMBER_NAME"
[[ -f "$MEMBER" ]] || err "ar named $MEMBER_NAME and then did not extract it"

OBSERVED="$(vtool -show-build "$MEMBER" 2>/dev/null | awk '/minos/ {print $2; exit}')"
rm -rf "$PROBE_DIR"
[[ -n "$OBSERVED" ]] || err "vtool reported no minos for $MEMBER, so the floor is unverified"

# Compared as numbers, because "11.0" and "11" are the same floor and different
# strings, and a build that failed on that would fail for a formatting reason.
python3 - "$OBSERVED" "$DEPLOYMENT_TARGET" <<'PY' || err "the archive does not carry the declared floor"
import sys

def parts(text):
    return tuple(int(piece) for piece in text.split(".")[:2] + ["0"][: 2 - len(text.split("."))])

observed, declared = sys.argv[1], sys.argv[2]
if parts(observed) != parts(declared):
    print(
        f"FAIL: the archive carries minos {observed} and contracts/apple/deployment-floor.json\n"
        f"      declares {declared}. Left to itself this build inherits the runner's SDK default,\n"
        f"      which the probe measured as 12.0 -- and a product linking that would fail to load\n"
        f"      on the oldest macOS this project says it supports. Either the gn argument name is\n"
        f"      not the one that decides this, or something later overrode it; the archive is not\n"
        f"      publishable until the bytes say {declared}.",
        file=sys.stderr)
    raise SystemExit(1)
PY
ok "the object carries minos $OBSERVED, which is the declared floor"

# ---------------------------------------------------------------------------
# Output
# ---------------------------------------------------------------------------

cp "$ARCHIVE" "$OUT_DIR/librusty_v8.a"
cp "$BINDING" "$OUT_DIR/src_binding.rs"

# The verbatim args.gn, minus the keys that name this machine. Recorded rather
# than restated: the point of the probe was that a pin has to be what a real
# build accepted, and the same holds for every build after it.
python3 - "$ARGS_GN" "$OUT_DIR/build-metadata.json" \
    "$ARCH" "$TRIPLE" "$RUSTY_V8_REVISION" "$DEPLOYMENT_TARGET" "$OBSERVED" \
    "$OUT_DIR/librusty_v8.a" <<'PY'
import hashlib
import json
import pathlib
import sys

args_gn, output, arch, triple, revision, declared, observed, archive = sys.argv[1:9]

# Machine-local keys. `clang_base_path` names a path inside whatever temporary
# directory this build used, so it describes the machine and not the build.
LOCAL_KEYS = {"clang_base_path"}

normalized = []
for line in pathlib.Path(args_gn).read_text(encoding="utf-8").splitlines():
    stripped = line.strip()
    if not stripped or stripped.startswith("#"):
        continue
    key = stripped.split("=", 1)[0].strip()
    if key in LOCAL_KEYS:
        continue
    normalized.append(stripped)

digest = hashlib.sha256(pathlib.Path(archive).read_bytes()).hexdigest()
pathlib.Path(output).write_text(
    json.dumps(
        {
            "schema": "migo-v8-build-metadata/v1",
            "platform": "macos",
            "arch": arch,
            "triple": triple,
            "rusty_v8_revision": revision,
            "declared_deployment_target": declared,
            "observed_minos": observed,
            "archive_sha256": digest,
            "archive_bytes": pathlib.Path(archive).stat().st_size,
            "normalized_gn_args": sorted(normalized),
            "_comment": [
                "normalized_gn_args is the verbatim args.gn this build accepted, minus keys",
                "that name the machine. It is recorded from the build rather than restated",
                "from a plan, which is the rule the Apple V8 probe was run to establish.",
                "",
                "observed_minos is what vtool read out of an object in the archive, not what",
                "the build was asked for. Those are different claims and only one of them is",
                "about the bytes a consumer links.",
                "",
                "How long the build took is deliberately absent: it is a fact about the build",
                "and not about its output, and a field that differs between two builds of one",
                "commit is a field that defeats the comparison this file would be used for.",
            ],
        },
        indent=2,
    )
    + "\n",
    encoding="utf-8",
)
PY

ok "done"
info "  $OUT_DIR/librusty_v8.a ($(stat -f %z "$OUT_DIR/librusty_v8.a") bytes)"
info "  $OUT_DIR/src_binding.rs"
info "  $OUT_DIR/build-metadata.json"
