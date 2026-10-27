#!/usr/bin/env bash
# scripts/test-android-ndk-constants-contract.sh
#
# Assert that every NDK constant the Android platform code declares by hand has the value the pinned
# NDK's headers give it.
#
# WHY THIS EXISTS (observed, not hypothetical):
# engine/crates/platform/src/android/video_decoder.rs declared AMEDIA_IMGREADER_NO_BUFFER_AVAILABLE as
# -30000. That is AMEDIA_IMGREADER_ERROR_BASE; the status is ERROR_BASE - 1. The decoder asks the image
# reader for the frame the codec has just rendered, and on a phone the frame lands a moment later: the
# reader answers -30001, which the decoder took for a failure instead of a reason to wait. createVideoDecoder
# started, the codec decoded every frame, and content received none -- on every real device, while the
# emulator, whose frames land before the first ask, passed. The Rust side cannot include the C headers,
# so nothing compared the copy with the original.
#
# WHY IT CHECKS WHAT IT CHECKS:
# The pinned NDK's clang is the authority on what the headers say, so the comparison is a compile: one
# `_Static_assert(NAME == value)` per constant, against the headers the code links. A name the headers do
# not declare fails the compile too, so a misspelt constant is caught like a wrong one. Constants are
# found by the NDK's own name prefixes; an empty scan fails, since a gate that found nothing has checked
# nothing. The comparison is then run against a copy with one value changed and must fail, or it is not
# a comparison.
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
TAG='[ndk-constants]'
SOURCES="$REPO_ROOT/engine/crates/platform/src/android"

pass() { echo -e "\033[0;32m$TAG PASS $*\033[0m"; }
fail() { echo -e "\033[0;31m$TAG FAIL $*\033[0m" >&2; failures=$((failures + 1)); }
failures=0

# shellcheck source=lib/android-ndk.sh
source "$SCRIPT_DIR/lib/android-ndk.sh"
android_ndk_read_pin "$REPO_ROOT/contracts/artifact-manifest/android-v8.lock.json" || exit 1
android_ndk_resolve || exit 1
clang="$(find "$ANDROID_NDK_HOME/toolchains/llvm/prebuilt" -mindepth 3 -maxdepth 3 -path '*/bin/clang' | head -1)"
[[ -x "$clang" ]] || { echo "$TAG no clang in $ANDROID_NDK_HOME" >&2; exit 1; }

scratch="$(mktemp -d)" || { echo "$TAG no writable temp directory" >&2; exit 1; }
trap 'rm -rf "$scratch"' EXIT

# <source dir> <output .c> [<name> <value>]: writes the assertions, prints how many; the optional pair
# replaces that constant's value, for the self-test.
generate() {
    python3 - "$@" <<'PY'
import pathlib, re, sys

sources, output = pathlib.Path(sys.argv[1]), pathlib.Path(sys.argv[2])
override = dict([sys.argv[3:5]]) if len(sys.argv) > 4 else {}
# The NDK's own namespaces, so a constant of Migo's that happens to start with an A is not taken for one.
prefixes = ("AMEDIA", "AIMAGE", "ANATIVEWINDOW", "AHARDWAREBUFFER", "AAUDIO", "ASENSOR", "AINPUT",
            "AKEYCODE", "AMOTION_EVENT", "ALOOPER", "ACONFIGURATION", "ASURFACE", "ACHOREOGRAPHER",
            "AASSET", "WINDOW_FORMAT")
const = re.compile(r"^\s*(?:pub(?:\([^)]*\))?\s+)?const\s+([A-Z][A-Z0-9_]*)\s*:\s*[\w:]+\s*=\s*"
                   r"(-?(?:0x[0-9A-Fa-f]+|[0-9]+))\s*;")
headers = ["media/NdkMediaCodec.h", "media/NdkMediaError.h", "media/NdkMediaExtractor.h",
           "media/NdkMediaFormat.h", "media/NdkMediaMuxer.h", "media/NdkImage.h", "media/NdkImageReader.h",
           "android/native_window.h", "android/hardware_buffer.h", "aaudio/AAudio.h", "android/sensor.h",
           "android/input.h", "android/keycodes.h", "android/looper.h", "android/configuration.h",
           "android/choreographer.h", "android/asset_manager.h"]
lines = [f"#include <{header}>" for header in headers]
found = 0
for path in sorted(sources.rglob("*.rs")):
    for number, text in enumerate(path.read_text().splitlines(), 1):
        match = const.match(text)
        if not match or not match.group(1).startswith(prefixes):
            continue
        name, value = match.groups()
        value = override.get(name, value)
        where = f"{path.relative_to(sources.parents[4])}:{number}"
        lines.append(f'_Static_assert(({name}) == ({value}), "{where}: {name} is not {value} in the NDK");')
        found += 1
output.write_text("\n".join(lines) + "\n")
print(found)
PY
}

# <.c file>: 0 when the pinned NDK's headers agree with every assertion in it.
agrees() {
    "$clang" --target=aarch64-linux-android26 -fsyntax-only -x c "$1" 2> "$scratch/compile.log"
}

count="$(generate "$SOURCES" "$scratch/constants.c")" || { echo "$TAG cannot scan $SOURCES" >&2; exit 1; }
if ((count == 0)); then
    fail "no NDK constant found under engine/crates/platform/src/android: the scan no longer matches"
elif agrees "$scratch/constants.c"; then
    pass "$count hand-declared NDK constant(s) match NDK $ANDROID_NDK_PIN's headers"
else
    fail "a hand-declared NDK constant disagrees with NDK $ANDROID_NDK_PIN's headers:"
    grep -E "error:" "$scratch/compile.log" | sed 's/^/      /' >&2
fi

# The comparison must be able to fail: once on a wrong value, once on a name the NDK does not have.
first="$(grep -m1 -oE '_Static_assert\(\([A-Z0-9_]+\) == \(-?[0-9a-fA-Fx]+\)' "$scratch/constants.c" \
    | sed -E 's/_Static_assert\(\(([A-Z0-9_]+)\) == \((.*)\)/\1 \2/')"
if [[ -n "$first" ]]; then
    read -r name value <<< "$first"
    generate "$SOURCES" "$scratch/wrong.c" "$name" "$((value + 1))" > /dev/null
    if agrees "$scratch/wrong.c"; then
        fail "self-test: $name = $((value + 1)) compared equal"
    else
        pass "self-test: a wrong value is refused"
    fi
    sed "s/(${name})/(${name}_NOT_IN_THE_NDK)/" "$scratch/constants.c" > "$scratch/unknown.c"
    if agrees "$scratch/unknown.c"; then
        fail "self-test: a name the NDK does not declare compared equal"
    else
        pass "self-test: a name the NDK does not declare is refused"
    fi
fi

((failures == 0)) || { echo "$TAG $failures failure(s)" >&2; exit 1; }
