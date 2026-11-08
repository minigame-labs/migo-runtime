"""JNI export policy must reach its cdylib and leave other target types alone."""
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import tomllib
import unittest

ROOT = Path(__file__).resolve().parents[2]
MANIFEST = ROOT / "engine/crates/android-jni"


class JniLinkScope(unittest.TestCase):
    def test_android_global_flags_do_not_require_jni_exports(self):
        config = tomllib.loads((ROOT / "engine/.cargo/config.toml").read_text())
        android = {name: value for name, value in config["target"].items() if "android" in name}
        self.assertEqual(len(android), 3)
        for name, settings in android.items():
            with self.subTest(target=name):
                self.assertFalse(any("version-script" in flag for flag in settings["rustflags"]))

    def test_actual_build_script_emits_only_android_cdylib_arguments(self):
        with tempfile.TemporaryDirectory(prefix="migo-jni-link-scope-") as temporary:
            executable = Path(temporary) / "build-script"
            subprocess.run(["rustc", "--edition=2024", str(MANIFEST / "build.rs"), "-o", str(executable)], check=True)
            for target in ("android", "linux", "ios"):
                environment = os.environ.copy()
                environment.update(CARGO_CFG_TARGET_OS=target, CARGO_MANIFEST_DIR=str(MANIFEST))
                output = subprocess.check_output([str(executable)], env=environment, text=True)
                arguments = [line for line in output.splitlines() if "link-arg=" in line]
                expected = [
                    f"cargo:rustc-cdylib-link-arg=-Wl,--version-script={ROOT}/engine/version.script",
                    "cargo:rustc-cdylib-link-arg=-Wl,--undefined-version",
                ] if target == "android" else []
                self.assertEqual(arguments, expected, target)

    @unittest.skipUnless(sys.platform == "linux" and shutil.which("readelf"), "ELF linker test")
    def test_actual_rust_cdylib_hides_callbacks_and_rejects_unreviewed_exports(self):
        with tempfile.TemporaryDirectory(prefix="migo-jni-export-elf-") as temporary:
            work = Path(temporary)
            source = work / "fixture.rs"
            source.write_text('''
#[unsafe(no_mangle)]
pub extern "C" fn temporal_rs_Instant_clone() -> i32 { 7 }
#[unsafe(no_mangle)]
pub extern "C" fn JNI_OnLoad() -> i32 { temporal_rs_Instant_clone() }
#[unsafe(no_mangle)]
pub extern "C" fn Java_fixture_callback() -> i32 { 1 }
''')
            wildcard = work / "wildcard.map"
            wildcard.write_text("{ global: JNI_OnLoad; Java_*; local: *; };\n")

            def compile_library(name, script):
                library = work / f"{name}.so"
                result = subprocess.run([
                    "rustc", "--edition=2024", "--crate-type=cdylib", str(source),
                    f"-Clink-arg=-Wl,--version-script={script}",
                    "-Clink-arg=-Wl,--undefined-version", "-o", str(library),
                ], capture_output=True, text=True)
                self.assertEqual(result.returncode, 0, result.stderr)
                return library

            def check(library):
                return subprocess.run([
                    "bash", "-c", 'source "$1"; android_jni_verify_exports "$2" "$3"',
                    "jni-elf-check", str(ROOT / "scripts/lib/android-ndk.sh"),
                    shutil.which("readelf"), str(library),
                ], capture_output=True, text=True)

            # The previous rule is insufficient in a real Rust cdylib: its
            # literal dependency exports beat wildcard local rules.
            leaked = check(compile_library("wildcard", wildcard))
            self.assertNotEqual(leaked.returncode, 0)
            self.assertIn("temporal_rs_Instant_clone", leaked.stderr)
            filtered = check(compile_library("filtered", ROOT / "engine/version.script"))
            self.assertEqual(filtered.returncode, 0, filtered.stderr)

            # A later dependency must fail the actual-ELF gate until its new
            # callback name has been reviewed and localized too.
            with source.open("a") as stream:
                stream.write('''
#[unsafe(no_mangle)]
pub extern "C" fn future_internal_callback() -> i32 { 9 }
''')
            unexpected = check(compile_library("future", ROOT / "engine/version.script"))
            self.assertNotEqual(unexpected.returncode, 0)
            self.assertIn("future_internal_callback", unexpected.stderr)


if __name__ == "__main__":
    unittest.main()
