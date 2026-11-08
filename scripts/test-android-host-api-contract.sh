#!/usr/bin/env bash
# The Android host API is frozen at v0: changing it must be deliberate.
#
# An embedder building against this SDK compiles against `com.migo.runtime` and
# ships that binding inside their app. A method that quietly changes shape costs
# them a release; one that disappears costs them a build. "We froze the host
# API" is a sentence that appears in licence conversations, and it is only true
# if something enforces it.
#
# The surface is read from the *compiled classes* with `javap`, not scraped from
# the sources. Source-scraping means inventing a rule for what looks public, and
# a rule like that is wrong in the direction that matters: it misses things, and
# what it misses is exactly what changes unnoticed. The classes are what an
# embedder links against, so they are what gets pinned.
#
# `internal` is excluded: it is the JNI plumbing, pinned separately and more
# strictly by `jni_profile_contract` since it must match on both sides of a
# native boundary.
#
# To change the surface on purpose:
#   bash scripts/test-android-host-api-contract.sh --update
# and commit the regenerated baseline with the change. Reviewing that diff is
# the point of the gate.
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BASELINE="$ROOT_DIR/platforms/android/host-api-v0.txt"
CLASSES="$ROOT_DIR/platforms/android/library/build/intermediates/javac/fullDebug/compileFullDebugJavaWithJavac/classes"
UPDATE=false

[[ "${1:-}" == "--update" ]] && UPDATE=true

# Compiled here rather than assumed, because the alternative is not "a clear
# error": bytecode left by an earlier build passes this gate against source that
# no longer exists. That is a false green in the one direction that matters -- a
# host API change reviewed as unchanged -- and it is invisible, where a missing
# directory at least announces itself. CI happened to be safe only because it
# runs `compileFullDebugJavaWithJavac` in the same step; the local lane derives
# the `bash ...` line alone, so it had both failure modes.
#
# `--offline --no-daemon` under the verifier for the reasons
# `test-camera-frame-jni-contract.sh` records: no reachable module repository
# there, and a daemon outliving its build still holds the project lock.
GRADLE_FLAGS=()
[[ -n "${MIGO_GRADLE_VERIFIER:-}" ]] && GRADLE_FLAGS+=(--offline --no-daemon)
( cd "$ROOT_DIR/platforms/android" && ./gradlew --quiet "${GRADLE_FLAGS[@]}" \
    :library:compileFullDebugJavaWithJavac ) \
    || { echo "ERROR: full debug Java compilation failed" >&2; exit 1; }

if [[ ! -d "$CLASSES" ]]; then
    echo "ERROR: compiled classes not found at $CLASSES" >&2
    echo "       :library:compileFullDebugJavaWithJavac reported success but wrote nothing" >&2
    exit 1
fi

command -v javap >/dev/null || { echo "ERROR: javap not on PATH" >&2; exit 1; }

surface="$(mktemp)"
host_api_diff="$(mktemp)"
trap 'rm -f "$surface" "$host_api_diff"' EXIT

# Public types under com.migo.runtime, excluding the internal plumbing and the
# synthetic inner classes the compiler emits (`Foo$1`), which are not API.
mapfile -t types < <(
    find "$CLASSES/com/migo/runtime" -name '*.class' \
        ! -path '*/internal/*' \
        ! -name '*$[0-9]*.class' \
        -printf '%P\n' \
    | sed 's/\.class$//' \
    | sed 's#/#.#g' \
    | sed 's/^/com.migo.runtime./' \
    | sort
)

if [[ ${#types[@]} -eq 0 ]]; then
    echo "ERROR: no public types found; the gate would pin an empty surface" >&2
    exit 1
fi

for type in "${types[@]}"; do
    # `-protected` covers public AND protected, and both are the embedder's.
    #
    # This ran `-public` and so froze nothing an embedder reaches by subclassing
    # -- which for `MigoGameActivity` is the documented integration path, the one
    # the README calls "zero-boilerplate": `onCreateGameListener`,
    # `onLaunchFailed`, `onSessionCreated`, `getGameSession` are all protected
    # and were all outside the freeze. That is not a theoretical hole: R8 once
    # marked those methods `final` in the release AAR and external subclasses
    # stopped compiling, and this gate could not have seen it. Private members
    # stay out, because nothing outside can name them.
    javap -cp "$CLASSES" -protected "$type" 2>/dev/null \
        | grep -v '^Compiled from' \
        | sed 's/[[:space:]]*$//' \
        | grep -v '^$' \
        >> "$surface" || true
done

# Sorted so member ordering inside a class -- which javac does not promise --
# cannot show up as a change.
sort -o "$surface" "$surface"

if [[ "$UPDATE" == true ]]; then
    cp "$surface" "$BASELINE"
    echo "baseline updated: $BASELINE ($(wc -l < "$BASELINE") entries)"
    echo "commit it with the change; the diff is the review."
    exit 0
fi

if [[ ! -f "$BASELINE" ]]; then
    echo "ERROR: no baseline at $BASELINE; create one with --update" >&2
    exit 1
fi

if diff -u "$BASELINE" "$surface" > "$host_api_diff" 2>&1; then
    echo "PASS: Android host API v0 unchanged ($(wc -l < "$BASELINE") entries)"
    exit 0
fi

echo "FAIL: the Android host API changed" >&2
echo >&2
sed -n '1,60p' "$host_api_diff" >&2
echo >&2
echo "Embedders compile against this surface and ship the binding inside their" >&2
echo "app. If the change is intended, re-run with --update and commit the new" >&2
echo "baseline alongside it." >&2
exit 1
