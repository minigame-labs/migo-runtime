"""Replay only the declared, fixed Android TLS source patches."""
import hashlib
from pathlib import Path
import subprocess
import tempfile


def digest(data):
    return hashlib.sha256(data).hexdigest()


def patched_source(vendor, metadata):
    original = (vendor / 'CertificateVerifier.kt').read_bytes()
    if digest(original) != metadata['files']['CertificateVerifier.kt']:
        raise ValueError('upstream Kotlin source hash mismatch')
    declared = metadata['patches']
    if [p['path'] for p in declared] != [p.relative_to(vendor).as_posix()
                                        for p in sorted((vendor / 'patches').glob('*.patch'))]:
        raise ValueError('Android TLS ordered patch inventory differs')
    with tempfile.TemporaryDirectory(prefix='migo-tls-patch-') as temporary:
        source = Path(temporary) / 'CertificateVerifier.kt'
        source.write_bytes(original)
        for patch in declared:
            data = (vendor / patch['path']).read_bytes()
            if digest(data) != patch['sha256']:
                raise ValueError('Android TLS source patch hash mismatch')
            for line in data.splitlines():
                if line.startswith((b'--- ', b'+++ ')) and line not in (
                        b'--- a/CertificateVerifier.kt', b'+++ b/CertificateVerifier.kt'):
                    raise ValueError('Android TLS patch must only modify its Kotlin source')
            subprocess.run(['patch', '--batch', '--silent', '--forward', '--fuzz=0', '-p1'],
                           input=data, cwd=temporary, check=True, capture_output=True)
        return source.read_bytes()
