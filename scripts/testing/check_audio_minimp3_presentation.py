#!/usr/bin/env python3
"""The product MP3 decoder retains encoder delay/padding; replacing it loses free-format.

Before product selection, check a checksum-pinned additive minimp3_ex bridge:
shared mature trim helpers, single bounded native calls, no index allocation,
complete frozen PCM and runtime rejection controls. This gate does not migrate
production decoding, prove scheduling cancellation, or rank performance.
"""
from __future__ import annotations
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import tarfile
import tomllib

ROOT = Path(__file__).resolve().parents[2]
PATCHES = ROOT / 'engine/third_party/minimp3-sys-patches'
FIXTURES = ROOT / 'scripts/testing/fixtures/minimp3-presentation'


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def run(command, log, cwd, env, expected=0):
    with log.open('w') as stream:
        result = subprocess.run(command, cwd=cwd, env=env, stdout=stream,
                                stderr=subprocess.STDOUT, timeout=300)
    assert result.returncode == expected, (result.returncode, log)
    return log.read_text()


def prepare(work, target, sanitize, plain):
    work.mkdir(parents=True)
    name, version, checksum = (PATCHES / 'UPSTREAM').read_text().split()
    assert (name, version) == ('minimp3-sys', '0.6.0')
    locked = tomllib.loads((ROOT / 'engine/Cargo.lock').read_text())['package']
    selected = [p for p in locked if p['name'] == name]
    assert len(selected) == 1 and selected[0]['version'] == version and selected[0].get('checksum', checksum) == checksum
    cache = Path(os.environ.get('CARGO_HOME', str(Path.home() / '.cargo'))) / 'registry/cache'
    archives = sorted(cache.glob(f'*/{name}-{version}.crate'))
    assert archives and sha(archives[0]) == checksum
    with tarfile.open(archives[0]) as stream:
        stream.extractall(work / 'vendor', filter='data')
    original_ex_sha = sha(work / 'vendor' / f'{name}-{version}' / 'minimp3/minimp3_ex.h')
    for patch in sorted(PATCHES.glob('*.patch')):
        run(['patch', '--batch', '--forward', '--fuzz=0', '-p0', '-i', str(patch)],
            work / (patch.stem + '.log'), work / 'vendor', os.environ.copy())
    vendor = work / 'vendor' / f'{name}-{version}'
    provenance = dict(archive_sha256=checksum, patches={p.name: sha(p) for p in PATCHES.glob('*.patch')},
        patched_files={str(p.relative_to(vendor)): sha(p) for p in vendor.rglob('*') if p.is_file()})
    product = ROOT / 'engine/third_party/minimp3-sys-0.6.0'
    if product.is_dir():
        assert {str(p.relative_to(product)): sha(p) for p in product.rglob('*') if p.is_file()} == provenance['patched_files']
    (work / 'library-source-provenance.json').write_text(json.dumps(provenance, indent=2) + '\n')
    package = work / 'probe'
    (package / 'src').mkdir(parents=True)
    (package / 'fixtures').mkdir()
    for p in FIXTURES.iterdir():
        if p.suffix in ('.mp3', '.i16le'):
            shutil.copy2(p, package / 'fixtures' / p.name)
    for name in ['stereo-cbr', 'mono-vbr']:
        shutil.copy2(ROOT / f'engine/crates/audio/src/decoder/mp3/fixtures/{name}.mp3',
                     package / f'fixtures/{name}.mp3')
    for name in ['music.mp3', 'music.i16le']:
        shutil.copy2(ROOT / 'engine/crates/audio/src/decoder/mp3/fixtures' / name, package / 'fixtures' / name)
    proof = json.loads((FIXTURES / 'sources.json').read_text())
    assert proof['upstream_crate_sha256'] == checksum
    assert proof['original_ex_header_sha256'] == original_ex_sha
    assert all(sha(ROOT / name) == value for name, value in proof['files'].items())
    shutil.copy2(FIXTURES / 'contracts.rs', package / 'src/contracts.rs')
    (package / 'src/lib.rs').write_text('#[cfg(test)]\nmod contracts;\n')
    (package / 'Cargo.toml').write_text(f'''[package]
name="minimp3-presentation-probe"
version="0.0.0"
edition="2024"
[workspace]
[features]
native-trace=[]
[dependencies]
minimp3-sys={{path={json.dumps(str(vendor))}}}
[profile.release]
opt-level=3
lto=false
codegen-units=16
panic="unwind"
''')
    shutil.copy2(ROOT / 'engine/Cargo.lock', package / 'Cargo.lock')
    env = os.environ.copy()
    env.update(RUSTUP_TOOLCHAIN='1.95.0', CARGO_TARGET_DIR=str(target), CARGO_NET_OFFLINE='true',
        CC='/usr/bin/clang', CXX='/usr/bin/clang++', CC_ENABLE_DEBUG_OUTPUT='1', CFLAGS='-DMIGO_MINIMP3_TEST -finstrument-functions')
    if plain:
        env['CFLAGS'] = ''
    for key in ['RUSTFLAGS', 'CARGO_ENCODED_RUSTFLAGS', 'CARGO_BUILD_TARGET']:
        env.pop(key, None)
    if sanitize:
        env['CFLAGS'] += ' -fsanitize=address,undefined -fno-omit-frame-pointer'
        env['RUSTFLAGS'] = '-C linker=/usr/bin/clang -C link-arg=-fsanitize=address -C link-arg=-fsanitize=undefined'
        env['ASAN_OPTIONS'] = 'detect_leaks=1:halt_on_error=1'
        env['UBSAN_OPTIONS'] = 'halt_on_error=1:print_stacktrace=1'
    run(['cargo', 'metadata', '--offline', '--format-version', '1'], work / 'metadata.jsonl', package, env)
    identities = lambda path: {(p['name'], p['version'], p.get('source'), p.get('checksum'))
        for p in tomllib.loads(path.read_text())['package']}
    metadata = next(json.loads(line) for line in (work / 'metadata.jsonl').read_text().splitlines()
        if line.startswith('{"packages":'))
    compiler_sources = []
    for package_row in metadata['packages']:
        if not package_row['source']:
            continue
        base = Path(package_row['manifest_path']).parent
        files = {str(p.relative_to(base)): sha(p) for p in base.rglob('*') if p.is_file()}
        compiler_sources.append(dict(name=package_row['name'], version=package_row['version'],
            source=str(base), files=files))
        for name in files:
            dest = work / 'compiler-source-inputs' / base.name / name
            dest.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(base / name, dest)
    assert {r['name'] for r in compiler_sources} == {'cc', 'shlex', 'find-msvc-tools'}
    (work / 'compiler-source-inputs.json').write_text(json.dumps(compiler_sources, indent=2) + '\n')
    before = identities(ROOT / 'engine/Cargo.lock')
    after = identities(package / 'Cargo.lock')
    allowed = {('minimp3-presentation-probe', '0.0.0', None, None),
        ('minimp3-sys', '0.6.0', None, None)} - before
    assert after - before == allowed, 'unrelated dependency identity drift'
    inputs = {Path(__file__), ROOT / '.gitignore', ROOT / 'engine/Cargo.toml', ROOT / 'engine/Cargo.lock',
        ROOT / 'scripts/testing/check_gitignore_contract.py'}
    inputs.update(p for folder in [PATCHES, FIXTURES, ROOT / 'engine/crates/audio/src/decoder/mp3']
        for p in folder.rglob('*') if p.is_file())
    inputs.add(ROOT / 'engine/crates/audio/src/decoder/mp3.rs')
    inputs.update(p for p in product.rglob('*') if p.is_file())
    frozen = {str(p.relative_to(ROOT)): sha(p) for p in sorted(inputs)}
    for name in frozen:
        dest = work / 'source-inputs' / name
        dest.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(ROOT / name, dest)
    (work / 'source-inputs.json').write_text(json.dumps(frozen, indent=2) + '\n')
    return package, vendor, env, frozen


def build(package, vendor, work, env, label, release=False):
    # Native dependency and Rust harness must both compile for every retained program.
    run(['cargo', 'clean', '--offline', '--locked', '-p', 'minimp3-sys'],
        work / (label + '-clean.log'), package, env)
    path = package / 'src/lib.rs'
    path.write_text(path.read_text())
    command = ['cargo', 'test', '--offline', '--locked', '--no-run', '--message-format=json']
    if env['CFLAGS']:
        command.extend(['--features', 'native-trace'])
    if release:
        command.append('--release')
    log = work / (label + '-build.jsonl')
    text = run(command, log, package, env)
    rows = []
    for line in text.splitlines():
        try:
            row = json.loads(line)
        except ValueError:
            continue
        if row.get('reason') == 'compiler-artifact':
            rows.append(row)
    native = [r for r in rows if r['target']['name'] == 'minimp3_sys']
    tests = [r for r in rows if r.get('executable') and r['target']['name'] == 'minimp3_presentation_probe']
    assert len(native) == len(tests) == 1 and native[0]['fresh'] is False and tests[0]['fresh'] is False
    binary = work / (label + '-tests')
    shutil.copy2(tests[0]['executable'], binary)
    return binary


def controls(originals):
    rows = []
    def change(name, path, before, after, test):
        assert originals[path].count(before) == 1, (name, 'mutation anchor drift')
        rows.append((name, path, before, after, 'contracts::' + test))
    pcm = 'complete_gapless_and_untagged_pcm_matches_frozen_upstream_output'
    meta = 'metadata_skips_only_tag_and_rejects_caller_and_global_pcm_limits'
    c = 'minimp3.c'
    h = 'minimp3/minimp3_ex.h'
    change('tag-frame-not-skipped', c, 'return p->state.start_offset == (uint64_t)size;', 'return 0;', pcm)
    change('delay-ignored', c, '    *expected = p->state.detected_samples;',
           '    p->state.to_skip = 0;\n    *expected = p->state.detected_samples;', pcm)
    change('padding-clamp-ignored', h, 'out_samples = dec->detected_samples - dec->cur_sample;',
           'out_samples = out_samples;', pcm)
    change('metadata-count-zeroed', c, '*expected = p->state.detected_samples;', '*expected = 0;', meta)
    change('caller-pcm-cap-ignored', c, 'MINIMP3_MIN(limit, UINT64_C(16777216))', 'UINT64_C(16777216)', meta)
    change('global-pcm-cap-ignored', c, 'MINIMP3_MIN(limit, UINT64_C(16777216))', 'limit', meta)
    change('partial-pcm-published', c,
           '(p->state.detected_samples && p->state.cur_sample != p->state.detected_samples)',
           '0', 'known_presentation_cannot_finish_with_partial_pcm')
    change('native-view-cap-ignored', c, 'size > 8192 || ', '', 'native_input_limits_invalid_order_and_owner_retirement')
    change('metadata-can-reinitialize', c, '!p || p->initialized || ', '!p || ', meta)
    change('metadata-allocates', c, '    *expected = p->state.detected_samples;',
           '    void *extra = malloc(1); free(extra);\n    *expected = p->state.detected_samples;', pcm)
    change('codec-called-twice', c, '    int samples = mp3dec_decode_frame(decoder, input, size, pcm, info);',
           '    mp3dec_decode_frame(decoder, input, size, NULL, info);\n    int samples = mp3dec_decode_frame(decoder, input, size, pcm, info);', pcm)
    change('native-error-can-resume', c, '        state->last_error = MP3D_E_DECODE;',
           '        state->last_error = 0;', 'format_change_retires_native_presentation_and_cannot_resume')
    change('native-owner-leaks', c, 'void migo_mp3_presentation_free(migo_mp3_presentation *p) { free(p); }',
           'void migo_mp3_presentation_free(migo_mp3_presentation *p) { (void)p; }',
           'native_input_limits_invalid_order_and_owner_retirement')
    return rows


def main(work, target, preflight, sanitize, plain):
    assert not (plain and sanitize)
    package, vendor, env, frozen = prepare(work, target, sanitize, plain)
    originals = {name: (vendor / name).read_text() for name in ['minimp3.c', 'minimp3/minimp3_ex.h']}
    rows = controls(originals)
    programs, positive, rejected = [], {}, []
    for release in ([True] if plain else [False] if preflight or sanitize else [False, True]):
        label = 'positive-' + ('release' if release else 'debug')
        binary = build(package, vendor, work, env, label, release)
        log = work / (label + '-tests.log')
        observed = run([str(binary), '--test-threads=1', '--nocapture'], log, package, env)
        count = 8 if plain else 9
        assert f'test result: ok. {count} passed; 0 failed; 0 ignored;' in observed
        programs.append(dict(name=label, sha256=sha(binary), positive=True, fresh_native=True, fresh_rust=True))
        positive[label] = dict(passed=count, ignored=0)
    if not preflight and not sanitize and not plain:
        for name, path, before, after, test in rows:
            try:
                (vendor / path).write_text(originals[path].replace(before, after))
                binary = build(package, vendor, work, env, name)
                log = work / (name + '-test.log')
                observed = run([str(binary), '--test-threads=1', '--exact', test, '--nocapture'], log, package, env, 101)
                assert 'panicked at' in observed and '1 failed' in observed and 'running 1 test' in observed
                programs.append(dict(name=name, sha256=sha(binary), positive=False, fresh_native=True, fresh_rust=True))
                rejected.append(dict(name=name, test=test, exit=101, binary_sha256=sha(binary)))
                binary.unlink()
                print('runtime assertion rejected:', name, flush=True)
            finally:
                (vendor / path).write_text(originals[path])
    assert all(sha(ROOT / path) == value and sha(work / 'source-inputs' / path) == value for path, value in frozen.items())
    compiler_sources = json.loads((work / 'compiler-source-inputs.json').read_text())
    for row in compiler_sources:
        base = Path(row['source'])
        assert all(sha(base / name) == value and
            sha(work / 'compiler-source-inputs' / base.name / name) == value
            for name, value in row['files'].items())
    assert all((vendor / path).read_text() == value for path, value in originals.items())
    provenance = json.loads((work / 'library-source-provenance.json').read_text())
    assert all(sha(vendor / path) == value for path, value in provenance['patched_files'].items())
    assert (package / 'src/contracts.rs').read_bytes() == (FIXTURES / 'contracts.rs').read_bytes()
    (work / 'executed-programs.json').write_text(json.dumps(programs, indent=2) + '\n')
    summary = dict(positive=positive, controls=rejected, root_inputs=len(frozen), root_inputs_unchanged=True,
        compiler_sources=sum(len(r['files']) for r in compiler_sources),
        source_copies_restored=True, sanitizer=sanitize, plain=plain, preflight=preflight, product_selected=(ROOT / 'engine/third_party/minimp3-sys-0.6.0').is_dir(),
        scope='isolated native presentation bridge; no production cursor/scheduler or performance acceptance')
    (work / 'summary.json').write_text(json.dumps(summary, indent=2) + '\n')
    print(json.dumps(summary, indent=2), flush=True)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--target-dir', type=Path, required=True)
    parser.add_argument('--preflight', action='store_true')
    parser.add_argument('--sanitize', action='store_true')
    parser.add_argument('--plain', action='store_true', help='release native ABI/PCM without test instrumentation')
    args = parser.parse_args()
    main(args.output.resolve(), args.target_dir.resolve(), args.preflight, args.sanitize, args.plain)
