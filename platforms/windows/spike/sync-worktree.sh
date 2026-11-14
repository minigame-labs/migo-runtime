#!/usr/bin/env bash
set -euo pipefail
# Put the WSL working tree -- HEAD plus every uncommitted edit -- on a Windows
# local disk for the Windows toolchain to build. That scope matches
# `verify-change.sh`'s ("master..HEAD plus the working tree"): a probe of this
# copy is a probe of the change a developer is actually testing, not only of
# what they have committed.
#
# LFS objects are skipped by default: the first probe layers need none of them,
# and the repository's LFS budget is exhausted for reads (uploads still work),
# so a smudge would fail rather than merely be slow. Task 3 copies the one
# archive it needs directly from the WSL-side LFS cache, which never touches
# the network.
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

if [[ -d "$WIN_WORKTREE_UNIX/.git" ]]; then
    echo "[sync] updating existing worktree at $WIN_WORKTREE_UNIX"
    GIT_LFS_SKIP_SMUDGE=1 git -C "$WIN_WORKTREE_UNIX" fetch --depth 1 origin HEAD
    GIT_LFS_SKIP_SMUDGE=1 git -C "$WIN_WORKTREE_UNIX" checkout --force FETCH_HEAD
    # --force overwrites tracked files but never removes untracked ones; without
    # this, anything a previous Windows build left behind survives every future
    # sync and can change what `cargo check` resolves. The overlay below then
    # re-adds exactly the untracked files the WSL tree carries.
    #
    # What the vendored skia-bindings build script downloads into its own
    # directory stays: Skia's sources at the version that directory is named for,
    # and the dependencies git-sync-deps clones beside them (ICU alone is a
    # quarter of an hour over a slow link). They are that version's and nothing
    # of a build's, the script takes a directory it finds over downloading one,
    # and removing them made every sync an hour of downloads before the first
    # line was compiled.
    git -C "$WIN_WORKTREE_UNIX" clean -xdff \
        -e 'engine/third_party/skia-bindings-*/skia/' \
        -e 'engine/third_party/skia-bindings-*/depot_tools/'
else
    echo "[sync] cloning into $WIN_WORKTREE_UNIX"
    rm -rf "$WIN_WORKTREE_UNIX"
    # file:// forces a real transport; a plain path would hardlink across the
    # WSL/Windows filesystem boundary and fail.
    GIT_LFS_SKIP_SMUDGE=1 git clone --depth 1 --no-local \
        "file://$REPO_ROOT" "$WIN_WORKTREE_UNIX"
fi

# Overlay the WSL working tree onto the committed checkout. The clone carries
# only committed refs, so without this a probe silently passes a tree missing
# every unstaged change -- the exact "edited and forgot to sync" failure the
# fingerprint guard in lib.sh exists to catch.
overlay_working_tree() {
    local tracked_diff untracked_count=0 path
    tracked_diff="$(git -C "$REPO_ROOT" diff HEAD --binary)"
    if [[ -n "$tracked_diff" ]]; then
        # No --index: the copy is left dirty relative to its own HEAD, which is
        # the WSL state it is meant to mirror. `git apply` reads and writes the
        # working tree only.
        printf '%s\n' "$tracked_diff" \
            | git -C "$WIN_WORKTREE_UNIX" apply --whitespace=nowarn
        echo "[sync] overlaid tracked working-tree changes"
    fi
    while IFS= read -r -d '' path; do
        install -D "$REPO_ROOT/$path" "$WIN_WORKTREE_UNIX/$path"
        untracked_count=$((untracked_count + 1))
    done < <(git -C "$REPO_ROOT" ls-files --others --exclude-standard -z)
    if [[ $untracked_count -gt 0 ]]; then
        echo "[sync] copied $untracked_count untracked file(s)"
    fi
}
overlay_working_tree

# The digest `require_synced_worktree` checks before every probe. Written last,
# so a sync interrupted mid-overlay leaves no fingerprint and the next probe
# refuses rather than trusting a half-applied tree.
compute_sync_fingerprint > "$SYNC_FINGERPRINT_FILE"

# Assigned before echoing: under `set -e`, a failing command substitution
# inside an echo's arguments does not abort the script (the echo itself still
# succeeds), so these would otherwise be status lines that cannot report
# failure.
WORKTREE_HEAD="$(git -C "$WIN_WORKTREE_UNIX" rev-parse --short HEAD)"
# `|| true`: du returns nonzero on an unreadable entry, and under `set -e`
# that would abort *after* a successful sync, making it look like a failure.
WORKTREE_SIZE="$(du -sh "$WIN_WORKTREE_UNIX" 2>/dev/null | cut -f1 || true)"
echo "[sync] HEAD: $WORKTREE_HEAD"
echo "[sync] size: $WORKTREE_SIZE"
