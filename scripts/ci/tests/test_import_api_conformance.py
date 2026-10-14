"""What makes an API verified, failing, present or absent in a conformance record (scripts/import-api-conformance.py)."""

from __future__ import annotations

import importlib.util
import pathlib
import unittest

_MODULE_PATH = pathlib.Path(__file__).resolve().parents[2] / "import-api-conformance.py"
_spec = importlib.util.spec_from_file_location("import_api_conformance", _MODULE_PATH)
importer = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(importer)


def run(*rows: tuple[str, bool]) -> dict:
    return {"results": [{"id": id_, "passed": passed} for id_, passed in rows]}


class StatusesTests(unittest.TestCase):
    APIS = ["connectSocket"]

    def status(self, *rows: tuple[str, bool]) -> dict:
        return importer.statuses(run(("api-spec/connectSocket/exists", True), *rows), self.APIS)["connectSocket"]

    def test_existing_and_nothing_more_checked_is_present(self):
        self.assertEqual(self.status(), {"status": "present"})

    def test_a_bundle_that_exercises_the_api_verifies_it(self):
        self.assertEqual(self.status(("lan-spec/connectSocket/a-task-opens-echoes-and-closes", True)), {"status": "verified"})

    def test_a_check_of_the_call_s_shape_alone_does_not_verify(self):
        # api-surface-spec's blind call: it ended in "fail invalid url", which is the contract kept.
        self.assertEqual(
            self.status(
                ("api-surface-spec/connectSocket/does-not-throw", True),
                ("api-surface-spec/connectSocket/ends-in-complete", True),
            ),
            {"status": "present"},
        )

    def test_a_failing_shape_check_still_makes_the_api_failing(self):
        self.assertEqual(
            self.status(("api-surface-spec/connectSocket/does-not-throw", False)),
            {"status": "failing", "failing": ["api-surface-spec/connectSocket/does-not-throw"]},
        )

    def test_an_api_that_does_not_exist_is_absent(self):
        recorded = importer.statuses(run(("api-spec/connectSocket/exists", False)), self.APIS)
        self.assertEqual(recorded["connectSocket"], {"status": "absent"})


if __name__ == "__main__":
    unittest.main()
