#!/usr/bin/env python3
"""Regressions for exact license approvals surviving a resolved-graph change."""

from __future__ import annotations

import copy
import hashlib
import importlib.util
import json
from pathlib import Path
import sys
import tempfile
import unittest
from types import SimpleNamespace

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from lib.supply_chain import validate_metadata

_spec = importlib.util.spec_from_file_location(
    "generate_sbom", Path(__file__).resolve().parents[1] / "generate-sbom.py",
)
assert _spec is not None and _spec.loader is not None
sbom = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(sbom)


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


class NativeSourceIdentityTests(unittest.TestCase):
    def setUp(self) -> None:
        self.work = tempfile.TemporaryDirectory()
        self.addCleanup(self.work.cleanup)
        self.root = Path(self.work.name)
        self.source = self.root / "native"
        self.source.mkdir()
        (self.source / "audio.c").write_text("int audio(void) { return 1; }\n")
        (self.source / "LICENSE").write_text("Fixture approved MIT license\n")
        self.record = {
            "name": "fixture-native", "version": "1.0.0",
            "owner_package": "fixture-audio", "platforms": ["android"],
            "license": "MIT", "source_dir": "native",
            "source_sha256": sbom.native_source_hash(self.source),
            "license_file": "LICENSE",
            "license_sha256": hashlib.sha256((self.source / "LICENSE").read_bytes()).hexdigest(),
            "repository": "https://example.invalid/native",
            "upstream_revision": "1" * 40, "upstream_archive_sha256": "2" * 64,
            "source_modified": False,
        }
        self.policy = {"licenses": {"allowed": ["MIT"]}, "native_components": [self.record]}
        self.packages = {"audio": {"name": "fixture-audio"}}
        self.refs = {"audio": "pkg:cargo/fixture-audio@1.0.0"}

    def components(self, targets: list[str] | None = None) -> tuple[list, dict]:
        return sbom.native_components(
            self.policy, self.root, self.packages, self.refs,
            [(target, {package["name"] for package in self.packages.values()})
             for target in (targets if targets is not None else ["aarch64-linux-android"])],
        )

    def test_android_native_source_is_a_dependency_of_its_actual_rust_owner(self) -> None:
        components, edges = self.components()
        self.assertEqual(len(components), 1)
        self.assertEqual(edges[self.refs["audio"]], [components[0]["bom-ref"]])
        self.assertEqual(components[0]["licenses"], [{"expression": "MIT"}])

    def test_native_dependency_is_bound_to_the_concrete_artifact_in_the_sbom(self) -> None:
        metadata = self.root / "metadata.json"
        metadata.write_text(json.dumps({
            "packages": [{"id": "audio", "name": "fixture-audio", "version": "1.0.0",
                          "source": None, "license": "MIT",
                          "manifest_path": str(self.root / "Cargo.toml")}],
            "workspace_members": ["audio"],
            "resolve": {"nodes": [{"id": "audio", "dependencies": []}]},
        }))
        policy = self.root / "policy.toml"
        policy.write_text('schema = 1\n[licenses]\nallowed = ["MIT"]\n[[native_components]]\n'
                          + "".join(f"{key} = {json.dumps(value)}\n"
                                    for key, value in self.record.items()))
        tree = self.root / "compiled.txt"
        tree.write_text("fixture-audio v1.0.0\n")
        artifact = self.root / "fixture.a"
        artifact.write_bytes(b"fixture artifact bytes")
        document = sbom.build(SimpleNamespace(
            metadata=[metadata], policy=policy, root_package="fixture-audio",
            workspace_root=self.root, compiled=[tree], cargo_target=["aarch64-linux-android"],
            artifact=artifact, source_revision="fixture-source", artifact_kind="fixture",
            profile="fixture", target="android",
        ))
        native_ref = document["components"][0]["bom-ref"]
        artifact_ref = document["metadata"]["component"]["bom-ref"]
        self.assertIn({"ref": artifact_ref, "dependsOn": [native_ref]}, document["dependencies"])
        self.assertEqual(document["metadata"]["component"]["hashes"][0]["content"],
                         hashlib.sha256(artifact.read_bytes()).hexdigest())

    def test_android_backend_is_absent_from_linux_only_artifacts(self) -> None:
        self.assertEqual(self.components(["x86_64-unknown-linux-gnu"]), ([], {}))

    def test_multiplatform_artifact_keeps_android_backend(self) -> None:
        self.assertEqual(len(self.components([
            "x86_64-unknown-linux-gnu", "aarch64-linux-android",
        ])[0]), 1)

    def test_audio_in_linux_slice_cannot_add_oboe_to_android_slice_without_audio(self) -> None:
        self.assertEqual(sbom.native_components(
            self.policy, self.root, self.packages, self.refs,
            [("aarch64-linux-android", set()),
             ("x86_64-unknown-linux-gnu", {"fixture-audio"})],
        ), ([], {}))

    def test_product_without_audio_never_gains_its_native_backend(self) -> None:
        self.packages.clear()
        self.assertEqual(self.components(), ([], {}))

    def test_platform_label_cannot_substitute_for_an_actual_cargo_target(self) -> None:
        with self.assertRaisesRegex(sbom.PolicyError, "requires --cargo-target"):
            self.components([])

    def test_changed_or_added_native_source_fails_its_pinned_identity(self) -> None:
        (self.source / "new.c").write_text("int unexpected;\n")
        with self.assertRaisesRegex(sbom.PolicyError, "source hash differs"):
            self.components()

    def test_source_reapproval_cannot_silently_change_license_approval(self) -> None:
        (self.source / "LICENSE").write_text("Changed license terms\n")
        self.record["source_sha256"] = sbom.native_source_hash(self.source)
        with self.assertRaisesRegex(sbom.PolicyError, "license hash differs"):
            self.components()

    def test_unapproved_native_license_fails(self) -> None:
        self.record["license"] = "GPL-3.0-only"
        with self.assertRaisesRegex(sbom.PolicyError, "license is not approved"):
            self.components()

    def test_reference_source_is_not_substituted_by_a_symlink(self) -> None:
        (self.source / "alias.c").symlink_to(self.source / "audio.c")
        with self.assertRaisesRegex(sbom.PolicyError, "regular vendored file"):
            self.components()


if __name__ == "__main__":
    unittest.main()
