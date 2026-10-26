"""Exercise the actual CI installer without downloading an SDK in a unit test."""

import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[3]
SCRIPT = ROOT / "scripts/ci/setup-android-ndk.sh"
PIN = json.loads((ROOT / "contracts/artifact-manifest/android-v8.lock.json").read_text())["ndk"]["version"]


class NdkSetupTests(unittest.TestCase):
    def test_action_reaches_the_verified_installer(self):
        action = (ROOT / ".github/actions/setup-android-ndk/action.yml").read_text()
        self.assertIn("run: bash scripts/ci/setup-android-ndk.sh", action)
        self.assertIn("MIGO_REQUESTED_NDK_REVISION: ${{ inputs.ndk-version }}", action)
        self.assertNotIn("default:", action, "the native build lock supplies the revision")

    def run_setup(self, revision, *, requested="", executable=True):
        with tempfile.TemporaryDirectory() as directory:
            work = Path(directory)
            sdk = work / "sdk"
            ndk = sdk / "ndk" / PIN
            ndk.mkdir(parents=True)
            if revision is not None:
                (ndk / "source.properties").write_text(f"Pkg.Revision = {revision}\n")
            toolchain = ndk / "toolchains/llvm/prebuilt/linux-x86_64/bin"
            toolchain.mkdir(parents=True)
            if executable:
                clang = toolchain / "clang"
                clang.write_text("#!/bin/sh\nexit 0\n")
                clang.chmod(0o755)
            fake_bin = work / "bin"
            fake_bin.mkdir()
            manager = fake_bin / "sdkmanager"
            manager.write_text('#!/bin/sh\nprintf "%s\\n" "$@" >> "$MIGO_TEST_SDK_CALLS"\n')
            manager.chmod(0o755)
            exports, paths, calls = [work / name for name in ("exports", "paths", "calls")]
            environment = os.environ.copy()
            environment.update(
                PATH=f"{fake_bin}:{environment['PATH']}",
                ANDROID_HOME=str(sdk),
                MIGO_REQUESTED_NDK_REVISION=requested,
                MIGO_TEST_SDK_CALLS=str(calls),
                GITHUB_ENV=str(exports),
                GITHUB_PATH=str(paths),
            )
            result = subprocess.run(["bash", str(SCRIPT)], env=environment, capture_output=True, text=True)
            read = lambda path: path.read_text() if path.exists() else ""
            return result, read(exports), read(paths), read(calls), ndk

    def test_lock_drives_install_and_all_four_consumers(self):
        for requested in ("", PIN):
            with self.subTest(requested=requested):
                result, exports, paths, calls, ndk = self.run_setup(PIN, requested=requested)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(calls, f"ndk;{PIN}\n")
                self.assertEqual(exports.splitlines(), [
                    f"{name}={ndk}" for name in
                    ("ANDROID_NDK", "ANDROID_NDK_HOME", "ANDROID_NDK_ROOT", "ANDROID_NDK_LATEST_HOME")
                ])
                self.assertEqual(paths, f"{ndk}/toolchains/llvm/prebuilt/linux-x86_64/bin\n")

    def test_override_cannot_select_a_different_cohort(self):
        result, exports, _, calls, _ = self.run_setup(PIN, requested="1.2.3")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("differs from native build lock", result.stderr)
        self.assertEqual((exports, calls), ("", ""))

    def test_noop_missing_compiler_and_false_directory_identity_fail_closed(self):
        for revision, executable in ((None, True), ("1.2.3", True), (PIN, False)):
            with self.subTest(revision=revision, executable=executable):
                result, exports, paths, calls, _ = self.run_setup(revision, executable=executable)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("did not install NDK", result.stderr)
                self.assertEqual((exports, paths), ("", ""))
                self.assertEqual(calls, f"ndk;{PIN}\n")


if __name__ == "__main__":
    unittest.main()
