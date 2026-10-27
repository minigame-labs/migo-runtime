import importlib.util
import json
import pathlib
import subprocess
import tempfile
import unittest


_MODULE_PATH = (
    pathlib.Path(__file__).resolve().parents[1].parent
    / "write-linux-v8-component-manifest.py"
)
_spec = importlib.util.spec_from_file_location("write_linux_v8_component_manifest", _MODULE_PATH)
writer = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(writer)


class LinuxV8ComponentWriterTest(unittest.TestCase):
    def test_native_tools_follow_the_selected_gn_toolchain(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            gn = root / "gn_out"
            gn.mkdir()
            toolchain = root / "pinned-clang/bin"
            toolchain.mkdir(parents=True)
            for name in ["clang++", "ld.lld"]:
                (toolchain / name).write_text("actual selected tool\n")
            project = {
                "targets": {"//:rusty_v8": {"toolchain": "target"}},
                "toolchains": {
                    "target": {"cxx": {"command": '../pinned-clang/bin/clang++ -c {{source}}'}},
                    "unrelated": {"cxx": {"command": '/wrong/ndk/clang++ -c {{source}}'}},
                },
            }
            (gn / "project.json").write_text(json.dumps(project))
            self.assertEqual(writer.native_tools(gn),
                             ((toolchain / "clang++").resolve(), (toolchain / "ld.lld").resolve()))
            project["targets"]["//:rusty_v8"]["toolchain"] = "unrelated"
            (gn / "project.json").write_text(json.dumps(project))
            with self.assertRaisesRegex(RuntimeError, "tool is missing"):
                writer.native_tools(gn)

    def test_native_tools_refuse_an_unidentified_wrapper(self):
        with tempfile.TemporaryDirectory() as directory:
            gn = pathlib.Path(directory)
            (gn / "project.json").write_text(json.dumps({
                "targets": {"//:rusty_v8": {"toolchain": "target"}},
                "toolchains": {"target": {"cxx": {"command": 'mystery-wrapper clang++ -c source.cc'}}},
            }))
            with self.assertRaisesRegex(RuntimeError, "cannot identify"):
                writer.native_tools(gn)

    def test_declared_patch_must_reproduce_the_exact_worktree_bytes(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            source = root / "rusty_v8"
            source.mkdir()

            def git(*arguments):
                return subprocess.run(
                    ["git", "-C", str(source), *arguments],
                    check=True,
                    capture_output=True,
                    text=True,
                )

            git("init", "--quiet")
            git("config", "user.name", "Migo Test")
            git("config", "user.email", "migo-test@example.invalid")
            build_rs = source / "build.rs"
            filler = "".join(f"// stable context {index}\n" for index in range(20))
            build_rs.write_text(
                f"fn main() {{}}\n{filler}fn helper() {{}}\n", encoding="utf-8"
            )
            (source / ".gitignore").write_text("v8/\n", encoding="utf-8")
            git("add", ".gitignore", "build.rs")
            git("commit", "--quiet", "-m", "baseline")

            v8 = source / "v8"
            v8.mkdir()
            subprocess.run(["git", "-C", str(v8), "init", "--quiet"], check=True)
            subprocess.run(
                ["git", "-C", str(v8), "config", "user.name", "Migo Test"],
                check=True,
            )
            subprocess.run(
                [
                    "git", "-C", str(v8), "config", "user.email",
                    "migo-test@example.invalid",
                ],
                check=True,
            )
            (v8 / "README").write_text("v8 baseline\n", encoding="utf-8")
            subprocess.run(["git", "-C", str(v8), "add", "README"], check=True)
            subprocess.run(
                ["git", "-C", str(v8), "commit", "--quiet", "-m", "baseline"],
                check=True,
            )

            declared_source = (
                f"fn main() {{ println!(\"declared\"); }}\n"
                f"{filler}fn helper() {{}}\n"
            )
            build_rs.write_text(declared_source, encoding="utf-8")
            patch = root / "declared.diff"
            patch.write_text(git("diff", "--binary", "HEAD").stdout, encoding="utf-8")

            identities = writer.verify_source_changes(
                source, [("declared", patch)]
            )
            self.assertEqual([identity["id"] for identity in identities], ["declared"])

            build_rs.write_text(
                declared_source.replace("fn helper() {}", "fn helper() { todo!() }"),
                encoding="utf-8",
            )
            # An edit beside the declared change, inside a file the patch is allowed
            # to touch. The message must name the file, because "something drifted"
            # is not actionable.
            with self.assertRaisesRegex(
                RuntimeError, r"build\.rs is not HEAD plus the declared patches"
            ):
                writer.verify_source_changes(source, [("declared", patch)])

    def test_a_dirty_build_submodule_is_rejected_with_a_clear_message(self):
        # A tree shared with the Android build can carry the Android build-submodule
        # patches (build/config/c++/c++.gni, build/rust/gni_impl/run_bindgen.py). The
        # Linux build declares none of those, so it must refuse -- and say why, not
        # surface git's cryptic " m build" pointer.
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            source = root / "rusty_v8"
            source.mkdir()

            def git_in(path, *arguments):
                subprocess.run(
                    ["git", "-C", str(path), *arguments],
                    check=True,
                    capture_output=True,
                    text=True,
                )

            git_in(source, "init", "--quiet")
            git_in(source, "config", "user.name", "Migo Test")
            git_in(source, "config", "user.email", "migo-test@example.invalid")
            (source / "build.rs").write_text("fn main() {}\n", encoding="utf-8")
            git_in(source, "add", "build.rs")
            git_in(source, "commit", "--quiet", "-m", "baseline")

            # Registered as real gitlinks, not merely nested checkouts. Without the
            # index entries the parent reports a bare `?? build/` and the proof never
            # descends, so this test would pass while exercising nothing.
            for nested in ("v8", "build"):
                path = source / nested
                path.mkdir()
                git_in(path, "init", "--quiet")
                git_in(path, "config", "user.name", "Migo Test")
                git_in(path, "config", "user.email", "migo-test@example.invalid")
                (path / "README").write_text("baseline\n", encoding="utf-8")
                git_in(path, "add", "README")
                git_in(path, "commit", "--quiet", "-m", "baseline")
                revision = subprocess.run(
                    ["git", "-C", str(path), "rev-parse", "HEAD"],
                    check=True, capture_output=True, text=True,
                ).stdout.strip()
                git_in(
                    source, "update-index", "--add",
                    "--cacheinfo", f"160000,{revision},{nested}",
                )
            git_in(source, "commit", "--quiet", "-m", "register submodules")

            # An Android build-submodule patch left in a tree the Linux build shares.
            (source / "build" / "config").mkdir(parents=True)
            (source / "build" / "config" / "c++.gni").write_text(
                "use_custom_libcxx = true\n", encoding="utf-8"
            )

            # The nested path is named, which is the whole point: git reports only a
            # dirty pointer for `build`, and a Linux build declaring no patches has
            # no way to act on that.
            with self.assertRaisesRegex(
                RuntimeError,
                r"undeclared change .*build/config/c\+\+\.gni.*"
                r"no declared patch touches",
            ):
                writer.verify_source_changes(source, [])

    def test_gn_arguments_are_sorted_and_duplicate_keys_are_rejected(self):
        self.assertEqual(
            writer.normalized_gn_arguments(
                "use_sysroot=true is_official_build=true symbol_level=0"
            ),
            ["is_official_build=true", "symbol_level=0", "use_sysroot=true"],
        )
        with self.assertRaisesRegex(ValueError, "duplicate GN argument key"):
            writer.normalized_gn_arguments("use_sysroot=true use_sysroot=false")

    def test_component_records_linux_floor_and_exact_v8_revisions(self):
        component = writer.build_component(
            arch="x86_64",
            rusty_v8_version="145.0.0",
            rusty_v8_revision="a" * 40,
            v8_revision="b" * 40,
            gn_args=["use_sysroot=true"],
            patches=[{"id": "fixture-patch", "sha256": "c" * 64}],
            archive_sha256="d" * 64,
            binding_sha256="e" * 64,
            rustc="rustc fixture",
            compiler="clang fixture",
            sdk="Debian bullseye sysroot fixture",
            linker="LLD fixture",
            recipe_sha256="f" * 64,
        )
        self.assertEqual(component["schema"], "migo-v8-component-manifest/v1")
        self.assertEqual(component["target"]["runtime_floor"]["glibc"], "2.31")
        self.assertEqual(component["target"]["runtime_floor"]["glibcxx"], "3.4.28")
        self.assertEqual(component["runtime"]["rusty_v8_revision"], "a" * 40)
        self.assertEqual(component["runtime"]["v8_revision"], "b" * 40)
        self.assertEqual(component["hashes"]["archive"], "d" * 64)


if __name__ == "__main__":
    unittest.main()
