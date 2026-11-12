import json
from pathlib import Path
import tempfile
import unittest

from android_runtime_deps import CONSUMER_LOCK, ROOT, check_consumer_lock, check_gradle, load


class RuntimeDependencies(unittest.TestCase):
    def test_live_canonical_inputs(self):
        check_gradle()
        check_consumer_lock()
        self.assertTrue(load()["maven_runtime"])

    def test_a_runtime_dependency_the_consumer_app_does_not_lock_fails(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            contract = root / "contracts/android-runtime-dependencies.json"
            contract.parent.mkdir()
            contract.write_text((ROOT / contract.relative_to(root)).read_text())
            for jar in load()["bundled_jars"]:
                (root / jar).parent.mkdir(parents=True, exist_ok=True)
                (root / jar).write_bytes(b"")
            lock = root / CONSUMER_LOCK
            lock.parent.mkdir(parents=True)
            live = (ROOT / CONSUMER_LOCK).read_text()
            lock.write_text(live)
            check_consumer_lock(root)
            first = load()["maven_runtime"][-1]
            coordinate = f"{first['group']}:{first['artifact']}:{first['version']}"
            # The dependency not locked at all: what a POM that gained one looks like to the app.
            lock.write_text("".join(line for line in live.splitlines(True) if not line.startswith(coordinate + "=")))
            with self.assertRaisesRegex(ValueError, "does not lock " + coordinate):
                check_consumer_lock(root)
            # Locked for one way of taking the engine and not the other.
            lock.write_text(live.replace(",deferredDebugRuntimeClasspath", ""))
            with self.assertRaisesRegex(ValueError, "deferredDebugRuntimeClasspath"):
                check_consumer_lock(root)

    def test_undeclared_gradle_dependency_and_bad_coordinates_fail(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            contract = root / "contracts/android-runtime-dependencies.json"
            contract.parent.mkdir()
            contract.write_text(json.dumps({"schema": 1, "maven_runtime": [], "bundled_jars": []}))
            gradle = root / "platforms/android/library/build.gradle"
            gradle.parent.mkdir(parents=True)
            script = (ROOT / gradle.relative_to(root)).read_text()
            gradle.write_text(script)
            check_gradle(root)
            gradle.write_text(script + '\nimplementation "unrecorded:runtime:1"\n')
            with self.assertRaisesRegex(ValueError, "canonical"):
                check_gradle(root)
            contract.write_text(json.dumps({"schema": 1, "maven_runtime": [{"group": "bad<xml", "artifact": "bad", "version": "1"}], "bundled_jars": []}))
            with self.assertRaisesRegex(ValueError, "invalid fixed"):
                load(root)


if __name__ == "__main__":
    unittest.main()
