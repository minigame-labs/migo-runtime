"""An old ABI binding must be rejected before the native compiler starts."""
import hashlib
import importlib.util
import json
from pathlib import Path
import subprocess
import tempfile
import unittest


spec = importlib.util.spec_from_file_location(
    "verify_binding", Path(__file__).resolve().parents[2] / "verify-v8-prebuilt-binding.py"
)
verifier = importlib.util.module_from_spec(spec)
spec.loader.exec_module(verifier)


class BindingIdentityTest(unittest.TestCase):
    def setUp(self):
        self.scratch = tempfile.TemporaryDirectory()
        self.addCleanup(self.scratch.cleanup)
        self.source = Path(self.scratch.name)
        (self.source / "Cargo.toml").write_text('[package]\nversion = "150.4.0"\n')
        self.binding = self.source / "binding.rs"
        self.binding.write_bytes(b"// actual generated binding\n")
        self.manifest = self.source / "component-manifest.json"
        for args in [("init", "-q"), ("add", "Cargo.toml"),
                     ("-c", "user.name=fixture", "-c", "user.email=fixture@example.invalid",
                      "commit", "-qm", "source")]:
            subprocess.run(["git", "-C", str(self.source), *args], check=True)
        revision = subprocess.check_output(
            ["git", "-C", str(self.source), "rev-parse", "HEAD"], text=True
        ).strip()
        self.identity = {
            "runtime": {"rusty_v8_version": "150.4.0", "rusty_v8_revision": revision},
            "hashes": {"rust_binding": hashlib.sha256(self.binding.read_bytes()).hexdigest()},
        }

    def verify(self):
        self.manifest.write_text(json.dumps(self.identity))
        verifier.verify(self.source, self.binding, self.manifest)

    def test_matching_bytes_version_and_revision_are_accepted(self):
        self.verify()

    def test_a_145_binding_cannot_be_reused_with_150(self):
        self.identity["runtime"]["rusty_v8_version"] = "145.0.0"
        with self.assertRaisesRegex(ValueError, "source is 150.4.0"):
            self.verify()

    def test_same_version_from_another_revision_is_rejected(self):
        self.identity["runtime"]["rusty_v8_revision"] = "0" * 40
        with self.assertRaisesRegex(ValueError, "source revision"):
            self.verify()

    def test_modified_binding_is_rejected(self):
        self.binding.write_bytes(b"// stale or modified binding\n")
        with self.assertRaisesRegex(ValueError, "binding bytes"):
            self.verify()


if __name__ == "__main__":
    unittest.main()
