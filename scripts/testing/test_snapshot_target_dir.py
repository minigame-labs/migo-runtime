"""Snapshot generation must materialise V8 under the selected Cargo target.

The executable path already followed Cargo, but the earlier materialisation
step still wrote to engine/target and failed on an imported/read-only cache.
Run the real entry point until the materialisation boundary with fake tool
providers: no native archive, build, device, registry, or default target writes.
"""
import argparse
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / 'scripts/gen-snapshot.sh'


class SnapshotTargetDirectory(unittest.TestCase):
    def test_linux_and_android_materialise_under_either_cargo_override(self):
        for platform in ['linux', 'android']:
            for variable in ['CARGO_TARGET_DIR', 'CARGO_BUILD_TARGET_DIR']:
                with self.subTest(platform=platform, variable=variable), tempfile.TemporaryDirectory(prefix='migo-snapshot-target-') as temp:
                    root = Path(temp)
                    libs = root / 'scripts/lib'
                    libs.mkdir(parents=True)
                    (root / 'engine').mkdir()
                    source = root / 'scripts/gen-snapshot.sh'
                    shutil.copyfile(SCRIPT, source)
                    for name in ['cargo-target-dir.sh', 'python-cmd.sh']:
                        shutil.copyfile(ROOT / 'scripts/lib' / name, libs / name)
                    (libs / 'snapshot-fingerprint.sh').write_text('snapshot_valid_os() { return 0; }\nsnapshot_v8_target_dir() { echo fixture-native; }\n')
                    (libs / 'android-ndk.sh').write_text('android_ndk_read_pin() { return 0; }\nandroid_ndk_resolve() { ANDROID_NDK_HOME="$FIXTURE_NDK"; export ANDROID_NDK_HOME; }\n')
                    (libs / 'v8-materialise.sh').write_text('v8_materialise() { printf "%s\\n" "$2" > "$FIXTURE_CAPTURE"; return 1; }\n')
                    tools = root / 'tools'
                    tools.mkdir()
                    cargo = tools / 'cargo'
                    cargo.write_text('#!/usr/bin/env python3\nimport json,os,sys\nassert sys.argv[1:] == ["metadata","--format-version","1","--no-deps","--offline"]\nprint(json.dumps({"target_directory":os.environ.get("CARGO_TARGET_DIR",os.environ.get("CARGO_BUILD_TARGET_DIR"))}))\n')
                    cargo.chmod(0o700)
                    environment = os.environ.copy()
                    environment.pop('CARGO_TARGET_DIR', None)
                    environment.pop('CARGO_BUILD_TARGET_DIR', None)
                    selected = root / 'selected-target'
                    capture = root / 'materialise-argument'
                    environment.update({variable: str(selected), 'PATH': str(tools) + os.pathsep + os.environ['PATH'],
                                        'FIXTURE_CAPTURE': str(capture), 'FIXTURE_NDK': str(root / 'fake-ndk')})
                    result = subprocess.run(['bash', str(source), 'x86_64', '--os', platform], env=environment, text=True, capture_output=True, timeout=10)
                    self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
                    self.assertIn('cannot use the', result.stderr)
                    self.assertEqual(capture.read_text().strip(), str(selected / 'v8-materialised'))
                    self.assertFalse((root / 'engine/target').exists())


if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('--script', type=Path, default=SCRIPT)
    args = parser.parse_args()
    SCRIPT = args.script.resolve()
    unittest.main(argv=['snapshot-target-dir'])
