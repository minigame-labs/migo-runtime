#!/usr/bin/env bash
# ============================================================
# Local verification entry point.
# Location: scripts/verify-change.sh
#
# Answers one question: has this change been verified for every target it
# touches. The platform-target verification rule is the reason it
# exists -- host `cargo check`, `cargo test` and `cargo clippy` skip
# `cfg(target_os = "android")` code entirely, so a green host run is evidence
# about the portable tree and nothing else. Three Android compile errors rode
# this branch for several sessions on exactly that gap, on the touch path, on
# session teardown and on the permission gate, while every host run stayed green.
#
# What it does:
#   1. audits the module walk the target selection depends on, and stops if any
#      crate source file is unreachable -- an unreached file has unknown
#      conditions, and guessing "portable" is how the gap reopens;
#   2. asks scripts/lib/verification_targets.py which targets the changed files
#      need, and why;
#   3. runs the host suites, always;
#   4. runs the source-structure contract gates, derived from the workflow that
#      already runs them so the local verdict cannot drift from CI's;
#   5. runs the target builds it knows how to run;
#   6. prints one verdict line per target, and fails when a required target was
#      not proven -- including when the toolchain for it is simply absent. A skip
#      there would reproduce the exact failure this script exists to prevent.
#
# Usage:
#   scripts/verify-change.sh [--base <ref>] [--plan-only] [--abi <abi>]
#
#   --base <ref>  compare against this ref's merge base with HEAD.
#                 Default `master`: the scope a pull request would gate, which is
#                 the honest default for a branch that is never pushed.
#                 `--base HEAD` narrows to the working tree alone.
#   --plan-only   report the plan and exit; run nothing.
#   --abi <abi>   Android ABI for the target builds (default arm64-v8a).
#
# The verdict block is meant to be copied into the ledger. "Any change touching
# conditional code names the target build that compiled it" is a specification
# requirement, and a sentence nobody can produce cheaply does not get written.
# ============================================================

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

BASE="master"
PLAN_ONLY=false
ABI="arm64-v8a"

print_info()    { printf '\033[0;36m[verify] %s\033[0m\n' "$*"; }
print_success() { printf '\033[0;32m[verify] %s\033[0m\n' "$*"; }
print_error()   { printf '\033[0;31m[verify] %s\033[0m\n' "$*" >&2; }

# ------------------------------------------------------------
# The host suites, declared before anything runs so the script can report them.
#
# `--list-host-crates` exists because the contract needs this list and used to
# recover it by grepping this file for `cargo test -p migo-...`. That coupled a
# check to a spelling: the day the steps stopped carrying the word `cargo`
# themselves, the grep matched nothing and -- under `set -e`, with no `|| true`
# -- the contract died before it could say so. Asking the script is one
# authority instead of a regular expression guessing at one.
# ------------------------------------------------------------
HOST_CARGO_STEPS=(
    "build --workspace --all-targets"
    # Before the suites that depend on it: a broken counting allocator would
    # otherwise surface as an unexplained allocation gate failure downstream.
    "test -p migo-alloc-probe"
    "test -p migo-contention-probe"
    "test -p migo-executor-probe"
    # The C boundary rules, and the only crate here with no dependencies at all --
    # which is why they were split out of `capi` in the first place: an ABI rule
    # must be provable without a device or a graphics stack. It ran in CI and in
    # no local step, so the ABI and versioned-header suites A6 names were absent
    # from every local verdict that claimed to cover the change touching them.
    #
    # `--all-targets`, and the distinction is not cosmetic: all 60 tests live in
    # `tests/`, the lib has none, so `test -p migo-capi-abi --lib` would run
    # exactly zero of them and pass forever. A step that cannot fail is the thing
    # this project treats as decoration, and here it would have been a step that
    # cannot even run.
    "test -p migo-capi-abi --all-targets"
    # `--all-targets` for the same reason `capi-abi` needs it: every case lives
    # in `tests/`, so `--lib` would run exactly zero of them and pass forever.
    "test -p migo-frame-wire --all-targets"
    # The command decoder, which the external-frame product needs and the
    # embedded runtime now calls through a two-method adapter. `--all-targets`
    # for the same reason as its neighbours: every case lives in `tests/`.
    "test -p migo-frame-decode --all-targets"
    "test -p migo-shared"
    # V8's locale data, filtered at build time. Its tests read the full ICU file and check the linked bytes are the
    # filter applied to it; `runtime-v8`'s `Intl` tests are what check V8 accepts the result.
    "test -p migo-icu-data"
    # `--tests`, not `--lib`: the ASTC fixture emitter lives in `tests/` and is
    # `#[ignore]`d, so this runs none of its cases -- but it compiles the binary,
    # which is the half a gate driving it from outside cannot do. A fixture
    # emitter that stopped compiling would otherwise be found by
    # `test-astc-encoder.sh` in CI rather than here.
    "test -p migo-io --tests"
    # `--tests` rather than `--lib` for the two crates that own integration
    # binaries: `snapshot_roundtrip` and `worker_snapshot_roundtrip` for the
    # runtime, and the five golden-image and decode binaries for graphics. Every
    # gate in this repository said `--lib`, on both sides, so those 35 tests ran
    # in no job and no local run -- 1.7 seconds of execution that existed only as
    # source. They need no GPU: Skia rasterises to memory here.
    "test -p migo-runtime-v8 --tests"
    # Carries the occupancy gate on the shared audio streaming worker, so a suite
    # that ran nowhere would leave that gate as decoration.
    "test -p migo-audio --lib"
    "test -p migo-graphics --tests"
    "test -p migo-core --lib"
    "test -p migo-capi --lib"
    "test -p migo-platform --lib"
    # The Slim product profile, which nothing ran until 2026-08-08. Every crate
    # above declares `default = ["profile-full"]`, so a plain `cargo test` compiles
    # `api-media`, `api-commerce`, `api-connectivity`, `api-sensors` and
    # `api-system` **on** and never builds a single `cfg(not(feature = ...))`
    # branch. The first Slim host run reported 36 failures, and one group of them
    # was a real defect rather than a test gap: the window-resize ingress lived in
    # the `api-connectivity` extension, so no Slim build ever adopted its surface
    # size and every canvas kept the size the window had before a rotation.
    #
    # `runtime-v8` and `core` are the two crates whose capability surface the
    # profile selects, and `capi` and `platform` are the two that re-export it to a
    # host: `platform`'s `profile-slim` drops all five `api-*` features, and A6's
    # lifecycle, reattachment and input-saturation suites are `capi` lib tests, so
    # without these two steps "both product profiles" covered neither of the three.
    # The comment that used to sit here said `capi` and `platform` "do not build on
    # the host at all", four lines under the two steps that build and test them.
    #
    # `graphics` has no Slim step because it cannot have a meaningful one:
    # `profile-full` and `profile-slim` both expand to exactly `["embed_icudtl"]`,
    # so the two builds are the same build. `capi-abi` has no step for the stronger
    # version of the same reason -- it declares no features and has no dependencies,
    # so one build of it is every build of it.
    "test -p migo-runtime-v8 --tests --no-default-features --features profile-slim"
    "test -p migo-core --lib --no-default-features --features profile-slim"
    "test -p migo-capi --lib --no-default-features --features profile-slim"
    "test -p migo-platform --lib --no-default-features --features profile-slim"
    "fmt --all --check"
)

if [[ "${1:-}" == "--list-host-crates" ]]; then
    for step in "${HOST_CARGO_STEPS[@]}"; do
        [[ "$step" =~ -p\ (migo-[a-z0-9-]+) ]] && printf '%s\n' "${BASH_REMATCH[1]}"
    done | sort -u
    exit 0
fi

# The steps verbatim, because the crate names are not enough to say what runs.
# Two lists naming the same crates still run different binaries when one of them
# says `--lib`, and that one word is what hid nine `migo-capi-abi` test binaries
# from every local run. Scope belongs to whoever checks coverage, so it is
# published rather than re-derived by a grep with a different idea of the syntax.
if [[ "${1:-}" == "--list-host-steps" ]]; then
    printf '%s\n' "${HOST_CARGO_STEPS[@]}"
    exit 0
fi


while [[ $# -gt 0 ]]; do
    case "$1" in
        --base)      shift; [[ $# -gt 0 ]] || { print_error "--base requires a ref"; exit 2; }; BASE="$1" ;;
        --base=*)    BASE="${1#*=}" ;;
        --plan-only) PLAN_ONLY=true ;;
        --abi)       shift; [[ $# -gt 0 ]] || { print_error "--abi requires a value"; exit 2; }; ABI="$1" ;;
        --abi=*)     ABI="${1#*=}" ;;
        --list-host-crates) ;;  # handled above, before any work
        --list-host-steps)  ;;  # handled above, before any work
        --help|-h)   sed -n '2,40p' "${BASH_SOURCE[0]}"; exit 0 ;;
        *)           print_error "unknown argument: $1"; exit 2 ;;
    esac
    shift
done

cd "$ROOT"

case "$ABI" in
    arm64-v8a|x86_64) ;;
    *) print_error "unknown ABI: $ABI (expected arm64-v8a or x86_64)"; exit 2 ;;
esac

# ------------------------------------------------------------
# The module walk has to be complete before its answers mean anything.
# ------------------------------------------------------------
unreached="$(python3 scripts/lib/verification_targets.py --root . --audit)" || true
if [[ -n "$unreached" ]]; then
    print_error "no mod declaration reaches these crate sources, so their platform conditions are unknown:"
    while read -r orphan; do
        printf '  %s\n' "$orphan" >&2
    done <<< "$unreached"
    print_error "fix scripts/lib/verification_targets.py's module walk, or the declaration it cannot parse"
    exit 1
fi

# ------------------------------------------------------------
# Every test binary the workspace has must be run by one of the steps above.
#
# The same argument as the module walk, one layer out: an unreached source file
# has unknown conditions, and an unrun test binary has unknown behaviour. Nothing
# checked this, and the result was not marginal -- thirteen integration-test
# binaries holding 95 tests, of which 35 ran in no job and no local run at all.
# The cause was uniform and invisible to a crate-name comparison: every step, in
# every gate, said `--lib`. `migo-capi-abi` is the case that shows the shape,
# because its lib has no tests, so `--lib` there is a step that cannot fail.
#
# A compile is not coverage. `build --workspace --all-targets` builds all of them
# and runs none, which is how those binaries stayed green-adjacent for months.
#
# Fails closed. A binary this cannot account for is one whose behaviour no
# verdict below covers, and printing the verdict anyway is the precise failure
# this script exists to prevent.
# ------------------------------------------------------------
unrun="$(printf '%s\n' "${HOST_CARGO_STEPS[@]}" \
    | python3 scripts/lib/host_test_coverage.py --root .)"
case "$?" in
    0)  if [[ -n "$unrun" ]]; then
            print_error "no host step runs these test binaries, so nothing here covers them:"
            printf '%s\n' "$unrun" | sed 's/^/  /' >&2
            print_error "add a step to HOST_CARGO_STEPS, or widen one from --lib to --tests"
            exit 1
        fi
        ;;
    3)  print_info "host: no engine workspace in this tree; no test binaries to account for" ;;
    *)  print_error "cannot tell which test binaries the host steps run; coverage is unknown"
        exit 1
        ;;
esac

# ------------------------------------------------------------
# Scope
# ------------------------------------------------------------
if ! git rev-parse --verify --quiet "$BASE" > /dev/null; then
    print_error "not a ref: $BASE"
    exit 2
fi
merge_base="$(git merge-base HEAD "$BASE")"

changed="$(
    {
        git diff --name-only "$merge_base" HEAD
        git diff --name-only HEAD
        git ls-files --others --exclude-standard
    } | sort -u
)"
changed_count="$(printf '%s' "$changed" | grep -c . || true)"
scope="$BASE..HEAD plus the working tree ($changed_count files)"
print_info "scope: $scope"

if [[ "$changed_count" -eq 0 ]]; then
    print_info "nothing changed against $BASE"
fi

plan="$(printf '%s\n' "$changed" | python3 scripts/lib/verification_targets.py --root .)"
if [[ -n "$plan" ]]; then
    printf '%s\n' "$plan"
fi

if grep -q '^UNDETERMINED$' <<< "$plan"; then
    print_error "changed sources whose platform conditions could not be determined; see UNDETERMINED above"
    exit 1
fi

if [[ "$PLAN_ONLY" == true ]]; then
    if gates="$(python3 "$SCRIPT_DIR/lib/ci_contract_gates.py" "$ROOT" 2>&1)"; then
        printf 'CONTRACT %s gate(s) derived from .github/workflows/pr-ci.yml\n' \
            "$(printf '%s\n' "$gates" | grep -c .)"
        printf '%s\n' "$gates" | sed 's/^/  /'
    else
        printf 'CONTRACT undeterminable: %s\n' "$gates"
    fi
    exit 0
fi

# ------------------------------------------------------------
# Verdict accounting
# ------------------------------------------------------------
labels=()
commands=()
verdicts=()

record() {
    labels+=("$1")
    commands+=("$2")
    verdicts+=("$3")
}

run_step() {
    local label="$1" command="$2"
    print_info "$label: $command"
    if ( eval "$command" ); then
        record "$label" "$command" "PASS"
        return 0
    fi
    record "$label" "$command" "FAIL"
    return 1
}

# ------------------------------------------------------------
# Host suites. Always: they are the only evidence for the portable tree, and a
# target build does not run a single test.
# ------------------------------------------------------------

# `migo-graphics`, `migo-core`, `migo-capi` and `migo-platform` link Skia, and a
# minimal Linux host cannot build it with a bare `cargo`: it needs the system
# clang rather than the NDK's, the Khronos headers, and the linux-gnu V8
# archive. `dev-test-host.sh` is where this repository already establishes all
# three, so these steps run through it instead of restating any of it here.
#
# **Without this, four of the fourteen host steps failed on an untouched tree**,
# which is the worst state for a verifier to be in: it reports the same red
# whatever the change did, and the only thing it can teach a reader is to stop
# reading it. Measured that way — `--base HEAD` with nothing modified, four
# FAILs — before the routing was added.
HOST_CARGO="cd engine && cargo"
if bash "$ROOT/scripts/dev-test-host.sh" --probe >/dev/null 2>&1; then
    HOST_CARGO="bash scripts/dev-test-host.sh"
else
    # Said out loud rather than absorbed. On a host without the native
    # toolchain the Skia-linked steps below will fail for a reason that has
    # nothing to do with the change under verification, and a reader has to
    # know that is what they are looking at.
    print_info "host: native toolchain unavailable; Skia-linked suites will \
report their environment, not this change"
fi

# Which of those steps this change actually needs. The selector answers with the
# changed packages plus their reverse-dependency closure, so a change to a leaf crate
# still runs the suites of everything that depends on it -- that is where its
# behaviour is observed. A Java-only change now pays for none of them instead of
# sixteen.
#
# Every branch here fails towards running more. `ALL` is what the selector returns for
# a tree cargo cannot describe, for a file under `engine/` belonging to no member, and
# for any path outside `engine/` that is not provably irrelevant; a missing
# `HOSTSUITES` line lands in the same branch. `build --workspace --all-targets` and
# `fmt --all --check` are kept whenever any package is implicated, because several
# workspace members have no suite of their own and that build is the only thing that
# compiles them.
host_selection="ALL"
if host_line="$(grep -m1 '^HOSTSUITES ' <<< "$plan")"; then
    host_selection="${host_line#HOSTSUITES }"
fi

selected_host_steps=()
case "$host_selection" in
    ALL)
        selected_host_steps=("${HOST_CARGO_STEPS[@]}")
        ;;
    NONE)
        # Nothing Rust changed. `fmt` is kept because it is a second and costs
        # nothing, and because a stray unformatted file is worth catching wherever it
        # came from.
        for step in "${HOST_CARGO_STEPS[@]}"; do
            [[ "$step" == fmt* ]] && selected_host_steps+=("$step")
        done
        print_info "host: no workspace member changed; running formatting only"
        ;;
    *)
        for step in "${HOST_CARGO_STEPS[@]}"; do
            if [[ "$step" =~ -p\ (migo-[a-z0-9-]+) ]]; then
                package="${BASH_REMATCH[1]}"
                for candidate in $host_selection; do
                    if [[ "$candidate" == "$package" ]]; then
                        selected_host_steps+=("$step")
                        break
                    fi
                done
            else
                selected_host_steps+=("$step")
            fi
        done
        print_info "host: $((${#selected_host_steps[@]})) of ${#HOST_CARGO_STEPS[@]} \
steps for $host_selection"
        ;;
esac

for step in "${selected_host_steps[@]}"; do
    run_step "host" "$HOST_CARGO $step" || true
done

run_step "host" "git diff --check" || true

# ------------------------------------------------------------
# Source-structure contract gates.
#
# This script had no concept of them, and that is the same defect that put
# `android-java` below, one layer further out. They are gates over structure a
# test cannot reach -- what a crate may depend on, which resolver an entry point
# calls, whether an event's payload keys match its reader -- and they lived only
# in `.github/workflows/pr-ci.yml`. Found by A12's own mutation evidence:
# reverting one ad entry point to a bare handler lookup, which is the exact
# defect that item fixed, left every unit test green and was caught by a contract
# script this script never ran. So the local verdict said "verified for every
# target this change touches" about a change CI rejects.
#
# The list is derived from the workflow rather than restated here. A second copy
# would drift, and it would drift silently in the direction that matters: a gate
# added to CI and not here is a gate the local run does not have.
#
# They run unconditionally, like the host suites. Each is seconds, and keying
# them to changed files means maintaining a file list per gate -- a list to
# forget an entry from, which is how a gate stops covering what it names.
# ------------------------------------------------------------
# Every Gradle build this run drives uses the verifier's mode: the dependency cache
# rather than the network, and no daemon left behind. See the `android-java` lane below
# for both reasons. Exported rather than appended to a command, because the contract
# gates are invoked with the exact command line CI uses -- that parity is what stops
# the local lane drifting from CI -- so the flag cannot live in the command string.
export MIGO_GRADLE_VERIFIER=1

have_tool() {
    case "$1" in
        rg)      command -v rg >/dev/null 2>&1 ;;
        pyyaml)  python3 -c "import yaml" >/dev/null 2>&1 ;;
        gradlew) [[ -x "$ROOT/platforms/android/gradlew" ]] ;;
        *)       return 1 ;;
    esac
}

contract_gates="$(python3 "$SCRIPT_DIR/lib/ci_contract_gates.py" "$ROOT" 2>&1)"
case "$?" in
    0) ;;
    3)  # No workflow in this tree, so there are no CI gates to mirror. Said out
        # loud rather than omitted, because a silent absence is how a lane stops
        # covering anything.
        record "contract" "no .github/workflows/pr-ci.yml in this tree" "CI ONLY"
        contract_gates=""
        ;;
    *)  # The workflow is there and the lane could not be derived from it. That is
        # a failure, not missing evidence: the alternative is a run that silently
        # stops checking a whole class of gate.
        record "contract" "derive gate list from .github/workflows/pr-ci.yml" "FAIL"
        print_error "contract lane: $contract_gates"
        contract_gates=""
        ;;
esac

# Into an array first, and every gate runs with stdin closed. Both matter for the
# same reason: read from a here-string, a gate that consumes stdin -- one of these
# runs `cargo`, which does -- swallows the rest of the list, and the gates after it
# vanish from the verdict without a word. That is the silent under-run this lane
# exists to prevent, and it happened here first: three gates and the `CI ONLY` line
# were missing from a run that reported success.
contract_lane=()
if [[ -n "$contract_gates" ]]; then
    mapfile -t contract_lane <<< "$contract_gates"
fi

for entry in "${contract_lane[@]}"; do
    disposition="${entry%% *}"
    command="${entry#* }"
    [[ -n "$disposition" && -n "$command" ]] || continue
    case "$disposition" in
        run)
            run_step "contract" "$command" </dev/null || true
            ;;
        needs:*)
            tool="${disposition#needs:}"
            if have_tool "$tool"; then
                run_step "contract" "$command" </dev/null || true
            else
                record "contract" "$command" "NOT PROVEN"
            fi
            ;;
        skip:*)
            record "contract" "$command" "CI ONLY"
            ;;
    esac
done

# ------------------------------------------------------------
# Target builds. A platform with no entry here is reported NOT PROVEN, never
# skipped: `windows` conditional code has no local build on this machine, and
# that is a fact about the evidence, not a detail to swallow.
#
# `ohos` was in that sentence too, and it was wrong. The OpenHarmony SDK,
# the `*-unknown-linux-ohos` Rust target and the prebuilt V8 archive for the
# triple are all present here, and the compile takes seconds once warm — so
# every change to `cfg`-conditional Linux code was collecting a permanent
# NOT PROVEN that could be evidence. Checked against the objects rather than
# the sentence: `scripts/dev-setup-ohos.sh` resolves the SDK, and
# `engine/third_party/rusty_v8/x86_64-linux-ohos/librusty_v8.a` is in tree.
# ------------------------------------------------------------
#
# `android-java` is the Android SDK's other half. It is here because this script
# had no idea Java existed: `platforms/android/**` produced an empty plan, so a
# change to the shipped AAR's own sources ran eleven Rust suites, cross-compiled
# Rust for Android, and printed "verified for every target this change touches"
# without compiling a line of Java. That is the same defect this script was
# written to prevent -- a green run that is evidence about one language and
# silent about another -- one layer out from the `cfg(target_os = "android")`
# gap in its header.
#
# Both product variants, because the Java source set is variant-independent
# while `BuildConfig` capability gating is not: Slim compiles the same files
# with different flags, and a handler behind `MIGO_API_COMMERCE` that only Full
# exercises is exactly the shape this repository has shipped broken before.
target_command() {
    case "$1:$2" in
        android:compile)      echo "bash scripts/build-android-so.sh --compile-only $ABI" ;;
        android:link)         echo "bash scripts/build-android-so.sh $ABI" ;;
        ohos:compile)
            # Probed like `android-java`, so a machine without the OpenHarmony
            # SDK reports NOT PROVEN rather than a FAIL that says "your change
            # broke this" about evidence that machine never had.
            #
            # `x86_64` because what the lane proves is that the `target_env =
            # "ohos"` view of the tree still compiles, and that is the same view
            # for either architecture; it is also the one whose V8 archive and
            # target directory are already warm.
            #
            # It calls the SDK script rather than restating its cargo line, so
            # the toolchain pins keep one home.
            if [[ -f "$ROOT/engine/third_party/rusty_v8/x86_64-linux-ohos/librusty_v8.a" ]] \
                && bash "$ROOT/scripts/dev-setup-ohos.sh" >/dev/null 2>&1; then
                echo "bash scripts/build-ohos-sdk.sh --compile-only x86_64"
            fi
            ;;
        windows:compile)
            # Probed like `ohos`, because the source of truth is in WSL and the
            # MSVC toolchain is on the Windows side: a machine without Visual
            # Studio, the pinned Windows V8 import library, or the staged Skia
            # build deps reports NOT PROVEN rather than a FAIL about evidence it
            # never had.
            #
            # `x86_64` only: the lane proves the `cfg(windows)` / `msvc` view of
            # the tree still compiles, which is the same view for either Windows
            # architecture, and x86_64 is the one whose Skia target directory is
            # kept warm here.
            #
            # `verify-compile.sh` runs `sync-worktree.sh` first, so the Windows
            # copy carries the working tree this run is scoped to, not only the
            # last commit.
            if [[ -f "/mnt/c/Program Files (x86)/Microsoft Visual Studio/Installer/vswhere.exe" ]] \
                && [[ -f "$ROOT/engine/third_party/rusty_v8/x86_64-pc-windows-msvc/rusty_v8.lib" ]] \
                && [[ -x "$ROOT/platforms/windows/spike/verify-compile.sh" ]] \
                && [[ -f "/mnt/c/migo-win-spike-tmp/bin/ninja.exe" ]] \
                && [[ -f "/mnt/c/migo-win-spike-tmp/include/EGL/egl.h" ]]; then
                echo "bash platforms/windows/spike/verify-compile.sh"
            fi
            ;;
        android-java:compile)
            # Probed rather than assumed, so a machine without the Android
            # build reports NOT PROVEN like every other absent target. Running
            # a missing `gradlew` would record FAIL, which says "your change
            # broke this" about a machine that never had the evidence.
            # `--offline` because this lane verifies the sources, not the dependency
            # graph, and without it Gradle tries to refresh its external modules:
            # measured at over twenty minutes against seventeen seconds offline on a
            # machine whose network cannot reach the repositories quickly. A gate that
            # can hang that long is a gate people learn to skip. The failure mode it
            # introduces is loud and names its own cause -- "No cached version
            # available for offline mode" -- unlike a silent stall.
            #
            # `--no-daemon` because a run drives more than one Gradle build and a daemon
            # outlives its own build while holding the project lock. Measured: five
            # daemons alive, the one owning this build parked on a lock for seventeen
            # minutes having used twenty seconds of CPU, with `--quiet` printing nothing
            # while it waited. A gate that stalls silently is one people learn to skip.
            [[ -x "$ROOT/platforms/android/gradlew" ]] && echo "cd platforms/android && \
./gradlew --quiet --offline --no-daemon :library:testFullDebugUnitTest :library:testSlimDebugUnitTest"
            ;;
        *)                    echo "" ;;
    esac
}

# What a machine is missing, so the verdict is actionable. A lane records NOT PROVEN
# when its toolchain is absent, which is right -- but "no local build for this target"
# was the whole message, and for OpenHarmony two different absences produced it: no
# SDK, which one command installs, and no V8 archive, which is a build measured in
# hours. Those are not the same news.
target_absence_reason() {
    case "$1:$2" in
        ohos:compile)
            if [[ ! -f "$ROOT/engine/third_party/rusty_v8/x86_64-linux-ohos/librusty_v8.a" ]]; then
                echo "no OpenHarmony V8 archive; build it with scripts/build-v8-ohos.sh x86_64"
            else
                echo "no OpenHarmony SDK; see scripts/dev-setup-ohos.sh"
            fi
            ;;
        android-java:compile)
            echo "no Android Gradle wrapper at platforms/android/gradlew"
            ;;
        windows:compile)
            if [[ ! -f "/mnt/c/Program Files (x86)/Microsoft Visual Studio/Installer/vswhere.exe" ]]; then
                echo "no Visual Studio Build Tools; see platforms/windows/README.md"
            elif [[ ! -f "$ROOT/engine/third_party/rusty_v8/x86_64-pc-windows-msvc/rusty_v8.lib" ]]; then
                echo "no Windows V8 import library; build it with scripts/build-v8-windows.sh x86_64"
            else
                echo "Windows Skia build deps not staged; run platforms/windows/spike/stage-deps.sh"
            fi
            ;;
        *) echo "no local build for this target" ;;
    esac
}

# Into an array first, and every target build runs with stdin closed -- the same
# reason the contract lane above does, and the same bug found twice. Read from a
# here-string, Gradle inherits the remaining plan as its stdin: the `android-java`
# lane then sat for twelve minutes having used one second of CPU, while the identical
# command run by hand finished in nineteen seconds. A build that consumes the loop's
# input is indistinguishable from a build that is simply slow.
target_plan=()
while IFS= read -r line; do
    [[ "$line" == TARGET* ]] && target_plan+=("$line")
done <<< "$plan"

for line in "${target_plan[@]}"; do
    read -r keyword platform tier <<< "$line"
    [[ "$keyword" == "TARGET" ]] || continue
    command="$(target_command "$platform" "$tier")"
    if [[ -z "$command" ]]; then
        # Why it is absent, not just that it is. Both OpenHarmony probes used to
        # collapse into this one sentence, so the verdict could not tell a reader
        # whether to install an SDK or to spend hours building a V8 archive.
        record "$platform $tier" "$(target_absence_reason "$platform" "$tier")" "NOT PROVEN"
        continue
    fi
    run_step "$platform $tier" "$command" </dev/null || true
done

# ------------------------------------------------------------
# Verdicts
# ------------------------------------------------------------
echo
echo "VERIFIED SCOPE  $scope"
failed=0
for index in "${!labels[@]}"; do
    printf '%-11s %-11s %s\n' "${verdicts[$index]}" "${labels[$index]}" "${commands[$index]}"
    # `CI ONLY` is the one non-PASS verdict that does not fail the run, and
    # the exception is closed: it marks a gate that runs *this script* over
    # fixture repositories, so running it here would nest the gate inside
    # itself. Everything else -- including a target whose toolchain is
    # simply absent -- stays a failure, because unproven is not verified.
    case "${verdicts[$index]}" in
        PASS|"CI ONLY") ;;
        *) failed=1 ;;
    esac
done

if [[ "$failed" -ne 0 ]]; then
    print_error "not verified: see the non-PASS lines above"
    exit 1
fi
print_success "verified for every target this change touches"
