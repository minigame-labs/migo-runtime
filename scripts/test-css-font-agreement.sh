#!/usr/bin/env bash
# Two parsers for `ctx.font`, held to one corpus.
#
# `ctx.font = "16px sans-serif"` answers whether the shorthand parsed, and on
# the Apple Performance+ lane the host's answer is a frame away -- so the
# contract gives `op_set_font` a LOCAL answer, which means the producer parses
# the shorthand itself (`platforms/apple/WebContent/PerformancePlus/src/css-font.mjs`)
# while the host parses it again to make a typeface
# (`engine/crates/shared/src/css_font_shorthand.rs`).
#
# THE DRIFT THIS EXISTS TO CATCH is silent in both directions. A shorthand the
# producer accepts and the host refuses is a font that never applies, with
# content told it did. One the producer refuses and the host would have taken is
# a `ctx.font =` that content sees fail while the frame draws the text anyway.
# Neither raises anything; both need a screenshot to notice.
#
# So: one corpus, both parsers, and a diff. Host-only: node and cargo.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

CORPUS="contracts/text/css-font-shorthand-corpus.txt"
PRODUCER="platforms/apple/WebContent/PerformancePlus/test/emit-css-font.mjs"
for required in "$CORPUS" "$PRODUCER"; do
    if [[ ! -f "$required" ]]; then
        echo "FAIL: $required is missing; the two parsers cannot be compared." >&2
        exit 1
    fi
done

entries="$(grep -vcE '^#|^[[:space:]]*$' "$CORPUS" || true)"
# A corpus that shrank to nothing would make this gate pass while checking
# nothing, which is the shape this repository keeps finding.
if (( entries < 30 )); then
    echo "FAIL: the corpus has $entries entries; it is meant to cover the grammar." >&2
    exit 1
fi

OUT="$(mktemp -d)"
trap 'rm -rf "$OUT"' EXIT

node "$PRODUCER" "$CORPUS" > "$OUT/producer.txt"

status=0
output="$(cd engine && MIGO_CSS_FONT_CORPUS="$ROOT/$CORPUS" MIGO_CSS_FONT_OUT="$OUT/host.txt" \
    cargo test -p migo-shared --test css_font_corpus -- --ignored --nocapture 2>&1)" || status=$?
if (( status != 0 )); then
    printf '%s\n' "$output" >&2
    echo "FAIL: the host's parser did not produce its verdicts." >&2
    exit 1
fi
printf '%s\n' "$output" | grep -E 'wrote [0-9]+ font verdicts' || true

produced="$(wc -l < "$OUT/producer.txt")"
if (( produced != entries )); then
    echo "FAIL: the producer answered $produced of $entries corpus entries." >&2
    exit 1
fi

if ! diff -u "$OUT/host.txt" "$OUT/producer.txt" > "$OUT/diff.txt"; then
    echo "FAIL: the two font parsers disagree (host on the left, producer on the right)." >&2
    sed -n '1,40p' "$OUT/diff.txt" >&2
    exit 1
fi

echo
echo "css font agreement: PASS -- $entries shorthands, one verdict each, from both parsers."
