#!/usr/bin/env bash
# Run every contract gate on this machine, and say which failures are the
# machine's rather than the tree's.
#
# WHY THIS EXISTS. Two changes that are each green on their own branch can be red
# together on master -- measured 2026-09-11, when a fixture that reports nothing
# about V8 met an assertion that treated an absent report as a failure. Neither
# PR could have seen it. The cheap way to find that class of problem is to run
# the gates after a batch of merges rather than to wait for the next PR to hit
# it, and the cheap way to make that happen is for it to be one command.
#
# It also carries the invocation knowledge that has bitten before:
#
#   `mapfile` rather than `while read`  -- an inner script that reads stdin eats
#                                          the loop's input and the sweep stops
#                                          silently, partway through.
#   `< /dev/null` on each gate          -- the same hazard from the other side.
#   `env -u CC -u CXX`                  -- a developer machine may point those at
#                                          an Android NDK, which breaks host
#                                          builds inside a gate in a way that
#                                          reads as the gate failing.
#
# Exit status is about the TREE: a gate that needs a build artifact this machine
# does not have is reported and does not fail the run. Anything else does.
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

# Gates that need a built artifact or another platform.
#
# "MAY fail without it", not "must" -- a machine that has the artifact runs them
# for real and that is a better run, not a broken one. Stating it the other way
# would make this list an assertion about what is missing, which is the shape
# that turns red the day somebody builds the thing.
NEEDS_ARTIFACT=(
    test-android-nojni-aar-contract.sh
    test-android-sdk-contract.sh
    test-android-snapshot-embedding-contract.sh
    test-capi-snapshot-embedding-contract.sh
    test-linux-sdk-contract.sh
    test-macos-archive-runs-js.sh
    test-ohos-sdk-contract.sh
    test-ohos-symbol-floor.sh
    test-release-asset-naming-contract.sh
    test-windows-sdk-contract.sh
)

needs_artifact() {
    local name="$1"
    for known in "${NEEDS_ARTIFACT[@]}"; do
        [[ "$name" == "$known" ]] && return 0
    done
    return 1
}

TIMEOUT="${MIGO_GATE_TIMEOUT:-180}"
mapfile -t GATES < <(ls scripts/test-*.sh 2>/dev/null | sort)

passed=0
expected=0
failed=()
for gate in "${GATES[@]}"; do
    name="$(basename "$gate")"
    if env -u CC -u CXX timeout "$TIMEOUT" bash "$gate" > "/tmp/migo-gate-$name.log" 2>&1 < /dev/null; then
        passed=$((passed + 1))
    elif needs_artifact "$name"; then
        expected=$((expected + 1))
    else
        failed+=("$name")
    fi
done

echo "contract gates: $passed passed, $expected skipped for a missing artifact, ${#failed[@]} failed"
if ((${#failed[@]} > 0)); then
    for name in "${failed[@]}"; do
        echo "  --- $name (full log: /tmp/migo-gate-$name.log) ---"
        tail -3 "/tmp/migo-gate-$name.log" | sed 's/^/      /'
    done
    exit 1
fi
