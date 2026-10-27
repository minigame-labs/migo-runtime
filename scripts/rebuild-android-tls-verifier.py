#!/usr/bin/env python3
"""Reproduce the source-patched Android JNI helper without a Gradle plugin.

Linux/bubblewrap, JDK 17, the pinned Kotlin compiler jars, and Android API36
stubs are build-only inputs. --write requires two byte-identical builds before
atomically replacing the jar and recording its provenance. Default: check.
"""
import argparse
import base64
import hashlib
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import zipfile
import urllib.request

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'scripts/lib'))
import android_tls_sandbox as sandbox
from android_tls_vendor import CLASSES, VENDOR
from android_tls_source import patched_source


def digest(data):
    return hashlib.sha256(data).hexdigest()



def deterministic_jar(classes):
    if set(classes) != CLASSES:
        raise ValueError('compiler output has unexpected classes')
    result = io.BytesIO()
    with zipfile.ZipFile(result, 'w', compression=zipfile.ZIP_STORED, allowZip64=False) as jar:
        for name in sorted(classes):
            data = classes[name]
            if not 8 <= len(data) <= 256 * 1024 or data[:4] != b'\xca\xfe\xba\xbe' or data[6:8] != b'\x00\x34':
                raise ValueError(f'invalid Java8 class: {name}')
            entry = zipfile.ZipInfo(name, date_time=(1980, 1, 1, 0, 0, 0))
            entry.create_system = 3
            entry.external_attr = 0o100644 << 16
            jar.writestr(entry, data)
    return result.getvalue()


def compile_jar(source, files, jdk):
    files = {**files, 'CertificateVerifier.kt': source}
    # A trusted collector emits only the expected small class files. The
    # enclosing sandbox retains no target-created files on the host.
    collector = '''import base64,json
from pathlib import Path
root=Path('/scratch/patched')
classes={}
for path in root.rglob('*.class'):
    data=path.read_bytes()
    if len(data)>262144: raise ValueError('class exceeds bound')
    classes[path.relative_to(root).as_posix()]=base64.b64encode(data).decode('ascii')
if len(classes)!=3: raise ValueError('unexpected class inventory')
print('MIGO_CLASSES='+json.dumps(classes,sort_keys=True,separators=(',',':')))
'''
    files['collect.py'] = collector.encode()
    output = sandbox.run(files, sandbox.compiler_script() + '/usr/bin/python3 /inputs/collect.py\n',
                         jdk, output_limit=1024 * 1024)
    marker = b'MIGO_CLASSES='
    prefix, marker, payload = output.rpartition(marker)
    if not marker:
        raise ValueError('compiler produced no bounded class result')
    classes = json.loads(payload)
    if not isinstance(classes, dict) or set(classes) != CLASSES:
        raise ValueError('compiler result has unexpected class inventory')
    return deterministic_jar({name: base64.b64decode(data, validate=True)
                              for name, data in classes.items()})


def replace(path, data):
    fd, name = tempfile.mkstemp(prefix='.migo-tls-', dir=path.parent)
    try:
        with os.fdopen(fd, 'wb') as output:
            output.write(data)
        os.chmod(name, 0o644)
        os.replace(name, path)
    finally:
        Path(name).unlink(missing_ok=True)


def fetch_tools(args, expected):
    # Build-only compiler inputs; no dependency is added to the Android SDK.
    for group, artifact, version, name in sandbox.COMPILER_JARS + [
            ('org.jetbrains.kotlin', 'kotlin-stdlib', '2.4.21', 'kotlin-stdlib.jar')]:
        candidates = list((args.gradle_cache / group / artifact / version).glob('*/*.jar'))
        if candidates:
            continue
        url = ('https://repo.maven.apache.org/maven2/' + group.replace('.', '/')
               + f'/{artifact}/{version}/{artifact}-{version}.jar')
        with urllib.request.urlopen(url, timeout=30) as response:
            data = response.read(128 * 1024 * 1024 + 1)
        if len(data) > 128 * 1024 * 1024 or digest(data) != expected[name]:
            raise ValueError(f'pinned compiler download differs: {name}')
        target = args.gradle_cache / group / artifact / version / expected[name] / f'{artifact}-{version}.jar'
        target.parent.mkdir(parents=True, exist_ok=True)
        replace(target, data)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--write', action='store_true')
    parser.add_argument('--fetch-tools', action='store_true', help='fetch missing hash-pinned build-only jars from Maven Central')
    sandbox.options(parser)
    args = parser.parse_args()
    vendor = ROOT / VENDOR
    metadata = json.loads((vendor / 'UPSTREAM.json').read_text())
    if args.fetch_tools:
        fetch_tools(args, metadata['build']['toolchain_inputs'])
    files = sandbox.inputs(args, True)
    if sandbox.hashes(files) != metadata['build']['toolchain_inputs']:
        raise ValueError('pinned Android TLS compiler/API/runtime inputs differ')
    source = patched_source(vendor, metadata)
    jar = compile_jar(source, files, args.jdk)
    if args.write:
        repeated = compile_jar(source, files, args.jdk)
        if jar != repeated:
            raise ValueError('two isolated Kotlin builds produced different bytecode')
        metadata['files']['classes.jar'] = digest(jar)
        metadata['build']['patched_source_sha256'] = digest(source)
        metadata['build']['builder_sha256'] = digest(Path(__file__).read_bytes())
        metadata['build']['sandbox_sha256'] = digest((ROOT / 'scripts/lib/android_tls_sandbox.py').read_bytes())
        metadata['build']['patch_replay_sha256'] = digest((ROOT / 'scripts/lib/android_tls_source.py').read_bytes())
        encoded = (json.dumps(metadata, indent=2) + '\n').encode()
        replace(vendor / 'classes.jar', jar)
        replace(vendor / 'UPSTREAM.json', encoded)
        replace(vendor / 'notice/META-INF/migo-rustls-platform-verifier/UPSTREAM.json', encoded)
        print('Android TLS helper: two isolated builds agree; derived jar/provenance installed')
    else:
        if jar != (vendor / 'classes.jar').read_bytes():
            raise ValueError('Android TLS shipped jar differs from its source rebuild')
        if metadata['build']['patched_source_sha256'] != digest(source):
            raise ValueError('Android TLS patched source provenance differs')
        print('Android TLS source rebuild matches shipped jar')
    print(f'classes=3 bytes={len(jar)} sha256={digest(jar)}')


if __name__ == '__main__':
    main()
