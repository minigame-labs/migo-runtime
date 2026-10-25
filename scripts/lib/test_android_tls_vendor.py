from pathlib import Path
import shutil
import tempfile
import unittest
import io
import zipfile

from android_tls_vendor import ROOT, VENDOR, check


class AndroidTlsVendor(unittest.TestCase):
    def test_actual_cohort(self):
        check()

    def test_packaged_jar_provenance_and_host_manifest_policy(self):
        notice = ROOT / VENDOR / 'notice/META-INF/migo-rustls-platform-verifier'
        def aar(*, jar=True, provenance=True, override=False):
            sdk = io.BytesIO()
            with zipfile.ZipFile(sdk, 'w') as classes:
                for name in ['UPSTREAM.json', 'LICENSE-MIT', 'LICENSE-APACHE']:
                    if name != 'UPSTREAM.json' or provenance:
                        classes.writestr('META-INF/migo-rustls-platform-verifier/' + name,
                                         (notice / name).read_bytes())
            data = io.BytesIO()
            with zipfile.ZipFile(data, 'w') as package:
                package.writestr('classes.jar', sdk.getvalue())
                if jar:
                    package.writestr('libs/classes.jar', (ROOT / VENDOR / 'classes.jar').read_bytes())
                attr = ' android:networkSecurityConfig="@xml/upstream"' if override else ''
                package.writestr('AndroidManifest.xml', '<manifest xmlns:android="http://schemas.android.com/apk/res/android"><application'
                                 + attr + '/></manifest>')
            data.seek(0)
            return data
        check(aar=aar())
        for flags, message in [({'jar': False}, 'exact source-derived'),
                               ({'provenance': False}, 'license/provenance'),
                               ({'override': True}, 'networkSecurityConfig')]:
            with self.subTest(flags=flags), self.assertRaisesRegex(ValueError, message):
                check(aar=aar(**flags))

    def test_bytecode_cohort_missing_runtime_and_r8_drift_are_refused(self):
        import json
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            shutil.copytree(ROOT / VENDOR, root / VENDOR)
            for file in ("engine/Cargo.lock", "contracts/android-runtime-dependencies.json",
                         "platforms/android/library/build.gradle", "platforms/android/library/consumer-rules-common.pro",
                         "scripts/rebuild-android-tls-verifier.py", "scripts/lib/android_tls_sandbox.py",
                         "scripts/lib/android_tls_source.py"):
                target = root / file
                target.parent.mkdir(parents=True, exist_ok=True)
                shutil.copyfile(ROOT / file, target)
            check(root)
            jar = root / VENDOR / "classes.jar"
            original = jar.read_bytes()
            jar.write_bytes(original + b"unrecorded edit")
            with self.assertRaisesRegex(ValueError, "hash mismatch"):
                check(root)
            jar.write_bytes(original)
            patch = root / VENDOR / 'patches/0001-use-host-trust-policy.patch'
            original_patch = patch.read_bytes()
            patch.write_bytes(original_patch + b'\n')
            with self.assertRaisesRegex(ValueError, 'patch hash'):
                check(root)
            patch.write_bytes(original_patch)
            meta = root / VENDOR / 'UPSTREAM.json'
            original_meta = meta.read_text()
            data = json.loads(original_meta)
            data['patches'] = data['patches'][1:]
            meta.write_text(json.dumps(data))
            with self.assertRaisesRegex(ValueError, 'patch inventory'):
                check(root)
            meta.write_text(original_meta)
            lock = root / "engine/Cargo.lock"
            original = lock.read_text()
            lock.write_text(original.replace('name = "rustls-platform-verifier-android"\nversion = "0.2.0"',
                                             'name = "rustls-platform-verifier-android"\nversion = "0.1.0"'))
            with self.assertRaisesRegex(ValueError, "cohort"):
                check(root)
            lock.write_text(original)
            contract = root / "contracts/android-runtime-dependencies.json"
            original = contract.read_text()
            data = json.loads(original)
            data["maven_runtime"] = []
            contract.write_text(json.dumps(data))
            with self.assertRaisesRegex(ValueError, "Kotlin runtime"):
                check(root)
            contract.write_text(original)
            rules = root / "platforms/android/library/consumer-rules-common.pro"
            rules.write_text(rules.read_text().replace('-keep class org.rustls.platformverifier.** { *; }', ''))
            with self.assertRaisesRegex(ValueError, "R8"):
                check(root)
