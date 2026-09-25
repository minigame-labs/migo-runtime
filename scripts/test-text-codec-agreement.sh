#!/usr/bin/env bash
# Two text codecs, held to one corpus.
#
# `migo.encodeMultiFormats(text, "ucs2")` is synchronous and takes no host
# resource, so the op boundary answers it on the producer -- which means the
# conversion happens twice in this repository: `shared::codec` for every other
# platform, and `text-codec.mjs` for the Apple Performance+ lane.
#
# THE DRIFT THIS EXISTS TO CATCH is a game that writes a save file on one
# platform and cannot read it on another. `ucs2` here is UTF-16 BIG-endian with
# a BOM, `ascii` masks rather than refuses, `latin1` refuses rather than masks,
# and `decode(base64)` encodes. Every one of those is a choice someone made
# once, none of them is what a second implementation would guess, and all of
# them are silent: the bytes differ, nothing raises.
#
# Host-only: node and cargo.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

CORPUS="contracts/text/codec-corpus.txt"
PRODUCER="platforms/apple/WebContent/PerformancePlus/test/emit-text-codec.mjs"
for required in "$CORPUS" "$PRODUCER"; do
    if [[ ! -f "$required" ]]; then
        echo "FAIL: $required is missing; the two codecs cannot be compared." >&2
        exit 1
    fi
done

entries="$(grep -vcE '^#|^[[:space:]]*$' "$CORPUS" || true)"
if (( entries < 25 )); then
    echo "FAIL: the corpus has $entries cases; it is meant to cover the table." >&2
    exit 1
fi

OUT="$(mktemp -d)"
trap 'rm -rf "$OUT"' EXIT

node "$PRODUCER" "$CORPUS" > "$OUT/producer.txt"

status=0
output="$(cd engine && MIGO_CODEC_CORPUS="$ROOT/$CORPUS" MIGO_CODEC_OUT="$OUT/host.txt" \
    cargo test -p migo-shared --features codec-gbk --test codec_corpus -- --ignored --nocapture 2>&1)" || status=$?
if (( status != 0 )); then
    printf '%s\n' "$output" >&2
    echo "FAIL: the host's codec did not produce its answers." >&2
    exit 1
fi
printf '%s\n' "$output" | grep -E 'wrote [0-9]+ codec answers' || true

produced="$(wc -l < "$OUT/producer.txt")"
if (( produced != entries )); then
    echo "FAIL: the producer answered $produced of $entries cases." >&2
    exit 1
fi

# The recorded differences, applied to the host's answers before the diff.
python3 scripts/lib/apply_codec_differences.py "$CORPUS" "$OUT/host.txt"

if ! diff -u "$OUT/host.txt" "$OUT/producer.txt" > "$OUT/diff.txt"; then
    echo "FAIL: the two codecs disagree (host on the left, producer on the right)." >&2
    sed -n '1,40p' "$OUT/diff.txt" >&2
    exit 1
fi

echo
echo "text codec agreement: PASS -- $entries cases, one answer each, from both codecs."
