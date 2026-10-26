"""A passing subset must not silently replace the pinned conformance corpus."""
import importlib.util
import json
from pathlib import Path
import shutil
import tempfile
import unittest

REPO = Path(__file__).resolve().parents[3]
spec = importlib.util.spec_from_file_location("url_wpt", REPO / "scripts/lib/url_wpt.py")
helper = importlib.util.module_from_spec(spec)
spec.loader.exec_module(helper)
DATA = REPO / "engine/crates/runtime-v8/testdata/url-wpt"


class UrlWptTest(unittest.TestCase):
    def test_real_pinned_fixtures_and_coverage(self):
        helper.verify(DATA)

    def test_changed_fixture_and_changed_origin_are_both_rejected(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary) / "wpt"
            shutil.copytree(DATA, directory)
            path = directory / "urltestdata.json"
            original = path.read_bytes()
            path.write_bytes(original + b"\n")
            with self.assertRaisesRegex(ValueError, "differs from its upstream identity"):
                helper.verify(directory)
            path.write_bytes(original)
            manifest = directory / "provenance.json"
            value = json.loads(manifest.read_text())
            value["files"]["urltestdata.json"]["url"] = value["files"]["urltestdata.json"]["url"].replace(value["revision"], "master")
            manifest.write_text(json.dumps(value))
            with self.assertRaisesRegex(ValueError, "pinned upstream path"):
                helper.verify(directory)
