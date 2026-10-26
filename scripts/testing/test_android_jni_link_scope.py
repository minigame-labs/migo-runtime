"""JNI export policy must reach its cdylib and leave other target types alone."""
import os
from pathlib import Path
import subprocess
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

    def test_actual_build_script_emits_only_an_android_cdylib_argument(self):
        with tempfile.TemporaryDirectory(prefix="migo-jni-link-scope-") as temporary:
            executable = Path(temporary) / "build-script"
            subprocess.run(["rustc", "--edition=2024", str(MANIFEST / "build.rs"), "-o", str(executable)], check=True)
            for target in ("android", "linux", "ios"):
                environment = os.environ.copy()
                environment.update(CARGO_CFG_TARGET_OS=target, CARGO_MANIFEST_DIR=str(MANIFEST))
                output = subprocess.check_output([str(executable)], env=environment, text=True)
                arguments = [line for line in output.splitlines() if "link-arg=" in line]
                expected = [f"cargo:rustc-cdylib-link-arg=-Wl,--version-script={ROOT}/engine/version.script"] if target == "android" else []
                self.assertEqual(arguments, expected, target)


if __name__ == "__main__":
    unittest.main()
