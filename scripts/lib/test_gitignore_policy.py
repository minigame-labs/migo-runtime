from pathlib import Path
import tempfile
import unittest
import subprocess

from gitignore_policy import check, tracked_ignored


class GitignorePolicy(unittest.TestCase):
    def test_authored_lf_and_raw_vendor_crlf_survive_windows_checkout(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            subprocess.run(['git', 'init', '--quiet', '--template=', str(root)], check=True)
            (root / '.gitattributes').write_bytes(
                (Path(__file__).resolve().parents[2] / '.gitattributes').read_bytes())
            fixtures = {
                'engine/crates/runtime-v8/src/start.js': b'authored source\nnext line\n',
                'engine/Cargo.lock': b'version = 4\n',
                'contracts/artifact-manifest/test.lock.json': b'{\n  "schema": 1\n}\n',
                'engine/third_party/v8-150.4.0/src/upstream.rs': b'original\r\nbytes\r\n',
                'engine/crates/runtime-v8/testdata/frozen.json': b'{\r\n}\r\n',
            }
            for name, data in fixtures.items():
                file = root / name
                file.parent.mkdir(parents=True, exist_ok=True)
                file.write_bytes(data)
            subprocess.run(['git', '-C', str(root), '-c', 'core.autocrlf=true',
                            'add', '--', '.gitattributes', *fixtures], check=True, capture_output=True)
            checkout = root / 'checkout'
            subprocess.run(['git', '-C', str(root), '-c', 'core.autocrlf=true',
                            'checkout-index', '--all', '--prefix=' + str(checkout) + '/'],
                           check=True, capture_output=True)
            for name, expected in fixtures.items():
                self.assertEqual((checkout / name).read_bytes(), expected, name)
            # Removing the default reproduces the authored-input hash drift;
            # the raw vendor/fixture exceptions continue to preserve bytes.
            attributes = root / '.gitattributes'
            attributes.write_bytes(attributes.read_bytes().replace(b'* text=auto eol=lf\n', b''))
            subprocess.run(['git', '-C', str(root), 'add', '--', '.gitattributes'],
                           check=True, capture_output=True)
            unprotected = root / 'unprotected'
            subprocess.run(['git', '-C', str(root), '-c', 'core.autocrlf=true',
                            'checkout-index', '--all', '--prefix=' + str(unprotected) + '/'],
                           check=True, capture_output=True)
            for name in list(fixtures)[:3]:
                self.assertNotEqual((unprotected / name).read_bytes(), fixtures[name], name)

    def test_byte_exact_vendor_inputs_survive_autocrlf_checkout(self):
        attributes = (Path(__file__).resolve().parents[2] / '.gitattributes').read_bytes()
        samples = [
            'engine/third_party/deno_core-0.413.0/Cargo.toml.orig',
            'engine/third_party/v8-150.4.0/src/lib.rs',
            'engine/third_party/ada-url-4.0.0/LICENSE',
            'engine/third_party/skia-bindings-0.93.1/Cargo.lock',
            'engine/third_party/rustls-platform-verifier-android-0.2.0/CertificateVerifier.kt',
            'engine/third_party/deno-core-patches/0001.patch',
            'engine/third_party/rusty_v8/x86_64-linux-gnu/src_binding.rs',
        ]
        # The empty-attributes control demonstrates a real checkout rewrite.
        for declared in [b'', attributes]:
            with self.subTest(protected=bool(declared)), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                subprocess.run(['git', 'init', '--quiet', '--template=', str(root)], check=True)
                (root / '.gitattributes').write_bytes(declared)
                original = b'byte-exact upstream source\nsecond line\n'
                for name in samples:
                    source = root / name
                    source.parent.mkdir(parents=True, exist_ok=True)
                    source.write_bytes(original)
                subprocess.run(['git', '-C', str(root), '-c', 'core.autocrlf=true',
                                'add', '--', '.gitattributes', *samples], check=True,
                               stdout=subprocess.PIPE, stderr=subprocess.PIPE)
                output = root / 'checkout'
                subprocess.run(['git', '-C', str(root), '-c', 'core.autocrlf=true',
                                'checkout-index', '--all', '--prefix=' + str(output) + '/'],
                               check=True, capture_output=True)
                for name in samples:
                    if declared:
                        self.assertEqual((output / name).read_bytes(), original)
                    else:
                        self.assertNotEqual((output / name).read_bytes(), original)

    def test_lockfile_exception_and_missing_checkout_input(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            rules = root / '.gitignore'
            source = 'vendor/core/Cargo.lock'
            # Inputs come from the upstream archive inventory, even if the
            # ignored file has already disappeared from a clean checkout.
            self.assertFalse((root / source).exists())
            rules.write_text('Cargo.lock\n')
            with self.assertRaisesRegex(ValueError, 'required source ignored'):
                check(root, [source])
            rules.write_text('Cargo.lock\n!vendor/core/Cargo.lock\n')
            check(root, [source])

    def test_nested_rule_overrides_root_exception(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / '.gitignore').write_text('Cargo.lock\n!vendor/core/Cargo.lock\n')
            nested = root / 'vendor/core/.gitignore'
            nested.parent.mkdir(parents=True)
            nested.write_text('Cargo.lock\n')
            with self.assertRaisesRegex(ValueError, 'required source ignored'):
                check(root, ['vendor/core/Cargo.lock'])
            nested.write_text('/target\n')
            check(root, ['vendor/core/Cargo.lock'])

    def test_generated_output_and_anchored_documentation(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            rules = root / '.gitignore'
            required = ['developer-docs/src/content/docs/index.mdx']
            generated = ['docs/results/run.json', 'profile.cpuprofile']
            rules.write_text('/docs/\n*.cpuprofile\n')
            check(root, required, generated)
            rules.write_text('/docs/\n')
            with self.assertRaisesRegex(ValueError, 'generated output not ignored'):
                check(root, required, generated)
            rules.write_text('docs/\n*.cpuprofile\n')
            with self.assertRaisesRegex(ValueError, 'required source ignored'):
                check(root, required, generated)

    def test_unsafe_paths_fail(self):
        with tempfile.TemporaryDirectory() as temporary:
            for path in ['../outside', '/absolute']:
                with self.subTest(path=path), self.assertRaises(ValueError):
                    check(Path(temporary), [path])

    def test_previously_tracked_output_is_detected_and_local_file_survives(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            subprocess.run(['git', 'init', '--quiet', '--template=', str(root)], check=True)
            (root / '.gitignore').write_text('/test-results/\n')
            report = root / 'test-results/result.json'
            report.parent.mkdir()
            report.write_text('{}\n')
            subprocess.run(['git', '-C', str(root), 'add', '--force', '--',
                            'test-results/result.json'], check=True)
            self.assertEqual(tracked_ignored(root), ['test-results/result.json'])
            subprocess.run(['git', '-C', str(root), 'rm', '--quiet', '--cached', '--',
                            'test-results/result.json'], check=True)
            self.assertEqual(tracked_ignored(root), [])
            self.assertEqual(report.read_text(), '{}\n')


if __name__ == '__main__':
    unittest.main()
