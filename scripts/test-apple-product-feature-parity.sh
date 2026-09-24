#!/usr/bin/env bash
# The Apple Performance+ product must resolve to the same graph as the Linux one.
#
# THE DRIFT THIS EXISTS TO CATCH cost an hour of CI to find and had been sitting
# in the tree since the external-frame product was created:
#
#   error[E0599]: no method named `enable_io` found for struct
#                 `tokio::runtime::Builder` in the current scope
#     --> crates/core/src/runtime/session_thread.rs:567
#
# `create_basic_runtime` calls `enable_io()` and `max_io_events_per_tick()`.
# tokio puts both behind `cfg_io_driver!`, which is `feature = "net"`. `migo-core`
# never declared it. On Linux and Android it compiled anyway, because
# `migo-shared` declares
#
#     [target.'cfg(any(target_os = "android", target_os = "linux"))'.dependencies]
#     tokio = { features = ["net"] }
#
# for `raf_signal`'s `AsyncFd` -- a legitimate declaration that happened to hand
# the IO driver to everybody else on those two operating systems. iOS is the
# first target that has neither that gate nor `migo-runtime-v8`, so the ENTIRE
# iOS architecture could never have been compiled, and the error read like a
# tokio version problem rather than a missing feature.
#
# The general shape is the one this repository keeps meeting: a crate uses an
# API that some other crate's feature enabled, and Cargo's feature unification
# makes the omission invisible until a build appears where that other crate is
# absent. `migo-core`'s manifest already carried a comment about exactly this,
# written when the same thing happened with `migo-runtime-v8`'s features -- and
# it was still incomplete, because the second supplier was a different crate.
#
# WHAT IS CHECKED, and why it is a comparison rather than a rule: the two
# resolutions must be identical, package for package and feature for feature.
# Not because platform differences are forbidden -- an Apple-only presenter will
# create real ones -- but because a difference here has to be somebody's
# decision. Today there are none, and an unexplained one is exactly the shape
# that made iOS unbuildable.
#
# Cheap enough for every PR: `cargo tree --target` resolves without compiling
# and without the target's toolchain installed.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT/engine"

PRODUCT_ARGS=(-p migo-capi --no-default-features --features external-frames)
APPLE_TARGET="aarch64-apple-ios"
REFERENCE_TARGET="x86_64-unknown-linux-gnu"

# Differences that are somebody's decision, as "package=reason" entries. An entry
# naming a package that no longer differs fails the gate: an exemption that
# accounts for nothing looks like it accounts for something.
#
# A plain indexed array rather than an associative one. `declare -A` does not
# exist in bash 3.2, which is what macOS ships, and the guarded key expansion an
# associative array needs under `set -u` -- `${!EXEMPT[@]+"${!EXEMPT[@]}"}` -- is
# not the guard it looks like: bash reads `!EXEMPT[@]` there as indirect
# expansion of the VALUE and dies with `invalid variable name`. That silently
# skipped the stale-exemption check when it was written that way, which the
# injection test caught only because the check failed to fire.
EXEMPT=(
    # skia-bindings compiles its jpeg/pdf/pathops C++ wrappers unconditionally,
    # while Skia builds pathops only as a dependency of optional("pdf"). With pdf
    # off, those wrappers reference symbols that do not exist. GNU ld drops the
    # referencing sections with --gc-sections before it has to resolve anything,
    # which is why the reference target has never needed this; ld64's
    # -dead_strip and MSVC's /OPT:REF both run after resolution and cannot.
    #
    # Not a hypothetical on Apple. The first executable ever to link libmigo.a --
    # the XCTest bundle in .github/workflows/apple-sdk.yml -- reported seventeen
    # undefined SkPathOps, SkJpeg and SkPDF symbols. Every Apple compile step had
    # passed, because compiling a library does not link one. So this difference
    # is what makes an Apple product linkable at all, and its cost is the PDF and
    # JPEG codecs' code size, which Android and OpenHarmony do not pay: their
    # hosts link libmigo_capi.a into a shared object, where undefined symbols are
    # resolved at load time and tolerated at link time.
    "skia-safe=pdf, so Skia builds the pathops its always-compiled wrappers reference; ld64 cannot hide them the way GNU ld does"
    "skia-bindings=jpeg/jpeg-decode/jpeg-encode/pdf arrive with skia-safe's pdf feature above, which is what supplies SkJpeg* and SkPDF* to the linker"
    # The audio device, which is the one thing a host cannot be portable about.
    # `host-audio` is part of `external-frames` because the Performance+ lane
    # plays sound in this process (WebContent never touches PCM), and cpal
    # reaches the device through ALSA on Linux and CoreAudio on Apple. So the
    # two resolutions differ by exactly one backend each, in both directions --
    # which is the difference existing rather than drifting.
    "alsa=cpal's Linux backend; the Apple resolution reaches the device through CoreAudio instead"
    "alsa-sys=alsa's bindings, for the same reason"
    "coreaudio-rs=cpal's Apple backend, where Linux uses ALSA"
    "coreaudio-sys=coreaudio-rs's bindings, for the same reason"
    "core-foundation-sys=coreaudio-sys's, for the CoreFoundation types its API takes"
    "mach2=coreaudio-rs's, for the Mach timebase its render callbacks are timed against"
    "bitflags=the CoreAudio crates take it with its default feature; the Linux graph has it without"
    # Content signing. The Performance+ session verifies signed packages before
    # it mounts them (migo-services' code-signing), so ed25519-dalek is in both
    # graphs; curve25519-dalek then takes this proc macro only on x86_64, where
    # it generates the AVX2 field-arithmetic backend. arm64 has no such backend
    # and uses the portable one -- an architecture's choice, not a drift.
    "curve25519-dalek-derive=curve25519-dalek's AVX2 backend generator, compiled only for x86_64; arm64 uses the portable backend"
)

problems=()
notes=()

# Resolve one target into "package<TAB>feature,feature,..." lines.
#
# `{f}` is the enabled feature list. The `(*)` cargo tree appends to a repeated
# subtree is display, not data, and comparing it as data reports a difference
# whenever the two graphs merely print in a different order.
#
# `--color never` for the same reason, and it was not hypothetical: CI sets
# `CARGO_TERM_COLOR: always`, so `(*)` arrives wrapped in escape sequences, the
# `sed` below does not match it, and the gate reported
#
#     cpufeatures features differ: apple-only=[' \x1b[33m\x1b[2m(*)\x1b[39m...']
#
# on a graph that was identical. Parsing must not depend on an environment
# variable, so the flag is passed rather than the escapes stripped.
resolve() {
    cargo tree ${PRODUCT_ARGS[@]+"${PRODUCT_ARGS[@]}"} --target "$1" -e normal --prefix none \
        --color never --format '{p}|{f}' 2>/dev/null \
        | sed -e 's/ (\*)$//' -e 's/ v[0-9][^|]*|/|/' -e 's/ ([^)]*)|/|/' \
        | awk -F'|' 'NF {print $1 "\t" $2}' \
        | sort -u
}

apple="$(resolve "$APPLE_TARGET")"
reference="$(resolve "$REFERENCE_TARGET")"

if [[ -z "$apple" || -z "$reference" ]]; then
    echo "FAIL: cargo tree resolved nothing for one of the targets; this gate inspected nothing" >&2
    exit 1
fi

compare() {
    python3 - "$1" "$2" <<'PY'
import sys

def load(text):
    out = {}
    for line in text.splitlines():
        if not line.strip():
            continue
        name, _, feats = line.partition("\t")
        out[name] = tuple(sorted(f for f in feats.split(",") if f))
    return out

left, right = load(sys.argv[1]), load(sys.argv[2])
for name in sorted(set(left) | set(right)):
    if name not in right:
        print(f"{name}\tonly in the Apple resolution")
    elif name not in left:
        print(f"{name}\tonly in the reference resolution")
    elif left[name] != right[name]:
        a = set(left[name]); b = set(right[name])
        print(f"{name}\tfeatures differ: apple-only={sorted(a - b)} reference-only={sorted(b - a)}")
PY
}

mapfile -t differences < <(compare "$apple" "$reference")
apple_packages="$(printf '%s\n' "$apple" | wc -l)"
notes+=("$APPLE_TARGET and $REFERENCE_TARGET each resolve $apple_packages package(s) for the Performance+ product")

for difference in ${differences[@]+"${differences[@]}"}; do
    [[ -n "$difference" ]] || continue
    package="${difference%%$'\t'*}"
    detail="${difference#*$'\t'}"
    reason=""
    for entry in ${EXEMPT[@]+"${EXEMPT[@]}"}; do
        [[ "${entry%%=*}" == "$package" ]] && reason="${entry#*=}"
    done
    if [[ -n "$reason" ]]; then
        notes+=("recorded difference in $package: $reason")
        continue
    fi
    problems+=("$package $detail.
      A package or feature that exists for one of these targets and not the
      other is a build the two platforms do not share. If it is intended, add it
      to EXEMPT with the reason; if it is not, it is the next enable_io.")
done

# Stale exemptions.
for entry in ${EXEMPT[@]+"${EXEMPT[@]}"}; do
    package="${entry%%=*}"
    found=no
    for difference in ${differences[@]+"${differences[@]}"}; do
        [[ "${difference%%$'\t'*}" == "$package" ]] && found=yes
    done
    if [[ "$found" != yes ]]; then
        problems+=("EXEMPT names $package, which no longer differs between the two targets")
    fi
done

# --- the control ---------------------------------------------------------------
#
# The comparison must be able to see a difference. Windows resolves the same
# product with its own platform crates, so a detector that reports nothing there
# is a detector that would report nothing here either.
control="$(resolve x86_64-pc-windows-msvc)"
if [[ -z "$control" ]]; then
    problems+=("the control target did not resolve, so the comparison is unverified")
else
    mapfile -t control_differences < <(compare "$apple" "$control")
    if (( ${#control_differences[@]} == 0 )); then
        problems+=("the comparison found no difference between $APPLE_TARGET and
      x86_64-pc-windows-msvc, which resolve different platform crates. The
      comparison is not comparing, so the clean result above proves nothing.")
    else
        notes+=("control: the comparison reports ${#control_differences[@]} difference(s) against x86_64-pc-windows-msvc")
    fi
fi

printf '\n'
for note in ${notes[@]+"${notes[@]}"}; do echo "  - $note"; done
printf '\n'

if (( ${#problems[@]} > 0 )); then
    echo "FAIL: the Apple Performance+ product no longer resolves like the Linux one." >&2
    printf '\n' >&2
    for problem in ${problems[@]+"${problems[@]}"}; do echo "  * $problem" >&2; done
    printf '\n' >&2
    cat >&2 <<'WHY'
  Why this matters: nothing on this machine can compile the Apple product -- it
  needs Skia built by an Apple SDK -- so the only cheap way to notice that it has
  drifted is to compare what Cargo resolves for it against a target that is
  compiled here every day. The last time nobody was comparing, the iOS
  architecture spent its whole existence unable to build.
WHY
    exit 1
fi

echo "PASS: the Apple and Linux Performance+ products resolve the same graph."
