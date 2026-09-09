#!/usr/bin/env bash
# scripts/test-v8-patch-application-contract.sh
#
# Assert that the V8 patch stage cannot report success while leaving a patch
# unapplied or half-applied.
#
# WHY THIS EXISTS (observed, not hypothetical):
# Applied-ness used to be decided by grepping the target file for a hand-picked
# SENTINEL string. A sentinel restates what a patch does, so it can drift from
# the patch, and it drifted four separate ways:
#
#   - a sentinel copied from a patch's FIRST hunk survived a later hunk failing,
#     so the next run reported the half-applied patch as complete;
#   - a sentinel was truncated by the `|` field separator of the array that held
#     the declarations, and the surviving prefix already occurred in the
#     UNPATCHED file -- so 0002-install-sysroot.patch reported "already in
#     effect" on every build that has ever run, and was never once applied. Its
#     absence is what made the Android aarch64 build declare use_sysroot=true
#     without installing a sysroot, which is a gn assertion failure;
#   - a sentinel named a string that also occurs elsewhere in its target file;
#   - 0007-windows-register-host-callbacks-from-rust.patch spans FIVE files and
#     the sentinel checked only src/V8.rs, the last of them. patch does not stop
#     at the first failing file, so a run where src/cppgc.rs failed still
#     patched src/V8.rs; the next run then saw the sentinel, said "already in
#     effect", and built with four of five files unpatched.
#
# WHY IT CHECKS WHAT IT CHECKS:
# The replacement asks `patch` whether the patch reverse-applies, which is
# derived from the patch itself and covers every file and hunk in it. But the
# obvious spelling of that probe is a trap: with `--reverse` alone, GNU patch
# hits its "Unreversed patch detected!  Ignoring -R." heuristic, decides we
# meant to apply the patch, applies it, and exits 0 -- so an unapplied patch is
# indistinguishable from an applied one, which is the same blindness in a new
# costume. `--forward` is what turns that heuristic into "Skipping patch." with
# a non-zero exit.
#
# Because the whole guard hinges on one flag, this test does not only exercise
# the fixtures: it re-runs them against a copy of the library with `--forward`
# removed and requires them to FAIL. A guard that passes with and without its
# load-bearing flag is not a guard.
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
LIB="$SCRIPT_DIR/lib/v8-patch-apply.sh"
LIB_HOST="$SCRIPT_DIR/lib/host-requirements.sh"
PATCH_DIR="$REPO_ROOT/engine/third_party/v8-patches"
TAG='[v8-patch-apply]'

pass() { echo -e "\033[0;32m$TAG PASS $*\033[0m"; }
fail() { echo -e "\033[0;31m$TAG FAIL $*\033[0m" >&2; failures=$((failures + 1)); }
info() { echo -e "\033[0;36m$TAG $*\033[0m"; }
failures=0

[[ -f "$LIB" ]] || { echo "$TAG missing library: $LIB" >&2; exit 1; }

# The fixtures need somewhere to build throwaway trees. Say so plainly if there
# is nowhere: a sandboxed reviewer with no writable temp directory otherwise sees
# a bare exit 1 and reads it as a failing contract.
scratch="$(mktemp -d 2>/dev/null)" || {
    echo "$TAG cannot create a temporary directory under TMPDIR=${TMPDIR:-/tmp};" >&2
    echo "$TAG set TMPDIR to a writable path to run the behavioural fixtures." >&2
    exit 2
}
rmdir "$scratch"

# The fixtures below drive the real functions directly. `run_fixtures` sources
# them again from a possibly-mutated copy in a subshell, so this outer source
# never sees those mutations.
# shellcheck source=scripts/lib/v8-patch-apply.sh
source "$LIB"

# ---------------------------------------------------------------------------
# Behavioural fixtures. `lib` is the library under test so the same fixtures
# can be pointed at a deliberately broken copy of it.
# ---------------------------------------------------------------------------
run_fixtures() {
    local lib="$1" quiet="${2:-}"
    local w rc local_failures=0
    w="$(mktemp -d)"
    mkdir -p "$w/tree" "$w/patches"

    cat > "$w/harness.sh" <<EOS
source "$lib"
EOS
    cat > "$w/patches/two-hunk.diff" <<'EOP'
--- a/f.txt
+++ b/f.txt
@@ -1,3 +1,3 @@
 alpha
-A1
+A2
 beta
@@ -4,3 +4,3 @@
 gamma
-B1
+B2
 delta
EOP
    local pristine='alpha
A1
beta
gamma
B1
delta'
    local applied='alpha
A2
beta
gamma
B2
delta'
    local half='alpha
A2
beta
gamma
B1
delta'
    # The same half-applied tree with the two hunks the other way round. This is
    # the ordering that matters: `patch --forward` walks hunks in file order, so
    # here it WRITES hunk 1 before it reaches the already-applied hunk 2 and
    # fails. Without a dry-run preflight the failed call leaves the tree more
    # modified than it found it, and the next run starts from that new state.
    local half_reversed='alpha
A1
beta
gamma
B2
delta'
    local drifted='alpha
A1
beta
GAMMA-RENAMED
B1
delta'

    # shellcheck disable=SC2317  # invoked through `check` below
    probe() { ( source "$w/harness.sh"; v8_patch_is_in_effect "$w/tree" "$w/patches/two-hunk.diff" ); }
    apply() { ( source "$w/harness.sh"; v8_require_patch "$w/tree" "$w/patches" 'two-hunk.diff' ); }

    check() { # description, expected(pass|fail), command
        local desc="$1" expect="$2"
        "$3" >/dev/null 2>&1; rc=$?
        if { [[ "$expect" == pass && $rc -eq 0 ]] || [[ "$expect" == fail && $rc -ne 0 ]]; }; then
            [[ -n "$quiet" ]] || pass "$desc"
        else
            [[ -n "$quiet" ]] || fail "$desc (rc=$rc, wanted $expect)"
            local_failures=$((local_failures + 1))
        fi
    }
    check_file() { # description, expected-content
        if [[ "$(cat "$w/tree/f.txt")" == "$2" ]]; then
            [[ -n "$quiet" ]] || pass "$1"
        else
            [[ -n "$quiet" ]] || fail "$1"
            local_failures=$((local_failures + 1))
        fi
    }

    printf '%s\n' "$pristine" > "$w/tree/f.txt"
    check "an unapplied patch is not reported in effect" fail probe
    check "an unapplied patch is applied" pass apply
    check_file "every hunk landed" "$applied"
    check "an applied patch is reported in effect" pass probe
    check "a second run is a no-op" pass apply
    check_file "the no-op left the file untouched" "$applied"

    printf '%s\n' "$half" > "$w/tree/f.txt"
    check "a half-applied tree is not reported in effect" fail probe
    check "a half-applied tree is refused, not half-fixed" fail apply
    check_file "the refusal did not mutate the half-applied tree" "$half"

    printf '%s\n' "$half_reversed" > "$w/tree/f.txt"
    check "an unapplied-then-applied tree is not reported in effect" fail probe
    check "an unapplied-then-applied tree is refused" fail apply
    check_file "the refusal did not write the leading hunk" "$half_reversed"

    printf '%s\n' "$drifted" > "$w/tree/f.txt"
    check "a hunk whose context no longer matches is refused" fail apply
    check_file "the refusal did not mutate the drifted tree" "$drifted"

    printf '%s\n' "$pristine" > "$w/tree/f.txt"
    absent() { ( source "$w/harness.sh"; v8_require_patch "$w/tree" "$w/patches" 'no-such-*.diff' ); }
    check "an absent patch file is refused" fail absent

    rm -rf "$w"
    return $local_failures
}

info "behavioural fixtures against the real library"
run_fixtures "$LIB" || failures=$((failures + $?))

# ---------------------------------------------------------------------------
# The fixtures must be sensitive to the flags the whole guard rests on.
#
# A mutant has to live in a directory beside a copy of host-requirements.sh. The
# library sources that by a path relative to itself, so a mutant dropped into a
# bare `mktemp` file resolves the dependency to $TMPDIR and fails to load at all
# -- and a mutant that never defines its functions makes every fixture fail for
# the wrong reason, which reads as perfect sensitivity. That is exactly the shape
# of false confidence this whole contract exists to prevent, so `make_mutant`
# additionally verifies the mutant loads and differs from the original.
# ---------------------------------------------------------------------------
MUTANT_DIRS=()
make_mutant() { # sed-expression -> prints the mutant library path
    local expression="$1" dir
    dir="$(mktemp -d)"
    MUTANT_DIRS+=("$dir")
    cp "$LIB_HOST" "$dir/$(basename "$LIB_HOST")"
    sed "$expression" "$LIB" > "$dir/$(basename "$LIB")"
    printf '%s' "$dir/$(basename "$LIB")"
}
mutant_is_usable() { # mutant path, description
    local mutant="$1" desc="$2"
    if cmp -s "$mutant" "$LIB"; then
        fail "could not construct the mutant: $desc"
        return 1
    fi
    if ! ( source "$mutant" >/dev/null 2>&1 \
           && [[ "$(type -t v8_assert_tree_is_exactly_patched)" == function ]] ); then
        fail "the mutant does not load, so the fixtures would fail for the wrong reason"
        return 1
    fi
    return 0
}

info "sensitivity: fixtures must fail without --forward on the reverse probe"
MUTANT="$(make_mutant 's|--dry-run --reverse --forward --fuzz=0|--dry-run --reverse --fuzz=0|')"
if ! mutant_is_usable "$MUTANT" "the reverse probe no longer spells --forward"; then
    :
elif run_fixtures "$MUTANT" quiet; then
    fail "fixtures still pass with --forward removed -- they do not test the guard"
else
    pass "fixtures detect the removal of --forward"
fi

info "sensitivity: fixtures must fail without the forward dry-run preflight"
# Drop the preflight by making it unconditionally succeed, so the mutant goes
# straight to the mutating apply the way the first draft of this library did.
MUTANT="$(make_mutant 's|if ! patch -p1 -d "$tree" --batch --dry-run --forward --fuzz=0 < "$pf" >/dev/null 2>&1; then|if false; then|')"
if ! mutant_is_usable "$MUTANT" "the preflight is not spelled as expected"; then
    :
elif run_fixtures "$MUTANT" quiet; then
    fail "fixtures still pass without the preflight -- a failed apply may leave the tree modified"
else
    pass "fixtures detect a missing preflight"
fi

info "sensitivity: fixtures must fail without --fuzz=0 on the forward invocations"
# Only the forward preflight and the forward apply. Stripping --fuzz=0 from the
# reverse probe as well would let the drifted fixture fail because *application*
# became fuzzy, which says nothing about the probe -- a sensitivity check that
# cannot attribute the failure it observes is not evidence.
MUTANT="$(make_mutant '/patch -p1/{/--reverse/! s| --fuzz=0||}')"
lib_fuzz=$(grep -c -- 'patch -p1.*--fuzz=0' "$LIB")
mut_fuzz=$(grep -c -- 'patch -p1.*--fuzz=0' "$MUTANT")
mut_reverse_fuzz=$(grep -c -- 'patch -p1.*--reverse.*--fuzz=0' "$MUTANT")
if (( lib_fuzz < 2 )); then
    fail "expected --fuzz=0 on the reverse probe and at least one forward invocation, found $lib_fuzz"
elif (( mut_fuzz != 1 || mut_reverse_fuzz != 1 )); then
    fail "the mutant did not isolate the forward --fuzz=0 (kept $mut_fuzz, reverse $mut_reverse_fuzz)"
elif ! mutant_is_usable "$MUTANT" "no forward invocation spells --fuzz=0"; then
    :
elif run_fixtures "$MUTANT" quiet; then
    fail "fixtures still pass with fuzzy forward application -- context drift goes undetected"
else
    pass "fixtures detect fuzzy forward application"
fi
rm -rf "${MUTANT_DIRS[@]}"

# ---------------------------------------------------------------------------
# No build script may re-introduce a target-file applied-ness gate.
# ---------------------------------------------------------------------------
info "a checkout must be HEAD plus exactly the declared patches, submodules included"
# The real rusty_v8 tree carries two of its four patches inside the `build`
# submodule, which surfaces in the parent as one opaque gitlink entry. So the
# fixture has a submodule too: without descent, a submodule change is reported as
# a single undeclared modification and the accept case below cannot pass.
replay() {
    local w super sub rc=0
    w="$(mktemp -d)"
    git_q() { git -c user.email=t@t -c user.name=t -c init.defaultBranch=main \
                  -c protocol.file.allow=always "$@"; }
    super="$w/super"; sub="$w/sub"
    mkdir -p "$super" "$sub" "$w/patches"

    printf 'inner-one\ninner-two\ninner-three\n' > "$sub/inner.txt"
    printf 'sub-untouched\n' > "$sub/other.txt"
    ( cd "$sub" && git_q init -q . && git_q add -A && git_q commit -q -m base )

    printf 'top-one\ntop-two\ntop-three\n' > "$super/top.txt"
    ( cd "$super" && git_q init -q . && git_q add -A && git_q commit -q -m base \
        && git_q submodule add -q "$sub" sub && git_q commit -q -m addsub )

    cat > "$w/patches/t-top.diff" <<'EOP'
--- a/top.txt
+++ b/top.txt
@@ -1,3 +1,3 @@
 top-one
-top-two
+TOP-TWO
 top-three
EOP
    cat > "$w/patches/t-sub.diff" <<'EOP'
--- a/sub/inner.txt
+++ b/sub/inner.txt
@@ -1,3 +1,3 @@
 inner-one
-inner-two
+INNER-TWO
 inner-three
EOP

    local -a globs=('t-top.diff' 't-sub.diff')
    local -a exempt=()
    tcheck() { # description, expected(pass|fail)
        local desc="$1" expect="$2" got
        v8_assert_tree_is_exactly_patched "$super" "$w/patches" \
            "${exempt[@]}" "${globs[@]}" >/dev/null 2>&1; got=$?
        if { [[ "$expect" == pass && $got -eq 0 ]] || [[ "$expect" == fail && $got -ne 0 ]]; }; then
            pass "$desc"
        else
            fail "$desc (rc=$got, wanted $expect)"; rc=1
        fi
    }
    apply_all() {
        patch -p1 -d "$super" --batch --forward --fuzz=0 < "$w/patches/t-top.diff" >/dev/null
        patch -p1 -d "$super" --batch --forward --fuzz=0 < "$w/patches/t-sub.diff" >/dev/null
    }

    tcheck "a pristine checkout is refused, the patches are not applied" fail
    apply_all
    tcheck "a checkout that is HEAD plus both patches is accepted" pass

    printf 'inner-one\nINNER-TWO\ninner-three\nsmuggled\n' > "$super/sub/inner.txt"
    tcheck "an extra edit inside a patched submodule file is refused" fail
    printf 'inner-one\nINNER-TWO\ninner-three\n' > "$super/sub/inner.txt"

    printf 'changed\n' > "$super/sub/other.txt"
    tcheck "an edit to an untouched submodule file is refused" fail

    # The same edit, with the parent configured not to mention the submodule at all.
    # `submodule.<name>.ignore = all` makes the parent's `git status` omit it, so a descent
    # driven by the parent reporting a dirty gitlink silently never happens and the edit
    # seals. The fixture first asserts the bypass is real, so a pass here cannot come from
    # the configuration having had no effect.
    git_q -C "$super" config submodule.sub.ignore all
    if git_q -C "$super" status --porcelain | grep -q '^ M sub$'; then
        fail "submodule.ignore=all did not hide the dirty submodule; the case is not exercised"
        rc=1
    else
        tcheck "an edit hidden by submodule.ignore=all is still refused" fail
    fi
    git_q -C "$super" config --unset submodule.sub.ignore
    printf 'sub-untouched\n' > "$super/sub/other.txt"

    printf 'top-one\nTOP-TWO\ntop-three\nsmuggled\n' > "$super/top.txt"
    tcheck "an extra edit inside a patched top-level file is refused" fail
    printf 'top-one\nTOP-TWO\ntop-three\n' > "$super/top.txt"

    # Equal bytes, different mode. A patch can carry old mode/new mode, so bytes
    # alone are not the whole of "is this what the patches produce".
    chmod +x "$super/top.txt"
    tcheck "a flipped executable bit on a patched file is refused" fail
    chmod -x "$super/top.txt"
    tcheck "restoring the mode makes it acceptable again" pass

    printf 'stray\n' > "$super/tool-binary"
    tcheck "an untracked file no declared patch creates is refused" fail
    exempt=(--accounted 'tool-binary')
    tcheck "an untracked file whose provenance is declared elsewhere is accepted" pass
    exempt=(--accounted 'tool-binar')
    tcheck "a near-miss accounted path does not exempt the file" fail
    exempt=()
    tcheck "the exemption does not persist once it is not passed" fail
    rm -f "$super/tool-binary"

    # A file another platform's committed patch *creates*. One checkout serves every
    # platform's V8 build, so this is the shape that made two of them mutually
    # exclusive: the file is explained by a committed patch, just not by one this
    # declaration applies.
    cat > "$w/patches/t-foreign-create.diff" <<'EOP'
--- /dev/null
+++ b/foreign/toolchain.gn
@@ -0,0 +1 @@
+foreign
EOP
    mkdir -p "$super/foreign"
    printf 'foreign\n' > "$super/foreign/toolchain.gn"
    tcheck "a file only a foreign patch creates is refused when not accounted for" fail
    exempt=(--accounted-patch 't-foreign-create.diff')
    tcheck "a foreign patch accounts for the path it creates" pass
    rm -rf "$super/foreign"

    # Accounting is derived from what the patch creates, so a patch that only modifies
    # cannot grant one: doing so would skip content verification on a file this
    # platform's own patches may also touch. With the tree otherwise exactly patched,
    # a refusal here can only come from that guard.
    exempt=(--accounted-patch 't-top.diff')
    tcheck "a foreign patch that creates nothing cannot account for a path" fail
    exempt=()

    # A submodule moved off the commit its parent records. The declared patch still
    # applies to inner.txt there, so descending would take that foreign HEAD as the
    # pristine baseline and report the tree clean -- certifying an artifact built
    # from sources the parent never pinned.
    ( cd "$sub" && printf 'a-later-commit\n' > other.txt \
        && git_q add -A && git_q commit -q -m later ) >/dev/null 2>&1
    local later pinned_at
    later="$(git_q -C "$sub" rev-parse HEAD 2>/dev/null)"
    ( cd "$super/sub" && git_q fetch -q origin && git_q checkout -q "$later" ) >/dev/null 2>&1
    pinned_at="$(git_q -C "$super" rev-parse HEAD:sub 2>/dev/null)"
    if [[ -n "$later" && "$later" != "$pinned_at" ]] \
       && [[ "$(git_q -C "$super/sub" rev-parse HEAD)" == "$later" ]]; then
        patch -p1 -d "$super" --batch --forward --fuzz=0 < "$w/patches/t-sub.diff" \
            >/dev/null 2>&1
        tcheck "a submodule moved off the commit its parent records is refused" fail
    else
        fail "could not move the fixture submodule off its pinned commit"
        rc=1
    fi

    rm -rf "$w"
    return $rc
}
replay || failures=$((failures + 1))

# ---------------------------------------------------------------------------
# The library has to answer honestly about a tree it cannot read.
#
# Neither case below is hypothetical. The vendored rusty_v8 checkout is owned by
# another account on a shared workspace, and every caller spells its path with a
# `..` component (`$PROJECT_ROOT/../rusty_v8_src`). git compares safe.directory
# literally against the repository path it discovers, so the unnormalised value
# never matches and the exception silently does not apply -- which is why this
# check derives `real_tree` below through `cd .. && pwd`, and therefore could
# never have observed what the build script hits.
# ---------------------------------------------------------------------------
unreadable_tree() {
    local w rc=0 dotted
    w="$(mktemp -d)"
    git_q() { git -c user.email=t@t -c user.name=t -c init.defaultBranch=main "$@"; }

    mkdir -p "$w/nested/tree" "$w/patches"
    printf 'one\ntwo\nthree\n' > "$w/nested/tree/top.txt"
    ( cd "$w/nested/tree" && git_q init -q . && git_q add -A && git_q commit -q -m base )
    cat > "$w/patches/t-top.diff" <<'EOP'
--- a/top.txt
+++ b/top.txt
@@ -1,3 +1,3 @@
 one
-two
+TWO
 three
EOP
    patch -p1 -d "$w/nested/tree" --batch --forward --fuzz=0 \
        < "$w/patches/t-top.diff" >/dev/null

    dotted="$w/nested/../nested/tree"
    if GIT_TEST_ASSUME_DIFFERENT_OWNER=1 \
       v8_assert_tree_is_exactly_patched "$dotted" "$w/patches" 't-top.diff' \
       >/dev/null 2>&1; then
        pass "a checkout this user does not own is read through a path carrying .."
    else
        fail "a path carrying .. defeats the safe.directory exception"
        rc=1
    fi

    # A tree git cannot read at all, against a declared patch that only *creates*
    # a file -- the shape of 0008-ohos-toolchain.patch. With no changed paths
    # enumerated, the replay succeeds into the scratch directory and the byte
    # comparison has nothing to iterate, so an unobserved `git status` failure
    # certifies a tree the library never managed to look at.
    mkdir -p "$w/plain" "$w/create-patches"
    printf 'not-a-checkout\n' > "$w/plain/marker.txt"
    cat > "$w/create-patches/t-new.diff" <<'EOP'
--- /dev/null
+++ b/created.gn
@@ -0,0 +1 @@
+created
EOP
    if GIT_CEILING_DIRECTORIES="$w" \
       v8_assert_tree_is_exactly_patched "$w/plain" "$w/create-patches" 't-new.diff' \
       >/dev/null 2>&1; then
        fail "a directory git cannot read is certified as HEAD plus the patches"
        rc=1
    else
        pass "a tree whose git status fails is refused, not read as unchanged"
    fi

    rm -rf "$w"
    return $rc
}
unreadable_tree || failures=$((failures + 1))

info "the real source tree is explained by the patches the build declares"
real_tree="${RUSTY_V8_SRC:-$(cd "$REPO_ROOT/.." && pwd)/rusty_v8_src}"
if [[ -d "$real_tree/.git" ]]; then
    # From the lock, the same single declaration the build script reads, so this
    # check cannot pass against a patch set the build does not actually apply.
    mapfile -t declared < <(
        python3 -c "
import json, sys
for e in json.load(open(sys.argv[1]))['required_patches']:
    print(e['file'])
" "$REPO_ROOT/contracts/artifact-manifest/android-v8.lock.json")
    # `--accounted-patch` before `--accounted` in the alternation: the shorter one is
    # a prefix of the longer, so the other order silently turns a patch glob into a
    # literal path and the accounting stops applying.
    mapfile -t accounted_args < <(
        sed -n '/^V8_ACCOUNTED_ARGS=(/,/^)/p' "$SCRIPT_DIR/build-v8-android.sh" \
        | grep -oE -- "--accounted-patch|--accounted|'[^']*'" | tr -d "'")
    if (( ${#declared[@]} == 0 )); then
        fail "cannot read V8_DECLARED_PATCHES out of build-v8-android.sh"
    else
        if v8_assert_tree_is_exactly_patched "$real_tree" "$PATCH_DIR" \
                "${accounted_args[@]}" "${declared[@]}"; then
            pass "$real_tree is HEAD plus exactly the ${#declared[@]} declared patches"
        else
            fail "$real_tree carries changes the declared patches do not explain"
        fi
    fi
else
    info "SKIP no rusty_v8 checkout at $real_tree"
fi

info "required tools are named, and a missing one is reported by name"
if require_host_tools patch git python3 >/dev/null 2>&1; then
    pass "this host has patch, git and python3"
else
    fail "this host is missing a required tool"
fi
if require_host_tools definitely-not-a-real-tool >/dev/null 2>&1; then
    fail "a missing tool is not reported"
else
    pass "a missing tool is reported"
fi

info "no script hardcodes a machine-specific absolute path"
# build-v8-android.sh once defaulted RUSTY_V8_SRC to a developer-specific path,
# while build-v8-linux.sh and build-v8-ohos.sh
# derived theirs from the repository location. Verified load-bearing against commit
# 8a15ae6, where this check fires on line 37.
#
# Scoped to the V8 and gn scripts. build-ohos-host.sh and run-ohos-host.sh default
# DEVECO_HOME to a /mnt/c path, which is a documented deliberate choice -- the
# emulator and hvigor live on the Windows side while the engine builds in WSL -- and
# both fail immediately naming DEVECO_HOME when it is absent. A default that
# degrades into an actionable error is not the defect this guards against; the
# defect is one that silently resolves somewhere wrong.
for f in "$SCRIPT_DIR"/build-v8-*.sh "$SCRIPT_DIR"/build-gn.sh "$SCRIPT_DIR"/lib/v8-patch-apply.sh \
         "$SCRIPT_DIR"/lib/gn-pin.sh "$SCRIPT_DIR"/lib/host-requirements.sh; do
    name="$(basename "$f")"
    if offenders="$(grep -nE ':-(/data/|/home/[a-z]|/mnt/[a-z]/)' "$f")"; then
        fail "$name defaults a path to a machine-specific location:"
        echo "$offenders" >&2
    else
        pass "$name has no machine-specific path default"
    fi
done

info "every V8 patch stage routes through the shared library"
for s in "$SCRIPT_DIR"/build-v8-*.sh; do
    name="$(basename "$s")"
    # Two scripts are exempt for two different reasons, and NEITHER exemption is
    # taken on trust. A skip list that only names files is a rule that stops
    # being true silently: the moment an exempt script starts patching, it routes
    # patching around the one place that decides applied-ness and the gate says
    # nothing. So each exemption asserts the fact it rests on.
    case "$name" in
        build-v8-linux.sh)
            # It drives patching through rusty_v8's own `--patch` argument and
            # decides applied-ness with `git apply --reverse --check`. If it ever
            # stops passing --patch, the reason for this exemption is gone.
            if grep -qE -- '[-]-patch ' "$s"; then
                pass "$name still delegates patching to rusty_v8's --patch"
            else
                fail "$name no longer passes --patch, so the reason it is exempt from the shared library no longer holds"
            fi
            continue
            ;;
        build-v8-apple.sh)
            # It applies no patches at all. That is not an oversight -- the
            # header records that darwin needed none of the workarounds the other
            # four platforms carry, measured by apple-v8-probe.yml before the
            # script existed. A script that patches nothing cannot route patching
            # anywhere, so requiring it to source the library would be requiring
            # an import it has no use for.
            #
            # This blanket rule was written before that script existed and went
            # red the day it landed, on a gate no workflow ran. Both halves are
            # fixed here.
            if offenders="$(grep -nE 'git apply|patch -p[0-9]|[-]-patch |v8_require_patch' "$s")"; then
                fail "$name now applies patches, so it can no longer be exempt from the shared library:"
                echo "$offenders" >&2
            else
                pass "$name applies no patches, which is what its exemption rests on"
            fi
            continue
            ;;
    esac
    if grep -q 'lib/v8-patch-apply.sh' "$s"; then
        pass "$name sources the shared library"
    else
        fail "$name does not source the shared library"
    fi
    if offenders="$(grep -n 'patch -p1' "$s")"; then
        fail "$name applies patch directly instead of via v8_require_patch:"
        echo "$offenders" >&2
    else
        pass "$name has no direct patch invocation"
    fi
done

# ---------------------------------------------------------------------------
# Every patch a build script names must resolve to exactly one file.
# ---------------------------------------------------------------------------
info "every declared patch resolves to exactly one file"
# Two sources, and both must contribute. Patch names live either as literals in a
# build script (windows, ohos) or as `required_patches[].file` in a lock (android,
# since task 1.1b made the lock the single declaration). Drawing from one source
# only is how this check silently narrowed from 8 patches to 4 the moment android's
# literals moved into the lock -- a zero-matches guard did not notice, because 4 is
# not zero. So each source is counted separately and an empty one is a failure.
mapfile -t script_declared < <(
    grep -ho "'[^']*\.\(patch\|diff\)'" "$SCRIPT_DIR"/build-v8-*.sh | tr -d "'" | sort -u)
mapfile -t lock_declared < <(
    python3 - "$REPO_ROOT/contracts/artifact-manifest" <<'PYEOF'
import json, pathlib, sys
for lock in sorted(pathlib.Path(sys.argv[1]).glob("*-v8.lock.json")):
    for entry in json.load(open(lock)).get("required_patches", []):
        if isinstance(entry, dict) and "file" in entry:
            print(entry["file"])
PYEOF
)
if (( ${#script_declared[@]} == 0 )); then
    fail "no patch literals found in any build-v8-*.sh -- the extraction is broken"
else
    pass "${#script_declared[@]} patch(es) declared as build-script literals"
fi
if (( ${#lock_declared[@]} == 0 )); then
    fail "no patch files declared in any lock -- the extraction is broken"
else
    pass "${#lock_declared[@]} patch(es) declared in a build lock"
fi
mapfile -t all_declared < <(printf '%s\n' "${script_declared[@]}" "${lock_declared[@]}" | sort -u)
for glob in "${all_declared[@]}"; do
    if v8_resolve_patch "$PATCH_DIR" "$glob" >/dev/null 2>&1; then
        pass "resolves: $glob"
    else
        fail "does not resolve to exactly one file: $glob"
    fi
done

if (( failures == 0 )); then
    echo -e "\033[0;32m$TAG all checks passed\033[0m"
else
    echo -e "\033[0;31m$TAG $failures check(s) failed\033[0m" >&2
fi
exit $(( failures > 0 ))
