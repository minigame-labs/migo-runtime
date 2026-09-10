#!/usr/bin/env bash
# The JavaScript frame encoder must reproduce the committed corpus exactly.
#
# contracts/frame-wire/wire-v1.md says the format has two implementations and
# that neither is the specification: both are measured against the fixed corpus
# in contracts/frame-wire/golden. For a while that was aspirational. There was
# one encoder, so "the corpus is what the Rust builder produces" was a
# tautology, and the sentence in the contract described a check nobody could run.
#
# THE DRIFT THIS EXISTS TO CATCH is the quiet kind. A JavaScript encoder that
# reads a 128-bit launch nonce through `Number` is correct for every value
# anyone types by hand and wrong for the value a real session uses -- and the
# symptom is a renderer in another process rejecting frames as foreign, on a
# device, with no way to see why. The corpus carries values past 2^53 so that
# mistake fails here instead.
#
# Host-only: node, no device, no Apple toolchain.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

TEST="platforms/apple/WebContent/PerformancePlus/test/golden-corpus.test.mjs"
ENCODER="platforms/apple/WebContent/PerformancePlus/src/wire-frame-packet.mjs"
SYNC_TEST="platforms/apple/WebContent/PerformancePlus/test/sync-mailbox.test.mjs"
SYNC_SRC="platforms/apple/WebContent/PerformancePlus/src/sync-mailbox.mjs"
RELAY_TEST="platforms/apple/WebContent/PerformancePlus/test/sync-relay.test.mjs"
RELAY_SRC="platforms/apple/WebContent/PerformancePlus/src/sync-relay.mjs"

for required in "$TEST" "$ENCODER" "$SYNC_TEST" "$SYNC_SRC" "$RELAY_TEST" "$RELAY_SRC"; do
    if [[ ! -f "$required" ]]; then
        echo "FAIL: $required is missing; the cross-language corpus check cannot run." >&2
        exit 1
    fi
done

if ! command -v node >/dev/null 2>&1; then
    # Not a pass. An environment without node cannot answer this question, and
    # reporting "skipped" as success is how a check stops being one.
    echo "FAIL: node is not available, so the JavaScript encoder is unverified." >&2
    echo "      Install node (any version with BigInt DataView support) or run this on a host that has it." >&2
    exit 1
fi

# The encoder must not have grown a dependency. It runs inside WebContent next
# to untrusted game code; every import is one more thing inside that boundary,
# and the test harness is the only place allowed to reach the filesystem.
if grep -nE '^\s*import\b' "$ENCODER" >/dev/null 2>&1; then
    echo "FAIL: $ENCODER imports something. It runs in WebContent beside untrusted" >&2
    echo "      content and must stay dependency-free; the test harness does the I/O." >&2
    grep -nE '^\s*import\b' "$ENCODER" >&2
    exit 1
fi

# The shipped bundle must carry the producer and not its test suite.
#
# `build-apple-sdk.sh` used to copy the whole producer directory into the
# SwiftPM resources, which put this test file and the packet emitter inside the
# app bundle -- dead weight that reads the repository's golden corpus by
# relative path, from a phone. Checked here because this is the gate that knows
# what the producer directory contains.
SDK_SCRIPT="scripts/build-apple-sdk.sh"
if [[ -f "$SDK_SCRIPT" ]]; then
    if ! grep -q 'WEBCONTENT_SRC/src' "$SDK_SCRIPT"; then
        echo "FAIL: $SDK_SCRIPT does not stage the producer's src/ specifically." >&2
        echo "      Copying the whole producer directory ships this test suite and the" >&2
        echo "      packet emitter inside the app bundle." >&2
        exit 1
    fi
    if grep -qE 'cp -R "\$WEBCONTENT_SRC"/\.' "$SDK_SCRIPT"; then
        echo "FAIL: $SDK_SCRIPT copies the entire producer directory into the bundle." >&2
        exit 1
    fi
fi

node "$TEST"

# --- the synchronous barrier's producer half --------------------------------
#
# Same rule as the corpus above and for the same reason: this side is checked
# against contracts/frame-wire/wire-v1.md, not against the Rust mailbox, because
# two implementations that agree with each other and not with the document is
# the failure the document exists to catch. It also runs a real `Atomics.wait`
# woken by a real worker -- "it blocks" is the entire claim, and a test whose
# host answered before the wait began would exercise every line except that one.
node "$SYNC_TEST"

# --- and the two halves against each other ----------------------------------
#
# Each half being correct alone is not the property that matters. This runs a
# real Worker blocked in `Atomics.wait` and a real relay answering it, because
# what has to hold is that a blocked agent is always woken and never with the
# wrong bytes -- and neither half can establish that by itself.
node "$RELAY_TEST"

# --- and the other direction ------------------------------------------------
#
# The corpus above pins three shapes byte for byte. It cannot answer "does this
# encoder ever produce something the reader refuses", because three cases do not
# cover section counts, ragged payload lengths, the padding those imply, or the
# wide-field values a real session uses. So the emitter writes a deterministic
# spread of packets and the Rust reader validates every one, checking each field
# came back at full width.
#
# The Rust side is `#[ignore]`d, because a `cargo test` run has no way to
# produce its input. That is the visible form of the dependency; this gate is
# what guarantees it actually runs, which is the half that a test returning
# early on a missing environment variable would lose.
if ! command -v cargo >/dev/null 2>&1; then
    echo "FAIL: cargo is not available, so the reader cannot check the emitter's packets." >&2
    exit 1
fi

PACKETS="$(mktemp -d)"
trap 'rm -rf "$PACKETS"' EXIT

node platforms/apple/WebContent/PerformancePlus/test/emit-packets.mjs "$PACKETS" 128

emitted="$(find "$PACKETS" -name 'packet-*.bin' | wc -l)"
if (( emitted < 128 )); then
    echo "FAIL: the emitter wrote $emitted packets, expected 128." >&2
    exit 1
fi

# `--nocapture` so the count the test prints is in the log. A run that
# validated zero packets and passed is the shape this repository keeps finding,
# and the count below is what makes it visible rather than inferred.
output="$(cd engine && MIGO_JS_PACKET_DIR="$PACKETS" \
    cargo test -p migo-frame-wire --test js_interop -- --ignored --nocapture 2>&1)"
status=$?
printf '%s\n' "$output" | grep -E 'validated [0-9]+ JavaScript-encoded packets|test result' || true
if (( status != 0 )); then
    printf '%s\n' "$output" >&2
    echo "FAIL: the Rust reader rejected packets built by the JavaScript encoder." >&2
    exit 1
fi
if ! printf '%s\n' "$output" | grep -qE 'validated 128 JavaScript-encoded packets'; then
    echo "FAIL: the interop test did not report validating 128 packets; it may not have run." >&2
    exit 1
fi

# --- the committed clear-to-blue frame still comes out of the emitter --------
#
# The committed clear-to-blue frame is committed so that a
# consumer needing a valid packet does not build one itself -- building one in a
# third language is the failure mode the wire document exists to prevent. A
# committed artifact then needs a reason to still be trustworthy, and this is it:
# the emitter is run and its bytes compared to the committed ones.
#
# `frame-wire`'s `clear_frame_fixture` test reads the same file and asserts what
# is IN it. The two answer different questions -- "is it still what the emitter
# makes" and "is it still a valid frame saying what it claims" -- and a committed
# fixture needs both.

FIXTURES="platforms/apple/Tests/MigoAppleRendererTests/Fixtures"
REGENERATED="$(mktemp -d)"
trap 'rm -rf "$PACKETS" "$SYNC_PARAMS" "$REGENERATED"' EXIT
node platforms/apple/WebContent/PerformancePlus/test/emit-clear-frame.mjs "$REGENERATED" >/dev/null

committed=0
for regenerated in "$REGENERATED"/*.bin; do
    name="$(basename "$regenerated")"
    if [[ ! -f "$FIXTURES/$name" ]]; then
        echo "FAIL: the emitter writes $name and $FIXTURES does not have it." >&2
        exit 1
    fi
    if ! cmp -s "$FIXTURES/$name" "$regenerated"; then
        echo "FAIL: the emitter no longer reproduces $FIXTURES/$name byte for byte." >&2
        echo "      Either the encoder changed and the fixtures need regenerating, or one" >&2
        echo "      was edited by hand. Regenerate with:" >&2
        echo "        node platforms/apple/WebContent/PerformancePlus/test/emit-clear-frame.mjs" >&2
        exit 1
    fi
    committed=$((committed + 1))
done
if (( committed < 3 )); then
    echo "FAIL: the emitter wrote $committed frames; it writes two flat ones and one scissored." >&2
    exit 1
fi
echo "the $committed committed frames still come out of the emitter byte for byte"

# --- the synchronous barrier's argument record, across the two languages -----
#
# Same shape, same reason. The producer encodes `readPixels`' arguments and
# `frame_wire::sync::ReadPixelsParams::decode` reads them; both were written
# from the same table in contracts/frame-wire/wire-v1.md, which is the right way
# to write them and no evidence at all that they agree. A table can be read two
# ways, and the way that disagreement reaches a user is a `readPixels` answered
# over the wrong rectangle -- a picture that is subtly not the one the game
# asked for, which nothing but a screenshot would catch.

SYNC_PARAMS="$(mktemp -d)"
trap 'rm -rf "$PACKETS" "$SYNC_PARAMS"' EXIT

node platforms/apple/WebContent/PerformancePlus/test/emit-sync-params.mjs "$SYNC_PARAMS" 64

emitted_params="$(find "$SYNC_PARAMS" -name 'params-*.bin' | wc -l)"
if (( emitted_params < 64 )); then
    echo "FAIL: the emitter wrote $emitted_params argument records, expected 64." >&2
    exit 1
fi

output="$(cd engine && MIGO_JS_SYNC_PARAM_DIR="$SYNC_PARAMS" \
    cargo test -p migo-frame-wire --test sync_js_interop -- --ignored --nocapture 2>&1)"
status=$?
printf '%s\n' "$output" | grep -E 'decoded [0-9]+ JavaScript-encoded readPixels|test result' || true
if (( status != 0 )); then
    printf '%s\n' "$output" >&2
    echo "FAIL: the Rust decoder rejected argument records built by the JavaScript producer." >&2
    exit 1
fi
if ! printf '%s\n' "$output" | grep -qE 'decoded 64 JavaScript-encoded readPixels'; then
    echo "FAIL: the interop test did not report decoding 64 records; it may not have run." >&2
    exit 1
fi
