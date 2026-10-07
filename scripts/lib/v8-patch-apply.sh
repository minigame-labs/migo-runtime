# shellcheck shell=bash
# Applying V8 patches to a rusty_v8 source tree, idempotently and honestly.
# Location: scripts/lib/v8-patch-apply.sh
#
# Sourced by scripts/build-v8-{android,ohos,windows}.sh. Self-contained: it
# writes its own diagnostics rather than calling a caller-provided `err`.
#
# Whether a patch is already in effect is decided by asking patch to reverse it,
# never by a hand-written sentinel string in the target file. A sentinel restates
# what a patch does, so it can drift from the patch, and it drifted four ways:
#
#   - copied from a patch's *first* hunk, so it survived a later hunk failing and
#     made the next run report the partly-applied patch as complete;
#   - truncated by the field separator of the list that held the declarations,
#     and the surviving prefix already occurred in the unpatched file, which
#     silenced the Android sysroot patch for every build ever run;
#   - naming a string that also occurs elsewhere in the target file;
#   - checking one file for a patch that spans five, so a failure in any of the
#     other four came back as "already in effect" on the following run.
#
# Reversibility cannot drift, because it is derived from the patch itself, and it
# covers every file and hunk the patch touches.
#
# `--forward` is what makes the probe honest. On its own, `--reverse` hits GNU
# patch's "Unreversed patch detected!  Ignoring -R." heuristic: patch decides we
# meant to apply the patch, applies it, and exits 0 -- so an unapplied patch is
# indistinguishable from an applied one. `--forward` turns that heuristic into
# "Skipping patch." with a non-zero exit. `--fuzz=0` stops loose context matching
# from reporting a hunk as reversible against code it does not actually match.
#
# `patch` rather than `git apply --reverse --check` (which scripts/build-v8-linux.sh
# can use) because some of these patches target files inside the `build`
# submodule, and git apply will not write across a submodule boundary.

_v8_patch_err() { printf '  ✗ %s\n' "$*" >&2; }

# shellcheck source=scripts/lib/host-requirements.sh
source "${BASH_SOURCE[0]%/*}/host-requirements.sh"
require_host_tools patch git python3 || {
    _v8_patch_err "cannot apply or verify V8 patches on this host"
    return 1 2>/dev/null || exit 1
}

# Resolve a single patch file from a glob, refusing an ambiguous or absent match.
v8_resolve_patch() {
    local dir="$1" glob="$2"
    local -a matches=("$dir"/$glob)
    if [[ ! -f "${matches[0]}" ]]; then
        _v8_patch_err "missing patch: $dir/$glob"
        return 1
    fi
    if (( ${#matches[@]} > 1 )); then
        _v8_patch_err "ambiguous patch glob matched ${#matches[@]} files: $glob"
        return 1
    fi
    printf '%s' "${matches[0]}"
}

v8_patch_is_in_effect() {
    local tree="$1" pf="$2"
    patch -p1 -d "$tree" --batch --dry-run --reverse --forward --fuzz=0 \
        < "$pf" >/dev/null 2>&1
}

# Apply a patch unless it is already in effect. A tree the patch does not apply to
# completely is left exactly as it was found.
v8_require_patch() {
    local tree="$1" dir="$2" glob="$3"
    local pf name
    pf="$(v8_resolve_patch "$dir" "$glob")" || return 1
    name="$(basename "$pf")"
    if v8_patch_is_in_effect "$tree" "$pf"; then
        echo "  = already in effect: $name"
        return 0
    fi
    # `--forward` is not transactional. Given a tree where an earlier hunk is
    # unapplied but a later one is already applied -- the exact shape the old
    # sentinel gate could leave behind for the five-file 0007 -- patch writes the
    # earlier hunk, then skips the later one and exits non-zero. The build fails
    # having left the tree more modified than it found it, and the next run starts
    # from that new state. Deciding on a dry run first keeps the mutating
    # invocation to trees the patch applies to completely.
    if ! patch -p1 -d "$tree" --batch --dry-run --forward --fuzz=0 < "$pf" >/dev/null 2>&1; then
        _v8_patch_err "$name neither applies cleanly nor is already applied"
        _v8_patch_err "the tree is partly patched or has drifted; leaving it untouched"
        return 1
    fi
    # No `</dev/null`: a second redirect on the same descriptor wins, so it fed
    # patch an empty stdin -- patch then exited 0 having applied nothing.
    # `--batch` already suppresses the prompting it guarded against.
    # `--no-backup-if-mismatch`: GNU patch writes a `.orig` beside any file whose
    # hunks applied with an offset, and these patches routinely apply at an offset
    # because the revisions they target move. That backup is an untracked file
    # inside the very tree the component manifest then proves is EXACTLY the
    # declared patch set, so the seal fails with `undeclared change: ??
    # build/config/c++/c++.gni.orig` -- measured, immediately after a first
    # successful apply. The failure only appears on a tree where the patch was not
    # already in effect, which is exactly the from-clean case the manifest exists to
    # make reproducible, so it cannot be left to be cleaned up by hand.
    if ! patch -p1 -d "$tree" --batch --forward --fuzz=0 --no-backup-if-mismatch < "$pf"; then
        _v8_patch_err "patch failed: $name"
        return 1
    fi
    echo "  ✓ applied $name"
}

# Runs git against a checkout this user may not own. `-c` keeps that judgement to
# the invocation instead of writing it into the user's global git config.
#
# The path is canonicalised first, and that is load-bearing rather than tidiness:
# git compares `safe.directory` literally against the repository path it
# discovers, and every caller derives this one as `$PROJECT_ROOT/../rusty_v8_src`.
# An unnormalised value therefore never matches, the exception does not apply, and
# every git call here fails with `dubious ownership` -- which the replay reports as
# the tree carrying changes no patch explains.
_v8_git() {
    local tree="$1"
    shift
    local canonical
    # Refused rather than defaulted: an empty `-C` runs git in the *current*
    # directory, so a mistyped tree would report the calling repository's status
    # as if it were the vendored checkout's.
    canonical="$(cd "$tree" 2>/dev/null && pwd -P)" || {
        _v8_patch_err "not a directory: $tree"
        return 1
    }
    git -c "safe.directory=$canonical" -C "$canonical" "$@"
}

# Emits one tab-separated record per changed path, descending into submodules:
#
#   <status> <root-relative path> <owning checkout> <path within that checkout>
#
# A modified submodule surfaces in its parent as a single gitlink entry, which
# says only "something in there changed". The patches, though, are written against
# the root and reach into submodules with `-p1` -- 0001 and 0003 both land inside
# `build/`. So the enumeration has to descend, or every submodule change reads as
# one opaque undeclared modification.
#
# Descending is only sound if the submodule is at the commit its parent records.
# Otherwise the replay would take a foreign HEAD as its pristine baseline: a
# submodule checked out at some other commit where the declared patches still
# apply would be reported clean, and an artifact built from unpinned sources would
# pass the gate. A moved gitlink is itself an undeclared change, so it is reported
# as one rather than followed.
_v8_changed_paths() {
    local tree="$1" prefix="$2"
    local listing record status path pinned actual submodule
    local -a records=() submodules=()
    # Submodules are enumerated from the index and scanned explicitly rather than
    # discovered from the parent's status, and the reason is not obvious:
    # `submodule.<name>.ignore = all` (or `dirty`) makes the parent's `git status` omit
    # the submodule entirely, so a descent triggered by the parent reporting a dirty
    # gitlink would silently never happen and unrecorded submodule edits would pass this
    # proof. `--ignore-submodules=all` on the parent scan below therefore says "do not
    # tell me about submodules" precisely because each one is asked directly.
    #
    # `scripts/lib/v8_source_proof.py` already worked this way; this copy -- the one the
    # *build* uses, where an unexplained edit reaches an artifact rather than a manifest --
    # did not. Two implementations of one rule, with the guard on the copy that seals.
    listing="$(mktemp)" || {
        _v8_patch_err "cannot create a temporary file to enumerate $tree"
        return 1
    }
    if ! _v8_git "$tree" ls-files --stage > "$listing"; then
        rm -f "$listing"
        _v8_patch_err "cannot read the index of $tree"
        return 1
    fi
    while read -r mode _ _ path; do
        [[ "$mode" == "160000" ]] && submodules+=("$path")
    done < "$listing"

    # A failed enumeration must not read as an empty one. The status output is
    # NUL-separated, so it cannot go through a command substitution, and a process
    # substitution's exit code is not observable -- hence the file. Read it out and
    # delete it before processing, so the recursion below cannot leak it.
    if ! _v8_git "$tree" status --porcelain=v1 -z --untracked-files=all \
            --ignore-submodules=all > "$listing"; then
        rm -f "$listing"
        _v8_patch_err "cannot read the git status of $tree"
        return 1
    fi
    mapfile -d '' -t records < "$listing"
    rm -f "$listing"
    for record in "${records[@]}"; do
        status="${record:0:2}"
        path="${record:3}"
        printf '%s\t%s\t%s\t%s\n' "$status" "$prefix$path" "$tree" "$path"
    done

    # Descending is only sound while a submodule sits at the commit its parent records.
    # Otherwise the pristine baseline would be a foreign HEAD, and a submodule checked out
    # elsewhere where the declared patches still apply would read as clean.
    for submodule in "${submodules[@]}"; do
        pinned="$(_v8_git "$tree" rev-parse "HEAD:$submodule" 2>/dev/null)"
        actual="$(_v8_git "$tree/$submodule" rev-parse HEAD 2>/dev/null)"
        if [[ -z "$pinned" || -z "$actual" || "$pinned" != "$actual" ]]; then
            printf '%s\t%s\t%s\t%s\n' \
                "submodule-moved" "$prefix$submodule" "$tree" "$submodule"
            continue
        fi
        _v8_changed_paths "$tree/$submodule" "$prefix$submodule/" || return 1
    done
}

# The declared patch set is read from a build lock. One reader, because a second
# copy of "where the declaration lives" is how the declaration and the thing
# that applies it drift -- an earlier Android defect was fixed by collapsing
# three statements into one, while the Windows lock still declared three
# `required_patches` that nothing read and the build named as literals.
#
# Prints one entry per line. A caller must use a command substitution rather than
# `mapfile < <(...)`: a process substitution's exit status is not what `||` observes,
# so a parser that printed a valid prefix and then rejected a malformed entry would
# leave a truncated declaration behind and pass a non-empty check -- the build would
# apply fewer patches than the lock requires and only discover it at manifest time.
v8_read_declared_patches() {
    local lock="$1"
    [[ -f "$lock" ]] || { _v8_patch_err "missing V8 build lock: $lock"; return 1; }
    python3 - "$lock" <<'PY'
import json, sys

lock = json.load(open(sys.argv[1]))
required = lock.get("required_patches")
if not isinstance(required, list) or not required:
    sys.exit("V8 lock declares no required_patches")
for entry in required:
    if not isinstance(entry, dict) or "file" not in entry:
        sys.exit(f"required_patches entry carries no file: {entry!r}")
    print(entry["file"])
PY
}

# Proves that a checkout is its committed HEAD plus exactly the declared patches,
# and nothing else.
#
#   v8_assert_tree_is_exactly_patched <tree> <patch-dir> \
#       [--accounted <root-relative path>]... <patch-glob>...
#
# `v8_patch_is_in_effect` answers "is this patch applied", which is not the same
# question: a checkout can carry every declared patch *and* an undeclared edit
# beside them, and every per-patch check still passes. Tool identity does not
# close the gap either -- a build stamped from `git describe HEAD` reports the
# same revision whether or not the worktree is dirty.
#
# The proof is a replay: materialise each modified path at the HEAD of whichever
# checkout owns it, apply the declared patches to that, and require the result to
# equal the worktree byte for byte. Anything the patches do not account for shows
# up as a difference.
#
# `--accounted` exempts a path whose provenance is established by something other
# than a patch -- the pinned gn and its receipt, identified by the receipt itself.
# Exemptions are arguments rather than a variable the library reads, so an exported
# value in a release environment cannot grant one: a caller that needs an exemption
# has to say so at the call site.
#
# `--accounted-patch <glob>` is for the one case a single checkout forces: the
# vendored tree is shared by every platform's V8 build, and OpenHarmony's
# `0008-ohos-toolchain.patch` *creates* `build/toolchain/ohos/BUILD.gn`, which the
# Android declaration does not touch. Without this, building OpenHarmony makes the
# Android build refuse a file that is explained by a committed patch, just not by one
# this build applies -- so the two platforms became mutually exclusive on one
# checkout. The paths are derived from the patch rather than listed, so they cannot
# drift from it, and **only paths the patch creates may be accounted for**: accounting
# for a path a foreign patch *modifies* would skip content verification on a file this
# platform also cares about, so that is refused rather than trusted.
v8_assert_tree_is_exactly_patched() {
    local tree="$1" dir="$2"
    shift 2
    local -A accounted=()
    while [[ "${1:-}" == "--accounted" || "${1:-}" == "--accounted-patch" ]]; do
        [[ -n "${2:-}" ]] || { _v8_patch_err "$1 needs a value"; return 1; }
        if [[ "$1" == "--accounted" ]]; then
            accounted["$2"]=1
        else
            local foreign target
            foreign="$(v8_resolve_patch "$dir" "$2")" || return 1
            local -a created=()
            while IFS= read -r target; do
                [[ -n "$target" ]] && created+=("$target")
            done < <(awk '
                /^--- / { from = $2 }
                /^\+\+\+ / { if (from == "/dev/null") { sub("^b/", "", $2); print $2 } }
            ' "$foreign")
            if (( ${#created[@]} == 0 )); then
                _v8_patch_err "$(basename "$foreign") creates no file, so it cannot account for one"
                return 1
            fi
            for target in "${created[@]}"; do
                accounted["$target"]=1
            done
        fi
        shift 2
    done
    local -a globs=("$@")
    if (( ${#globs[@]} == 0 )); then
        _v8_patch_err "no patch globs given"
        return 1
    fi

    local -A targets=()
    local -a resolved=()
    local glob pf path
    for glob in "${globs[@]}"; do
        pf="$(v8_resolve_patch "$dir" "$glob")" || return 1
        resolved+=("$pf")
        while IFS= read -r path; do
            [[ -n "$path" ]] && targets["$path"]=1
        done < <(sed -n 's|^+++ b/||p' "$pf")
    done
    if (( ${#targets[@]} == 0 )); then
        _v8_patch_err "the declared patches name no target files"
        return 1
    fi

    local -a changed=() owners=() owner_paths=()
    local status owner owner_path records
    # A command substitution rather than a process substitution: the enumeration
    # can fail (a tree git cannot read), and `< <(...)` discards that, leaving the
    # loop to see no changed paths and the proof to conclude the tree is clean.
    records="$(_v8_changed_paths "$tree" "")" || return 1
    while IFS=$'\t' read -r status path owner owner_path; do
        [[ -n "$path" ]] || continue
        [[ -n "${accounted[$path]:-}" ]] && continue
        if [[ "$status" == "submodule-moved" ]]; then
            _v8_patch_err "submodule $path is not at the commit $tree records"
            return 1
        fi
        if [[ -z "${targets[$path]:-}" ]]; then
            _v8_patch_err "undeclared change in $tree: $status $path"
            _v8_patch_err "no declared patch touches that path"
            return 1
        fi
        changed+=("$path")
        owners+=("$owner")
        owner_paths+=("$owner_path")
    done <<< "$records"

    local scratch
    scratch="$(mktemp -d)" || {
        _v8_patch_err "cannot create a temporary directory to replay the patches"
        return 1
    }
    local rc=0 index head_mode
    for index in "${!changed[@]}"; do
        mkdir -p "$scratch/$(dirname "${changed[$index]}")"
        # A patch may create a file, in which case there is no HEAD version and
        # the pristine state is its absence.
        if ! _v8_git "${owners[$index]}" show "HEAD:${owner_paths[$index]}" \
                > "$scratch/${changed[$index]}" 2>/dev/null; then
            rm -f "$scratch/${changed[$index]}"
            continue
        fi
        # `git show` writes through the umask and drops the recorded mode, so the
        # pristine copy would always come out non-executable and the mode
        # comparison below would be measuring this materialisation rather than
        # what the patches produce.
        head_mode="$(_v8_git "${owners[$index]}" ls-tree HEAD -- "${owner_paths[$index]}" \
                     2>/dev/null | awk 'NR == 1 { print $1 }')"
        [[ "$head_mode" == "100755" ]] && chmod +x "$scratch/${changed[$index]}"
    done
    for pf in "${resolved[@]}"; do
        if ! patch -p1 -d "$scratch" --batch --forward --fuzz=0 < "$pf" >/dev/null 2>&1; then
            _v8_patch_err "cannot replay $(basename "$pf") onto the pristine sources"
            rc=1
        fi
    done
    if (( rc == 0 )); then
        local expected actual expected_x actual_x
        for index in "${!changed[@]}"; do
            expected="$scratch/${changed[$index]}"
            actual="$tree/${changed[$index]}"
            if ! cmp -s "$expected" "$actual"; then
                _v8_patch_err "${changed[$index]} is not HEAD plus the declared patches"
                rc=1
                continue
            fi
            # A patch can carry `old mode`/`new mode`, so equal bytes are not
            # sufficient: a flipped executable bit would otherwise pass unnoticed.
            expected_x=0; [[ -x "$expected" ]] && expected_x=1
            actual_x=0;   [[ -x "$actual" ]] && actual_x=1
            if (( expected_x != actual_x )); then
                _v8_patch_err "${changed[$index]} has a mode the declared patches do not produce"
                rc=1
            fi
        done
    fi
    rm -rf "$scratch"
    return $rc
}
