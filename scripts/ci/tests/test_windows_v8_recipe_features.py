"""Native identity records the features selected by the build command."""
import importlib.util
from pathlib import Path
import unittest


spec = importlib.util.spec_from_file_location(
    "windows_v8_writer", Path(__file__).resolve().parents[2] / "write-windows-v8-component-manifest.py"
)
writer = importlib.util.module_from_spec(spec)
spec.loader.exec_module(writer)


class WindowsRecipeFeaturesTest(unittest.TestCase):
    def test_real_recipe_selects_matching_compression_and_simdutf(self):
        recipe = (Path(__file__).resolve().parents[2] / "build-v8-windows.sh").read_text()
        self.assertEqual(writer.recipe_cargo_features(recipe), {
            "use_custom_libcxx": True, "simdutf": True, "v8_enable_pointer_compression": True,
        })

    def test_comments_cannot_enable_or_disable_features(self):
        self.assertEqual(writer.recipe_cargo_features(
            "# --no-default-features --features simdutf,v8_enable_pointer_compression\n"
            "cargo build --release\n"
        ), {"use_custom_libcxx": True, "simdutf": False, "v8_enable_pointer_compression": False})

    def test_explicit_selection_with_no_defaults(self):
        self.assertEqual(writer.recipe_cargo_features(
            'cargo build --no-default-features --features="simdutf use_custom_libcxx"\n'
        ), {"use_custom_libcxx": True, "simdutf": True, "v8_enable_pointer_compression": False})

    def test_missing_or_ambiguous_commands_fail_closed(self):
        for recipe in ["# cargo build\n", "cargo build\ncargo build --features simdutf\n"]:
            with self.assertRaisesRegex(RuntimeError, "unambiguous"):
                writer.recipe_cargo_features(recipe)
