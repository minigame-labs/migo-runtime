#!/usr/bin/env bash
# Every contract gate in scripts/ must be invoked by something.
#
# A gate that nothing runs is not a weaker gate, it is a file. Whether it passes
# stops being anyone's information the moment the last caller drops it, and it
# then rots at the speed of the code it guards -- silently, because the way you
# find out a gate is red is that something ran it.
#
# This is not hypothetical. On 2026-08-25 an audit of scripts/test-*.sh against
# everything that invokes one found NINE gates referenced by no workflow and no
# other script -- out of 62. Two were already red. test-q14-codegen-profiles.sh
# asserted that build-aar.sh rejects a malformed SOURCE_DATE_EPOCH, and a
# `--skip-rust` guard added above that validation had been shadowing it, so the
# command kept failing on a different message. test-android-ndk-pin-contract.sh
# had two gates taking whatever readelf the environment offered. Both had been
# reporting those failures to nobody. The other seven passed -- equally
# unobserved, and equally free to stop passing.
#
# Nine, not five: the first pass at the audit was a one-line grep that missed
# four, for the reason spelled out at the --exclude below. An audit run once by
# hand is also how you get a number that is quietly wrong, which is the other
# argument for this being a gate.
#
# And this repository had already found the same failure once, fixing only that
# instance: build-ohos-sdk.sh still carries a comment saying its API floor gate
# "was previously wired to nothing at all -- it existed, passed when invoked by
# hand, and no path in the project ever invoked it."
#
# There is deliberately no exemption list, because a gate always has somewhere to
# live. Host-only, it goes in pr-ci.yml. Needs a device, device-test.yml. Needs a
# toolchain no runner has -- an OpenHarmony SDK, MSVC on a Windows host -- then
# the build script that owns that toolchain runs it, which is where
# test-ohos-toolchain-contract.sh and test-windows-v8-dll.sh now live. And a gate
# with no reason left to exist should be deleted along with the reason, not left
# unreferenced where it still reads as coverage. If you are reaching for an
# escape hatch here, one of those four is the thing you actually want.
#
# Host-only: reads the tree, runs no gate.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

self="$(basename "${BASH_SOURCE[0]}")"
orphans=()

for gate in scripts/test-*.sh; do
    name="$(basename "$gate")"
    [[ "$name" == "$self" ]] && continue

    # A reference has to be something that could RUN the gate, not something
    # that merely names it. Comment lines are excluded, and that is not
    # pedantry: on 2026-09-10 test-v8-patch-application-contract.sh was found
    # red on master, invoked by nothing, and passing this audit -- because
    # another gate's comment mentioned it while explaining where its fixtures
    # lived. That is exactly the state this file's own header describes as the
    # reason it exists ("existed, passed when invoked by hand, and no path in
    # the project ever invoked it"), and a bare mention had made it invisible
    # again.
    #
    # `--exclude` is the other half: the first pass at this audit grepped
    # scripts/ without it, every gate names itself somewhere in its own usage
    # text, and four orphans came back clean.
    #
    # docs/ is deliberately NOT searched even though runbooks live there. It is
    # git-ignored, so counting a mention in one would make this gate answer
    # differently on a maintainer's machine than in CI -- and the environment
    # where it would go quiet is the one that matters.
    # `grep -c` and NOT `grep -q`. A `-q` at the end of this pipe exits on its
    # first selected line and SIGPIPEs the recursive grep still walking the
    # tree; under `set -o pipefail` that 141 becomes the pipeline's status, and
    # a properly invoked gate is reported as an orphan. Which gates it hits
    # depends on how much tree was left to scan when the reader quit, so it is
    # not even reliably wrong -- the first version of this change reported five
    # orphans, three of which are invoked from a workflow by name.
    runnable="$(grep -rnF --exclude="$name" "$name" .github/ scripts/ 2>/dev/null \
        | grep -cvE '^[^:]+:[0-9]+:[[:space:]]*#' || true)"
    if (( runnable > 0 )); then
        continue
    fi
    orphans+=("$name")
done

if (( ${#orphans[@]} > 0 )); then
    echo "FAIL: ${#orphans[@]} contract gate(s) are invoked by nothing." >&2
    printf '\n'
    for o in "${orphans[@]}"; do
        printf '  %s\n' "$o"
    done
    cat >&2 <<'MSG'

  Whether these pass is nobody's information. Add each to a workflow that runs
  it -- .github/workflows/pr-ci.yml for a host-only gate, device-test.yml for
  one that needs hardware -- or delete it along with the reason it existed.
MSG
    exit 1
fi

echo "PASS: every contract gate in scripts/ is invoked by something ($(ls scripts/test-*.sh | wc -l) checked)"
