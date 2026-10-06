#!/usr/bin/env bash
# Shared constants for the Windows spike drivers.
#
# The source of truth stays in WSL; Windows builds from a local-disk copy.
# This is not a preference: `cargo check` over \\wsl.localhost\... produced no
# output in six minutes and never created its target directory, while the same
# check on a local-disk copy finished in 10.7 seconds. cmd.exe also refuses a
# UNC working directory outright (`cd /d` fails; `pushd` maps a drive letter
# but does not make cargo usable).
#
# This file is sourced, not executed: it must not set shell options (each
# entry-point script sets its own `set -euo pipefail`) and must not have side
# effects that run merely because a script sourced it, for callers that never
# trigger them.

WIN_WORKTREE_UNIX="/mnt/c/migo-win"
WIN_WORKTREE_DOS='C:\migo-win'
# Kept off the worktree so a resync never has to delete gigabytes of artifacts.
_migo_target_default='C:\mt'
# Short is a correctness constraint here, not tidiness: ninja invokes the ANSI
# GetFullPathNameA, capped at MAX_PATH even with LongPathsEnabled set. Skia's
# deepest headers (harfbuzz's OT/Layout/GSUB tree) run ~165 characters on their
# own, so every character spent on this prefix comes off the budget.
# `C:\migo-win-target` is 18 characters and failed at ninja step 1005.
WIN_TARGET_DOS="${MIGO_WIN_TARGET_DIR:-$_migo_target_default}"
unset _migo_target_default
# A fixed path rather than %TEMP%: the WSL user name and the Windows user name
# happen to match on this machine, and a script that silently depends on that
# breaks on the first machine where they differ.
WIN_TMP_UNIX="/mnt/c/migo-win-spike-tmp"
WIN_TARGET_TRIPLE="x86_64-pc-windows-msvc"

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"

# Short roots: ninja calls the ANSI GetFullPathNameA during the Skia build and
# is capped at MAX_PATH regardless of the LongPathsEnabled registry setting.
WIN_CARGO_HOME_DOS="${MIGO_WIN_CARGO_HOME:-C:\\cg}"
# Staging area for tools and headers this spike downloads (ninja, Khronos EGL).
WIN_TOOLS_DOS="${MIGO_WIN_TOOLS_DIR:-C:\\migo-win-spike-tmp\\bin}"
WIN_HEADERS_DOS="${MIGO_WIN_HEADERS_DIR:-C:\\migo-win-spike-tmp\\include}"
# The probe builds a PATH allowlist rather than filtering the inherited one:
# the NDK must be absent, and "absent by construction" is checkable where
# "filtered out" depends on getting a match pattern right. The cost is that
# tools the Skia build needs have to be named. Skia probes for `python`/
# `python3` and git-sync-deps needs `git`; both are discovered here so the
# allowlist does not have to be hand-maintained per machine.
discover_win_extra_path() {
    if [[ -n "${MIGO_WIN_EXTRA_PATH:-}" ]]; then
        printf '%s' "$MIGO_WIN_EXTRA_PATH"
        return
    fi
    local parts=() dir finder python_win python_unix
    for dir in "/mnt/c/Program Files/Git/cmd" "/mnt/c/Program Files/Git/bin"; do
        [[ -x "$dir/git.exe" ]] && parts+=("$(wslpath -w "$dir")")
    done
    # Ask Windows to resolve Python through its configured PATH. This supports
    # per-user and system installs without publishing a user-profile path.
    finder="/mnt/c/Windows/System32/where.exe"
    if [[ -f "$finder" ]]; then
        while IFS= read -r python_win; do
            python_win="${python_win%$'\r'}"
            [[ -n "$python_win" && "$python_win" == *\\* ]] || continue
            dir="${python_win%\\*}"
            parts+=("$dir")
            python_unix="$(wslpath -u "$dir" 2>/dev/null || true)"
            [[ -n "$python_unix" && -d "$python_unix/Scripts" ]] && parts+=("$dir\\Scripts")
        done < <("$finder" python.exe 2>/dev/null || true)
    fi
    local IFS=';'
    printf '%s' "${parts[*]}"
}

WIN_EXTRA_PATH_DOS="$(discover_win_extra_path)"
# Default is the LLVM installer's own default location, which is also the first
# path skia-bindings probes for clang-cl.exe.
MIGO_WIN_LLVM_DIR_DOS="${MIGO_WIN_LLVM_DIR:-C:\\Program Files\\LLVM}"

# Locates vcvars64.bat through vswhere rather than a hardcoded path, so a
# different Visual Studio edition or install location still works.
find_vcvars64() {
    local vswhere="/mnt/c/Program Files (x86)/Microsoft Visual Studio/Installer/vswhere.exe"
    if [[ ! -f "$vswhere" ]]; then
        echo "error: vswhere not found at $vswhere -- install Visual Studio Build Tools with the C++ workload" >&2
        exit 93
    fi
    local root
    root="$("$vswhere" -products '*' -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 \
        -latest -format value -property installationPath 2>/dev/null | tr -d '\r')"
    if [[ -z "$root" ]]; then
        echo "error: no Visual Studio install carries the MSVC x86/x64 build tools" >&2
        exit 93
    fi
    printf '%s\\VC\\Auxiliary\\Build\\vcvars64.bat' "$root"
}

# Prints the DOS path of the CMake bin directory that VS Build Tools bundles.
# VS ships cmake + ninja under Common7/IDE/CommonExtensions, and they are not on
# PATH by default -- a consumer that wants find_package(migo) needs this on PATH.
find_windows_cmake_dir() {
    local vswhere="/mnt/c/Program Files (x86)/Microsoft Visual Studio/Installer/vswhere.exe"
    if [[ ! -f "$vswhere" ]]; then
        echo "error: vswhere not found at $vswhere -- install Visual Studio Build Tools with the C++ workload" >&2
        exit 93
    fi
    local root root_unix cmake_bin_unix
    root="$("$vswhere" -products '*' -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 \
        -latest -format value -property installationPath 2>/dev/null | tr -d '\r')"
    if [[ -z "$root" ]]; then
        echo "error: no Visual Studio install carries the MSVC x86/x64 build tools" >&2
        exit 93
    fi
    root_unix="$(echo "$root" | sed 's|\\|/|g; s|^\([A-Za-z]\):|/mnt/\L\1|')"
    cmake_bin_unix="$root_unix/Common7/IDE/CommonExtensions/Microsoft/CMake/CMake/bin"
    if [[ ! -f "$cmake_bin_unix/cmake.exe" ]]; then
        echo "error: no CMake under $cmake_bin_unix -- add the 'C++ CMake tools' component to the VS install" >&2
        exit 93
    fi
    printf '%s\\Common7\\IDE\\CommonExtensions\\Microsoft\\CMake\\CMake\\bin' "$root"
}

# Runs a batch file on the Windows side and returns its exit code.
#
# A .bat is used rather than `cmd.exe /c "<command>"` because quoting a Windows
# path through bash into cmd mangles it -- `cmd.exe /c 'call "C:\...\x.bat"'`
# reports "not an internal or external command" purely from escaping.
run_windows_batch() {
    local batch_unix="$1"
    local batch_dir_unix
    mkdir -p "$WIN_TMP_UNIX"
    batch_dir_unix="$(cd "$(dirname "$batch_unix")" && pwd)"
    ( cd "$batch_dir_unix" && cmd.exe /c "$(basename "$batch_unix")" )
}

# A content digest of the WSL tree exactly as `verify-change.sh` scopes it:
# HEAD, plus the tracked diff against it, plus every untracked-not-ignored
# file's bytes. `sync-worktree.sh` writes this into the Windows copy; a probe
# recomputes it and refuses to run if it has drifted. Both sides call this one
# function so the two can never disagree about what "synced" means.
#
# Untracked files are hashed with their path, so a rename registers; the diff
# already carries tracked renames and deletions.
SYNC_FINGERPRINT_FILE="$WIN_WORKTREE_UNIX/.migo-sync-fingerprint"

compute_sync_fingerprint() {
    {
        git -C "$REPO_ROOT" rev-parse HEAD
        git -C "$REPO_ROOT" diff HEAD --binary
        ( cd "$REPO_ROOT" \
            && git ls-files --others --exclude-standard -z \
            | xargs -0 -r sha256sum )
    } | sha256sum | cut -d' ' -f1
}

# Aborts if the Windows-side worktree does not reflect the WSL tree as it is
# right now, so a probe can never silently report a clean pass for a stale
# checkout.
require_synced_worktree() {
    local win_sha repo_sha
    win_sha="$(git -C "$WIN_WORKTREE_UNIX" rev-parse HEAD)"
    repo_sha="$(git -C "$REPO_ROOT" rev-parse HEAD)"
    if [[ "$win_sha" != "$repo_sha" ]]; then
        echo "error: $WIN_WORKTREE_UNIX is at $win_sha but $REPO_ROOT (WSL) is at $repo_sha -- worktree is stale, run sync-worktree.sh" >&2
        exit 91
    fi
    # Matching HEADs are not enough. `sync-worktree.sh` overlays the WSL
    # working tree onto the checkout -- uncommitted edits included -- so what
    # the probe must confirm is that that overlay still matches, not that the
    # WSL tree is clean. A bare sha comparison misses every unstaged change;
    # forbidding a dirty tree, as this guard used to, made the probe unusable
    # for the case a developer actually has.
    local recorded current
    recorded="$(cat "$SYNC_FINGERPRINT_FILE" 2>/dev/null || true)"
    current="$(compute_sync_fingerprint)"
    if [[ "$recorded" != "$current" ]]; then
        echo "error: $WIN_WORKTREE_UNIX reflects a different tree than $REPO_ROOT (WSL) has now -- run sync-worktree.sh" >&2
        exit 92
    fi
}

# DOS path as WSL sees it. Used to inspect Windows-side build state from here.
win_to_unix_path() {
    printf '/mnt/%s' "$(printf '%s' "$1" | sed 's|\\|/|g; s|^\(.\):|\L\1|')"
}

# A Windows-reachable HTTP proxy, or empty.
#
# A cold Skia build downloads a prebuilt binaries tarball from GitHub, and
# rusty-skia's fetch runs on the Windows side where a WSL-local proxy
# (loopback as WSL sees it) is not reachable -- the download then hangs
# rather than failing. The probe passes this through as `MIGO_WIN_PROXY` /
# `HTTPS_PROXY`. Most local proxy clients also listen on the
# Windows loopback, so try the usual ports and keep the first that answers.
#
# Skipped entirely once `$MIGO_WIN_PROXY` is set by the caller, and once the
# Skia tarball is already cached (a warm `C:\mt` needs no network at all).
detect_windows_proxy() {
    if [[ -n "${MIGO_WIN_PROXY:-}" ]]; then
        printf '%s' "$MIGO_WIN_PROXY"
        return
    fi
    local curl="/mnt/c/Windows/System32/curl.exe" port
    [[ -x "$curl" ]] || return 0
    for port in 10808 10809 7890 7897 1080 8080 8888; do
        if "$curl" -s -o /dev/null --max-time 6 \
            -x "http://127.0.0.1:$port" https://github.com 2>/dev/null; then
            printf 'http://127.0.0.1:%s' "$port"
            return
        fi
    done
}
