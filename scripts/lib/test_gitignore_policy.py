from pathlib import Path
import tempfile
import unittest
import subprocess

from gitignore_policy import check, tracked_ignored


class GitignorePolicy(unittest.TestCase):
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
