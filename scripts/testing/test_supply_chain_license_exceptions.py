#!/usr/bin/env python3
"""Regressions for exact license approvals surviving a resolved-graph change."""

from __future__ import annotations

import copy
import hashlib
from pathlib import Path
import sys
import tempfile
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from lib.supply_chain import validate_metadata


class LicenseExceptionScopeTests(unittest.TestCase):
    def setUp(self) -> None:
        self.work = tempfile.TemporaryDirectory()
        self.addCleanup(self.work.cleanup)
        self.root = Path(self.work.name)
        license_file = self.root / "LICENSE"
        license_file.write_text("Fixture exact license approval\n")
        self.dep = {
            "id": "registry+https://github.com/rust-lang/crates.io-index#approved@1.0.0",
            "name": "approved", "version": "1.0.0",
            "source": "registry+https://github.com/rust-lang/crates.io-index",
            "license": None, "license_file": "LICENSE",
            "manifest_path": str(self.root / "Cargo.toml"),
        }
        self.other = {
            **self.dep, "id": "other", "name": "other", "license": "MIT",
        }
        self.policy = {
            "licenses": {"allowed": ["MIT"]},
            "license_file_exceptions": [{
                "package": "approved", "version": "1.0.0", "expression": "MIT",
                "sha256": hashlib.sha256(license_file.read_bytes()).hexdigest(),
            }],
        }

    def errors(self, packages: list[dict], selected: set[str] | None = None) -> list[str]:
        _, errors = validate_metadata({"packages": packages}, self.policy, self.root, selected)
        return errors

    def assert_stale(self, errors: list[str]) -> None:
        self.assertIn(
            "approved@1.0.0: license-file approval is stale or was not exercised", errors,
        )

    def test_matching_license_file_approval_is_still_accepted(self) -> None:
        self.assertEqual(self.errors([self.dep]), [])

    def test_package_removed_from_complete_graph_invalidates_approval(self) -> None:
        self.assert_stale(self.errors([self.other]))

    def test_version_replaced_in_complete_graph_invalidates_old_approval(self) -> None:
        replacement = {**self.dep, "version": "1.0.1", "license": "MIT"}
        self.assert_stale(self.errors([replacement]))

    def test_package_now_declares_spdx_invalidates_unused_approval(self) -> None:
        self.assert_stale(self.errors([{**self.dep, "license": "MIT"}]))

    def test_partial_scan_does_not_invalidate_another_products_approval(self) -> None:
        self.assertEqual(self.errors([self.dep, self.other], {self.other["id"]}), [])

    def test_partial_metadata_cannot_prove_global_approval_is_stale(self) -> None:
        self.assertEqual(self.errors([self.other], {self.other["id"]}), [])

    def test_selected_package_still_requires_exact_license_hash(self) -> None:
        policy = copy.deepcopy(self.policy)
        policy["license_file_exceptions"][0]["sha256"] = "0" * 64
        _, errors = validate_metadata({"packages": [self.dep]}, policy, self.root, {self.dep["id"]})
        self.assertTrue(any("differs from approved" in error for error in errors), errors)


if __name__ == "__main__":
    unittest.main()
