"""Cross FFI must parse pinned target headers without contaminating host tools."""
import importlib.util
import json
from pathlib import Path
import shlex
import subprocess
import tempfile
import unittest

REPO = Path(__file__).resolve().parents[3]
spec = importlib.util.spec_from_file_location("v8_bindgen", REPO / "scripts/lib/v8_bindgen.py")
helper = importlib.util.module_from_spec(spec)
spec.loader.exec_module(helper)


class LinuxBindgenTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="v8 bindgen ")
        self.addCleanup(self.temp.cleanup)
        self.source = Path(self.temp.name)
        recipe = {}
        for arch, multiarch in helper.TARGETS.values():
            entry = {"SysrootDir": f"debian_bullseye_{arch}-sysroot",
                     "URL": "https://example.invalid/sysroot", "Sha256Sum": arch}
            recipe[f"bullseye_{arch}"] = entry
            root = self.source / "build/linux" / entry["SysrootDir"]
            for header in ("features.h", f"{multiarch}/bits/wordsize.h"):
                path = root / "usr/include" / header
                path.parent.mkdir(parents=True, exist_ok=True)
                path.touch()
            (root / ".stamp").write_text(entry["URL"] + "/" + arch)
        path = self.source / "build/linux/sysroot_scripts/sysroots.json"
        path.parent.mkdir()
        path.write_text(json.dumps(recipe))

    def test_each_target_uses_its_pinned_sysroot_and_preserves_resource_path(self):
        for target, (arch, _) in helper.TARGETS.items():
            args = shlex.split(helper.linux_arguments(self.source, target,
                {"BINDGEN_EXTRA_CLANG_ARGS": "'-resource-dir=/clang resource' --sysroot=/wrong --target=wrong"}))
            self.assertEqual(args[0], "-resource-dir=/clang resource")
            self.assertEqual(args[-2], f"--target={target}")
            self.assertEqual(args[-1], f"--sysroot={self.source}/build/linux/debian_bullseye_{arch}-sysroot")

    def test_target_precedence_and_scoped_environment_leaves_host_unchanged(self):
        target = "aarch64-unknown-linux-gnu"
        key = f"BINDGEN_EXTRA_CLANG_ARGS_{target}"
        env = {"BINDGEN_EXTRA_CLANG_ARGS": "-DGLOBAL",
               key.replace("-", "_"): "-DUNDERSCORE", key: "-DEXACT"}
        value = helper.linux_arguments(self.source, target, env)
        self.assertEqual(shlex.split(value)[0], "-DEXACT")
        # Exercise the same env invocation the recipe uses (dashes are legal in
        # OS environment keys, but cannot be assigned with shell export).
        result = subprocess.run(["env", f"{key}={value}", "python3", "-c",
            "import os; print(os.environ['BINDGEN_EXTRA_CLANG_ARGS']); print(os.environ[" + repr(key) + "])"],
            env=env, capture_output=True, text=True, check=True)
        self.assertEqual(result.stdout.splitlines(), ["-DGLOBAL", value])
        del env[key]
        self.assertEqual(shlex.split(helper.linux_arguments(self.source, target, env))[0], "-DUNDERSCORE")

    def test_wrong_stamp_and_missing_target_header_fail_before_build(self):
        root = self.source / "build/linux/debian_bullseye_arm64-sysroot"
        stamp = root / ".stamp"
        original = stamp.read_text()
        stamp.write_text("wrong")
        with self.assertRaisesRegex(ValueError, "pinned recipe"):
            helper.linux_arguments(self.source, "aarch64-unknown-linux-gnu", {})
        stamp.write_text(original)
        (root / "usr/include/aarch64-linux-gnu/bits/wordsize.h").unlink()
        with self.assertRaisesRegex(ValueError, "missing target header"):
            helper.linux_arguments(self.source, "aarch64-unknown-linux-gnu", {})


if __name__ == "__main__":
    unittest.main()
