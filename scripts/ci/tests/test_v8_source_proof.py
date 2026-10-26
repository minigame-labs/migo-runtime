import importlib.util
import pathlib
import subprocess
import tempfile
import unittest

_MODULE_PATH = (
    pathlib.Path(__file__).resolve().parents[2] / "lib" / "v8_source_proof.py"
)
_spec = importlib.util.spec_from_file_location("v8_source_proof", _MODULE_PATH)
proof = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(proof)


def git(path, *arguments):
    return subprocess.run(
        [
            "git",
            "-c", "user.name=fixture",
            "-c", "user.email=fixture@example.invalid",
            "-c", "protocol.file.allow=always",
            "-C", str(path),
            *arguments,
        ],
        check=True,
        capture_output=True,
        text=True,
    )


class SourceProofTest(unittest.TestCase):
    """The proof behind every V8 component manifest's provenance claim.

    These are the cases where a weaker check reads as a pass. The interesting one
    is submodule suppression: `submodule.<name>.ignore` makes the parent's
    `git status` omit the submodule entirely, so any design that discovers
    submodules from the parent's report silently stops checking them.
    """

    def build_tree(self, directory):
        root = pathlib.Path(directory)
        sub, super_ = root / "sub", root / "super"
        (sub / "nested").mkdir(parents=True)
        (sub / "inner.txt").write_text("inner-one\ninner-two\n")
        git(sub.parent, "init", "-q", str(sub))
        git(sub, "add", "-A")
        git(sub, "commit", "-qm", "base")

        super_.mkdir()
        (super_ / "top.txt").write_text("top-one\ntop-two\n")
        git(super_.parent, "init", "-q", str(super_))
        git(super_, "add", "-A")
        git(super_, "commit", "-qm", "base")
        git(super_, "submodule", "add", "-q", str(sub), "sub")
        git(super_, "commit", "-qm", "addsub")

        top_patch = root / "top.diff"
        top_patch.write_text(
            "--- a/top.txt\n+++ b/top.txt\n@@ -1,2 +1,2 @@\n top-one\n-top-two\n+TOP-TWO\n"
        )
        sub_patch = root / "sub.diff"
        sub_patch.write_text(
            "--- a/sub/inner.txt\n+++ b/sub/inner.txt\n"
            "@@ -1,2 +1,2 @@\n inner-one\n-inner-two\n+INNER-TWO\n"
        )
        return root, super_, [top_patch, sub_patch]

    def apply_all(self, tree, patches):
        for patch in patches:
            with patch.open("rb") as patch_stream:
                subprocess.run(
                    ["patch", "-p1", "-d", str(tree), "--batch", "--forward", "--fuzz=0"],
                    stdin=patch_stream,
                    check=True,
                    capture_output=True,
                )

    def test_portable_top_level_application_is_idempotent_and_verified(self):
        with tempfile.TemporaryDirectory() as directory:
            _, tree, patches = self.build_tree(directory)
            selected = [("top", patches[0])]
            proof.apply_locked_top_level_patches(tree, selected)
            before = (tree / "top.txt").read_bytes()
            proof.apply_locked_top_level_patches(tree, selected)
            self.assertEqual((tree / "top.txt").read_bytes(), before)
            identities = proof.verified_patch_identities(tree, selected)
            self.assertEqual(identities[0]["id"], "top")
            self.assertEqual(len(identities[0]["sha256"]), 64)

    def test_portable_applier_refuses_submodule_patches_without_mutation(self):
        with tempfile.TemporaryDirectory() as directory:
            _, tree, patches = self.build_tree(directory)
            before = (tree / "top.txt").read_bytes()
            with self.assertRaisesRegex(proof.SourceProofError, "cannot patch a submodule"):
                proof.apply_locked_top_level_patches(tree, list(zip(["top", "sub"], patches)))
            self.assertEqual((tree / "top.txt").read_bytes(), before)

    def test_portable_applier_refuses_partial_and_unexplained_changes(self):
        with tempfile.TemporaryDirectory() as directory:
            root, tree, patches = self.build_tree(directory)
            second = root / "second.diff"
            second.write_text("--- a/top.txt\n+++ b/top.txt\n@@ -1,2 +1,2 @@\n-top-one\n+TOP-ONE\n TOP-TWO\n")
            self.apply_all(tree, patches[:1])
            before = (tree / "top.txt").read_bytes()
            with self.assertRaises(proof.SourceProofError):
                proof.apply_locked_top_level_patches(tree, [("top", patches[0]), ("second", second)])
            self.assertEqual((tree / "top.txt").read_bytes(), before)
            (tree / "stray.txt").write_text("unexplained\n")
            with self.assertRaisesRegex(proof.SourceProofError, "stray"):
                proof.apply_locked_top_level_patches(tree, [("top", patches[0])])

    def test_lock_resolution_refuses_missing_ambiguous_and_duplicate_entries(self):
        with tempfile.TemporaryDirectory() as directory:
            import json
            root = pathlib.Path(directory)
            lock = root / "lock.json"
            for entries in [
                [],
                [{"id": "x", "file": "missing.patch"}],
                [{"id": "x", "file": "*.patch"}],
                [{"id": "x", "file": "one.patch"}, {"id": "x", "file": "two.patch"}],
            ]:
                (root / "one.patch").write_text("one\n")
                (root / "two.patch").write_text("two\n")
                lock.write_text(json.dumps({"required_patches": entries}))
                with self.assertRaises(proof.SourceProofError):
                    proof.locked_patches(lock, root)

    def test_an_exactly_patched_tree_including_a_submodule_is_accepted(self):
        with tempfile.TemporaryDirectory() as directory:
            _, tree, patches = self.build_tree(directory)
            self.apply_all(tree, patches)
            proof.assert_tree_is_exactly_patched(tree, patches)

    def test_an_unapplied_patch_is_reported_against_its_target(self):
        # Both patches are unapplied here, and paths are compared in a stable byte
        # order, so the message names whichever target sorts first rather than the
        # patch. Naming a file is the point: it is where the operator has to look.
        with tempfile.TemporaryDirectory() as directory:
            _, tree, patches = self.build_tree(directory)
            with self.assertRaisesRegex(
                proof.SourceProofError,
                r"(top\.txt|sub/inner\.txt) is not HEAD plus",
            ):
                proof.assert_tree_is_exactly_patched(tree, patches)

    def test_a_submodule_edit_hidden_by_ignore_config_is_still_caught(self):
        # `submodule.sub.ignore = all` empties the parent's status for that
        # submodule. A proof that learned about submodules from the parent's report
        # would stop looking and seal a manifest over unrecorded edits.
        with tempfile.TemporaryDirectory() as directory:
            _, tree, patches = self.build_tree(directory)
            self.apply_all(tree, patches)
            git(tree, "config", "submodule.sub.ignore", "all")
            (tree / "sub" / "inner.txt").write_text("inner-one\nSMUGGLED\n")

            suppressed = git(
                tree, "status", "--porcelain=v1", "--untracked-files=all"
            ).stdout
            self.assertNotIn(
                "sub",
                suppressed,
                "fixture is void: the parent still reports the submodule",
            )
            with self.assertRaisesRegex(
                proof.SourceProofError, r"sub/inner\.txt is not HEAD plus"
            ):
                proof.assert_tree_is_exactly_patched(tree, patches)

    def test_an_edit_beside_a_declared_change_in_the_same_file_is_caught(self):
        with tempfile.TemporaryDirectory() as directory:
            _, tree, patches = self.build_tree(directory)
            self.apply_all(tree, patches)
            (tree / "top.txt").write_text("top-one\nTOP-TWO\nsmuggled\n")
            with self.assertRaisesRegex(
                proof.SourceProofError, r"top\.txt is not HEAD plus"
            ):
                proof.assert_tree_is_exactly_patched(tree, patches)

    def test_an_undeclared_path_is_named(self):
        with tempfile.TemporaryDirectory() as directory:
            _, tree, patches = self.build_tree(directory)
            self.apply_all(tree, patches)
            (tree / "stray.bin").write_text("tool\n")
            with self.assertRaisesRegex(proof.SourceProofError, r"stray\.bin"):
                proof.assert_tree_is_exactly_patched(tree, patches)

    def test_an_accounted_path_is_exempt_but_only_when_named_exactly(self):
        with tempfile.TemporaryDirectory() as directory:
            _, tree, patches = self.build_tree(directory)
            self.apply_all(tree, patches)
            (tree / "stray.bin").write_text("tool\n")
            proof.assert_tree_is_exactly_patched(
                tree, patches, frozenset({"stray.bin"})
            )
            with self.assertRaisesRegex(proof.SourceProofError, r"stray\.bin"):
                proof.assert_tree_is_exactly_patched(
                    tree, patches, frozenset({"stray.bi"})
                )

    def test_a_submodule_moved_off_its_pinned_commit_is_refused(self):
        with tempfile.TemporaryDirectory() as directory:
            root, tree, patches = self.build_tree(directory)
            self.apply_all(tree, patches)
            sub = root / "sub"
            (sub / "unrelated.txt").write_text("later\n")
            git(sub, "add", "-A")
            git(sub, "commit", "-qm", "later")
            later = git(sub, "rev-parse", "HEAD").stdout.strip()
            git(tree / "sub", "fetch", "-q", "origin")
            git(tree / "sub", "checkout", "-q", later)
            with self.assertRaisesRegex(
                proof.SourceProofError, r"submodule sub is not at the commit"
            ):
                proof.assert_tree_is_exactly_patched(tree, patches)

    def test_declaring_no_patches_requires_a_pristine_tree(self):
        with tempfile.TemporaryDirectory() as directory:
            _, tree, patches = self.build_tree(directory)
            proof.assert_tree_is_exactly_patched(tree, [])
            self.apply_all(tree, patches)
            with self.assertRaisesRegex(proof.SourceProofError, r"top\.txt"):
                proof.assert_tree_is_exactly_patched(tree, [])

    def test_a_patch_that_creates_a_nested_file_replays(self):
        # GNU patch will not create missing directories, so the pristine
        # materialisation has to make the parent even when there is no HEAD blob to
        # write into it. Otherwise a perfectly good tree is rejected -- the real
        # 0008-ohos-toolchain.patch is exactly this shape.
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            tree = root / "tree"
            tree.mkdir()
            (tree / "keep.txt").write_text("x\n")
            git(root, "init", "-q", str(tree))
            git(tree, "add", "-A")
            git(tree, "commit", "-qm", "base")

            patch = root / "create.diff"
            patch.write_text(
                "--- /dev/null\n+++ b/deep/nested/new.txt\n"
                "@@ -0,0 +1 @@\n+created\n"
            )
            self.apply_all(tree, [patch])
            proof.assert_tree_is_exactly_patched(tree, [patch])

    def test_a_flipped_executable_bit_is_refused(self):
        with tempfile.TemporaryDirectory() as directory:
            _, tree, patches = self.build_tree(directory)
            self.apply_all(tree, patches)
            (tree / "top.txt").chmod(0o755)
            with self.assertRaisesRegex(
                proof.SourceProofError, r"top\.txt has a mode"
            ):
                proof.assert_tree_is_exactly_patched(tree, patches)


if __name__ == "__main__":
    unittest.main()
