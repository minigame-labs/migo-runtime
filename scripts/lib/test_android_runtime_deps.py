import json
from pathlib import Path
import tempfile
import unittest

from android_runtime_deps import ROOT, check_gradle, load


class RuntimeDependencies(unittest.TestCase):
    def test_live_canonical_inputs(self):
        check_gradle()
        self.assertTrue(load()["maven_runtime"])

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
