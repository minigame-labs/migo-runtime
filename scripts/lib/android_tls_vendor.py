"""Verify source-derived JNI bytecode selected by the native TLS shim."""
from pathlib import Path
import hashlib
import io
import json
import tomllib
import zipfile
import xml.etree.ElementTree as ET

from android_runtime_deps import ROOT, check_gradle, load
from android_tls_source import patched_source

VENDOR = Path("engine/third_party/rustls-platform-verifier-android-0.2.0")
CLASSES = {
    "org/rustls/platformverifier/CertificateVerifier.class",
    "org/rustls/platformverifier/StatusCode.class",
    "org/rustls/platformverifier/VerificationResult.class",
}


def check(root=ROOT, aar=None):
    vendor = root / VENDOR
    meta = json.loads((vendor / "UPSTREAM.json").read_text())
    if meta.get('schema') != 2:
        raise ValueError('Android TLS source-derived provenance schema differs')
    packages = {p["name"]: p["version"] for p in tomllib.loads((root / "engine/Cargo.lock").read_text())["package"]}
    for name, key in (("rustls-platform-verifier", "verifier_version"), ("rustls-platform-verifier-android", "version")):
        if packages.get(name) != meta[key]:
            raise ValueError(f"{name} Cargo version differs from Android bytecode cohort")
    for name, expected in meta["files"].items():
        if hashlib.sha256((vendor / name).read_bytes()).hexdigest() != expected:
            raise ValueError(f"Android TLS upstream hash mismatch: {name}")
    with zipfile.ZipFile(vendor / "rustls-platform-verifier-0.2.0.aar") as upstream:
        upstream_jar = upstream.read("classes.jar")
    if hashlib.sha256(upstream_jar).hexdigest() != meta['upstream_classes_sha256']:
        raise ValueError('Android TLS pristine upstream classes hash differs')
    declared = meta['patches']
    if [p['path'] for p in declared] != [p.relative_to(vendor).as_posix()
                                        for p in sorted((vendor / 'patches').glob('*.patch'))]:
        raise ValueError('Android TLS ordered patch inventory differs')
    for patch in declared:
        if hashlib.sha256((vendor / patch['path']).read_bytes()).hexdigest() != patch['sha256']:
            raise ValueError('Android TLS source patch hash mismatch')
    # Packaging also runs on Windows; it verifies recorded inputs and bytes
    # without requiring GNU patch or a Kotlin compiler. The separate Linux
    # source gate replays the patches and rebuilds the actual jar in isolation.
    if aar is None:
        source = patched_source(vendor, meta)
        if hashlib.sha256(source).hexdigest() != meta['build']['patched_source_sha256']:
            raise ValueError('Android TLS patched source provenance differs')
    for file, key in [('scripts/rebuild-android-tls-verifier.py', 'builder_sha256'),
                      ('scripts/lib/android_tls_sandbox.py', 'sandbox_sha256'),
                      ('scripts/lib/android_tls_source.py', 'patch_replay_sha256')]:
        if hashlib.sha256((root / file).read_bytes()).hexdigest() != meta['build'][key]:
            raise ValueError('Android TLS builder provenance differs')
    jar = (vendor / 'classes.jar').read_bytes()
    if jar == upstream_jar:
        raise ValueError('Android TLS source-derived helper was replaced by upstream selector')
    with zipfile.ZipFile(io.BytesIO(jar)) as classes:
        if set(classes.namelist()) != CLASSES:
            raise ValueError("Android TLS jar contains unexpected classes/resources")
    notice = vendor / "notice/META-INF/migo-rustls-platform-verifier"
    if (notice / "UPSTREAM.json").read_bytes() != (vendor / "UPSTREAM.json").read_bytes():
        raise ValueError("packaged Android TLS provenance is stale")
    for name in ("LICENSE-MIT", "LICENSE-APACHE"):
        if not (notice / name).is_file() or (notice / name).stat().st_size < 1000:
            raise ValueError(f"packaged Android TLS license missing: {name}")
    dependencies = load(root)
    if VENDOR.joinpath("classes.jar").as_posix() not in dependencies["bundled_jars"]:
        raise ValueError("upstream Android TLS jar is not packaged")
    if not any(d["group"] == "org.jetbrains.kotlin" and d["artifact"] == "kotlin-stdlib"
               for d in dependencies["maven_runtime"]):
        raise ValueError("Android TLS Kotlin runtime dependency is missing")
    check_gradle(root)
    rules = (root / "platforms/android/library/consumer-rules-common.pro").read_text()
    if "-keep class org.rustls.platformverifier.** { *; }" not in rules:
        raise ValueError("upstream Android TLS JNI names are not retained by R8")
    if aar is not None:
        with zipfile.ZipFile(aar) as packaged:
            jars = [packaged.read(name) for name in packaged.namelist() if name.endswith('.jar')]
            def inventory(data):
                with zipfile.ZipFile(io.BytesIO(data)) as classes:
                    names = classes.namelist()
                    if len(names) != len(CLASSES) or set(names) != CLASSES:
                        return None
                    return {name: classes.read(name) for name in names}
            # AGP normalizes ZIP timestamps in local jars. Require identical
            # executable bytes and inventory, rather than incidental ZIP times.
            expected_classes = inventory(jar)
            if sum(inventory(data) == expected_classes for data in jars) != 1:
                raise ValueError('AAR must package the exact source-derived TLS bytecode once')
            manifest = ET.fromstring(packaged.read('AndroidManifest.xml'))
            attribute = '{http://schemas.android.com/apk/res/android}networkSecurityConfig'
            if any(attribute in element.attrib for element in manifest.iter()):
                raise ValueError('AAR must not override the host networkSecurityConfig')
            with zipfile.ZipFile(io.BytesIO(packaged.read('classes.jar'))) as sdk:
                for name in ['UPSTREAM.json', 'LICENSE-MIT', 'LICENSE-APACHE']:
                    entry = 'META-INF/migo-rustls-platform-verifier/' + name
                    if entry not in sdk.namelist() or sdk.read(entry) != (notice / name).read_bytes():
                        raise ValueError(f'AAR TLS license/provenance missing or stale: {name}')


if __name__ == "__main__":
    import argparse
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--aar', type=Path)
    args = parser.parse_args()
    check(aar=args.aar)
    print("Android TLS native/bytecode cohort, source patches/build provenance, packaging, Kotlin runtime and R8 names agree")
