"""A declared Rust consumer projection of a checksum-verified source archive.

Only V8 has a projection: its native component is built from the separate source
lock. The complete Rust sources/FFI and recorded patches remain byte-verified.
"""
import json
from pathlib import Path


def load(patches, name, version):
    file = patches / 'SOURCE_LAYOUT.json'
    if not file.exists():
        return None
    data = json.loads(file.read_text())
    if (data.get('schema') != 1 or data.get('crate') != name or name != 'v8'
            or data.get('version') != version or data.get('mode') != 'migo-native-component-consumer'
            or data.get('retained_roots') != ['gen', 'src']
            or data.get('authored_files') != ['.gitignore']):
        raise ValueError('invalid V8 component-consumer source layout')
    for name in data['retained_files']:
        path = Path(name)
        if path.is_absolute() or '..' in path.parts:
            raise ValueError('unsafe retained vendor path')
    if len(set(data['retained_files'])) != len(data['retained_files']):
        raise ValueError('duplicate retained vendor path')
    return data


def selected(name, layout):
    return (layout is None or name in layout['retained_files'] or name in layout['authored_files']
            or name.split('/')[0] in layout['retained_roots'])


def directory_needed(name, layout):
    return (layout is None or name.split('/')[0] in layout['retained_roots']
            or any(file.startswith(name + '/') for file in layout['retained_files']))


def validate_archive(layout, inventory):
    """Inventory is relative name -> original uncompressed size."""
    if layout is None:
        return
    if not all(file in inventory for file in layout['retained_files']):
        raise ValueError('retained source input absent from published V8 archive')
    if any(file in inventory for file in layout['authored_files']):
        raise ValueError('authored source-layout file already exists upstream')
    kept = [name for name in inventory if selected(name, layout)]
    omitted = [name for name in inventory if not selected(name, layout)]
    expected = {'retained_files': len(kept), 'omitted_files': len(omitted),
                'omitted_bytes': sum(inventory[name] for name in omitted)}
    if expected != layout['published_inventory']:
        raise ValueError('published V8 projection counts/bytes differ')


def project(inventory, layout):
    return {name: value for name, value in inventory.items() if selected(name, layout)}
