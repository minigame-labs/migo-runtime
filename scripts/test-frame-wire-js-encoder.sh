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
DOWN_TEST="platforms/apple/WebContent/PerformancePlus/test/downlink.test.mjs"
DOWN_SRC="platforms/apple/WebContent/PerformancePlus/src/downlink.mjs"
SESSION_TEST="platforms/apple/WebContent/PerformancePlus/test/frame-session.test.mjs"
SESSION_SRC="platforms/apple/WebContent/PerformancePlus/src/frame-session.mjs"
BOOTSTRAP_SRC="platforms/apple/WebContent/PerformancePlus/src/worker-bootstrap.mjs"
CONTROL_SRC="platforms/apple/WebContent/PerformancePlus/src/control.mjs"
SRC_DIR="platforms/apple/WebContent/PerformancePlus/src"
TEST_DIR="platforms/apple/WebContent/PerformancePlus/test"
# Every suite this gate runs, collected as it runs them, so the check at the
# bottom can compare against what is on disk. See there for why.
RAN_TESTS=()

for required in "$TEST" "$ENCODER" "$SYNC_TEST" "$SYNC_SRC" "$RELAY_TEST" "$RELAY_SRC" \
                "$DOWN_TEST" "$DOWN_SRC" "$SESSION_TEST" "$SESSION_SRC" "$BOOTSTRAP_SRC" \
                "$CONTROL_SRC"; do
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
# A sibling in this same directory is not a dependency -- `sync-relay.mjs`
# imports `sync-mailbox.mjs` and both ship together -- so what is forbidden is
# an import that reaches OUT of the producer: a package name, a parent
# directory, a URL, a Node builtin. The rule was once "no imports at all",
# which was true of the one file it was applied to and would have refused the
# split the producer has since grown.
# DERIVED, not listed. It was a list of six names, and the producer then grew
# `uplink.mjs`, `page-entry.mjs` and `producer-worker.mjs` -- three modules that
# ship into WebContent beside untrusted content and that this firewall did not
# cover, because nobody remembered to add three lines. A firewall with a
# hand-written membership list protects the files somebody remembered.
#
# `find -maxdepth 1`, so a subdirectory somebody adds later is a file this loop
# does not see -- which would be the same failure again. There are none today,
# and the check below says so rather than trusting it.
if find "$SRC_DIR" -mindepth 1 -maxdepth 1 -type d | grep -q .; then
    echo "FAIL: $SRC_DIR has a subdirectory, and this gate only walks its top level." >&2
    echo "      Either flatten it or teach this loop to recurse; a module the firewall" >&2
    echo "      does not see is a module that can import anything." >&2
    exit 1
fi
SHIPPED_SOURCES=()
while IFS= read -r found; do
    SHIPPED_SOURCES+=("$found")
done < <(find "$SRC_DIR" -maxdepth 1 -name '*.mjs' | sort)
if (( ${#SHIPPED_SOURCES[@]} == 0 )); then
    echo "FAIL: no producer modules found under $SRC_DIR." >&2
    exit 1
fi

for shipped in ${SHIPPED_SOURCES[@]+"${SHIPPED_SOURCES[@]}"}; do
    outside="$(grep -nE "^\s*(import|export)\b.*\bfrom\s+[\"']" "$shipped" \
        | grep -vE "from\s+[\"']\./[A-Za-z0-9_.-]+\.mjs[\"']" || true)"
    if [[ -n "$outside" ]]; then
        echo "FAIL: $shipped imports from outside the producer. It runs in WebContent" >&2
        echo "      beside untrusted content; only ./sibling.mjs is allowed, and the test" >&2
        echo "      harness does the I/O." >&2
        printf '%s\n' "$outside" >&2
        exit 1
    fi
done

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
RAN_TESTS+=("$TEST")

# --- the synchronous barrier's producer half --------------------------------
#
# Same rule as the corpus above and for the same reason: this side is checked
# against contracts/frame-wire/wire-v1.md, not against the Rust mailbox, because
# two implementations that agree with each other and not with the document is
# the failure the document exists to catch. It also runs a real `Atomics.wait`
# woken by a real worker -- "it blocks" is the entire claim, and a test whose
# host answered before the wait began would exercise every line except that one.
node "$SYNC_TEST"
RAN_TESTS+=("$SYNC_TEST")

# --- and the two halves against each other ----------------------------------
#
# Each half being correct alone is not the property that matters. This runs a
# real Worker blocked in `Atomics.wait` and a real relay answering it, because
# what has to hold is that a blocked agent is always woken and never with the
# wrong bytes -- and neither half can establish that by itself.
node "$RELAY_TEST"
RAN_TESTS+=("$RELAY_TEST")

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

FIXTURES="platforms/apple/Sources/MigoAppleFrameHarness/Fixtures"
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
if (( committed < 4 )); then
    echo "FAIL: the emitter wrote $committed frames; it writes two flat, one scissored and one 2D." >&2
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

# Filtered to its own test: the file also holds the one-body call tests below,
# which need directories this step does not make.
output="$(cd engine && MIGO_JS_SYNC_PARAM_DIR="$SYNC_PARAMS" \
    cargo test -p migo-frame-wire --test sync_js_interop -- --ignored --nocapture \
    read_pixels_arguments_from_the_javascript_producer 2>&1)"
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

# --- the WebGL queries' arguments -------------------------------------------
#
# A query is three numbers and a name, and every one of them is a way to ask
# about the wrong thing: an object id taken from the wrong word asks about
# another program, and a name length read as characters rather than bytes
# truncates a uniform's name into one nothing has. Both are ANSWERED rather than
# refused -- `getUniformLocation` returns -1 for a name that does not exist --
# so the failure is a uniform that silently does nothing in a frame that draws.
GL_QUERIES="$(mktemp -d)"
trap 'rm -rf "$PACKETS" "$SYNC_PARAMS" "$GL_QUERIES"' EXIT

node platforms/apple/WebContent/PerformancePlus/test/emit-gl-query-params.mjs "$GL_QUERIES" 60
emitted_queries="$(find "$GL_QUERIES" -name 'query-*.bin' | wc -l)"
if (( emitted_queries < 60 )); then
    echo "FAIL: the emitter wrote $emitted_queries query records, expected 60." >&2
    exit 1
fi

output="$(cd engine && MIGO_JS_GL_QUERY_DIR="$GL_QUERIES" \
    cargo test -p migo-frame-wire --test sync_js_interop -- --ignored --nocapture \
    gl_query_arguments_from_the_javascript_producer 2>&1)"
status=$?
printf '%s\n' "$output" | grep -E 'read [0-9]+ JavaScript-encoded query records|test result' || true
if (( status != 0 )); then
    printf '%s\n' "$output" >&2
    echo "FAIL: the Rust decoder rejected query records built by the JavaScript producer." >&2
    exit 1
fi
if ! printf '%s\n' "$output" | grep -qE 'read 15 JavaScript-encoded query records'; then
    echo "FAIL: the query interop did not report every kind; it may not have run." >&2
    exit 1
fi

# The Canvas2D queries' arguments, whose two strings are where a pair of
# payloads goes wrong: a second length read from the first's unpadded end takes
# the font out of the middle of the text, and `measureText` then measures a
# string nobody passed -- answered, not refused.
output="$(cd engine && MIGO_JS_GL_QUERY_DIR="$GL_QUERIES" \
    cargo test -p migo-frame-wire --test sync_js_interop -- --ignored --nocapture \
    canvas2d_query_arguments_from_the_javascript_producer 2>&1)"
status=$?
printf '%s\n' "$output" | grep -E 'read [0-9]+ JavaScript-encoded Canvas2D query records|test result' || true
if (( status != 0 )); then
    printf '%s\n' "$output" >&2
    echo "FAIL: the Rust decoder rejected Canvas2D query records built by the producer." >&2
    exit 1
fi
if ! printf '%s\n' "$output" | grep -qE 'read [1-9][0-9]* JavaScript-encoded Canvas2D query records'; then
    echo "FAIL: the Canvas2D query interop reported nothing; it may not have run." >&2
    exit 1
fi

# --- the synchronous call as one body, in both directions --------------------
#
# The Apple lane's content origin has no SharedArrayBuffer, so a readback there
# is a Worker's synchronous request: `sync-call.mjs` encodes the call body and
# decodes the answer body, and `frame_wire::sync::{SyncCall, SyncAnswer}` are the
# host's halves. The unit suite checks the JavaScript against the document's
# tables; this puts bytes through both languages, because two halves each
# faithful to a table can still read it two ways -- and the way that reaches a
# user is a `readPixels` answered with another field's value.
SYNC_CALL_TEST="$TEST_DIR/sync-call.test.mjs"
node "$SYNC_CALL_TEST"
RAN_TESTS+=("$SYNC_CALL_TEST")

SYNC_CALLS="$(mktemp -d)"
SYNC_ANSWERS="$(mktemp -d)"
trap 'rm -rf "$PACKETS" "$SYNC_PARAMS" "$REGENERATED" "$SYNC_CALLS" "$SYNC_ANSWERS"' EXIT
node "$TEST_DIR/emit-sync-calls.mjs" write "$SYNC_CALLS"
status=0
output="$(cd engine && MIGO_JS_SYNC_CALL_DIR="$SYNC_CALLS" \
    cargo test -p migo-frame-wire --test sync_js_interop -- \
    --ignored --nocapture calls_from_the_javascript_producer 2>&1)" || status=$?
printf '%s\n' "$output" | grep -E 'decoded [0-9]+ JavaScript-encoded synchronous calls|test result' || true
if (( status != 0 )); then
    printf '%s\n' "$output" >&2
    echo "FAIL: the Rust decoder rejected call bodies built by the JavaScript producer." >&2
    exit 1
fi
if ! printf '%s\n' "$output" | grep -qE 'decoded 64 JavaScript-encoded synchronous calls'; then
    echo "FAIL: the call interop test did not report decoding 64 bodies; it may not have run." >&2
    exit 1
fi
status=0
output="$(cd engine && MIGO_SYNC_ANSWER_OUT_DIR="$SYNC_ANSWERS" \
    cargo test -p migo-frame-wire --test sync_js_interop -- \
    --ignored --nocapture the_rust_host_writes_answers 2>&1)" || status=$?
printf '%s\n' "$output" | grep -E 'wrote [0-9]+ synchronous answers|test result' || true
if (( status != 0 )); then
    printf '%s\n' "$output" >&2
    echo "FAIL: the Rust host could not write the answer corpus." >&2
    exit 1
fi
node "$TEST_DIR/emit-sync-calls.mjs" read "$SYNC_ANSWERS"

# --- the host-to-producer direction, in both directions ----------------------
#
# `downlink.mjs` reads what `frame_wire::downlink` writes: per-frame verdicts
# and frame-clock ticks, over the loopback socket. In production only one
# direction runs -- the host writes, the producer reads -- and that is exactly
# why both are checked here. A pair of implementations that agree only in the
# direction somebody remembered to test is one implementation with extra steps.
#
# The corpus is one list, held twice (`downlink_js_interop.rs` and
# `emit-downlink.mjs`), and each side asserts the other's length. A corpus that
# grows on one side only therefore fails rather than quietly covering less.

node "$DOWN_TEST"
RAN_TESTS+=("$DOWN_TEST")

# The credit accounting and the frame clock, on bytes this repository's own
# encoder produced -- so a failure here is about what the producer DOES with a
# message rather than about what a message is.
node "$SESSION_TEST"
RAN_TESTS+=("$SESSION_TEST")

# --- which uplink a frame leaves on -----------------------------------------
#
# Above `MigoFrameChannelPolicy.socketCeilingBytes` the producer POSTs to the
# content origin instead of sending on the socket, because G0's P3 measured the
# scheme 4.4x faster and 4.6x cheaper at 1 MiB and the socket better below.
#
# What this suite pins is the boundary -- at the ceiling the socket, one byte
# over the scheme -- and that the threshold is the HOST's. The producer carries
# no copy of it: a constant in both languages would drift the first time the
# measurement is redone on new hardware, silently, because each half would still
# be self-consistent. The suite asserts a scheme URL without a ceiling is
# refused rather than defaulted, which is what keeps that true.
UPLINK_TEST="$TEST_DIR/uplink.test.mjs"
node "$UPLINK_TEST"
RAN_TESTS+=("$UPLINK_TEST")

DOWN_FROM_JS="$(mktemp -d)"
DOWN_FROM_RUST="$(mktemp -d)"
trap 'rm -rf "$PACKETS" "$SYNC_PARAMS" "$REGENERATED" "$SYNC_CALLS" "$SYNC_ANSWERS" "$DOWN_FROM_JS" "$DOWN_FROM_RUST"' EXIT

node platforms/apple/WebContent/PerformancePlus/test/emit-downlink.mjs write "$DOWN_FROM_JS"

output="$(cd engine && MIGO_DOWNLINK_IN_DIR="$DOWN_FROM_JS" \
    cargo test -p migo-frame-wire --test downlink_js_interop -- \
    --ignored --nocapture messages_from 2>&1)" || status=$?
status=${status:-0}
printf '%s\n' "$output" | grep -E 'read [0-9]+ JavaScript-encoded downlink messages|test result' || true
if (( status != 0 )); then
    printf '%s\n' "$output" >&2
    echo "FAIL: the Rust reader rejected downlink messages built by the JavaScript writer." >&2
    exit 1
fi
if ! printf '%s\n' "$output" | grep -qE 'read [0-9]+ JavaScript-encoded downlink messages'; then
    echo "FAIL: the downlink interop test did not report reading anything; it may not have run." >&2
    exit 1
fi

status=0
output="$(cd engine && MIGO_DOWNLINK_OUT_DIR="$DOWN_FROM_RUST" \
    cargo test -p migo-frame-wire --test downlink_js_interop -- \
    --ignored --nocapture the_rust_writer 2>&1)" || status=$?
printf '%s\n' "$output" | grep -E 'wrote [0-9]+ downlink messages|test result' || true
if (( status != 0 )); then
    printf '%s\n' "$output" >&2
    echo "FAIL: the Rust writer could not produce the downlink corpus." >&2
    exit 1
fi

# The direction production actually runs: the producer reading what the host
# wrote. A failure here is the one a device would show as a frame clock that
# stops ticking, with nothing in either log saying why.
node platforms/apple/WebContent/PerformancePlus/test/emit-downlink.mjs read "$DOWN_FROM_RUST"

# --- the uplink's control messages, in both directions -----------------------
#
# The producer asks for every frame with a control message on the socket, and
# `frame_wire::control` reads it. In production only JavaScript writes and Rust
# reads; a request the host refuses or misreads is a producer waiting for a tick
# nobody arms, which on a device is a game that draws one frame and stops. The
# JavaScript half is checked byte for byte against the Rust reference writer,
# including through the buffer the producer reuses every frame.
CONTROL_TEST="$TEST_DIR/control.test.mjs"
node "$CONTROL_TEST"
RAN_TESTS+=("$CONTROL_TEST")

CONTROL_FROM_JS="$(mktemp -d)"
CONTROL_FROM_RUST="$(mktemp -d)"
trap 'rm -rf "$PACKETS" "$SYNC_PARAMS" "$REGENERATED" "$SYNC_CALLS" "$SYNC_ANSWERS" "$DOWN_FROM_JS" "$DOWN_FROM_RUST" "$CONTROL_FROM_JS" "$CONTROL_FROM_RUST"' EXIT

node "$TEST_DIR/emit-control.mjs" write "$CONTROL_FROM_JS"
status=0
output="$(cd engine && MIGO_CONTROL_IN_DIR="$CONTROL_FROM_JS" \
    cargo test -p migo-frame-wire --test control_js_interop -- \
    --ignored --nocapture control_messages_from 2>&1)" || status=$?
printf '%s\n' "$output" | grep -E 'read [0-9]+ JavaScript-encoded control messages|test result' || true
if (( status != 0 )); then
    printf '%s\n' "$output" >&2
    echo "FAIL: the Rust reader refused control messages written by the JavaScript producer." >&2
    exit 1
fi
if ! printf '%s\n' "$output" | grep -qE 'read [0-9]+ JavaScript-encoded control messages'; then
    echo "FAIL: the control interop test did not report reading anything; it may not have run." >&2
    exit 1
fi

status=0
output="$(cd engine && MIGO_CONTROL_OUT_DIR="$CONTROL_FROM_RUST" \
    cargo test -p migo-frame-wire --test control_js_interop -- \
    --ignored --nocapture the_rust_writer 2>&1)" || status=$?
printf '%s\n' "$output" | grep -E 'wrote [0-9]+ control messages|test result' || true
if (( status != 0 )); then
    printf '%s\n' "$output" >&2
    echo "FAIL: the Rust writer could not produce the control corpus." >&2
    exit 1
fi
node "$TEST_DIR/emit-control.mjs" read "$CONTROL_FROM_RUST"

# --- the service stream, in both directions ----------------------------------
#
# Everything content asks the host to do that is not drawing -- a file read, a
# storage write, an image load -- travels as `MUS1` and is answered as `MDS1`.
# The producer writes one and reads the other, and a disagreement reaches a user
# as a write the host reads under another key or a read answered with another
# request's bytes. The node suite covers what only the producer has (batching
# per task, the hybrid send, parked answers, a refusal breaking the stream); the
# corpus is checked byte for byte against the Rust writer, and the Rust answers
# are read back by the producer's reader.
SERVICE_TEST="$TEST_DIR/service.test.mjs"
node "$SERVICE_TEST"
RAN_TESTS+=("$SERVICE_TEST")

SERVICE_FROM_JS="$(mktemp -d)"
SERVICE_FROM_RUST="$(mktemp -d)"
trap 'rm -rf "$PACKETS" "$SYNC_PARAMS" "$REGENERATED" "$SYNC_CALLS" "$SYNC_ANSWERS" "$DOWN_FROM_JS" "$DOWN_FROM_RUST" "$CONTROL_FROM_JS" "$CONTROL_FROM_RUST" "$SERVICE_FROM_JS" "$SERVICE_FROM_RUST"' EXIT

node "$TEST_DIR/emit-service.mjs" write "$SERVICE_FROM_JS"
status=0
output="$(cd engine && MIGO_SERVICE_IN_DIR="$SERVICE_FROM_JS" \
    cargo test -p migo-frame-wire --test service_js_interop -- \
    --ignored --nocapture service_messages_from 2>&1)" || status=$?
printf '%s\n' "$output" | grep -E 'read [0-9]+ JavaScript-encoded service messages|test result' || true
if (( status != 0 )); then
    printf '%s\n' "$output" >&2
    echo "FAIL: the Rust reader refused service messages written by the JavaScript producer." >&2
    exit 1
fi
if ! printf '%s\n' "$output" | grep -qE 'read [1-9][0-9]* JavaScript-encoded service messages'; then
    echo "FAIL: the service interop test did not report reading anything; it may not have run." >&2
    exit 1
fi

status=0
output="$(cd engine && MIGO_SERVICE_OUT_DIR="$SERVICE_FROM_RUST" \
    cargo test -p migo-frame-wire --test service_js_interop -- \
    --ignored --nocapture the_rust_writer 2>&1)" || status=$?
printf '%s\n' "$output" | grep -E 'wrote [0-9]+ service answer messages|test result' || true
if (( status != 0 )); then
    printf '%s\n' "$output" >&2
    echo "FAIL: the Rust writer could not produce the service answer corpus." >&2
    exit 1
fi
node "$TEST_DIR/emit-service.mjs" read "$SERVICE_FROM_RUST"

# --- an op's arguments, as deno_core converts them --------------------------
#
# The stream lane answers ops whose Rust bodies see arguments deno_core has
# already converted: -1 as a u32 is 0xFFFFFFFF, 1.9 is 1, a BigInt keeps its low
# bits, a string where a number belongs is a TypeError. A lane that converted
# differently would turn a call that is an error in one runtime into a different
# call in the other, and nothing downstream could tell.
node "$TEST_DIR/op-args.test.mjs"
RAN_TESTS+=("$TEST_DIR/op-args.test.mjs")

# --- a read larger than one service answer ----------------------------------
#
# The file lanes read into a caller's buffer in pieces, so no answer approaches
# the synchronous reply ceiling or the host's outbox bound. The pieces have to
# be the embedded op's one read: continuing by position or by cursor, stopping
# at end of file, and asking once even for nothing.
node "$TEST_DIR/files.test.mjs"
RAN_TESTS+=("$TEST_DIR/files.test.mjs")

# --- what an audio command refuses before it is sent ------------------------
#
# Audio plays on the host, and almost every audio op is a command: nothing
# answers it, so an argument the embedded op throws for has to be refused by
# the producer, before it leaves, with the same class and words. The answers
# are generated from `migo_services::audio`'s own checks (the Rust test
# `the_producer_s_command_checks_answer_as_these_do` fails when they drift), and
# the strings are measured three ways, because the Rust check reads UTF-8 bytes
# and a producer that counted UTF-16 units would pass every ASCII case.
node "$TEST_DIR/audio-checks.test.mjs"
RAN_TESTS+=("$TEST_DIR/audio-checks.test.mjs")

# --- what a socket event is on both sides -----------------------------------
#
# A WebSocket cannot ride the record-and-replay harness the fetch calls do: a
# replay would need a server, and every address one could listen on is one the
# address filter refuses. So the shape the two halves must agree about -- the
# tagged event -- is pinned by a fixture the host writes
# (`the_producer_s_socket_events_are_the_ones_this_writes`) and the producer
# rebuilds the facade's object from here.
node "$TEST_DIR/ws-events.test.mjs"
RAN_TESTS+=("$TEST_DIR/ws-events.test.mjs")

# --- a frame larger than one packet, and what the host will decode ----------
#
# The host refuses a packet whose decoded storage is over its budget, and on the
# Apple lane that ends the content. The producer cannot know the host's type
# sizes, so it estimates with the contract's upper bounds and splits frames into
# barriers against that. Two things have to hold and neither side can show them
# alone: the producer's estimate is EXACTLY the Rust formula over those bounds
# (a drift toward caution would pass a weaker "at least" check and then split
# frames for no reason), and the packets it splits a frame into are admitted in
# order, each within budget, with every 2D record decoding against a selected
# canvas. The node suite writes streams and split packets; the Rust test reads
# them, and the counts it prints are asserted so a run that checked nothing
# cannot pass.
ENGINE_FRAMES_TEST="$TEST_DIR/engine-frames.test.mjs"
ENGINE_FRAMES_OUT="$(mktemp -d)"
trap 'rm -rf "$PACKETS" "$SYNC_PARAMS" "$REGENERATED" "$ENGINE_FRAMES_OUT"' EXIT
node "$ENGINE_FRAMES_TEST" "$ENGINE_FRAMES_OUT"
RAN_TESTS+=("$ENGINE_FRAMES_TEST")
status=0
output="$(cd engine && MIGO_ENGINE_FRAMES_TEST_DIR="$ENGINE_FRAMES_OUT" \
    cargo test -p migo-frame-decode --test decode_budget_js_agreement -- --ignored --nocapture 2>&1)" \
    || status=$?
printf '%s\n' "$output" | grep -E 'producer estimates agree|admitted [0-9]+ barriers|test result' || true
if (( status != 0 )); then
    printf '%s\n' "$output" >&2
    echo "FAIL: the producer's decode estimate or its split frames disagree with the host." >&2
    exit 1
fi
if ! printf '%s\n' "$output" | grep -qE '[0-9]+ producer estimates agree, [1-9][0-9]* of them over budget'; then
    echo "FAIL: the estimate agreement did not report checking streams over the budget." >&2
    exit 1
fi
if ! printf '%s\n' "$output" | grep -qE 'admitted [1-9][0-9]* barriers and [1-9][0-9]* presenting packets'; then
    echo "FAIL: the split-frame check did not report admitting barriers; it may not have run." >&2
    exit 1
fi

# --- the engine's API layer on the producer ---------------------------------
#
# `engine-bundle.test.mjs` needs the staged engine and a Rust check of the frames
# it draws, so scripts/test-performance-plus-engine-contract.sh runs it with
# both. Named here so the coverage check below does not run it a second time
# without the half that reads its output.
RAN_TESTS+=("$TEST_DIR/engine-bundle.test.mjs")
# The same for `host-events.test.mjs`: it delivers events the Rust host encodes
# to the staged engine's bridge, so the engine contract runs it with both.
RAN_TESTS+=("$TEST_DIR/host-events.test.mjs")

# --- the producer's own suites: run the named ones, then prove that was all ---
#
# Each `node "$..."` above has a paragraph saying what that suite establishes,
# which is worth keeping and is exactly what a `for` loop over the directory
# would throw away. What a hand-written list loses instead is the file somebody
# adds later: `uplink.test.mjs` was written, committed, and run by nobody,
# because adding it here is a step with no failure attached to forgetting it.
#
# So: narrative by hand, coverage derived. A suite on disk that this gate never
# ran fails here by name.
UNRUN=()
while IFS= read -r suite; do
    found=0
    for ran in ${RAN_TESTS[@]+"${RAN_TESTS[@]}"}; do
        [[ "$ran" == "$suite" ]] && found=1 && break
    done
    if (( found == 0 )); then
        node "$suite"
        UNRUN+=("$suite")
    fi
done < <(find "$TEST_DIR" -maxdepth 1 -name '*.test.mjs' | sort)

if (( ${#UNRUN[@]} > 0 )); then
    echo
    echo "NOTE: these suites are not named in this gate and were run generically:" >&2
    printf '        %s\n' ${UNRUN[@]+"${UNRUN[@]}"} >&2
    echo "      They passed. Give each one a paragraph above saying what it establishes," >&2
    echo "      the way the others have -- a suite nobody can say the purpose of is one" >&2
    echo "      nobody will notice going quiet." >&2
fi
