#!/usr/bin/env bash
# Every relative link in a tracked Markdown file must point at a file that
# exists.
#
# `.github/PULL_REQUEST_TEMPLATE.md` linked `[CLA](CLA.md)` -- resolved from
# `.github/`, that is `.github/CLA.md`, which does not exist; the file is at the
# repository root. A first-time contributor, the one audience of that line,
# clicked a 404. Nothing checked it.
#
# Scope: relative links (`](path)` and `](path#anchor)`) in tracked `*.md`,
# resolved against the linking file's own directory. `http(s)://`, `mailto:`,
# bare `#anchor` fragments, and links whose target is a directory are left
# alone. Anchors within a target are not verified -- only that the target file
# is there.
#
# THE SECOND THING THIS CHECKS, and the reason it is not enough to ask whether
# the file exists: existence is asked on the machine running the gate, and this
# repository keeps its maintainer notes in `docs/`, which is gitignored. A link
# from a tracked file into `docs/` resolves here and 404s for everyone who
# cloned -- the audience the check exists for. So the target must be *tracked*,
# not merely present.
#
# THE THIRD THING THIS CHECKS is this gate itself. Both enumerations below are
# `git ls-files`, and on 2026-09-07 this gate printed
# `PASS (0 relative Markdown link(s) resolve)` on a machine whose `.git` had
# become unreadable. `set -e` cannot see that: a failing command inside
# `< <(...)` is a process substitution whose exit status nobody collects, so both
# loops ran zero times, zero broken links were found, and zero broken is what
# PASS means. Announcing an invariant it was unable to check is the one thing a
# gate must never do -- this repository has already spent weeks on a gate that
# reported a violation when ripgrep was merely not installed, and
# `check-repository-hygiene.py`, reading the same `git ls-files`, said ERROR on
# the same machine in the same minute. So the lists are collected before they are
# walked, a git failure is exit 127 with "missing tool, not a violation", and an
# empty list or a zero check count is refused too: a gate that cannot notice its
# own scope shrinking to nothing is the failure mode, not the messenger.
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

cannot_run() {
    echo "Doc links contract: could not run -- $1" >&2
    echo "  Nothing was checked, so this is neither a PASS nor a violation." >&2
    exit 127
}

if ! md_list="$(git ls-files '*.md' 2>/dev/null)"; then
    cannot_run "\`git ls-files\` failed in $ROOT_DIR (is .git readable?)."
fi
if [[ -z "$md_list" ]]; then
    cannot_run "\`git ls-files '*.md'\` matched nothing, which cannot be right here."
fi
if ! contract_list="$(git ls-files 'contracts/*' 2>/dev/null)"; then
    cannot_run "\`git ls-files contracts/*\` failed in $ROOT_DIR."
fi
if [[ -z "$contract_list" ]]; then
    cannot_run "\`git ls-files 'contracts/*'\` matched nothing, which cannot be right here."
fi

checked=0
broken=0

while IFS= read -r md; do
    dir="$(dirname "$md")"
    while IFS= read -r match; do
        link="${match#](}"
        link="${link%)}"
        case "$link" in
            http://*|https://*|mailto:*|"#"*|"") continue ;;
        esac
        target="${link%%#*}"
        [[ -n "$target" ]] || continue
        checked=$((checked + 1))
        resolved="$(cd "$dir" && realpath -m -- "$target" 2>/dev/null)" || resolved=""
        if [[ -z "$resolved" || ! -e "$resolved" ]]; then
            echo "  broken: $md -> $link" >&2
            broken=$((broken + 1))
        elif [[ -f "$resolved" ]] \
            && ! git ls-files --error-unmatch -- "$resolved" >/dev/null 2>&1; then
            echo "  untracked: $md -> $link (present here, absent in a clone)" >&2
            broken=$((broken + 1))
        fi
    done < <(grep -oE '\]\([^)[:space:]]+\)' "$md" 2>/dev/null || true)
done <<< "$md_list"

# ── The same rule for the contracts, which are read rather than clicked ──────
#
# `contracts/` is tracked and ships with the source; `docs/` is the maintainer's
# notes and does not. A contract whose stated reasoning is "see
# docs/something.md" sends the one audience that reads contracts -- somebody
# integrating against them -- to a path their checkout does not contain. It is
# the Markdown case again in a file type the link scan above cannot see, because
# a JSON comment is prose and not a link.
#
# Scoped to `docs/`, and the narrowing is not laziness. The first version of
# this also flagged `build/`, `dist/` and `out/`, and caught two real paths that
# are not this repository's at all: the V8 lock files name `build/config/
# sysroot.gni` and `build/toolchain/ohos/BUILD.gn` inside the *V8* checkout,
# which is exactly what a lock over someone else's source is supposed to do. A
# rule that cannot tell those apart from a dangling pointer produces noise, and
# noise is how a gate gets an exemption list. `docs/` is unambiguous: it is this
# repository's own gitignored tree, and nothing outside it is called that here.
while IFS= read -r contract; do
    while IFS= read -r referenced; do
        [[ -n "$referenced" ]] || continue
        if ! git ls-files --error-unmatch -- "$referenced" >/dev/null 2>&1; then
            echo "  untracked reference: $contract -> $referenced" >&2
            echo "    contracts ship to consumers; that path does not." >&2
            broken=$((broken + 1))
        fi
        checked=$((checked + 1))
    done < <(grep -ohE '\bdocs/[A-Za-z0-9._/-]+' "$contract" 2>/dev/null | sort -u || true)
done <<< "$contract_list"


if [[ "$broken" -gt 0 ]]; then
    echo "ERROR: $broken broken relative link(s) in tracked Markdown (of $checked checked)." >&2
    exit 1
fi

# Zero checked means every link disappeared, not that every link resolves. The
# enumerations above are non-empty by then, so this can only be a scan that
# stopped matching -- and reporting PASS over nothing is the defect recorded in
# the header, one layer further in.
if [[ "$checked" -eq 0 ]]; then
    cannot_run "0 link(s) found across $(wc -l <<< "$md_list") Markdown file(s) and \
$(wc -l <<< "$contract_list") contract(s); the scan matched nothing."
fi

echo "Doc links contract: PASS ($checked relative Markdown link(s) resolve)"
