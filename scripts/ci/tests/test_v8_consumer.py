"""The component consumer must work without GN, downloaders or build crates."""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT / 'scripts/lib'))
import vendor_layout


class V8Consumer(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.scratch = tempfile.TemporaryDirectory(prefix='migo-v8-consumer-gate-')
        cls.base = Path(cls.scratch.name)
        cls.builder = cls.base / 'builder'
        subprocess.run(['rustc', '--edition', '2024',
                        str(ROOT / 'engine/third_party/v8-150.4.0/build.rs'),
                        '-o', str(cls.builder)], cwd=ROOT / 'engine', check=True, capture_output=True)
        (cls.base / 'librusty_v8.a').touch()
        (cls.base / 'rusty_v8.lib').touch()
        (cls.base / 'src_binding.rs').touch()

    @classmethod
    def tearDownClass(cls):
        cls.scratch.cleanup()

    def environment(self, **extra):
        return {'CARGO_CFG_TARGET_OS': 'linux', 'CARGO_CFG_TARGET_ENV': 'gnu',
                'TARGET': 'x86_64-unknown-linux-gnu', 'CARGO_FEATURE_USE_CUSTOM_LIBCXX': '1',
                'RUSTY_V8_ARCHIVE': str(self.base / 'librusty_v8.a'),
                'RUSTY_V8_SRC_BINDING_PATH': str(self.base / 'src_binding.rs'), **extra}

    def run_builder(self, environment):
        return subprocess.run([str(self.builder)], env=environment, cwd=self.base,
                              capture_output=True, text=True, timeout=5)

    def test_local_component_links_directly_without_build_directory(self):
        for target, env, archive in [('linux', 'gnu', 'librusty_v8.a'),
                                     ('windows', 'msvc', 'rusty_v8.lib')]:
            result = self.run_builder(self.environment(CARGO_CFG_TARGET_OS=target,
                CARGO_CFG_TARGET_ENV=env, RUSTY_V8_ARCHIVE=str(self.base / archive)))
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn('cargo:rustc-link-search=native=' + str(self.base), result.stdout)
            self.assertFalse((self.base / 'gn_out').exists())

    def test_native_source_mode_and_network_locations_are_refused(self):
        result = self.run_builder(self.environment(V8_FROM_SOURCE='1'))
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('scripts/build-v8-<os>.sh', result.stderr)
        result = self.run_builder(self.environment(RUSTY_V8_ARCHIVE='https://example.invalid/v8.a'))
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('materialised local file', result.stderr)

    def test_missing_binding_or_archive_never_falls_back(self):
        for missing in ['RUSTY_V8_ARCHIVE', 'RUSTY_V8_SRC_BINDING_PATH']:
            environment = self.environment()
            del environment[missing]
            result = self.run_builder(environment)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn(missing, result.stderr)

    def test_documentation_and_compile_only_paths_do_not_require_archive(self):
        for mode in ['DOCS_RS', 'DENO_TRYBUILD']:
            environment = self.environment(**{mode: '1'})
            del environment['RUSTY_V8_ARCHIVE']
            result = self.run_builder(environment)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn('cargo:rustc-env=RUSTY_V8_SRC_BINDING_PATH=', result.stdout)
            self.assertNotIn('cargo:rustc-link-search=', result.stdout)
            if mode == 'DOCS_RS':
                self.assertNotIn('cargo:rustc-link-lib=', result.stdout)

    def test_layout_keeps_all_rust_sources_and_rejects_inventory_drift(self):
        patches = ROOT / 'engine/third_party/rusty-v8-rust-patches'
        layout = vendor_layout.load(patches, 'v8', '150.4.0')
        for path in ['src/future_new_api.rs', 'gen/future_binding.rs', 'Cargo.lock']:
            self.assertTrue(vendor_layout.selected(path, layout))
        self.assertFalse(vendor_layout.selected('third_party/icu/source/unused.cc', layout))
        self.assertTrue(vendor_layout.selected('third_party/highway/src/debian/copyright', layout))
        with self.assertRaisesRegex(ValueError, 'absent'):
            vendor_layout.validate_archive(layout, {'src/lib.rs': 1})
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            invalid = {**layout, 'retained_roots': ['src']}
            (root / 'SOURCE_LAYOUT.json').write_text(json.dumps(invalid))
            with self.assertRaisesRegex(ValueError, 'layout'):
                vendor_layout.load(root, 'v8', '150.4.0')


if __name__ == '__main__':
    unittest.main()
