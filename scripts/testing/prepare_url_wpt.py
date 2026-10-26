"""Convert the pinned URL WPT input's strings to WebIDL USVString for Rust.

V8 ops and the external UTF-8 wire replace unpaired UTF-16 surrogates. Python's
JSON decoder retains them; this standard-codec conversion models that boundary
without rewriting the upstream test fixture or implementing another parser.
"""
import argparse
import hashlib
import json
from pathlib import Path


def usv(value):
    if isinstance(value, str):
        return value.encode('utf-16-le', 'surrogatepass').decode('utf-16-le', 'replace')
    if isinstance(value, list):
        return [usv(item) for item in value]
    if isinstance(value, dict):
        return {key: usv(item) for key, item in value.items()}
    return value


def prepare(source, output):
    provenance = json.loads((source.parent/'provenance.json').read_text(encoding='utf-8'))
    for path in [source, source.parent/'LICENSE.md']:
        data = path.read_bytes()
        expected = provenance['files'][path.name]
        if len(data) != expected['bytes'] or hashlib.sha256(data).hexdigest() != expected['sha256']:
            raise ValueError(f'URL WPT fixture differs from pinned provenance: {path.name}')
    original = json.loads(source.read_text(encoding='utf-8'))
    output.write_text(json.dumps(usv(original), ensure_ascii=False), encoding='utf-8')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('source', type=Path)
    parser.add_argument('output', type=Path)
    options = parser.parse_args()
    prepare(options.source, options.output)
