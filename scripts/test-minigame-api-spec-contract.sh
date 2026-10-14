#!/usr/bin/env bash
# The API spec is what the platform's reference says, for every API the platform has.
#
# contracts/runtime/minigame-api-spec.json records, per API of the common mini-game platform, the facts its reference
# page states -- how it is called, Promise support, the fields of its result, return value or listener argument, the
# scope it needs, which of the reference's clients support it. It is what the verification lane checks an
# implementation against, so a spec that drifted from the reference, or that misses an API the platform has, would
# have the lane verify against the wrong thing.
#
# So it is generated (scripts/gen-minigame-api-spec.py) from the reference's local mirror, docs/minigame-reference/
# (gitignored, fetched by hand). Where the mirror is here, this gate regenerates the spec and fails on any
# difference; everywhere, it checks that the spec covers minigame-api.json exactly and names a kind for each API.
# Then it proves it would fail on each drift it exists to catch.
#
# Host-only: reads files, runs no engine.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
GEN="$ROOT/scripts/gen-minigame-api-spec.py"
fail() { echo "FAIL: $*" >&2; exit 1; }

python3 "$GEN" --root "$ROOT" --check || fail "the API spec does not match the reference"

# ---- each drift turns the gate red ---------------------------------------------

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

inputs=(
    contracts/runtime/minigame-api.json
    contracts/runtime/minigame-api-spec.json
    engine/crates/shared/src/services/permission.rs
)
stage() {
    rm -rf "$TMP/tree"
    for input in "${inputs[@]}"; do
        mkdir -p "$TMP/tree/$(dirname "$input")"
        cp "$ROOT/$input" "$TMP/tree/$input"
    done
    if [[ -d "$ROOT/docs/minigame-reference" ]]; then
        mkdir -p "$TMP/tree/docs"
        ln -s "$ROOT/docs/minigame-reference" "$TMP/tree/docs/minigame-reference"
    fi
}

red() {
    local what="$1" file="$2" edit="$3"
    stage
    python3 - "$TMP/tree/$file" "$edit" <<'PY'
import json, sys
path, edit = sys.argv[1], sys.argv[2]
document = json.load(open(path))
exec(edit, {"document": document})
open(path, "w").write(json.dumps(document, ensure_ascii=False, indent=2) + "\n")
PY
    local out
    if out="$(python3 "$GEN" --root "$TMP/tree" --check 2>&1)"; then
        fail "not red: $what"
    fi
    echo "  red: $what -- $(echo "$out" | tail -1)"
}

echo "minigame-api-spec: proving each drift turns it red"
red "an API the spec forgot" contracts/runtime/minigame-api-spec.json \
    'del document["apis"]["getWindowInfo"]'
red "an API the platform gained that the spec does not describe" contracts/runtime/minigame-api.json \
    'document["apis"]["getNothingAtAll"] = "base/system"'
red "an API described as a kind of call that does not exist" contracts/runtime/minigame-api-spec.json \
    'document["apis"]["getWindowInfo"]["kind"] = "maybe"'
if [[ -d "$ROOT/docs/minigame-reference" ]]; then
    red "a result field edited by hand" contracts/runtime/minigame-api-spec.json \
        'document["apis"]["getWindowInfo"]["result"][0][1] = "string"'
else
    echo "  (the reference's mirror is not here: the hand-edit proof needs it)"
fi

count="$(python3 -c 'import json,sys; print(len(json.load(open(sys.argv[1]))["apis"]))' "$ROOT/contracts/runtime/minigame-api-spec.json")"
echo "minigame-api-spec contract: PASS ($count APIs)"
