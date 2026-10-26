#!/usr/bin/env python3
"""Replay WAVE library patches, exercise borrowed framing and real PCM oracles.

All source mutations/builds are isolated. Product codec selection is unchanged.
"""
from __future__ import annotations

import argparse
import importlib.util
import json
import os
from pathlib import Path
import re
import shutil

ROOT = Path(__file__).resolve().parents[2]


def module(name: str, path: Path):
    spec = importlib.util.spec_from_file_location(name, path)
    result = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(result)
    return result


cpu = module('cpu_contract', ROOT / 'scripts/testing/check_audio_cpu_admission.py')
codecs = module('codec_candidates', ROOT / 'scripts/bench/audio-codecs.py')
transport = module('transport_contract', ROOT / 'scripts/testing/check_audio_streaming_transport.py')


def build(package: Path, output: Path, env: dict, name: str, library: bool) -> Path:
    args = ['-p', 'symphonia-format-riff', '--no-default-features', '--features', 'streaming'] if library else []
    target = 'symphonia_format_riff' if library else 'migo_wave_streaming_probe'
    if library:
        assert cpu.run(['cargo', 'clean', '--offline', '--locked', '-p', 'symphonia-format-riff'],
                       output / f'{name}-clean.log', package, env) == 0
    log = output / f'{name}-build.jsonl'
    assert cpu.run(['cargo', 'test', '--offline', '--locked', *args, '--lib', '--no-run', '--message-format=json'],
                   log, package, env) == 0, ('compile failure is not a control', log)
    binaries = []
    for line in log.read_text().splitlines():
        try:
            row = json.loads(line)
        except ValueError:
            continue
        if row.get('reason') == 'compiler-artifact' and row.get('executable') and row.get('target', {}).get('name') == target:
            assert not row.get('fresh'), 'local test artifact was not rebuilt'
            binaries.append(Path(row['executable']))
    assert len(binaries) == 1, binaries
    retained = output / f'{name}-tests'
    shutil.copy2(binaries[0], retained)
    return retained


def main(output: Path, target: Path) -> None:
    assert not output.exists(), 'use a fresh result directory'
    output.mkdir(parents=True)
    patches = codecs.library_patches(ROOT, output)
    package = output / 'probe'
    (package / 'src').mkdir(parents=True)
    manifest = '''[package]
name="migo-wave-streaming-probe"
version="0.0.0"
edition="2024"
[dependencies]
symphonia-core="=0.6.1"
symphonia-format-riff={version="=0.6.1",default-features=false,features=["streaming"]}
symphonia-codec-pcm="=0.6.1"
'''
    manifest += '\n[dev-dependencies]\nmigo-alloc-probe={path=' + json.dumps(str(ROOT / 'engine/testing/alloc-probe')) + '}\n'
    workspace = '[workspace]\nresolver="2"\nmembers=["probe","vendor/symphonia-format-riff-0.6.1"]\n[patch.crates-io]\n'
    for name, path in patches.items():
        workspace += f'{name}={{path={json.dumps(str(path))}}}\n'
    (output / 'Cargo.toml').write_text(workspace)
    (package / 'Cargo.toml').write_text(manifest)
    (output / 'Cargo.lock').write_bytes((ROOT / 'engine/Cargo.lock').read_bytes())
    fixture = ROOT / 'scripts/testing/fixtures/audio-codec-probe/wave_pcm_contracts.rs'
    shutil.copyfile(fixture, package / 'src/pcm_contracts.rs')
    (package / 'src/lib.rs').write_text('#[cfg(test)] #[global_allocator] static COUNTING: migo_alloc_probe::CountingAllocator = migo_alloc_probe::CountingAllocator::system();\n#[cfg(test)] mod pcm_contracts;\n')
    inputs = [ROOT / 'engine/Cargo.lock', fixture, Path(__file__).resolve(),
              ROOT / 'scripts/bench/audio-codecs.py', ROOT / 'scripts/testing/check_audio_cpu_admission.py',
              ROOT / 'scripts/testing/check_audio_streaming_transport.py']
    inputs += sorted((ROOT / 'engine/testing/alloc-probe/src').rglob('*.rs'))
    inputs += [ROOT / 'engine/testing/alloc-probe/Cargo.toml']
    for name in patches:
        inputs += sorted((ROOT / f'engine/third_party/{name}-patches').glob('*'))
    checksums = {str(p.relative_to(ROOT)): cpu.sha(p) for p in inputs if p.is_file()}
    (output / 'source-inputs.json').write_text(json.dumps(checksums, indent=2) + '\n')
    for path in inputs:
        if path.is_file():
            destination = output / 'source-inputs' / path.relative_to(ROOT)
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(path, destination)
    env = os.environ.copy()
    env.update(RUSTUP_TOOLCHAIN='1.95.0', CARGO_TARGET_DIR=str(target), CARGO_NET_OFFLINE='true',
               CC='/usr/bin/clang', CXX='/usr/bin/clang++')
    for name in ['ANDROID_NDK', 'ANDROID_NDK_HOME']:
        env.pop(name, None)
    assert cpu.run(['cargo', 'metadata', '--offline', '--format-version', '1'],
                   output / 'metadata.jsonl', package, env) == 0
    # Verify the published PCM decoder too, rather than using a synthetic codec.
    metadata = json.loads(next(line for line in (output / 'metadata.jsonl').read_text().splitlines() if line.startswith('{"packages"')))
    pcm = [p for p in metadata['packages'] if p['name'] == 'symphonia-codec-pcm']
    assert len(pcm) == 1 and pcm[0]['version'] == '0.6.1'
    cargo_home = Path(os.environ.get('CARGO_HOME', str(Path.home() / '.cargo')))
    published = list((cargo_home / 'registry/src').glob('*/symphonia-codec-pcm-0.6.1'))
    assert len(published) == 1 and Path(pcm[0]['manifest_path']).parent == patches['symphonia-codec-pcm']
    inventory = transport.published_source(published[0], cargo_home, pcm[0])
    (output / 'pcm-published-source.json').write_text(json.dumps(inventory, indent=2) + '\n')
    positive = build(package, output, env, 'positive', True)
    assert cpu.run([str(positive)], output / 'positive-test.log', package, env) == 0
    assert '11 passed; 0 failed' in (output / 'positive-test.log').read_text()
    numeric = build(package, output, env, 'pcm-positive', False)
    assert cpu.run([str(numeric), '--nocapture'], output / 'pcm-positive-test.log', package, env) == 0
    assert 'W09_WAVE_PCM 300 numeric/pull/push comparisons' in (output / 'pcm-positive-test.log').read_text()
    controls = [
        ('parent-bound-removed', 'common.rs', 'len - self.consumed < u64::from(chunk_len)', 'false',
         'wave::streaming::tests::declared_misalignment_zero_rate_and_working_set_overflow_are_rejected'),
        ('metadata-step-unbounded', 'wave/streaming.rs', '.min(4096)', '',
         'wave::streaming::tests::large_unused_metadata_is_skipped_in_bounded_steps'),
        ('duration-wrong', 'wave/streaming.rs', 'Duration::from(size as u64 / width)', 'Duration::from(1_u64)',
         'wave::streaming::tests::all_pcm_widths_channels_and_partitions_preserve_borrowed_packets_and_timestamps'),
        ('pcm-waits-for-full-packet', 'wave/streaming.rs', 'let size = input.len().min(target)',
         'if input.len() < target && !eof { return Ok(PacketStep::NeedMore); }\n                let size = input.len().min(target)',
         'wave::streaming::tests::one_complete_frame_can_be_published_before_the_rest_of_the_extent'),
        ('failure-not-sticky', 'wave/streaming.rs', 'self.failed = true;', 'self.failed = false;',
         'wave::streaming::tests::truncated_fields_and_data_fail_normally_and_errors_are_sticky'),
        ('narrow-alignment-overflow', 'wave/chunks.rs',
         'u32::from(num_channels) * u32::from(bits_per_sample / 8)', 'u32::from(num_channels * (bits_per_sample / 8))',
         'wave::streaming::tests::excessive_channel_counts_return_errors_instead_of_panicking'),
        ('borrowed-packet-copied', 'wave/streaming.rs',
         'PacketRef::new(0, self.next_pts, duration, &input[..size])',
         'PacketRef::new(0, self.next_pts, duration, Box::leak(input[..size].to_vec().into_boxed_slice()))',
         'wave::streaming::tests::all_pcm_widths_channels_and_partitions_preserve_borrowed_packets_and_timestamps'),
        ('adpcm-profile-expanded', 'wave/streaming.rs', 'matches!(format.format_data, FormatData::Adpcm(_))', 'false',
         'wave::streaming::tests::a_one_frame_adpcm_block_does_not_expand_the_pcm_profile'),
    ]
    riff = patches['symphonia-format-riff']
    source = riff / 'src/wave/chunks.rs'
    original = source.read_text()
    start = original.index('        let additions = ((1_u64 << channel_diff as u32) - 1) << shift;')
    end = original.index('        channel_mask |= additions;', start)
    controls.append(('channel-mask-shift-overflow', 'wave/chunks.rs', original[start:end],
                     '        let additions = ((1_u32 << channel_diff as u32) - 1) << shift;\n',
                     'wave::chunks::test_channel_mask_boundaries_do_not_overflow_or_truncate'))
    for name, relative, before, after, test in controls:
        source = riff / 'src' / relative
        original = source.read_text()
        assert original.count(before) == 1, (name, 'mutation anchor drift')
        try:
            source.write_text(original.replace(before, after))
            mutant = build(package, output, env, name, True)
            log = output / f'{name}-test.log'
            status = cpu.run([str(mutant), '--exact', test, '--nocapture'], log, package, env, timeout=20)
            assert status == 101 and 'panicked at' in log.read_text(), (name, status, 'must fail an executed assertion')
        finally:
            source.write_text(original)
        print(f'compiled WAVE control {name}: runtime assertion rejected', flush=True)
    pcm_controls = [
        ('pcm-partial-frame-accepted', 'symphonia-codec-pcm', 'src/lib.rs',
         'packet.data.len() % self.bytes_per_coded_frame != 0', 'false',
         'pcm_contracts::pcm_short_packets_and_partial_frames_obey_the_coded_length'),
        ('plane-views-spill-after-eight', 'symphonia-core', 'src/audio/buf.rs',
         'const MAX_STACK_PLANE_SLICES: usize = 32;', 'const MAX_STACK_PLANE_SLICES: usize = 8;',
         'pcm_contracts::pcm_small_packet_decode_and_convert_has_no_steady_rust_allocation'),
    ]
    for name, crate, relative, before, after, test in pcm_controls:
        source = patches[crate] / relative
        original = source.read_text()
        assert original.count(before) == 1
        try:
            source.write_text(original.replace(before, after))
            assert cpu.run(['cargo', 'clean', '--offline', '--locked', '-p', crate],
                           output / f'{name}-clean.log', package, env) == 0
            mutant = build(package, output, env, name, False)
            log = output / f'{name}-test.log'
            status = cpu.run([str(mutant), '--exact', test, '--nocapture'], log, package, env, timeout=20)
            assert status == 101 and 'panicked at' in log.read_text(), (name, status)
        finally:
            source.write_text(original)
        print(f'compiled PCM control {name}: runtime assertion rejected', flush=True)
    # Restore the old EOF-as-control-flow implementation only in the test copy.
    name = 'pcm-short-packet-eof-allocation'
    source = patches['symphonia-codec-pcm'] / 'src/lib.rs'
    original = source.read_text()
    changed = original.replace('buf.render_with(Some($num_frames),', 'buf.render_with(None,')
    assert original.count('buf.render_with(Some($num_frames),') == 4
    changed = changed.replace('        match self.params.codec {', '        let _ = match self.params.codec {', 1)
    changed = changed.replace('            _ => unsupported_error("pcm: codec is unsupported"),\n        }?;',
                              '            _ => unsupported_error("pcm: codec is unsupported"),\n        };', 1)
    try:
        source.write_text(changed)
        assert cpu.run(['cargo', 'clean', '--offline', '--locked', '-p', 'symphonia-codec-pcm'],
                       output / f'{name}-clean.log', package, env) == 0
        mutant = build(package, output, env, name, False)
        log = output / f'{name}-test.log'
        status = cpu.run([str(mutant), '--exact', 'pcm_contracts::pcm_small_packet_decode_and_convert_has_no_steady_rust_allocation', '--nocapture'],
                         log, package, env, timeout=20)
        assert status == 101 and 'panicked at' in log.read_text(), (name, status)
    finally:
        source.write_text(original)
    print(f'compiled PCM control {name}: runtime assertion rejected', flush=True)
    assert all(cpu.sha(ROOT / path) == checksum for path, checksum in checksums.items())
    inventories = json.loads((output / 'library-source-provenance.json').read_text())
    for row in inventories:
        for relative, checksum in row['source_files'].items():
            assert cpu.sha(patches[row['crate']] / relative) == checksum, 'library mutation not restored'
    summary = {'library_positive_tests': 11, 'pcm_numeric_pull_push_comparisons': 300,
               'compiled_controls': len(controls) + len(pcm_controls) + 1, 'positive_binary_sha256': cpu.sha(positive),
               'pcm_binary_sha256': cpu.sha(numeric), 'root_lock_sha256': cpu.sha(ROOT / 'engine/Cargo.lock'),
               'root_inputs_unchanged': True, 'library_sources_restored': True,
               'scope': 'candidate library framing/PCM profile; no HTTP/DSP/player product integration or device/performance acceptance'}
    (output / 'summary.json').write_text(json.dumps(summary, indent=2) + '\n')
    print(json.dumps(summary), flush=True)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--target-dir', type=Path, required=True)
    args = parser.parse_args()
    main(args.output.resolve(), args.target_dir.resolve())
