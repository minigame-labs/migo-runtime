#!/usr/bin/env bash
# Contract for scripts/verify-change.sh.
#
# The properties here are the ones whose absence would make the script worse than
# nothing -- a verification entry point that reports a pass it did not earn is how
# three Android compile errors survived several sessions of green host runs.
#
#   * a crate source the module walk cannot reach stops the run, because an
#     unreached file has unknown platform conditions;
#   * a required target with no local build is reported NOT PROVEN and fails, never
#     skipped;
#   * the scope is what the flags say it is, so a narrow run cannot be read as a
#     broad one.
#
# The fixtures are throwaway repositories holding the real script and the real
# selector, so the contract is against the shipped code rather than a paraphrase.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

failures=0

fail() {
    printf '\033[0;31m[FAIL] %s\033[0m\n' "$*" >&2
    failures=$((failures + 1))
}

pass() {
    printf '\033[0;32m[ok]\033[0m %s\n' "$*"
}

assert_contains() {
    local haystack="$1" needle="$2" what="$3"
    if grep -qF -- "$needle" <<< "$haystack"; then
        pass "$what"
    else
        fail "$what -- expected to find '$needle' in:"
        printf '%s\n' "$haystack" >&2
    fi
}

assert_absent() {
    local haystack="$1" needle="$2" what="$3"
    if grep -qF -- "$needle" <<< "$haystack"; then
        fail "$what -- did not expect '$needle' in:"
        printf '%s\n' "$haystack" >&2
    else
        pass "$what"
    fi
}

assert_status() {
    local actual="$1" expected="$2" what="$3"
    if [[ "$actual" == "$expected" ]]; then
        pass "$what"
    else
        fail "$what -- expected exit $expected, got $actual"
    fi
}

assert_no_line_starting() {
    local haystack="$1" prefix="$2" what="$3"
    if grep -q "^$prefix" <<< "$haystack"; then
        fail "$what -- found a line starting '$prefix' in:"
        printf '%s\n' "$haystack" >&2
    else
        pass "$what"
    fi
}

git_quiet() {
    git -c user.name=fixture -c user.email=fixture@example.invalid -C "$1" "${@:2}"
}

# Builds a repository holding the real script, the real selector, and an engine
# workspace whose host suites pass.
#
# The workspace is what makes the verdict assertions mean anything. With no engine
# to build, every host step fails, the run exits non-zero for that reason, and an
# assertion on the exit code alone cannot tell "failed because a target was not
# proven" from "failed because the fixture has no engine" -- it would pass with the
# NOT PROVEN rule deleted. So the stubs are real crates that really compile.
STUB_CRATES=(
    demo
    shared io runtime-v8 audio graphics core capi platform
    # The C boundary rules crate. Its host step is the one that names
    # `--all-targets`, so a fixture without it would fail that step on a missing
    # package rather than on anything this contract is about.
    capi-abi
)

# Mirrors the real tree's third category: `engine/testing/` holds crates that
# measure the engine and ship nowhere. Placed faithfully rather than folded into
# `crates/` so the fixture also exercises the audit's indifference to it.
STUB_TESTING_CRATES=(
    alloc-probe
    contention-probe
    executor-probe
)

add_stub_crate() {
    local repo="$1" group="$2" name="$3"
    mkdir -p "$repo/engine/$group/$name/src"
    # `io` depends on `shared`, mirroring the real graph, because the host-suite
    # closure is only observable across an edge: without one, a change to a leaf
    # selects that leaf and the under-running this contract is meant to catch would
    # look identical to correct behaviour.
    local dependency=""
    if [[ "$name" == "io" ]]; then
        dependency='shared = { path = "../shared", package = "migo-shared" }'
    fi
    # `demo` is the crate no host step names, so it carries no tests and says so the
    # way the real tree's test-less targets do. A lib cargo builds a test binary
    # from is one the audit requires a step for, and every fixture would otherwise
    # fail there rather than on the property it exists to check.
    local lib_section=""
    if [[ "$name" == "demo" ]]; then
        lib_section=$'[lib]\ntest = false'
    fi
    cat > "$repo/engine/$group/$name/Cargo.toml" <<EOF
[package]
name = "migo-$name"
version = "0.0.0"
edition = "2021"

$lib_section

[dependencies]
$dependency

# The real crates select their capability surface with these, and the host steps
# run both, so a stub without them fails the Slim step on a missing feature
# rather than on anything this contract is about.
[features]
default = ["profile-full"]
profile-full = []
profile-slim = []
EOF
    printf 'pub fn present() -> bool {\n    true\n}\n' > "$repo/engine/$group/$name/src/lib.rs"
    if [[ "$name" != "demo" ]]; then
        printf '%s\n' "$STUB_UNIT_TEST" >> "$repo/engine/$group/$name/src/lib.rs"
    fi
}

STUB_UNIT_TEST='
#[cfg(test)]
mod tests {
    #[test]
    fn present() {
        assert!(super::present());
    }
}'

new_fixture() {
    local name="$1"
    local repo="$WORK/$name"
    mkdir -p "$repo/scripts/lib" "$repo/docs" "$repo/engine"
    cp "$ROOT/scripts/verify-change.sh" "$repo/scripts/"
    cp "$ROOT/scripts/lib/verification_targets.py" "$repo/scripts/lib/"
    # Every helper the script calls, not just the selector: a missing one makes the
    # run fail for a reason unrelated to the property under test, which is the
    # shape of an always-red gate.
    cp "$ROOT/scripts/lib/ci_contract_gates.py" "$repo/scripts/lib/"
    cp "$ROOT/scripts/lib/host_test_coverage.py" "$repo/scripts/lib/"
    {
        printf '[workspace]\nresolver = "2"\nmembers = [\n'
        for crate in "${STUB_CRATES[@]}"; do
            printf ' "crates/%s",\n' "$crate"
        done
        for crate in "${STUB_TESTING_CRATES[@]}"; do
            printf ' "testing/%s",\n' "$crate"
        done
        printf ']\n'
    } > "$repo/engine/Cargo.toml"
    for crate in "${STUB_CRATES[@]}"; do
        add_stub_crate "$repo" crates "$crate"
    done
    for crate in "${STUB_TESTING_CRATES[@]}"; do
        add_stub_crate "$repo" testing "$crate"
    done
    printf 'baseline\n' > "$repo/docs/note.md"
    git_quiet "$repo" init -q -b master
    git_quiet "$repo" add -A
    git_quiet "$repo" commit -q -m "baseline"
    echo "$repo"
}

# A fixture whose workflow really has a quality-gate job, so the lane has gates to
# run. Without one the only contract line a run produces is the "no workflow here"
# verdict, which is recorded before the loop -- and a mutant that deleted the loop
# entirely walked past an assertion on that line.
#
# Sixteen stubs because the derivation refuses a job with fewer than fifteen gates,
# on the grounds that a lane that small is a parse which stopped matching. The
# fixture has to clear the real floor rather than the floor being made tunable: a
# knob that switches off an anti-vacuity check is a knob that switches it off in
# production too.
add_stub_gates() {
    local repo="$1" failing="${2:-}"
    mkdir -p "$repo/.github/workflows"
    {
        printf 'jobs:\n  quality-gate:\n    steps:\n'
        for index in $(seq 1 16); do
            printf '      - name: stub %s\n        run: bash scripts/test-stub-%s-contract.sh\n' \
                "$index" "$index"
            # Stub 1 drains stdin on purpose. A real gate does -- one of them runs
            # `cargo` -- and when the lane iterated a here-string it swallowed
            # every gate after it, which is how three gates and a verdict line
            # went missing from a run that still reported success.
            if [[ "$index" == "1" ]]; then
                printf '#!/usr/bin/env bash\ncat >/dev/null\nexit 0\n' \
                    > "$repo/scripts/test-stub-$index-contract.sh"
            else
                printf '#!/usr/bin/env bash\nexit 0\n' > "$repo/scripts/test-stub-$index-contract.sh"
            fi
            chmod +x "$repo/scripts/test-stub-$index-contract.sh"
        done
    } > "$repo/.github/workflows/pr-ci.yml"
    if [[ -n "$failing" ]]; then
        # The last gate, so a refusal is only observable if the lane got that far.
        printf '#!/usr/bin/env bash\necho "stub gate refuses"\nexit 1\n' \
            > "$repo/scripts/test-stub-16-contract.sh"
    fi
}

run_verify() {
    local repo="$1"
    shift
    ( cd "$repo" && bash scripts/verify-change.sh "$@" 2>&1 )
}

# ------------------------------------------------------------
# An unreached crate source stops the run.
# ------------------------------------------------------------
repo="$(new_fixture unreachable)"
printf 'pub fn orphaned() {}\n' > "$repo/engine/crates/demo/src/orphan.rs"
status=0
output="$(run_verify "$repo" --plan-only)" || status=$?
assert_status "$status" 1 "an unreached crate source fails the run"
assert_contains "$output" "engine/crates/demo/src/orphan.rs" \
    "the unreached file is named"

# The tree-wide half of it: an unreached file nobody touched still stops the run.
# Committed on the base, so it is outside the changed set and only the audit can
# see it -- which is the point of auditing the whole tree rather than the diff.
repo="$(new_fixture unreached_untouched)"
printf 'pub fn orphaned() {}\n' > "$repo/engine/crates/demo/src/orphan.rs"
git_quiet "$repo" add -A
git_quiet "$repo" commit -q -m "orphan"
printf 'edited\n' > "$repo/docs/note.md"
status=0
output="$(run_verify "$repo" --plan-only)" || status=$?
assert_status "$status" 1 "an unreached file outside the changed set fails the run"
assert_contains "$output" "engine/crates/demo/src/orphan.rs" \
    "the untouched unreached file is named"

# The other half: a crate directory with no manifest is invisible to the tree
# audit, so the changed file's unknown conditions have to stop the run on their own.
repo="$(new_fixture unmanifested)"
mkdir -p "$repo/engine/crates/nomanifest/src"
printf 'pub mod known;\n' > "$repo/engine/crates/nomanifest/src/lib.rs"
printf 'pub fn known() {}\n' > "$repo/engine/crates/nomanifest/src/known.rs"
printf 'pub fn orphaned() {}\n' > "$repo/engine/crates/nomanifest/src/orphan.rs"
status=0
output="$(run_verify "$repo" --plan-only)" || status=$?
assert_status "$status" 1 "a changed source with unknown conditions fails the run"
assert_contains "$output" "UNDETERMINED" "the unknown-condition file is reported"

# ------------------------------------------------------------
# A test binary no host step runs stops the run.
#
# The same argument as the unreached source above, applied to behaviour instead of
# to platform conditions: a binary nothing executes has unknown behaviour, and a
# verdict printed over it covers less than it says. This is the case that was live
# -- thirteen integration-test binaries, 95 tests, none of them named by any step,
# because every step in every gate said `--lib`.
#
# `--plan-only`, so the assertion is about the audit refusing rather than about any
# suite's result. A stub crate is used deliberately: `demo` has no host step at all,
# which is the weaker of the two ways to be uncovered and therefore the one a
# `--lib`-only check would miss.
# ------------------------------------------------------------
repo="$(new_fixture unrun_test_binary)"
mkdir -p "$repo/engine/crates/demo/tests"
printf '#[test]\nfn nobody_runs_me() {}\n' > "$repo/engine/crates/demo/tests/orphan.rs"
status=0
output="$(run_verify "$repo" --plan-only)" || status=$?
assert_status "$status" 1 "a test binary no host step runs fails the run"
assert_contains "$output" "migo-demo::orphan" "the unrun test binary is named"

# ------------------------------------------------------------
# Unit tests in a lib or a bin are test binaries too.
#
# Cargo builds a test binary from every lib, bin and integration-test target whose
# `test` flag is set, and the audit counted only the last kind. That is how
# `migo-services` -- 272 tests, all in its lib -- and the tests in `migo-player`'s
# bin were named by no local step and no job while the audit reported nothing
# missing.
# ------------------------------------------------------------
repo="$(new_fixture unrun_unit_tests)"
sed -i '/^test = false$/d' "$repo/engine/crates/demo/Cargo.toml"
printf '%s\n' "$STUB_UNIT_TEST" >> "$repo/engine/crates/demo/src/lib.rs"
printf 'fn main() {}\n\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn runs() {}\n}\n' \
    > "$repo/engine/crates/demo/src/main.rs"
mkdir -p "$repo/engine/crates/demo/tests"
printf '#[test]\nfn runs() {}\n' > "$repo/engine/crates/demo/tests/it.rs"
status=0
output="$(run_verify "$repo" --plan-only)" || status=$?
assert_status "$status" 1 "unit tests no host step runs fail the run"
assert_contains "$output" "migo-demo::migo_demo (lib)" "the lib whose tests nothing runs is named"
assert_contains "$output" "migo-demo::migo-demo (bin)" "the bin whose tests nothing runs is named"

# Each selection flag reaches its own kind and no other. `--lib` standing for every
# target is the original defect -- it hid thirteen integration-test binaries -- and
# no real host step happens to depend on it, so it is pinned here rather than left
# to the real tree to notice.
#
# An audit that cannot answer prints nothing, which every `assert_absent` below would
# read as coverage -- so its refusal is a failure here, not an empty string. Checked
# at each call, because `fail` inside the substitution would count in a subshell.
audit_demo() {
    printf '%s\n' "$1" | python3 "$ROOT/scripts/lib/host_test_coverage.py" --root "$repo"
}
unanswered="the audit could not describe the fixture"
covered="$(audit_demo 'test -p migo-demo --lib')" || fail "$unanswered"
assert_absent "$covered" "migo-demo::migo_demo (lib)" "--lib runs the lib's tests"
assert_contains "$covered" "migo-demo::migo-demo (bin)" "--lib does not run a bin's tests"
assert_contains "$covered" "migo-demo::it (test)" "--lib does not run an integration test"
covered="$(audit_demo 'test -p migo-demo --bins')" || fail "$unanswered"
assert_absent "$covered" "migo-demo::migo-demo (bin)" "--bins runs the bins' tests"
assert_contains "$covered" "migo-demo::migo_demo (lib)" "--bins does not run the lib's tests"
covered="$(audit_demo 'test -p migo-demo --tests')" || fail "$unanswered"
assert_absent "$covered" "migo-demo::" "--tests runs every test binary, the lib's and the bin's included"

# And a compile is not coverage. `build --workspace --all-targets` builds every test
# binary and runs none, so an audit that accepted it would report the whole class as
# covered -- which is how these binaries stayed invisible while the tree was green.
# Asked of the parser directly: that one step alone must leave every test binary in
# the real workspace unaccounted for.
compile_only="$(printf 'build --workspace --all-targets\n' \
    | python3 "$ROOT/scripts/lib/host_test_coverage.py" --root "$ROOT" || true)"
if [[ "$(printf '%s\n' "$compile_only" | grep -c .)" -ge 10 ]]; then
    pass "compiling every test binary is not counted as running one"
else
    fail "the audit treats a --all-targets *build* as coverage, so it cannot see an unrun binary"
fi

# ------------------------------------------------------------
# A change outside the engine needs no target build.
# ------------------------------------------------------------
repo="$(new_fixture documentation)"
printf 'edited\n' > "$repo/docs/note.md"
status=0
output="$(run_verify "$repo" --plan-only)" || status=$?
assert_status "$status" 0 "a documentation change plans cleanly"
assert_absent "$output" "TARGET" "a documentation change requires no target"

# ------------------------------------------------------------
# The Android SDK's Java half is a target.
#
# It was not, and the omission had the exact shape this file exists to catch: a
# change to `platforms/android/**` -- the shipped AAR's own sources -- produced
# an empty plan, so the run printed its success line having compiled no Java at
# all. The plan assertion is what pins the lane; the second one pins that the
# lane is *reported unproven* rather than skipped where Gradle cannot run, which
# is the same rule every other target already gets.
# ------------------------------------------------------------
repo="$(new_fixture android_java)"
mkdir -p "$repo/platforms/android/library/src/main/java/com/migo/runtime"
printf 'class Probe {}\n' \
    > "$repo/platforms/android/library/src/main/java/com/migo/runtime/Probe.java"
status=0
output="$(run_verify "$repo" --plan-only)" || status=$?
assert_status "$status" 0 "an Android Java change plans cleanly"
assert_contains "$output" "TARGET android-java compile" \
    "a change to the shipped AAR's sources asks for the Java build"

status=0
output="$(run_verify "$repo")" || status=$?
assert_status "$status" 1 "the Java lane with no local Gradle fails the run"
assert_contains "$output" "NOT PROVEN" "a machine without Gradle reports no evidence, not a break"
assert_contains "$output" "android-java compile" "the Java lane is named in the verdict"
# Executed, not merely planned. `--plan-only` prints the lane from the same helper,
# so a mutant that deletes the loop which *runs* the gates would leave the plan
# check green; only a verdict line proves the run reached them.
assert_contains "$output" "no .github/workflows/pr-ci.yml in this tree" \
    "the contract lane appears in the verdict of an actual run"
assert_no_line_starting "$output" "FAIL" \
    "the Java lane's absence is the only thing that failed this run"

# The other half of the rule: a Gradle build script is an input too, so editing
# one asks for the lane even though it is not a source file.
repo="$(new_fixture android_gradle)"
mkdir -p "$repo/platforms/android/library"
printf 'android {}\n' > "$repo/platforms/android/library/build.gradle"
status=0
output="$(run_verify "$repo" --plan-only)" || status=$?
assert_contains "$output" "TARGET android-java compile" \
    "a Gradle build script is an input to the Java lane"

# ------------------------------------------------------------
# A target with no local build is NOT PROVEN, and that fails.
# ------------------------------------------------------------
repo="$(new_fixture ohos)"
cat > "$repo/engine/crates/demo/src/lib.rs" <<'RUST'
#[cfg(all(target_os = "linux", target_env = "ohos"))]
pub mod ohos;
RUST
printf 'pub fn present() {}\n' > "$repo/engine/crates/demo/src/ohos.rs"
status=0
output="$(run_verify "$repo" --plan-only)" || status=$?
assert_status "$status" 0 "an OpenHarmony-conditional change plans cleanly"
assert_contains "$output" "TARGET ohos compile" \
    "OpenHarmony is recognised through target_env, not target_os"

status=0
output="$(run_verify "$repo")" || status=$?
assert_status "$status" 1 "a target with no local build fails the run"
assert_contains "$output" "NOT PROVEN" "the missing target build is reported, not skipped"
assert_contains "$output" "ohos compile" "the unproven target is named"
# The discriminating part. Without it, the exit code above is also produced by a
# fixture whose host steps fail, so the assertion would hold with the NOT PROVEN
# rule deleted.
assert_no_line_starting "$output" "FAIL" \
    "NOT PROVEN is the only thing that failed this run"

# ------------------------------------------------------------
# A run with nothing to prove passes. Without this, an always-red script would
# satisfy every assertion above.
# ------------------------------------------------------------
repo="$(new_fixture clean)"
printf 'edited\n' > "$repo/docs/note.md"
status=0
output="$(run_verify "$repo")" || status=$?
assert_status "$status" 0 "a change needing no target build passes"
assert_contains "$output" "verified for every target this change touches" \
    "the passing verdict is stated"
assert_absent "$output" "NOT PROVEN" "nothing is left unproven"

# ------------------------------------------------------------
# Android's two tiers.
# ------------------------------------------------------------
repo="$(new_fixture android)"
cat > "$repo/engine/crates/demo/src/lib.rs" <<'RUST'
#[cfg(target_os = "android")]
pub mod android;
RUST
printf 'pub fn attach() {}\n' > "$repo/engine/crates/demo/src/android.rs"
status=0
output="$(run_verify "$repo" --plan-only)" || status=$?
assert_status "$status" 0 "an Android-conditional change plans cleanly"
assert_contains "$output" "TARGET android compile" "the Android compile tier is selected"

repo="$(new_fixture cdylib)"
mkdir -p "$repo/engine/crates/android-jni/src"
printf '[package]\nname = "migo-android-jni"\n' > "$repo/engine/crates/android-jni/Cargo.toml"
printf 'pub fn register() {}\n' > "$repo/engine/crates/android-jni/src/lib.rs"
status=0
output="$(run_verify "$repo" --plan-only)" || status=$?
assert_status "$status" 0 "a cdylib change plans cleanly"
assert_contains "$output" "TARGET android link" \
    "the cdylib asks for a link, which compiling its dependencies does not prove"

# ------------------------------------------------------------
# Scope is what the flags say.
# ------------------------------------------------------------
repo="$(new_fixture scope)"
git_quiet "$repo" checkout -q -b feature
cat > "$repo/engine/crates/demo/src/lib.rs" <<'RUST'
#[cfg(target_os = "android")]
pub mod android;
RUST
printf 'pub fn attach() {}\n' > "$repo/engine/crates/demo/src/android.rs"
git_quiet "$repo" add -A
git_quiet "$repo" commit -q -m "android module"
status=0
output="$(run_verify "$repo" --plan-only --base HEAD)" || status=$?
assert_status "$status" 0 "a working-tree scope plans cleanly"
assert_absent "$output" "TARGET" \
    "--base HEAD covers the working tree only, so a committed change is out of scope"
assert_contains "$output" "HEAD..HEAD plus the working tree" "the scope is stated"

status=0
output="$(run_verify "$repo" --plan-only --base master)" || status=$?
assert_contains "$output" "TARGET android compile" \
    "the default branch scope covers the branch's own commits"

# ------------------------------------------------------------
# The Android compile tier covers the crates it claims to cover.
#
# Against the real workspace, not a fixture: --compile-only builds one package and
# relies on its dependency closure to reach the other three. Narrow that selection
# and the mode still prints SUCCESS while covering less, which is the failure this
# whole script exists to prevent -- so the closure is asserted rather than assumed.
# ------------------------------------------------------------
selection="$(grep -o 'package_args=(-p [a-z0-9-]*)' "$ROOT/scripts/build-android-so.sh" || true)"
if [[ -z "$selection" ]]; then
    fail "cannot find the package --compile-only selects in scripts/build-android-so.sh"
else
    selection="${selection#package_args=(-p }"
    selection="${selection%)}"
    closure="$(cd "$ROOT/engine" && cargo tree --offline -p "$selection" \
        --edges normal --prefix none --no-dedupe 2>/dev/null | awk '{print $1}' | sort -u)"
    for crate in migo-core migo-graphics migo-platform migo-capi; do
        assert_contains "$closure" "$crate" \
            "--compile-only reaches $crate through $selection"
    done
fi

# ------------------------------------------------------------
# CI tests every build of every package this entry point tests, and no other.
#
# The gap this closes is the one that made the entry point necessary: two lists of
# crates in two files, one of them missing four crates, and nothing comparing them.
# It is compared per build -- a package and the features it is tested with -- not
# per crate name, because a name cannot tell two builds of one crate apart: the four
# Slim steps ran here and in no CI job while every assertion in this file passed,
# and master collected 42 Slim failures in that gap.
# ------------------------------------------------------------
#
# The local half asks the script rather than grepping it. It used to grep for
# `cargo test -p migo-...`, which coupled this check to a spelling: the day the
# host steps stopped carrying the word `cargo` themselves, the grep matched
# nothing -- and with no `|| true` under `set -euo pipefail`, this file died at
# that assignment, *before* reaching the `fail` below that exists to report
# exactly that case. Both extractions are non-fatal so the empty case is
# reportable, and both sides are read by the one parser the binary audit uses.
#
# Comments stripped before the extraction, not after: this workflow explains its own
# `--lib`/`--all-targets` choices in prose next to the commands, and a comment parsed
# as a command is a list that agrees with itself about the wrong thing.
ci_test_lines="$(sed 's/#.*//' "$ROOT/.github/workflows/pr-ci.yml" \
    | grep -E 'cargo test ' || true)"
ci_crates="$(printf '%s\n' "$ci_test_lines" \
    | grep -oE '\-p migo-[a-z0-9-]+' | grep -oE 'migo-[a-z0-9-]+' | sort -u || true)"
local_builds="$(bash "$ROOT/scripts/verify-change.sh" --list-host-steps 2>/dev/null \
    | python3 "$ROOT/scripts/lib/host_test_coverage.py" --configurations || true)"
ci_builds="$(printf '%s\n' "$ci_test_lines" | grep -oE 'cargo test .*' | sed 's/^cargo //' \
    | python3 "$ROOT/scripts/lib/host_test_coverage.py" --configurations || true)"
# Whole lines: `migo-services default` is a prefix of `migo-services default+code-signing`.
has_build() { grep -qxF -- "$2" <<< "$1"; }
if [[ -z "$ci_builds" ]]; then
    fail "cannot find the builds pr-ci.yml tests"
elif [[ -z "$local_builds" ]]; then
    fail "scripts/verify-change.sh --list-host-steps reported no tested build"
else
    while read -r build; do
        if has_build "$ci_builds" "$build"; then
            pass "pr-ci.yml tests $build too"
        else
            fail "pr-ci.yml tests $build too -- no pr-ci.yml cargo test builds it"
        fi
    done <<< "$local_builds"

    # ------------------------------------------------------------
    # And the other direction, which is the one that was missing.
    #
    # Only `local ⊆ CI` was ever asserted, and it is the harmless half: a local step
    # CI lacks makes CI narrower than a developer's machine, which is a hole in the
    # merge gate. `CI ⊆ local` is the half that makes the local verdict *false* --
    # the run prints "verified for every target this change touches" about a change
    # CI will reject. `migo-capi-abi` lived in exactly that gap: 60 ABI and
    # versioned-header tests, run by CI, run by no local step, and every assertion
    # in this file passed.
    # ------------------------------------------------------------
    while read -r build; do
        if has_build "$local_builds" "$build"; then
            pass "scripts/verify-change.sh tests $build too"
        else
            fail "scripts/verify-change.sh tests $build too -- no host step builds it"
        fi
    done <<< "$ci_builds"
fi

# The comparison is only as fine as the parser: one that dropped features would read
# Full and Slim as the same build on both sides and agree. So a feature set is pinned
# as a build of its own, and two spellings of one set as one build.
builds_of() {
    printf '%s\n' "$@" | python3 "$ROOT/scripts/lib/host_test_coverage.py" --configurations \
        | grep -c . || true
}
if [[ "$(builds_of 'test -p migo-core --lib' \
    'test -p migo-core --lib --no-default-features --features profile-slim')" -eq 2 ]]; then
    pass "a feature set is a build of its own"
else
    fail "the parser reads Full and Slim as one build, so neither list can be short of one"
fi
if [[ "$(builds_of 'test -p migo-io --features a,b' 'test -p migo-io --features "b a"')" -eq 1 ]]; then
    pass "two spellings of one feature set are one build"
else
    fail "the parser reads one feature set as two builds, so equal lists would disagree"
fi

# ------------------------------------------------------------
# CI's own commands leave no test binary unrun.
#
# `verify-change.sh` now refuses to produce a verdict when one of its steps does
# not cover a test binary. CI has no such refusal and had the identical gap for the
# identical reason -- every command said `--lib` -- so 35 tests in seven binaries
# ran in no job at all. Asking the *same parser* about the workflow's own cargo
# lines is what holds the two sides at one scope: a second implementation of "which
# targets does this flag select" would disagree with the first exactly when it
# matters.
# ------------------------------------------------------------
if [[ -n "$ci_test_lines" ]]; then
    ci_unrun=""
    audit_status=0
    ci_unrun="$(printf '%s\n' "$ci_test_lines" \
        | grep -oE 'cargo test .*' | sed 's/^cargo //' \
        | python3 "$ROOT/scripts/lib/host_test_coverage.py" --root "$ROOT")" \
        || audit_status=$?
    if [[ "$audit_status" -ne 0 ]]; then
        fail "the test-binary audit could not describe this workspace, so CI's scope is unknown"
    elif [[ -n "$ci_unrun" ]]; then
        fail "no pr-ci.yml cargo command runs these test binaries:"
        printf '%s\n' "$ci_unrun" | sed 's/^/    /' >&2
    else
        pass "every test binary is run by a pr-ci.yml cargo command"
    fi
fi

# ------------------------------------------------------------
# CI lints every crate it tests.
#
# The same two-lists-in-one-file drift, one step later. Clippy for graphics, core,
# capi, platform and audio ran in no job at all while their tests ran in one --
# nothing compared the lint list to the test list, so the omission was invisible.
# Comparing them means a crate added to a test line without a clippy line fails
# here rather than being linted nowhere for months.
#
# Split across two jobs on purpose (one installs system packages, one does not),
# so the check is against the file, not against any single job.
# ------------------------------------------------------------
#
# `|| true` because the whole point of the empty case is to report it: under
# `set -euo pipefail` a grep that matches nothing kills the script, and a contract
# that dies silently when its subject disappears is exactly the vacuous pass this
# file exists to prevent.
clippy_crates="$(grep -E 'cargo clippy' "$ROOT/.github/workflows/pr-ci.yml" \
    | grep -oE '\-p migo-[a-z0-9-]+' | grep -oE 'migo-[a-z0-9-]+' | sort -u || true)"
if [[ -z "$clippy_crates" ]]; then
    fail "cannot find the crates pr-ci.yml runs clippy on"
else
    while read -r crate; do
        assert_contains "$clippy_crates" "$crate" "pr-ci.yml lints $crate too"
    done <<< "$ci_crates"
fi

# ------------------------------------------------------------
# The contract lane exists, and is derived rather than restated.
#
# `verify-change.sh` had no concept of the source-structure gates for months: they
# lived only in pr-ci.yml, so a change that only one of them can catch -- a
# resolver an entry point stopped calling, an import a crate must not have -- was
# reported locally as "verified for every target this change touches". This asserts
# the lane is present, is not vacuous, and names the ad reward gate specifically,
# because that is the one whose absence was measured.
# ------------------------------------------------------------
lane="$(bash "$ROOT/scripts/verify-change.sh" --base HEAD --plan-only 2>&1 || true)"
assert_contains "$lane" "CONTRACT" "the plan reports a contract lane"
assert_contains "$lane" "scripts/test-ad-reward-integrity-contract.sh" \
    "the contract lane includes the ad reward integrity gate"

derived="$(python3 "$ROOT/scripts/lib/ci_contract_gates.py" "$ROOT" | grep -c . || true)"
if [[ "$derived" -ge 15 ]]; then
    pass "the contract lane derives $derived gate(s) from pr-ci.yml"
else
    fail "the contract lane derived only $derived gate(s); a lane this small is a parse that stopped matching"
fi

# Every gate the workflow's quality-gate job runs must be in the lane, answered by
# the same parse that builds the lane rather than by a second grep -- a grep would
# also match the build and Qt jobs, whose gates need release artifacts, and would
# report them as missing forever.
unaccounted="$(python3 "$ROOT/scripts/lib/ci_contract_gates.py" --audit "$ROOT" || true)"
if [[ -z "$unaccounted" ]]; then
    pass "no quality-gate contract script is missing from the local lane"
else
    fail "contract script(s) CI runs are absent from the local lane:"
    printf '%s\n' "$unaccounted" >&2
fi

# ------------------------------------------------------------
# The lane is run, and its verdict decides the run.
#
# Two properties, and a fixture with real gates is what separates them: that the
# loop executes (a passing gate appears in the verdict) and that a gate's refusal
# fails the run (an exit code, not a printed line). A mutant that planned the lane
# and never ran it satisfied every earlier check.
# ------------------------------------------------------------
repo="$(new_fixture lane)"
add_stub_gates "$repo"
status=0
output="$(run_verify "$repo")" || status=$?
assert_contains "$output" "test-stub-16-contract.sh" \
    "a gate after one that drains stdin still reaches the verdict"
assert_contains "$output" "PASS" "a passing gate is recorded as PASS"

repo="$(new_fixture lane_refusal)"
add_stub_gates "$repo" failing
status=0
output="$(run_verify "$repo")" || status=$?
assert_status "$status" 1 "a contract gate that refuses fails the whole run"
assert_contains "$output" "FAIL" "the refusing gate is recorded as FAIL"

# ------------------------------------------------------------
# The host suites are selective, and selective in the safe direction.
#
# Running all sixteen on every invocation made a Java-only change pay for the whole
# Rust tree. The risk of fixing that is the opposite failure, which is silent: a suite
# that should have run and did not. So the closure is checked in both directions --
# a leaf change reaches its dependents, and anything unreasonable-about widens to
# everything.
# ------------------------------------------------------------
repo="$(new_fixture hostsuites)"
printf 'pub fn changed() {}\n' >> "$repo/engine/crates/shared/src/lib.rs"

status=0
output="$(run_verify "$repo" --plan-only)" || status=$?
assert_contains "$output" "HOSTSUITES" "the plan names the host suites it needs"
assert_contains "$output" "migo-shared" "a changed crate is in its own closure"
assert_contains "$output" "migo-io" "a leaf change reaches the crates that depend on it"

status=0
output="$(run_verify "$repo")" || status=$?
assert_contains "$output" "test -p migo-io" \
    "the dependent's suite is actually run, not merely planned"
assert_absent "$output" "test -p migo-capi" \
    "a crate that cannot see the change does not pay for it"

# A path the selector cannot reason about must widen, not narrow.
repo="$(new_fixture hostsuites_unknown)"
printf 'echo changed\n' >> "$repo/scripts/verify-change.sh"
status=0
output="$(run_verify "$repo" --plan-only)" || status=$?
assert_contains "$output" "HOSTSUITES ALL" \
    "a change outside engine/ that could affect anything runs every suite"

repo="$(new_fixture hostsuites_java)"
mkdir -p "$repo/platforms/android/library/src/main/java/com/migo/runtime"
printf 'class Changed {}\n' \
    > "$repo/platforms/android/library/src/main/java/com/migo/runtime/Changed.java"
status=0
output="$(run_verify "$repo" --plan-only)" || status=$?
assert_contains "$output" "HOSTSUITES NONE" \
    "a Java-only change asks for no cargo suite"

if [[ "$failures" -ne 0 ]]; then
    printf '\033[0;31m%s contract check(s) failed\033[0m\n' "$failures" >&2
    exit 1
fi
printf '\033[0;32mAll verify-change contract checks passed\033[0m\n'
