#!/usr/bin/env bash
# The prescreen scanner must not under-report.
#
# `scripts/prescreen-game.sh` answers "which APIs does this bundle need, and
# does this build publish them". Its dangerous failure is silent under-reporting:
# a bundle reaches the namespace through an alias or a computed key, the scanner
# does not follow it, and the report comes back saying everything is supported.
# That is the one wrong answer that costs a customer, so the scanner's job is
# either to resolve an access or to say out loud that it could not.
#
# The fixture in `tools/api-surface/fixture/` exercises each access form once,
# against a checked-in synthetic surface rather than a live dump -- the scanner's
# logic is what is under test, and binding it to a player run would make this
# slow, machine-dependent, and prone to changing whenever the runtime does.
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
FIXTURE="$ROOT_DIR/tools/api-surface/fixture"
SCANNER="$ROOT_DIR/scripts/prescreen-game.sh"

for required in "$FIXTURE/bundle" "$FIXTURE/surface.json" "$SCANNER"; do
    [[ -e "$required" ]] || { echo "ERROR: missing $required" >&2; exit 1; }
done

# Hermetic: the fixture carries its own wx reference. The repo's real dump is
# git-ignored, so a test that reached for it passed on the author's machine and
# failed on a fresh checkout -- which is exactly what happened.
report="$(bash "$SCANNER" "$FIXTURE/bundle" \
    --surface "$FIXTURE/surface.json" \
    --wx-reference "$FIXTURE/wx-reference.json" 2>/dev/null)"

# And the degraded path is a behaviour worth pinning: without a reference the
# report must say so, not silently merge the two buckets.
degraded="$(bash "$SCANNER" "$FIXTURE/bundle" \
    --surface "$FIXTURE/surface.json" \
    --wx-reference "$FIXTURE/definitely-absent.json" 2>/dev/null)"

[[ -n "$report" ]] || { echo "FAIL: scanner produced no report" >&2; exit 1; }

failures=0
check() { # description, then a grep -E pattern the report must contain
    if grep -qE "$2" <<<"$report"; then
        printf '  ok    %s\n' "$1"
    else
        printf '  FAIL  %s\n' "$1" >&2
        failures=$((failures + 1))
    fi
}
refute() {
    if grep -qE "$2" <<<"$report"; then
        printf '  FAIL  %s\n' "$1" >&2
        failures=$((failures + 1))
    else
        printf '  ok    %s\n' "$1"
    fi
}

echo "prescreen scanner contract:"

# Ten distinct names across every access form the fixture uses. If this number
# drops, some form stopped being followed -- which is under-reporting.
check "resolves all ten referenced names" '\| .wx\.\*. names referenced \| 10 \|'

# The alias form (`const W = wx; W.reportMonitor()`) is the one a naive scanner
# misses, and minified bundles are full of it.
check "follows an alias binding" '\| of those, published by this build \| 8 \|'

# Present on wx, absent from this build: the only bucket that is a real gap.
check "reports a genuine gap" 'wx\.createOffScreenCanvas'
check "counts exactly one genuine gap" '\| \*\*not published, and wx has it\*\* \| \*\*1\*\* \|'

# Absent from wx too: a different thing entirely, and must not be counted as a
# gap in this runtime.
check "separates non-wx names" 'thisApiDoesNotExistAnywhere'
check "counts exactly one non-wx name" '\| not published, and not a wx API either \| 1 \|'

# The unresolvable bucket is what keeps the counts honest.
check "reports both unresolvable sites" '\| \*\*sites this scanner cannot resolve\*\* \| \*\*2\*\* \|'
check "names the computed access" 'computed member access'
check "names the reflection" 'for-in over the namespace'
check "says the counts are a lower bound" 'lower bound'

# With unresolvable sites present, the report must never claim completeness.
refute "does not claim complete coverage while sites are unresolved" \
    'Every namespace access in this bundle is a literal name'

# --- three forms that look like references and are not ------------------
#
# Each was found on a real customer bundle, and each inflated the gap count.
# Over-reporting is not the safe direction either: a prescreen whose gaps are
# imaginary is worth less than none, because the customer checks and stops
# believing the rest of the report.

# A name inside a string literal is not a call. `"wx.cn.minigame.iap"` is a
# storage key; `"com.wx.minigame"` is an Android package name.
refute "does not count a name inside a string literal" '`wx\.cn`'
refute "does not count a package name inside a string" '`wx\.minigame`'
check "lists what it found inside strings instead of dropping it silently" \
    'Namespace-looking text inside string literals'

# `wx.foo = ...` is the bundle installing its own shim -- the opposite of a gap.
check "separates names the bundle installs itself" 'Names the bundle installs itself'
check "names the installed shim" '`wx\.__loadSubpackage__`'
# Section-scoped: the name legitimately appears under "installs itself", so a
# whole-report grep would pass for the wrong reason. What must be absent is the
# name in a *gap* section.
gaps_section() {
    sed -n '/^## Referenced, wx has it, this build does not$/,/^## /p' <<<"$report"
}
if grep -qE '__loadSubpackage__' <<<"$(gaps_section)"; then
    printf '  FAIL  %s\n' "does not report an installed shim as a gap" >&2
    failures=$((failures + 1))
else
    printf '  ok    %s\n' "does not report an installed shim as a gap"
fi

# `const c = migo.createCanvas()` binds a canvas, not the namespace. Treating it
# as an alias turns every `c.getContext` into a missing `migo.getContext`.
refute "does not alias a call result to the namespace" 'getContext'

# A plugin is not an API name, so only game.json says the bundle needs one -- and without the platform's plugin
# service it does not start. The fixture declares one; a copy that declares none must report zero and no section.
check "counts the plugin game.json declares" '\| \*\*plugins `game\.json` asks the platform to serve\*\* \| \*\*1\*\* \|'
check "names it, with its provider" '`cocos` -- provider `wx0000000000000000`, version `2\.4\.3`'
check "says how to ship without it" 'Separate Engine'
plain_bundle="$(mktemp -d)"
cp -r "$FIXTURE/bundle/." "$plain_bundle/"
printf '{"deviceOrientation":"portrait"}\n' > "$plain_bundle/game.json"
plain="$(bash "$SCANNER" "$plain_bundle" --surface "$FIXTURE/surface.json" \
    --wx-reference "$FIXTURE/wx-reference.json" 2>/dev/null)"
rm -rf "$plain_bundle"
if grep -qE '\| \*\*plugins `game\.json` asks the platform to serve\*\* \| \*\*0\*\* \|' <<<"$plain" \
        && ! grep -q '## Plugins the platform would serve' <<<"$plain"; then
    printf '  ok    %s\n' "a bundle that declares no plugin counts none and says nothing more"
else
    printf '  FAIL  %s\n' "a bundle that declares no plugin counts none and says nothing more" >&2
    failures=$((failures + 1))
fi

# The caveats are the difference between a report and a claim.
check "states it is not a compatibility verdict" 'Not a compatibility verdict'
check "states Linux cannot answer it" 'Linux player has no device services'

# --- degraded path -------------------------------------------------------
check_degraded() {
    if grep -qE "$2" <<<"$degraded"; then
        printf '  ok    %s\n' "$1"
    else
        printf '  FAIL  %s\n' "$1" >&2
        failures=$((failures + 1))
    fi
}
refute_degraded() {
    if grep -qE "$2" <<<"$degraded"; then
        printf '  FAIL  %s\n' "$1" >&2
        failures=$((failures + 1))
    else
        printf '  ok    %s\n' "$1"
    fi
}
check_degraded "says the wx reference is unavailable" 'wx reference table unavailable'
check_degraded "warns the gap count is overstated" 'overstates them'
refute_degraded "does not claim a classified gap count without a reference" \
    '\*\*not published, and wx has it\*\*'

# ---------------------------------------------------------------------------
# The surface dumper must not fail a run it completed.
#
# `dump-api-surface.sh` cleans up in an EXIT trap, and the trap's last command
# used to be `[[ -n "$STAGE" ]] && rm -rf "$STAGE"`. With no --adapter, STAGE is
# empty, that test is false, the function returns 1, and an EXIT trap's status
# becomes the script's. So the dumper exited 1 on every successful run in its
# default mode -- after printing `surface -> <path>` and a complete, correct
# surface -- and `set -e` in prescreen-game.sh turned that into a prescreen that
# produced no report and said nothing about why. The adapter path happened to
# set STAGE and return 0, so the runs anyone tried worked.
#
# Checked by running the real trap body with STAGE unset, rather than by running
# the dumper: the dumper needs a built player, and a check nobody can run on a
# laptop is a check that stops being run.
# Brace-counted, not line-ranged. A `sed '/^cleanup/,/^}/p'` works only while
# the function is written across several lines; against the one-line form it
# finds no closing line and swallows the rest of the file, so this check went red
# for a completely different reason than the one it prints.
trap_body="$(awk '
    /^cleanup\(\)/ { collecting = 1 }
    collecting {
        print
        n = gsub(/{/, "{"); depth += n
        n = gsub(/}/, "}"); depth -= n
        if (depth <= 0) exit
    }
' "$ROOT_DIR/scripts/dump-api-surface.sh")"
if [[ -z "$trap_body" ]]; then
    printf '  FAIL  %s\n' "cannot find cleanup() in dump-api-surface.sh -- this check is broken, not the script" >&2
    failures=$((failures + 1))
else
    if ( eval "$trap_body"; raw=""; STAGE=""; cleanup ); then
        printf '  ok    %s\n' "the surface dumper's EXIT trap returns 0 with no adapter staged"
    else
        printf '  FAIL  %s\n' "the surface dumper's EXIT trap returns non-zero with no adapter staged; every successful default-mode run will report failure" >&2
        failures=$((failures + 1))
    fi
fi

if [[ $failures -gt 0 ]]; then
    echo "FAIL: prescreen scanner contract ($failures check(s))" >&2
    exit 1
fi

echo "PASS: prescreen scanner contract"
