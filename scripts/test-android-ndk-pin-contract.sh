#!/usr/bin/env bash
# scripts/test-android-ndk-pin-contract.sh
#
# Assert that the Android build uses the NDK the lock pins, and finds it rather
# than guessing where it lives.
#
# WHY THIS EXISTS (observed, not hypothetical):
# Seven scripts independently defaulted ANDROID_NDK_HOME to `$HOME/Android/Ndk`.
# That path exists on none of the machines this project has been built on -- the
# NDK actually in use is at `$HOME/Android/Sdk/ndk/23.2.8568313` -- so every
# successful build silently depended on the variable already being set correctly in
# the environment, and a fresh checkout on a new machine would fail on a path
# nobody ever had. Nothing asserted *which* NDK it pointed at either, so the V8
# archive and the AAR could be produced by different NDKs with nothing noticing,
# even though the component manifest records the NDK revision, the target compiler,
# the sysroot and the linker as part of the artifact's identity.
#
# WHY IT CHECKS WHAT IT CHECKS:
# A directory name is not an identity: `$SDK/ndk/23.2.8568313` is just a name
# somebody could rename or populate with anything. The check therefore reads
# `Pkg.Revision` from the NDK's own `source.properties`, which is the same fact the
# component manifest stamps into the artifact. An explicit ANDROID_NDK_HOME is
# honoured but still checked, so an override cannot substitute a different
# toolchain silently.
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
LIB="$SCRIPT_DIR/lib/android-ndk.sh"
LOCK="$REPO_ROOT/contracts/artifact-manifest/android-v8.lock.json"
TAG='[ndk-pin]'

pass() { echo -e "\033[0;32m$TAG PASS $*\033[0m"; }
fail() { echo -e "\033[0;31m$TAG FAIL $*\033[0m" >&2; failures=$((failures + 1)); }
info() { echo -e "\033[0;36m$TAG $*\033[0m"; }
failures=0

# Exercise the CI entry as well as local resolvers: the action once kept a
# separate r23 default while native consumers needed the updated build lock.
if python3 "$SCRIPT_DIR/ci/tests/test_setup_android_ndk.py"; then
    pass "CI installation and all four consumer variables follow the build lock"
else
    fail "CI installation no longer follows the native build lock"
fi

[[ -f "$LIB" ]]  || { echo "$TAG missing library: $LIB" >&2; exit 1; }
[[ -f "$LOCK" ]] || { echo "$TAG missing lock: $LOCK" >&2; exit 1; }

# shellcheck source=scripts/lib/android-ndk.sh
source "$LIB"

info "the lock pins an NDK version"
if android_ndk_read_pin "$LOCK"; then
    pass "pinned NDK: $ANDROID_NDK_PIN"
else
    fail "the lock does not pin an NDK version"
    exit 1
fi

info "resolution is driven by the NDK's own Pkg.Revision, not by its path"
# A fake SDK layout, so the fixtures do not depend on which NDKs this machine has.
w="$(mktemp -d)"
make_ndk() { # dir, revision-in-source.properties
    mkdir -p "$1"
    printf 'Pkg.Desc = Android NDK\nPkg.Revision = %s\n' "$2" > "$1/source.properties"
}
resolve_in() { # sdk-root, [explicit ANDROID_NDK_HOME]
    local sdk="$1" explicit="${2:-}"
    (
        unset ANDROID_NDK_ROOT
        if [[ -n "$explicit" ]]; then export ANDROID_NDK_HOME="$explicit"; else unset ANDROID_NDK_HOME; fi
        export ANDROID_HOME="$sdk" ANDROID_SDK_ROOT="$sdk" HOME="$w/nohome"
        source "$LIB"
        ANDROID_NDK_PIN="$ANDROID_NDK_PIN"
        android_ndk_resolve >/dev/null 2>&1 || exit 1
        printf '%s' "$ANDROID_NDK_HOME"
    )
}

make_ndk "$w/sdk-good/ndk/$ANDROID_NDK_PIN" "$ANDROID_NDK_PIN"
if got="$(resolve_in "$w/sdk-good")" && [[ "$got" == "$w/sdk-good/ndk/$ANDROID_NDK_PIN" ]]; then
    pass "an NDK whose revision matches the pin is selected"
else
    fail "the matching NDK was not selected (got '${got:-<none>}')"
fi

# The directory carries the pinned name but reports a different revision -- the
# case a path-only check cannot see.
make_ndk "$w/sdk-liar/ndk/$ANDROID_NDK_PIN" "27.0.11902837"
if resolve_in "$w/sdk-liar" >/dev/null 2>&1; then
    fail "an NDK whose revision contradicts its directory name was accepted"
else
    pass "an NDK whose revision contradicts its directory name is refused"
fi

mkdir -p "$w/sdk-empty/ndk/$ANDROID_NDK_PIN"
if resolve_in "$w/sdk-empty" >/dev/null 2>&1; then
    fail "a directory with no source.properties was accepted"
else
    pass "a directory with no source.properties is refused"
fi

if resolve_in "$w/sdk-absent" >/dev/null 2>&1; then
    fail "resolution succeeded with no NDK installed"
else
    pass "resolution fails when no NDK matches the pin"
fi

# An explicit override must be checked like any other candidate.
make_ndk "$w/explicit-wrong" "27.0.11902837"
if got="$(resolve_in "$w/sdk-good" "$w/explicit-wrong")" \
   && [[ "$got" == "$w/sdk-good/ndk/$ANDROID_NDK_PIN" ]]; then
    pass "an override pointing at the wrong NDK does not win"
else
    fail "an override pointing at the wrong NDK was used (got '${got:-<none>}')"
fi
make_ndk "$w/explicit-right" "$ANDROID_NDK_PIN"
if got="$(resolve_in "$w/sdk-absent" "$w/explicit-right")" \
   && [[ "$got" == "$w/explicit-right" ]]; then
    pass "an override pointing at the pinned NDK is honoured"
else
    fail "an override pointing at the pinned NDK was ignored (got '${got:-<none>}')"
fi
rm -rf "$w"

info "this machine can resolve the pinned NDK without help from the environment"
if got="$(env -u ANDROID_NDK_HOME -u ANDROID_NDK_ROOT bash -c "
    source '$LIB'
    android_ndk_read_pin '$LOCK' || exit 1
    android_ndk_resolve >/dev/null 2>&1 || exit 1
    printf '%s' \"\$ANDROID_NDK_HOME\"")"; then
    pass "resolved to $got with ANDROID_NDK_HOME unset"
else
    info "SKIP NDK $ANDROID_NDK_PIN is not installed on this machine"
fi

info "no script guesses the NDK path"
# Enumerated rather than listed, so a script added later cannot reintroduce the
# guess unnoticed: anything that *consumes* the NDK must resolve it. Scripts whose
# only mention is an `unset` are doing the opposite -- host builds deliberately
# refusing an Android toolchain that happens to be in the environment -- and are
# classified as such rather than failed. The resolver itself and this contract are
# the two files allowed to name the variable freely.
mapfile -t ndk_users < <(
    grep -ln 'ANDROID_NDK_HOME' "$SCRIPT_DIR"/*.sh "$SCRIPT_DIR"/lib/*.sh 2>/dev/null \
    | grep -v -e '/lib/android-ndk\.sh$' -e '/test-android-ndk-pin-contract\.sh$')
if (( ${#ndk_users[@]} == 0 )); then
    fail "found no scripts using ANDROID_NDK_HOME -- the enumeration is broken"
fi
# Which library functions count as "resolves the pinned NDK" is read out of the
# library, not written here. This check used to grep for the one literal name
# `android_ndk_resolve`, and the day a second entry point appeared that resolves
# on the caller's behalf -- `android_ndk_readelf`, which reads the pin, resolves,
# and hands back a tool path -- every consumer of it was reported as using the
# NDK without resolving it. The rule was a name, not the property it stood for.
mapfile -t resolving_fns < <(
    awk '
        /^android_ndk_[a-z_]*\(\)/ { fn = substr($1, 1, index($1, "(") - 1); body = "" }
        fn != "" { body = body $0 }
        /^}/ { if (fn != "" && (fn == "android_ndk_resolve" || body ~ /android_ndk_resolve/)) print fn; fn = "" }
    ' "$SCRIPT_DIR/lib/android-ndk.sh")
if (( ${#resolving_fns[@]} == 0 )); then
    fail "no resolving entry point found in lib/android-ndk.sh -- the derivation is broken"
fi
resolving_re="$(IFS='|'; echo "${resolving_fns[*]}")"
info "resolving entry points derived from the library: ${resolving_fns[*]}"

for f in "${ndk_users[@]}"; do
    name="$(basename "$f")"
    if offenders="$(grep -n 'ANDROID_NDK_HOME:-' "$f")"; then
        fail "$name still defaults ANDROID_NDK_HOME instead of resolving it:"
        echo "$offenders" >&2
    elif grep -qE "($resolving_re)" "$f"; then
        pass "$name resolves the pinned NDK"
    elif ! grep 'ANDROID_NDK_HOME' "$f" | grep -qv '^\s*unset '; then
        pass "$name only unsets it, refusing an ambient Android toolchain"
    else
        fail "$name uses ANDROID_NDK_HOME without resolving it"
    fi
done

info "no PowerShell entry point consumes an unpinned NDK"
# The enumeration above globs `*.sh`, and that scope -- not its assertions -- was
# the hole: `build-android-so.ps1` took $env:ANDROID_NDK_HOME as given and checked
# nothing, so a Windows AAR could link the pinned V8 archive with any NDK while
# every gate reported the pin enforced. A `.ps1` is a shipped entry point too, so
# it is enumerated on the same terms.
mapfile -t ps_ndk_users < <(
    grep -ln 'ANDROID_NDK_HOME' "$SCRIPT_DIR"/*.ps1 2>/dev/null)
if (( ${#ps_ndk_users[@]} == 0 )); then
    fail "found no PowerShell scripts using ANDROID_NDK_HOME -- the enumeration is broken"
fi
for f in "${ps_ndk_users[@]}"; do
    name="$(basename "$f")"
    if grep -q 'Resolve-MigoPinnedNdk' "$f"; then
        pass "$name resolves the pinned NDK"
    else
        fail "$name uses ANDROID_NDK_HOME without resolving it"
    fi
done

info "the PowerShell resolver decides on the same fact as the shell one"
# One rule, two languages, and the gate has to equate them or the second copy
# drifts into checking a directory *name* -- which is what makes a pin decorative.
# Asserted behaviourally rather than by grepping the module for `Pkg.Revision`: that
# string occurs in its own prose, so the grep would pass over a module that had
# stopped reading it. The discriminating case is a directory named exactly like the
# pin whose own source.properties reports something else, which only an
# identity check can refuse.
PS_LIB="$SCRIPT_DIR/lib/AndroidNdk.psm1"
if [[ ! -f "$PS_LIB" ]]; then
    fail "missing $PS_LIB"
elif ! command -v pwsh >/dev/null 2>&1; then
    info "SKIP pwsh is not installed, so the PowerShell resolver cannot be exercised"
else
    ps_probe="$(mktemp -d)"
    mkdir -p "$ps_probe/sdk/ndk/$ANDROID_NDK_PIN"
    printf 'Pkg.Revision = 1.2.3456789\n' \
        > "$ps_probe/sdk/ndk/$ANDROID_NDK_PIN/source.properties"
    if env -u ANDROID_NDK_HOME -u ANDROID_NDK_ROOT -u ANDROID_SDK_ROOT \
           HOME="$ps_probe/empty" ANDROID_HOME="$ps_probe/sdk" \
           pwsh -NoProfile -Command "
                Import-Module '$PS_LIB' -Force
                Resolve-MigoPinnedNdk -Lock '$LOCK'
           " >/dev/null 2>&1; then
        fail "the PowerShell resolver accepted a directory named $ANDROID_NDK_PIN reporting another revision"
    else
        pass "a directory carrying the pinned name but another revision is refused"
    fi
    printf 'Pkg.Revision = %s\n' "$ANDROID_NDK_PIN" \
        > "$ps_probe/sdk/ndk/$ANDROID_NDK_PIN/source.properties"
    if got="$(env -u ANDROID_NDK_HOME -u ANDROID_NDK_ROOT -u ANDROID_SDK_ROOT \
                  HOME="$ps_probe/empty" ANDROID_HOME="$ps_probe/sdk" \
                  pwsh -NoProfile -Command "
                       Import-Module '$PS_LIB' -Force
                       Resolve-MigoPinnedNdk -Lock '$LOCK'
                  " 2>/dev/null)"; then
        pass "an NDK whose own revision equals the pin is accepted ($(basename "$got"))"
    else
        fail "the PowerShell resolver refused an NDK reporting exactly the pinned revision"
    fi
    rm -rf "$ps_probe"
fi

if (( failures == 0 )); then
    echo -e "\033[0;32m$TAG all checks passed\033[0m"
else
    echo -e "\033[0;31m$TAG $failures check(s) failed\033[0m" >&2
fi
exit $(( failures > 0 ))
