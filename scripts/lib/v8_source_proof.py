"""Proving a rusty_v8 checkout is its HEAD plus exactly the declared patches.

Location: scripts/lib/v8_source_proof.py

Shared by the per-platform V8 component manifest writers. Each platform keeps its
own writer, because each records a different toolchain and a different artifact
shape, and each decides its own policy about which paths may differ. What is shared
here is the *mechanics*: how to ask git about a checkout it may not own, how to read
what a patch claims to touch, and how to prove the worktree is the pristine sources
plus those patches and nothing else.

Why a proof rather than a list of allowed paths: the Android writer used to compare
modified paths against a hardcoded set. That set is a restatement of which files the
patches touch, so it drifts the moment a patch grows a file, and it cannot see an
edit *inside* an allowed file at all. Replaying the patches onto the pristine blobs
and comparing byte for byte answers the actual question.

Bytes are not sufficient on their own: a patch can carry `old mode`/`new mode`, so
the executable bit is compared too. The pristine content comes from `git cat-file`
with the mode from `git ls-tree`, never from `git show` into a file, because that
writes through the umask and loses the recorded mode.
"""

from __future__ import annotations

import pathlib
import hashlib
import json
import subprocess
import tempfile


class SourceProofError(RuntimeError):
    """A checkout does not match its declared patches, or cannot be inspected."""


def locked_patches(lock: pathlib.Path, patch_dir: pathlib.Path) -> list[tuple[str, pathlib.Path]]:
    entries = json.loads(lock.read_text())["required_patches"]
    if not isinstance(entries, list) or not entries:
        raise SourceProofError("source lock declares no required patches")
    selected = []
    ids = set()
    for entry in entries:
        patch_id, pattern = entry["id"], entry["file"]
        if not patch_id or patch_id in ids or pathlib.Path(pattern).is_absolute() or ".." in pathlib.Path(pattern).parts:
            raise SourceProofError("invalid or duplicate patch declaration")
        matches = sorted(patch_dir.glob(pattern))
        if len(matches) != 1 or not matches[0].is_file():
            raise SourceProofError(f"patch declaration must resolve exactly once: {pattern}")
        ids.add(patch_id)
        selected.append((patch_id, matches[0].resolve()))
    return selected


def verified_patch_identities(
    tree: pathlib.Path, patches: list[tuple[str, pathlib.Path]]
) -> list[dict]:
    assert_tree_is_exactly_patched(tree, [path for _, path in patches])
    return sorted(
        [{"id": patch_id, "sha256": hashlib.sha256(path.read_bytes()).hexdigest()}
         for patch_id, path in patches],
        key=lambda entry: entry["id"],
    )


def apply_locked_top_level_patches(
    tree: pathlib.Path, patches: list[tuple[str, pathlib.Path]]
) -> None:
    """Portable atomic Git applier for patches that stay outside submodules.

    macOS's default bash cannot source the Bash 4 tree-audit helper. Use Git's
    all-hunk transaction and the same Python provenance proof instead. Refuse
    a partly patched tree; never repair it by applying its missing prefix.
    """
    submodules = submodule_paths(tree)
    for _, patch in patches:
        for path in patch_target_paths(patch):
            if any(path == submodule or path.startswith(submodule + "/") for submodule in submodules):
                raise SourceProofError("top-level Git applier cannot patch a submodule")
    try:
        verified_patch_identities(tree, patches)
        return
    except SourceProofError as patched_error:
        try:
            assert_tree_is_exactly_patched(tree, [])
        except SourceProofError as pristine_error:
            raise SourceProofError(
                f"source is neither pristine nor fully patched: {patched_error}; {pristine_error}"
            ) from pristine_error
    patch_bytes = b"\n".join(path.read_bytes() for _, path in patches)
    command = ["git", "-c", f"safe.directory={tree.resolve()}", "-C", str(tree), "apply"]
    for extra in [["--check"], []]:
        result = subprocess.run(command + extra, input=patch_bytes, capture_output=True)
        if result.returncode:
            raise SourceProofError(f"locked patches do not apply cleanly: {result.stderr.decode(errors='replace')}")
    verified_patch_identities(tree, patches)


def _git(tree: pathlib.Path, *arguments: str) -> subprocess.CompletedProcess[bytes]:
    """Run git against a checkout this user may not own.

    `-c safe.directory` is passed per invocation rather than written into the
    user's git config. The rusty_v8 tree here is owned by another account on a
    shared, group-writable workspace, which makes git refuse to operate on it at
    all; that is what stopped the component manifest from being sealed. The trust
    this grants is already granted by the build, which executes that tree's
    build.rs.
    """
    return subprocess.run(
        ["git", "-c", f"safe.directory={tree}", "-C", str(tree), *arguments],
        check=False,
        capture_output=True,
    )


def _git_text(tree: pathlib.Path, *arguments: str, label: str) -> str:
    result = _git(tree, *arguments)
    if result.returncode != 0:
        raise SourceProofError(
            f"{label} failed in {tree}: {result.stderr.decode(errors='replace').strip()}"
        )
    return result.stdout.decode(errors="replace").strip()


def head_revision(tree: pathlib.Path, label: str) -> str:
    value = _git_text(tree, "rev-parse", "HEAD", label=label)
    if len(value) != 40 or any(character not in "0123456789abcdef" for character in value):
        raise SourceProofError(f"{label} is not a full revision: {value!r}")
    return value


def patch_target_paths(patch: pathlib.Path) -> set[str]:
    """The repository-relative paths a patch claims to touch."""
    paths: set[str] = set()
    for line in patch.read_text(encoding="utf-8").splitlines():
        for prefix in ("--- a/", "+++ b/"):
            if not line.startswith(prefix):
                continue
            relative = line.removeprefix(prefix)
            candidate = pathlib.PurePosixPath(relative)
            if candidate.is_absolute() or ".." in candidate.parts:
                raise SourceProofError(f"patch contains an unsafe path {relative!r}: {patch}")
            paths.add(relative)
    if not paths:
        raise SourceProofError(f"patch declares no changed paths: {patch}")
    return paths


def accounted_paths_from_patch(patch_root: pathlib.Path, glob: str) -> frozenset[str]:
    """The paths a foreign patch *creates*, for accounting them in a proof.

    One vendored checkout serves every platform's V8 build, and OpenHarmony's
    toolchain patch creates a file the Android declaration does not touch -- so
    without this, building one platform makes the other's proof refuse a path that is
    explained by a committed patch, just not by one that proof claims.

    Only *created* paths qualify. Accounting for a path a foreign patch merely
    modifies would skip content verification on a file this platform's own patches may
    also touch, so a patch that creates nothing is refused rather than trusted.
    """
    matches = sorted(patch_root.glob(glob))
    if len(matches) != 1:
        raise SourceProofError(
            f"accounted patch glob matched {len(matches)} files: {glob}"
        )
    patch = matches[0]
    created: set[str] = set()
    from_dev_null = False
    for line in patch.read_text(encoding="utf-8").splitlines():
        if line.startswith("--- "):
            from_dev_null = line.removeprefix("--- ").split("\t", 1)[0].strip() == "/dev/null"
        elif line.startswith("+++ ") and from_dev_null:
            relative = line.removeprefix("+++ ").split("\t", 1)[0].strip()
            relative = relative.removeprefix("b/")
            candidate = pathlib.PurePosixPath(relative)
            if candidate.is_absolute() or ".." in candidate.parts:
                raise SourceProofError(f"patch contains an unsafe path {relative!r}: {patch}")
            created.add(relative)
    if not created:
        raise SourceProofError(
            f"{patch.name} creates no file, so it cannot account for one"
        )
    return frozenset(created)


class Change:
    """One changed path, located in whichever checkout actually owns it."""
    __slots__ = ("status", "path", "owner", "owner_path")

    def __init__(self, status: str, path: str, owner: pathlib.Path, owner_path: str):
        self.status = status
        self.path = path
        self.owner = owner
        self.owner_path = owner_path


SUBMODULE_MOVED = "submodule-moved"


def direct_submodules(tree: pathlib.Path) -> list[str]:
    """Paths of the submodules registered directly in this checkout's index."""
    listing = _git(tree, "ls-files", "--stage")
    if listing.returncode != 0:
        raise SourceProofError(
            f"git ls-files failed in {tree}: "
            f"{listing.stderr.decode(errors='replace').strip()}"
        )
    paths: list[str] = []
    for line in listing.stdout.decode(errors="replace").splitlines():
        metadata, separator, path = line.partition("\t")
        if separator and metadata.split(maxsplit=1)[0] == "160000":
            paths.append(path)
    return paths


def changed_paths(tree: pathlib.Path, prefix: str = "") -> list[Change]:
    """Every changed path, descending into submodules.

    Submodules are enumerated from the index and scanned explicitly rather than
    discovered from the parent's status. That is deliberate, and the reason is not
    obvious: `submodule.<name>.ignore = all` (or `dirty`) makes the parent's
    `git status` omit the submodule entirely, so a descent triggered by the parent
    reporting a dirty gitlink would silently never happen and unrecorded submodule
    edits would seal into a manifest. `--ignore-submodules=all` on the parent scan
    therefore says "do not tell me about submodules" precisely because this function
    asks each of them directly.

    Patches are written against the root and reach inside submodules, so the paths
    reported here are root-relative while each carries the checkout that owns it.

    Descending is only sound while a submodule sits at the commit its parent
    records. Otherwise the pristine baseline would be a foreign HEAD, and a
    submodule checked out elsewhere with the declared patches still applying would
    read as clean. A moved gitlink is reported as the change it is.
    """
    result = _git(
        tree, "status", "--porcelain=v1", "-z", "--untracked-files=all",
        "--ignore-submodules=all",
    )
    if result.returncode != 0:
        raise SourceProofError(
            f"git status failed in {tree}: {result.stderr.decode(errors='replace').strip()}"
        )
    changes: list[Change] = []
    for record in result.stdout.decode(errors="replace").split("\0"):
        if not record:
            continue
        changes.append(Change(record[:2], prefix + record[3:], tree, record[3:]))

    for path in direct_submodules(tree):
        pinned = _git(tree, "rev-parse", f"HEAD:{path}")
        actual = _git(tree / path, "rev-parse", "HEAD")
        if pinned.returncode != 0 or actual.returncode != 0:
            raise SourceProofError(
                f"submodule {prefix}{path} is registered but not checked out"
            )
        if pinned.stdout.strip() != actual.stdout.strip():
            changes.append(Change(SUBMODULE_MOVED, prefix + path, tree, path))
            continue
        changes.extend(changed_paths(tree / path, f"{prefix}{path}/"))
    return changes


def submodule_paths(tree: pathlib.Path, prefix: str = "") -> list[str]:
    """Root-relative paths of every registered submodule, nested ones included."""
    paths: list[str] = []
    for path in direct_submodules(tree):
        paths.append(prefix + path)
        paths.extend(submodule_paths(tree / path, f"{prefix}{path}/"))
    return paths


def _owner_of(tree: pathlib.Path, relative: str, submodules: list[str]) -> tuple[pathlib.Path, str]:
    """Which checkout holds a root-relative path, and its path within it.

    The patches are written against the root and reach into submodules, but a
    submodule's blobs live in its own object store: `git ls-tree HEAD -- build/x`
    in the parent yields the gitlink for `build`, not the file. So the deepest
    submodule prefix wins.
    """
    best = ""
    for candidate in submodules:
        if relative.startswith(f"{candidate}/") and len(candidate) > len(best):
            best = candidate
    if not best:
        return tree, relative
    return tree / best, relative[len(best) + 1 :]


def _head_blob(tree: pathlib.Path, relative: str) -> tuple[bytes, bool] | None:
    """One HEAD regular-file blob and its executable bit, or None if absent."""
    listing = _git(tree, "ls-tree", "-z", "HEAD", "--", relative)
    if listing.returncode != 0:
        raise SourceProofError(
            f"git ls-tree failed for {relative}: "
            f"{listing.stderr.decode(errors='replace').strip()}"
        )
    if not listing.stdout:
        return None
    metadata, separator, recorded = listing.stdout.rstrip(b"\0").partition(b"\t")
    if not separator or recorded.decode("utf-8") != relative:
        raise SourceProofError(f"cannot parse the HEAD identity of {relative!r}")
    mode, object_type, object_id = metadata.decode("ascii").split()
    if object_type != "blob" or mode not in {"100644", "100755"}:
        raise SourceProofError(
            f"a declared patch path must be a regular tracked file: "
            f"{relative} ({mode} {object_type})"
        )
    blob = _git(tree, "cat-file", "blob", object_id)
    if blob.returncode != 0:
        raise SourceProofError(
            f"git cat-file failed for {relative}: "
            f"{blob.stderr.decode(errors='replace').strip()}"
        )
    return blob.stdout, mode == "100755"


def assert_tree_is_exactly_patched(
    tree: pathlib.Path,
    patch_files: list[pathlib.Path],
    accounted_paths: frozenset[str] = frozenset(),
) -> None:
    """Prove the checkout is HEAD plus exactly these patches, and nothing else.

    `accounted_paths` names paths whose provenance is established by something
    other than a patch -- a pinned tool and its build receipt, say. They are passed
    in by the caller rather than read from the environment, and named exactly rather
    than as a directory prefix, so a new file appearing beside them is a failure
    instead of inheriting the exemption.
    """
    declared: set[str] = set()
    for patch in patch_files:
        if not patch.is_file():
            raise SourceProofError(f"declared patch is missing: {patch}")
        declared |= patch_target_paths(patch)
    if patch_files and not declared:
        raise SourceProofError("the declared patches name no target files")
    # Zero declared patches is a legitimate declaration -- the Linux V8 build makes
    # it when its prebuilt-binding diff is not in use -- and it means the checkout
    # must be pristine. The scan below then refuses every change, which is exactly
    # that statement.

    for change in changed_paths(tree):
        if change.path in accounted_paths:
            continue
        if change.status == SUBMODULE_MOVED:
            raise SourceProofError(
                f"submodule {change.path} is not at the commit {tree} records"
            )
        if change.path not in declared:
            raise SourceProofError(
                f"undeclared change in {tree}: {change.status} {change.path} "
                f"(no declared patch touches that path)"
            )

    # There is deliberately no separate "is each patch applied" probe. It was
    # redundant once every declared path -- not merely the changed ones -- became
    # part of the replay, because the comparison below cannot pass unless each patch
    # is present. Worse, it misreported: an edit made *beside* a declared change in
    # the same file breaks the patch's reverse-applicability, so the probe blamed
    # the patch rather than naming the file that had drifted.
    submodules = submodule_paths(tree)
    ordered = sorted(declared, key=lambda value: value.encode("utf-8"))

    with tempfile.TemporaryDirectory(prefix="migo-v8-source-proof.") as scratch_name:
        scratch = pathlib.Path(scratch_name)
        # Every declared path, not only the changed ones. Materialising just the
        # changed set leaves a patch whose target is unmodified with nothing to
        # apply to, and the comparison below is what should report that.
        for relative in ordered:
            owner, owner_relative = _owner_of(tree, relative, submodules)
            target = scratch / relative
            # The parent directory is created even when the file is not, because GNU
            # patch will not create missing directories: a patch that adds a nested
            # file whose directory no other declared target materialises would
            # otherwise fail to replay and a perfectly good tree would be rejected.
            target.parent.mkdir(parents=True, exist_ok=True)
            pristine = _head_blob(owner, owner_relative)
            # A patch may create a file, in which case the pristine state is its
            # absence and the patch is what brings it into being.
            if pristine is None:
                continue
            content, executable = pristine
            target.write_bytes(content)
            target.chmod(0o755 if executable else 0o644)

        for patch in patch_files:
            with patch.open("rb") as patch_stream:
                applied = subprocess.run(
                    ["patch", "-p1", "-d", str(scratch), "--batch", "--forward", "--fuzz=0"],
                    stdin=patch_stream,
                    check=False,
                    capture_output=True,
                )
            if applied.returncode != 0:
                raise SourceProofError(
                    f"cannot replay {patch.name} onto the pristine sources: "
                    f"{applied.stdout.decode(errors='replace').strip()}"
                )

        for relative in ordered:
            expected = scratch / relative
            actual = tree / relative
            if not expected.is_file() or expected.is_symlink():
                raise SourceProofError(
                    f"the declared patches do not produce a regular file at {relative}"
                )
            if not actual.is_file() or actual.is_symlink():
                raise SourceProofError(f"{relative} is not a regular file")
            if expected.read_bytes() != actual.read_bytes():
                raise SourceProofError(
                    f"{relative} is not HEAD plus the declared patches"
                )
            if bool(expected.stat().st_mode & 0o100) != bool(actual.stat().st_mode & 0o100):
                raise SourceProofError(
                    f"{relative} has a mode the declared patches do not produce"
                )
