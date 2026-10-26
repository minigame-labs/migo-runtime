#!/usr/bin/env python3
"""Replay AAC/ADTS library framing, real codec partitions and compiled controls.

A partial final ADTS header used to become a successful EOF, and estimated
bitrate duration was exposed as an exact PCM count. The candidate pull/push
paths now share parsing and signal checks. No product dependency is selected.
"""
from __future__ import annotations

import argparse
import importlib.util
import json
import os
import re
from pathlib import Path
import shutil

ROOT = Path(__file__).resolve().parents[2]


def module(name: str, path: Path):
    spec = importlib.util.spec_from_file_location(name, path)
    result = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(result)
    return result


cpu = module('cpu_contract', ROOT / 'scripts/testing/check_audio_cpu_admission.py')
codecs = module('codec_candidates', ROOT / 'scripts/bench/audio-codecs.py')


def shared_adapter(output: Path, env: dict) -> dict:
    """Compile the real common adapter in debug, including its borrow assertions."""
    work = output / 'shared-adapter'
    work.mkdir()
    codecs.make_package(ROOT, work, env, True, True, streaming_probe=True)
    package = work / 'probe'
    source = work / 'source'
    source.mkdir()
    fixtures = ROOT / 'scripts/testing/fixtures/audio-codec-probe'
    for filename in ['streaming.rs', 'incremental_decoder.rs', 'adts_streaming.rs', 'wave_streaming.rs']:
        shutil.copyfile(fixtures / filename, source / filename)
    main = package / 'src/main.rs'
    main.write_text(main.read_text().replace(str(fixtures / 'streaming.rs'), str(source / 'streaming.rs')))

    def compile(name: str) -> Path:
        codecs.run(['cargo', 'clean', '--offline', '--locked', '-p', 'migo-codec-candidate'],
                   work / f'{name}-clean.log', cwd=package, env=env)
        log = work / f'{name}-build.jsonl'
        codecs.run(['cargo', 'test', '--offline', '--locked', '--bin', 'migo-codec-candidate',
                    '--features', 'simd,metadata-budgets,adts-streaming-probe', '--no-run', '--message-format=json'],
                   log, cwd=package, env=env)
        binaries = []
        for line in log.read_text().splitlines():
            try:
                row = json.loads(line)
            except ValueError:
                continue
            if row.get('reason') == 'compiler-artifact' and row.get('executable') and row.get('profile', {}).get('test'):
                assert not row.get('fresh')
                binaries.append(Path(row['executable']))
        assert len(binaries) == 1
        retained = work / f'{name}-tests'
        shutil.copy2(binaries[0], retained)
        return retained

    positive = compile('positive')
    codecs.run([str(positive), 'adts_adapter_contracts::', '--nocapture', '--test-threads=1'],
               work / 'positive.log', cwd=package, env=env)
    assert '3 passed' in (work / 'positive.log').read_text()
    # ADTS borrows the payload after its header. Reinstating the old equality
    # invariant must fail while all payload bytes still belong to the input.
    common = source / 'incremental_decoder.rs'
    original = common.read_text()
    comment = original.index('// Some encapsulations (ADTS)')
    match = re.search(r'debug_assert!\([\s\S]*?\n\s*\);', original[comment:])
    assert match is not None
    start, end = comment + match.start(), comment + match.end()
    try:
        common.write_text(original[:start] + 'debug_assert_eq!(packet.data.as_ptr(), input.as_ptr());' + original[end:])
        mutant = compile('payload-must-start-at-input')
        log = work / 'payload-must-start-at-input-test.log'
        status = cpu.run([str(mutant), '--exact', 'adts_adapter_contracts::adts_shared_partitions_equal_the_actual_whole_codec', '--nocapture'],
                         log, package, env, timeout=30)
        assert status == 101 and 'panicked at' in log.read_text() and '1 failed' in log.read_text()
    finally:
        common.write_text(original)
    for path, checksum in json.loads((work / 'source-inputs.json').read_text()).items():
        assert codecs.sha(ROOT / path) == checksum
    for row in json.loads((work / 'library-source-provenance.json').read_text()):
        for path, checksum in row['source_files'].items():
            assert codecs.sha(work / 'vendor' / f"{row['crate']}-{row['version']}" / path) == checksum
    assert common.read_text() == original
    print('compiled common ADTS payload control: runtime assertion rejected', flush=True)
    return {'shared_adapter_tests': 3, 'shared_adapter_comparisons': 20,
            'shared_adapter_binary_sha256': codecs.sha(positive)}


def build(output: Path, env: dict, name: str, library: bool) -> Path:
    package = 'symphonia-codec-aac' if library else 'migo-adts-framing-probe'
    # Every mutant must compile, including when restore/copy retains an old mtime.
    for crate in ['symphonia-codec-aac', 'migo-adts-framing-probe']:
        assert cpu.run(['cargo', 'clean', '--offline', '--locked', '-p', crate],
                       output / f'{name}-{crate}-clean.log', output, env) == 0
    log = output / f'{name}-build.jsonl'
    features = ['--features', 'streaming'] if library else []
    assert cpu.run(['cargo', 'test', '--offline', '--locked', '-p', package, '--lib', *features,
                    '--no-run', '--message-format=json'], log, output, env) == 0, ('compile failure is not a control', log)
    binaries = []
    for line in log.read_text().splitlines():
        try:
            row = json.loads(line)
        except ValueError:
            continue
        if row.get('reason') == 'compiler-artifact' and row.get('executable') and row.get('profile', {}).get('test'):
            assert not row.get('fresh'), 'test binary was not rebuilt'
            binaries.append(Path(row['executable']))
    assert len(binaries) == 1, binaries
    retained = output / f'{name}-tests'
    shutil.copy2(binaries[0], retained)
    return retained


def main(output: Path, target: Path) -> None:
    assert not output.exists(), 'use a fresh result directory'
    output.mkdir(parents=True)
    libraries = codecs.library_patches(ROOT, output)
    probe = output / 'probe'
    (probe / 'src').mkdir(parents=True)
    manifest = '[workspace]\nresolver="2"\nmembers=["probe","vendor/symphonia-codec-aac-0.6.1"]\n[patch.crates-io]\n'
    for name, path in libraries.items():
        manifest += f'{name}={{path={json.dumps(str(path))}}}\n'
    (output / 'Cargo.toml').write_text(manifest)
    (probe / 'Cargo.toml').write_text('''[package]
name="migo-adts-framing-probe"
version="0.0.0"
edition="2024"
[dependencies]
symphonia-core="=0.6.1"
symphonia-codec-aac={version="=0.6.1",features=["streaming"]}
[dev-dependencies]
''' + 'migo-alloc-probe={path=' + json.dumps(str(ROOT / 'engine/testing/alloc-probe')) + '}\n')
    fixture = ROOT / 'scripts/testing/fixtures/audio-codec-probe/adts_streaming_contracts.rs'
    shutil.copyfile(fixture, probe / 'src/contracts.rs')
    (probe / 'src/lib.rs').write_text('#[cfg(test)] mod contracts;\n')
    (output / 'Cargo.lock').write_bytes((ROOT / 'engine/Cargo.lock').read_bytes())
    env = os.environ.copy()
    env.update(RUSTUP_TOOLCHAIN='1.95.0', CARGO_TARGET_DIR=str(target), CARGO_NET_OFFLINE='true',
               W09_ADTS_FIXTURES=str(output / 'fixtures'))
    for name in ['ANDROID_NDK', 'ANDROID_NDK_HOME']:
        env.pop(name, None)
    inputs = [Path(__file__).resolve(), fixture, ROOT / 'engine/Cargo.lock',
              ROOT / 'scripts/bench/audio-codecs.py', ROOT / 'scripts/testing/check_audio_cpu_admission.py',
              ROOT / 'engine/testing/alloc-probe/Cargo.toml']
    inputs += sorted((ROOT / 'engine/testing/alloc-probe/src').rglob('*.rs'))
    for name in libraries:
        inputs += sorted((ROOT / f'engine/third_party/{name}-patches').glob('*'))
    checksums = {str(p.relative_to(ROOT)): codecs.sha(p) for p in inputs if p.is_file()}
    (output / 'source-inputs.json').write_text(json.dumps(checksums, indent=2) + '\n')
    for path in inputs:
        if path.is_file():
            destination = output / 'source-inputs' / path.relative_to(ROOT)
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(path, destination)
    fixtures = output / 'fixtures'
    fixtures.mkdir()
    codecs.run(['ffmpeg', '-version'], output / 'ffmpeg-version.txt', cwd=output)
    for i, (rate, channels, frames) in enumerate([(44100, 1, 17), (48000, 2, 9737),
                                                (22050, 1, 9737), (48000, 2, 240137)]):
        source = fixtures / f'{i}.wav'
        codecs.fixture_pcm(source, rate, channels, frames)
        codecs.run(['ffmpeg', '-nostdin', '-v', 'error', '-i', str(source), '-c:a', 'aac',
                    '-profile:a', 'aac_low', '-b:a', '128k', '-f', 'adts', str(fixtures / f'{i}.aac')],
                   output / f'{i}-encode.log', cwd=output)
    (output / 'fixture-sha256.json').write_text(json.dumps({p.name: codecs.sha(p) for p in fixtures.iterdir()}, indent=2) + '\n')
    codecs.run(['rustc', '-Vv'], output / 'rustc-version.txt', cwd=output, env=env)
    codecs.run(['cargo', 'metadata', '--offline', '--format-version', '1'], output / 'metadata.jsonl', cwd=output, env=env)
    records = [json.loads(line) for line in (output / 'metadata.jsonl').read_text().splitlines()
               if line.startswith('{')]
    assert len(records) == 1 and 'packages' in records[0], 'missing Cargo metadata record'
    metadata = records[0]
    for name in ['symphonia-codec-aac', 'symphonia-core', 'symphonia-metadata']:
        selected = [p for p in metadata['packages'] if p['name'] == name]
        assert len(selected) == 1 and selected[0]['source'] is None
        assert Path(selected[0]['manifest_path']).parent == libraries[name]
    positive = build(output, env, 'library-positive', True)
    assert cpu.run([str(positive), '--nocapture', '--test-threads=1'], output / 'library-positive.log', output, env) == 0
    assert '4 passed' in (output / 'library-positive.log').read_text()
    numeric = build(output, env, 'real-positive', False)
    assert cpu.run([str(numeric), '--nocapture', '--test-threads=1'], output / 'real-positive.log', output, env) == 0
    assert '3 passed' in (output / 'real-positive.log').read_text()
    controls = [
        ('packet-heap-copy', 'streaming.rs', '        let pts = self.next_pts;',
         '        std::hint::black_box(input[..len].to_vec());\n        let pts = self.next_pts;', False,
         'contracts::actual_borrowed_aac_decode_and_convert_has_no_steady_rust_allocation'),
        ('crc-payload-offset', 'streaming.rs', '&input[header_len..len]', '&input[7..len]', True,
         'streaming::tests::first_complete_frame_is_borrowed_without_future_input'),
        ('wait-for-next-frame', 'streaming.rs', '        let pts = self.next_pts;',
         '        if input.len() == len && !eof { return Ok(PacketStep::NeedMore); }\n        let pts = self.next_pts;', True,
         'streaming::tests::first_complete_frame_is_borrowed_without_future_input'),
        ('metadata-step-unbounded', 'streaming.rs', '.min(4096)', '', True,
         'streaming::tests::metadata_is_skipped_in_bounded_steps_without_parsing_fake_sync'),
        ('failure-not-sticky', 'streaming.rs', 'if self.failed {', 'if false {', True,
         'streaming::tests::strict_profile_fields_truncation_and_failures_are_terminal'),
        ('timestamp-not-advanced', 'streaming.rs', 'self.next_pts = pts', 'let _next_pts = pts', True,
         'streaming::tests::first_complete_frame_is_borrowed_without_future_input'),
        ('partial-header-eof', 'adts.rs', '&& self.reader.pos() == before_header', '&& true', False,
         'contracts::pull_rejects_partial_headers_after_valid_audio'),
        ('bitrate-estimate-exact', 'adts.rs', '            track.with_duration(Duration::from(num_frames));',
         '            track.with_num_frames(num_frames);\n            track.with_duration(Duration::from(num_frames));', False,
         'contracts::real_aac_lc_partitions_equal_pull_pcm'),
    ]
    aac = libraries['symphonia-codec-aac'] / 'src'
    original = (aac / 'adts.rs').read_text()
    start = original.index('        if self.profile != AudioObjectType::Lc')
    end = original.index(' {\n', start) + 2
    controls.append(('changing-signal-accepted', 'adts.rs', original[start:end], '        if false {', True,
                     'streaming::tests::strict_profile_fields_truncation_and_failures_are_terminal'))
    for name, relative, before, after, library, test in controls:
        source = aac / relative
        original = source.read_text()
        assert original.count(before) == 1, (name, 'mutation anchor drift')
        try:
            source.write_text(original.replace(before, after))
            mutant = build(output, env, name, library)
            log = output / f'{name}-test.log'
            status = cpu.run([str(mutant), '--exact', test, '--nocapture'], log, output, env, timeout=30)
            text = log.read_text()
            assert status == 101 and 'panicked at' in text and '1 failed' in text, (name, status)
        finally:
            source.write_text(original)
        print(f'compiled ADTS control {name}: runtime assertion rejected', flush=True)
    shared = shared_adapter(output, env)
    assert all(codecs.sha(ROOT / p) == checksum for p, checksum in checksums.items())
    for row in json.loads((output / 'library-source-provenance.json').read_text()):
        for relative, checksum in row['source_files'].items():
            assert codecs.sha(libraries[row['crate']] / relative) == checksum, 'library mutation not restored'
    summary = {'library_tests': 4, 'real_codec_tests': 3, 'ffmpeg_encoded_files': 4,
               'pull_push_pcm_comparisons': 20, 'compiled_controls': len(controls) + 1, **shared,
               'library_binary_sha256': codecs.sha(positive), 'real_codec_binary_sha256': codecs.sha(numeric),
               'root_lock_sha256': codecs.sha(ROOT / 'engine/Cargo.lock'), 'root_inputs_unchanged': True,
               'library_sources_restored': True,
               'scope': 'candidate AAC-LC mono/stereo framing/typed codec/shared Migo adapter, host Rust allocation; no HTTP/DSP/player/product/device/performance acceptance'}
    (output / 'summary.json').write_text(json.dumps(summary, indent=2) + '\n')
    print(json.dumps(summary), flush=True)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--target-dir', type=Path, required=True)
    args = parser.parse_args()
    main(args.output.resolve(), args.target_dir.resolve())
