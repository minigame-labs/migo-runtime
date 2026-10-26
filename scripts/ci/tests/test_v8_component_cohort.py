"""Hash-correct old artifacts and ABI-mismatched artifacts must never be linked."""
import copy
import importlib.util
import json
from pathlib import Path
import subprocess
import tempfile
import unittest

REPO = Path(__file__).resolve().parents[3]
spec = importlib.util.spec_from_file_location("v8_cohort", REPO / "scripts/lib/v8_cohort.py")
cohort = importlib.util.module_from_spec(spec)
spec.loader.exec_module(cohort)


class ComponentCohortTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.identity = dict(zip(cohort.FIELDS, ("150.4.0", "1" * 40, "2" * 40)))
        for platform in ("android", "apple", "ohos", "windows"):
            self.write(f"contracts/artifact-manifest/{platform}-v8.lock.json", json.dumps(self.identity))
        self.write("engine/Cargo.lock", '[[package]]\nname = "v8"\nversion = "150.4.0"\n')
        for name in ("engine/Cargo.toml", "engine/crates/runtime-v8/Cargo.toml"):
            self.write(name, (REPO / name).read_text())
        self.value = {"target": {"triple": "x86_64-unknown-linux-gnu"}, "runtime": {
            **self.identity, "normalized_gn_args": ["v8_enable_pointer_compression=true"]}}
        self.manifest = self.root / "component.json"

    def write(self, name, data):
        path = self.root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(data)

    def verify(self, value=None):
        self.manifest.write_text(json.dumps(self.value if value is None else value))
        cohort.verify_component(self.root, self.manifest)

    def test_all_ten_matching_targets(self):
        for triple in sorted(cohort.COMPRESSED_TARGETS | cohort.UNCOMPRESSED_TARGETS):
            with self.subTest(triple=triple):
                value = copy.deepcopy(self.value)
                value["target"]["triple"] = triple
                compressed = str(triple in cohort.COMPRESSED_TARGETS).lower()
                value["runtime"]["normalized_gn_args"] = [f"v8_enable_pointer_compression={compressed}"]
                self.verify(value)

    def test_each_component_identity_field_is_required_to_match(self):
        for key in cohort.FIELDS:
            value = copy.deepcopy(self.value)
            value["runtime"][key] = "stale"
            with self.subTest(key=key), self.assertRaisesRegex(ValueError, "differs from selected cohort"):
                self.verify(value)

    def test_missing_false_or_duplicate_gn_mode_is_rejected(self):
        for args in ([], ["v8_enable_pointer_compression=false"],
                     ["v8_enable_pointer_compression=true"] * 2):
            value = copy.deepcopy(self.value)
            value["runtime"]["normalized_gn_args"] = args
            with self.subTest(args=args), self.assertRaisesRegex(ValueError, "pointer-compression ABI"):
                self.verify(value)

    def test_removed_rust_feature_or_widened_default_is_rejected(self):
        runtime = self.root / "engine/crates/runtime-v8/Cargo.toml"
        original = runtime.read_text()
        runtime.write_text(original.replace('features = ["v8_enable_pointer_compression"]', 'features = []'))
        with self.assertRaisesRegex(ValueError, "target policy"):
            self.verify()
        runtime.write_text(original)
        cargo = self.root / "engine/Cargo.toml"
        data = cargo.read_text()
        cargo.write_text(data.replace('features = ["v8", "reactor-tokio"]',
                                      'features = ["v8", "reactor-tokio", "v8_enable_pointer_compression"]'))
        with self.assertRaisesRegex(ValueError, "only by the target policy"):
            self.verify()

    def test_materialisers_reject_a_hash_correct_stale_component_before_staging(self):
        # Exercise the actual public shell functions, not just the Python validator.
        # The stub files hash correctly; only their old cohort is invalid.
        import hashlib
        component = self.root / "v8"
        component.mkdir()
        value = copy.deepcopy(self.value)
        value["runtime"]["rusty_v8_version"] = "145.0.0"
        value["hashes"] = {}
        for name, field in (("librusty_v8.a", "archive"), ("src_binding.rs", "rust_binding")):
            payload = name.encode()
            (component / name).write_bytes(payload)
            value["hashes"][field] = hashlib.sha256(payload).hexdigest()
        (component / "component-manifest.json").write_text(json.dumps(value))
        for name in ("rusty_v8.lib", "rusty_v8.dll", "rusty_v8.dll.lib"):
            (component / name).write_bytes(b"librusty_v8.a")
        for function in ("v8_materialise", "v8_materialise_windows"):
            with self.subTest(function=function):
                destination = self.root / function
                result = subprocess.run(["bash", "-c", 'source "$1"; "$2" "$3" "$4"', "fixture",
                    str(REPO / "scripts/lib/v8-materialise.sh"), function,
                    str(component), str(destination)], text=True, capture_output=True)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("differs from selected cohort", result.stderr)
                self.assertFalse(destination.exists())


if __name__ == "__main__":
    unittest.main()
