#!/usr/bin/env bash
# The APIs the engine treats as having no Promise style are the ones the reference says have none.
#
# Every asynchronous `migo.*` call that carries no callbacks returns a Promise. Where the reference gives an API no
# Promise style ("not supported" for calling it that way, on its page; `promise: false` in
# contracts/runtime/minigame-api-spec.json), content written for the reference does not look at what the call
# returns, so the engine does not report that Promise's rejection as unhandled -- with no rejection listener an
# unhandled rejection reaches the game's `onError`, and every such call that failed used to arrive there as an error
# the platform never raises. Which APIs those are is a list in base/02_async.js (`WITHOUT_PROMISE_STYLE`), and a
# list kept by hand beside a generated spec drifts: an API the spec gains is reported again, one it loses is
# silenced. This holds the two to the same names.
#
# Host-only: reads files, runs no engine.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SPEC=contracts/runtime/minigame-api-spec.json
ASYNC=engine/crates/runtime-v8/src/base/02_async.js
fail() { echo "FAIL: $*" >&2; exit 1; }

# Prints nothing and succeeds when the list in <root>/$ASYNC names exactly the spec's callback APIs without a
# Promise style; otherwise says what differs and fails.
check() {
    python3 - "$1/$SPEC" "$1/$ASYNC" <<'PY'
import json, re, sys
spec_path, async_path = sys.argv[1], sys.argv[2]
apis = json.load(open(spec_path))["apis"]
expected = {name for name, api in apis.items() if api.get("kind") == "callback" and api.get("promise") is False}
source = open(async_path).read()
found = re.search(r"var WITHOUT_PROMISE_STYLE = new Set\(\[(.*?)\]\);", source, re.S)
if not found:
    sys.exit(f"{async_path}: no `var WITHOUT_PROMISE_STYLE = new Set([...]);` to read")
listed = re.findall(r'"([A-Za-z0-9_]+)"', found.group(1))
problems = []
duplicates = sorted({name for name in listed if listed.count(name) > 1})
if duplicates:
    problems.append("listed twice: " + ", ".join(duplicates))
missing = sorted(expected - set(listed))
if missing:
    problems.append("the reference gives no Promise style, the list leaves out: " + ", ".join(missing))
extra = sorted(set(listed) - expected)
if extra:
    problems.append("listed, but the reference gives a Promise style or names no such API: " + ", ".join(extra))
if problems:
    sys.exit("; ".join(problems))
PY
}

check "$ROOT" || fail "base/02_async.js's WITHOUT_PROMISE_STYLE does not match the API spec"

# ---- each drift turns the gate red ---------------------------------------------

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

stage() {
    rm -rf "$TMP/tree"
    for input in "$SPEC" "$ASYNC"; do
        mkdir -p "$TMP/tree/$(dirname "$input")"
        cp "$ROOT/$input" "$TMP/tree/$input"
    done
}

# red <what> <file> <python edit of `text`>
red() {
    local what="$1" file="$2" edit="$3" out
    stage
    python3 - "$TMP/tree/$file" "$edit" <<'PY'
import sys
path, edit = sys.argv[1], sys.argv[2]
scope = {"text": open(path).read()}
exec(edit, scope)
open(path, "w").write(scope["text"])
PY
    if out="$(check "$TMP/tree" 2>&1)"; then
        fail "not red: $what"
    fi
    echo "  red: $what -- $(echo "$out" | tail -1)"
}

echo "promise-style: proving each drift turns it red"
red "an API the reference gives no Promise style, left out of the list" "$ASYNC" \
    'text = text.replace("\"reportScene\", ", "", 1)'
red "an API with a Promise style, added to the list" "$ASYNC" \
    'text = text.replace("var WITHOUT_PROMISE_STYLE = new Set([", "var WITHOUT_PROMISE_STYLE = new Set([\n    \"getStorage\",", 1)'
red "a name listed twice" "$ASYNC" \
    'text = text.replace("var WITHOUT_PROMISE_STYLE = new Set([", "var WITHOUT_PROMISE_STYLE = new Set([\n    \"reportScene\",", 1)'
red "an API the spec newly gives no Promise style" "$SPEC" \
    'import json; d = json.loads(text); d["apis"]["getStorage"]["promise"] = False; text = json.dumps(d, ensure_ascii=False, indent=2) + "\n"'
red "the list renamed out from under the gate" "$ASYNC" \
    'text = text.replace("var WITHOUT_PROMISE_STYLE", "var NO_PROMISE_STYLE", 1)'

count="$(python3 -c 'import json,sys; d=json.load(open(sys.argv[1]))["apis"]; print(sum(1 for a in d.values() if a.get("kind")=="callback" and a.get("promise") is False))' "$ROOT/$SPEC")"
echo "promise-style contract: PASS ($count APIs without a Promise style)"
