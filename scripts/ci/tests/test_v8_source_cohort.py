"""Native version labels cannot silently diverge from the actual source."""
import importlib.util
import json
from pathlib import Path
import subprocess
import tempfile
import unittest


spec = importlib.util.spec_from_file_location(
    "source_cohort", Path(__file__).resolve().parents[2] / "verify-v8-source-cohort.py"
)
cohort = importlib.util.module_from_spec(spec)
spec.loader.exec_module(cohort)


class SourceCohortTest(unittest.TestCase):
    def setUp(self):
        self.scratch = tempfile.TemporaryDirectory()
        self.addCleanup(self.scratch.cleanup)
        self.root = Path(self.scratch.name)
        self.source = self.root / "source"
        (self.source / "v8").mkdir(parents=True)
        (self.source / "Cargo.toml").write_text('[package]\nversion = "150.4.0"\n')
        for source, name in [(self.source / "v8", "VERSION"), (self.source, "Cargo.toml")]:
            if name == "VERSION": (source / name).write_text("150\n")
            for args in [("init", "-q"), ("add", name),
                         ("-c", "user.name=fixture", "-c", "user.email=fixture@example.invalid",
                          "commit", "-qm", "source")]:
                subprocess.run(["git", "-C", str(source), *args], check=True)
        (self.root / "engine").mkdir()
        (self.root / "engine/Cargo.lock").write_text('[[package]]\nname = "v8"\nversion = "150.4.0"\n')
        self.locks = self.root / "contracts/artifact-manifest"
        self.locks.mkdir(parents=True)
        self.identity = {"rusty_v8_version": "150.4.0", "rusty_v8_revision": cohort.revision(self.source),
                         "v8_revision": cohort.revision(self.source / "v8")}
        self.write_locks(self.identity)

    def write_locks(self, identity):
        for platform in ["android", "apple", "ohos", "windows"]:
            (self.locks / f"{platform}-v8.lock.json").write_text(json.dumps(identity))

    def test_matching_native_source_and_cargo_are_accepted(self):
        cohort.verify(self.root, self.source)

    def test_one_platform_cannot_silently_use_another_revision(self):
        changed = {**self.identity, "rusty_v8_revision": "0" * 40}
        (self.locks / "windows-v8.lock.json").write_text(json.dumps(changed))
        with self.assertRaisesRegex(ValueError, "different V8 cohorts"):
            cohort.verify(self.root, self.source)

    def test_same_version_but_wrong_native_source_is_rejected(self):
        self.write_locks({**self.identity, "v8_revision": "0" * 40})
        with self.assertRaisesRegex(ValueError, "checkout differs"):
            cohort.verify(self.root, self.source)

    def test_old_cargo_cannot_consume_the_new_archive(self):
        (self.root / "engine/Cargo.lock").write_text('[[package]]\nname = "v8"\nversion = "145.0.0"\n')
        with self.assertRaisesRegex(ValueError, "different V8 versions"):
            cohort.verify(self.root, self.source)
