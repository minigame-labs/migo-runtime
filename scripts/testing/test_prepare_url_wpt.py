import json
from pathlib import Path
import shutil
import tempfile
import unittest

from prepare_url_wpt import prepare, usv

FIXTURE = Path(__file__).resolve().parents[2]/'engine/crates/runtime-v8/testdata/url-wpt'


class UrlWptPreparation(unittest.TestCase):
    def test_usv_replaces_lone_surrogates_and_keeps_valid_pairs(self):
        self.assertEqual(usv(['a\ud800b', '\udc00', '\ud83d\ude00', '😀']), ['a�b', '�', '😀', '😀'])

    def test_real_pinned_corpus_remains_complete(self):
        with tempfile.TemporaryDirectory(prefix='migo-url-wpt-') as temp:
            output = Path(temp)/'prepared.json'
            prepare(FIXTURE/'urltestdata.json', output)
            rows = json.loads(output.read_text(encoding='utf-8'))
            self.assertEqual(sum(isinstance(row, dict) for row in rows), 896)

    def test_fixture_and_license_drift_are_rejected_before_conversion(self):
        for damaged in ['urltestdata.json', 'LICENSE.md']:
            with self.subTest(damaged=damaged), tempfile.TemporaryDirectory(prefix='migo-url-wpt-') as temp:
                root = Path(temp)
                for name in ['urltestdata.json', 'LICENSE.md', 'provenance.json']:
                    shutil.copyfile(FIXTURE/name, root/name)
                path = root/damaged
                path.write_bytes(path.read_bytes()+b'\n')
                with self.assertRaisesRegex(ValueError, 'differs from pinned provenance'):
                    prepare(root/'urltestdata.json', root/'prepared.json')
                self.assertFalse((root/'prepared.json').exists())


if __name__ == '__main__':
    unittest.main()
