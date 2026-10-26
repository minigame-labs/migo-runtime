#!/usr/bin/env python3
"""Replay borrowed Ogg/Vorbis framing with real codecs and compiled controls.

The old logical reader owned a new encoded payload per packet and assembled
unused comments. Reuse its parser/timeline while proving bounded borrowed and
cross-page paths. Product codec selection is deliberately separate.
"""
from __future__ import annotations
import argparse
import importlib.util
import json
import os
from pathlib import Path
import shutil
import struct

ROOT = Path(__file__).resolve().parents[2]


def module(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    result = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(result)
    return result


cpu = module('cpu', ROOT / 'scripts/testing/check_audio_cpu_admission.py')
codecs = module('codecs', ROOT / 'scripts/bench/audio-codecs.py')
metadata = module('metadata_skip', ROOT / 'scripts/testing/check_audio_metadata_skip.py')


def crc(data):
    value = 0
    for i, byte in enumerate(data):
        value ^= (0 if 22 <= i < 26 else byte) << 24
        for _ in range(8):
            value = ((value << 1) ^ (0x04c11db7 if value & 0x80000000 else 0)) & 0xffffffff
    return value


def fixture_page(header, lacing, payload, flags, sequence, granule):
    h = bytearray(header[:27])
    h[5], h[26] = flags, len(lacing)
    struct.pack_into('<Q', h, 6, granule)
    struct.pack_into('<I', h, 18, sequence)
    result = h + bytes(lacing) + payload
    struct.pack_into('<I', result, 22, crc(result))
    return result

def fixtures(output):
    dest = output / 'fixtures'
    dest.mkdir()
    codecs.run(['ffmpeg', '-version'], output / 'ffmpeg-version.txt', cwd=output)
    for i, (rate, channels, frames) in enumerate([(44100, 1, 17), (48000, 2, 9737),
                                                (22050, 1, 9737), (48000, 6, 240137)]):
        source = dest / f'{i}.wav'
        codecs.fixture_pcm(source, rate, channels, frames)
        codecs.run(['ffmpeg', '-nostdin', '-v', 'error', '-i', str(source), '-c:a', 'libvorbis',
                    '-q:a', '4', str(dest / f'{i}.ogg')], output / f'{i}-encode.log', cwd=output)
    shutil.copyfile(dest / '1.ogg', dest / 'clean.ogg')
    (dest / 'comments.txt').write_text(';FFMETADATA1\ntitle=' + 'x' * (2 * 1024 * 1024) + '\n')
    codecs.run(['ffmpeg', '-nostdin', '-v', 'error', '-i', str(dest / 'clean.ogg'), '-f', 'ffmetadata',
                '-i', str(dest / 'comments.txt'), '-map', '0:a', '-map_metadata', '1', '-c:a', 'copy',
                str(dest / 'remuxed.ogg')], output / 'metadata-remux.log', cwd=output)
    metadata.normalize_ogg_fixture(dest / 'clean.ogg', dest / 'remuxed.ogg', dest / 'large.ogg')
    original = (dest / '3.ogg').read_bytes()
    position, shift, split = 0, 0, False
    cross = bytearray()


    while position < len(original):
        assert original[position:position + 4] == b'OggS'
        n = original[position + 26]
        end = position + 27 + n + sum(original[position + 27:position + 27 + n])
        p = original[position:end]
        lacing, payload = list(p[27:27 + n]), p[27 + n:]
        sequence, granule = struct.unpack_from('<I', p, 18)[0], struct.unpack_from('<Q', p, 6)[0]
        if not split and not p[5] & 3 and lacing and lacing[0] == 255:
            cross += fixture_page(p, [255], payload[:255], 0, sequence, 2**64 - 1)
            cross += fixture_page(p, lacing[1:], payload[255:], p[5] | 1, sequence + 1, granule)
            shift, split = 1, True
        else:
            cross += fixture_page(p, lacing, payload, p[5], sequence + shift, granule)
        position = end
    assert split, 'real fixture needs a cross-page audio packet'
    (dest / 'cross.ogg').write_bytes(cross)
    # Valid silent long-block packets: mode 1, long neighbours, unused floors.
    # Put priming on its own page so the EOS granule has an unambiguous anchor.
    codecs.run(['ffmpeg', '-nostdin', '-v', 'error', '-f', 'lavfi', '-i', 'anullsrc=r=48000:cl=7.1',
                '-frames:a', '3', '-c:a', 'libvorbis', '-q:a', '4', str(dest / 'silent8-template.ogg')],
               output / 'silent8-encode.log', cwd=output)
    template = (dest / 'silent8-template.ogg').read_bytes()
    pages, pos = [], 0
    while pos < len(template):
        n = template[pos + 26]
        end = pos + 27 + n + sum(template[pos + 27:pos + 27 + n])
        pages.append(template[pos:end])
        pos = end
    assert len(pages) == 3
    ident = bytearray(pages[0])
    start = 27 + ident[26]
    assert ident[start:start + 7] == b'\x01vorbis' and ident[start + 11] == 8
    ident[start + 28] = (13 << 4) | (ident[start + 28] & 15)
    struct.pack_into('<I', ident, 22, crc(ident))
    max_window = (ident + pages[1] + fixture_page(pages[2], [2], bytes([0x0e, 0]), 0, 2, 0)
                  + fixture_page(pages[2], [2, 2], bytes([0x0e, 0]) * 2, 4, 3, 8192))
    (dest / 'max-window-8.ogg').write_bytes(max_window)
    codecs.run(['ffmpeg', '-nostdin', '-v', 'error', '-i', str(dest / 'max-window-8.ogg'),
                '-f', 'f32le', '-c:a', 'pcm_f32le', str(dest / 'max-window-8.f32le')],
               output / 'max-window-8-oracle.log', cwd=output)
    oracle = codecs.floats(dest / 'max-window-8.f32le')
    assert len(oracle) == 8192 * 8 and all(value == 0 for value in oracle)
    codecs.run(['ffmpeg', '-nostdin', '-v', 'error', '-i', str(dest / 'cross.ogg'), '-f', 'null', '-'],
               output / 'cross-ffmpeg-oracle.log', cwd=output)
    (output / 'fixture-sha256.json').write_text(json.dumps({p.name: codecs.sha(p) for p in dest.iterdir()}, indent=2) + '\n')


def silent_stream_fixture(output):
    dest = output / 'fixtures'
    codecs.run(['ffmpeg', '-nostdin', '-v', 'error', '-f', 'lavfi', '-i', 'anullsrc=r=8000:cl=mono',
                '-frames:a', '3', '-c:a', 'libvorbis', '-q:a', '4', str(dest / 'silent-mono-template.ogg')],
               output / 'silent-mono-encode.log', cwd=output)
    template = (dest / 'silent-mono-template.ogg').read_bytes()
    pages, position = [], 0
    while position < len(template):
        n = template[position + 26]
        end = position + 27 + n + sum(template[position + 27:position + 27 + n])
        pages.append(template[position:end])
        position = end
    assert len(pages) == 3
    payload = 27 + pages[0][26]
    assert pages[0][payload:payload + 7] == b'\x01vorbis' and pages[0][payload + 11] == 1
    quantum = 1 << ((pages[0][payload + 28] >> 4) - 1)
    file = (pages[0] + pages[1] + fixture_page(pages[2], [1], bytes([0x0e]), 0, 2, 0)
            + fixture_page(pages[2], [1, 1], bytes([0x0e]) * 2, 4, 3, 2 * quantum))
    (dest / 'silence-8000.ogg').write_bytes(file)
    codecs.run(['ffmpeg', '-nostdin', '-v', 'error', '-i', str(dest / 'silence-8000.ogg'),
                '-f', 'f32le', '-c:a', 'pcm_f32le', str(dest / 'silence-8000.f32le')],
               output / 'silent-mono-oracle.log', cwd=output)
    oracle = codecs.floats(dest / 'silence-8000.f32le')
    assert len(oracle) == 2 * quantum and all(value == 0 for value in oracle)
    (output / 'fixture-sha256.json').write_text(json.dumps({p.name: codecs.sha(p) for p in dest.iterdir()}, indent=2) + '\n')


def build(output, env, name, library):
    package = 'symphonia-format-ogg' if library else 'migo-ogg-streaming-probe'
    codecs.run(['cargo', 'clean', '--offline', '--locked', '-p', 'symphonia-format-ogg', '-p', 'migo-ogg-streaming-probe'],
               output / f'{name}-clean.log', cwd=output, env=env)
    log = output / f'{name}-build.jsonl'
    features = ['--features', 'streaming'] if library else []
    codecs.run(['cargo', 'test', '--offline', '--locked', '-p', package, '--lib', *features, '--no-run', '--message-format=json'],
               log, cwd=output, env=env)
    binaries = []
    for line in log.read_text().splitlines():
        try:
            row = json.loads(line)
        except ValueError:
            continue
        if row.get('reason') == 'compiler-artifact' and row.get('executable') and row.get('profile', {}).get('test'):
            assert not row.get('fresh'), 'test artifact was not rebuilt'
            binaries.append(Path(row['executable']))
    assert len(binaries) == 1
    retained = output / f'{name}-tests'
    shutil.copy2(binaries[0], retained)
    return retained


def shared_adapter(output, env):
    work = output / 'shared-adapter'
    work.mkdir()
    mp3 = module('mp3_stream', ROOT / 'scripts/testing/check_audio_mp3_streaming.py')
    env = env.copy()
    env['W09_STREAM_FIXTURES'] = str(mp3.fixtures(work))
    adts = work / 'adts-fixtures'
    adts.mkdir()
    env['W09_ADTS_FIXTURES'] = str(adts)
    for i, (rate, channels, frames) in enumerate([(44100, 1, 17), (48000, 2, 9737),
                                                (22050, 1, 9737), (48000, 2, 240137)]):
        source = adts / f'{i}.wav'
        codecs.fixture_pcm(source, rate, channels, frames)
        codecs.run(['ffmpeg', '-nostdin', '-v', 'error', '-i', str(source), '-c:a', 'aac',
                    '-profile:a', 'aac_low', '-f', 'adts', str(adts / f'{i}.aac')],
                   work / f'adts-{i}-encode.log', cwd=work)
    codecs.make_package(ROOT, work, env, True, True, streaming_probe=True)
    package = work / 'probe'
    source = work / 'source'
    source.mkdir()
    fixtures = ROOT / 'scripts/testing/fixtures/audio-codec-probe'
    names = ['streaming.rs', 'incremental_decoder.rs', 'vorbis_streaming.rs', 'adts_streaming.rs', 'wave_streaming.rs']
    for name in names:
        shutil.copyfile(fixtures / name, source / name)
    main = package / 'src/main.rs'
    main.write_text(main.read_text().replace(str(fixtures / 'streaming.rs'), str(source / 'streaming.rs')))
    initial = {p.name: codecs.sha(p) for p in source.glob('*.rs')}

    def compile(name):
        codecs.run(['cargo', 'clean', '--offline', '--locked', '-p', 'migo-codec-candidate'], work / f'{name}-clean.log', cwd=package, env=env)
        log = work / f'{name}-build.jsonl'
        codecs.run(['cargo', 'test', '--offline', '--locked', '--bin', 'migo-codec-candidate', '--no-run',
                    '--features', 'simd,metadata-budgets,wave-streaming-probe,adts-streaming-probe,vorbis-streaming-probe',
                    '--message-format=json'], log, cwd=package, env=env)
        bins = []
        for line in log.read_text().splitlines():
            try:
                row = json.loads(line)
            except ValueError:
                continue
            if row.get('reason') == 'compiler-artifact' and row.get('executable') and row.get('profile', {}).get('test'):
                assert not row.get('fresh')
                bins.append(Path(row['executable']))
        assert len(bins) == 1
        retained = work / f'{name}-tests'
        shutil.copy2(bins[0], retained)
        return retained

    positive = compile('positive')
    # libtest uses substring filters: the shared suffix selects MP3 and WAVE.
    groups = [('vorbis_adapter_contracts::', 4), ('streaming_contracts::', 16),
              ('adts_adapter_contracts::', 3)]
    for label, count in groups:
        log = work / f'{label[:-2]}-positive.log'
        codecs.run([str(positive), label, '--nocapture', '--test-threads=1'], log, cwd=package, env=env)
        assert f'{count} passed' in log.read_text()
    controls = [
        ('vorbis-budget-too-small', 'vorbis_streaming.rs', 'const OUTPUT_SAMPLES_PER_STEP: usize = 32 * 1024;',
         'const OUTPUT_SAMPLES_PER_STEP: usize = incremental::SAMPLES_PER_STEP;',
         'maximum_legal_vorbis_window_and_eight_channels_fit_publication_scratch'),
        ('zero-consumed-packet-rejected', 'incremental_decoder.rs',
         'PacketStep::BufferedPacket { consumed } => {\n                    if consumed > input.len()',
         'PacketStep::BufferedPacket { consumed } => {\n                    if consumed == 0 || consumed > input.len()',
         'vorbis_shared_partitions_match_whole_codec_and_source_lengths'),
        ('buffered-packet-missing', 'vorbis_streaming.rs', 'self.0.packet(input)', 'None',
         'vorbis_shared_partitions_match_whole_codec_and_source_lengths'),
        ('buffered-packet-copied', 'vorbis_streaming.rs', 'self.0.packet(input)',
         '{let packet = self.0.packet(input)?; std::hint::black_box(packet.data.to_vec()); Some(packet)}',
         'vorbis_common_steps_have_no_steady_rust_allocation'),
    ]
    for name, relative, before, after, test in controls:
        path = source / relative
        original = path.read_text()
        assert original.count(before) == 1, (name, 'mutation anchor drift')
        try:
            path.write_text(original.replace(before, after))
            binary = compile(name)
            log = work / f'{name}-test.log'
            status = cpu.run([str(binary), '--exact', 'vorbis_adapter_contracts::' + test, '--nocapture'], log, package, env, timeout=60)
            text = log.read_text()
            assert status == 101 and 'panicked at' in text and '1 failed' in text, (name, status)
        finally:
            path.write_text(original)
        print(f'compiled common Vorbis control {name}: runtime assertion rejected', flush=True)
    assert all(codecs.sha(source / p) == digest for p, digest in initial.items())
    for p, digest in json.loads((work / 'source-inputs.json').read_text()).items():
        assert codecs.sha(ROOT / p) == digest
    return {'shared_adapter_tests': 4, 'common_regression_tests': 19, 'shared_adapter_comparisons': 35,
            'shared_binary_sha256': codecs.sha(positive), 'shared_compiled_controls': len(controls)}


def main(output, target):
    assert not output.exists(), 'use a fresh result directory'
    output.mkdir(parents=True)
    libraries = codecs.library_patches(ROOT, output)
    probe = output / 'probe'
    (probe / 'src').mkdir(parents=True)
    workspace = '[workspace]\nresolver="2"\nmembers=["probe","vendor/symphonia-format-ogg-0.6.1"]\n[patch.crates-io]\n'
    for name, path in libraries.items():
        workspace += f'{name}={{path={json.dumps(str(path))}}}\n'
    (output / 'Cargo.toml').write_text(workspace)
    (probe / 'Cargo.toml').write_text('''[package]
name="migo-ogg-streaming-probe"
version="0.0.0"
edition="2024"
[dependencies]
symphonia-core="=0.6.1"
symphonia-format-ogg={version="=0.6.1",features=["streaming"]}
symphonia-codec-vorbis="=0.6.1"
[dev-dependencies]
''' + 'migo-alloc-probe={path=' + json.dumps(str(ROOT / 'engine/testing/alloc-probe')) + '}\n')
    fixture = ROOT / 'scripts/testing/fixtures/audio-codec-probe/ogg_streaming_contracts.rs'
    shutil.copyfile(fixture, probe / 'src/contracts.rs')
    (probe / 'src/lib.rs').write_text('#[cfg(test)] mod contracts;\n')
    (output / 'Cargo.lock').write_bytes((ROOT / 'engine/Cargo.lock').read_bytes())
    inputs = [Path(__file__).resolve(), fixture, ROOT / 'engine/Cargo.lock', ROOT / 'scripts/bench/audio-codecs.py',
              ROOT / 'scripts/testing/check_audio_cpu_admission.py', ROOT / 'scripts/testing/check_audio_metadata_skip.py',
              ROOT / 'scripts/testing/check_audio_codec_timeline.py', ROOT / 'scripts/testing/check_audio_mp3_streaming.py', ROOT / 'engine/testing/alloc-probe/Cargo.toml']
    inputs += sorted((ROOT / 'engine/testing/alloc-probe/src').rglob('*.rs'))
    for name in libraries:
        inputs += sorted((ROOT / f'engine/third_party/{name}-patches').glob('*'))
    checksums = {str(p.relative_to(ROOT)): codecs.sha(p) for p in inputs if p.is_file()}
    (output / 'source-inputs.json').write_text(json.dumps(checksums, indent=2) + '\n')
    for path in inputs:
        if path.is_file():
            retained = output / 'source-inputs' / path.relative_to(ROOT)
            retained.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(path, retained)
    fixtures(output)
    env = os.environ.copy()
    env.update(RUSTUP_TOOLCHAIN='1.95.0', CARGO_TARGET_DIR=str(target), CARGO_NET_OFFLINE='true', W09_OGG_FIXTURES=str(output / 'fixtures'))
    for name in ['ANDROID_NDK', 'ANDROID_NDK_HOME']:
        env.pop(name, None)
    codecs.run(['rustc', '-Vv'], output / 'rustc-version.txt', cwd=output, env=env)
    codecs.run(['cargo', 'metadata', '--offline', '--format-version', '1'], output / 'metadata.jsonl', cwd=output, env=env)
    rows = [json.loads(line) for line in (output / 'metadata.jsonl').read_text().splitlines() if line.startswith('{')]
    assert len(rows) == 1
    for name in ['symphonia-format-ogg', 'symphonia-core', 'symphonia-metadata']:
        selected = [p for p in rows[0]['packages'] if p['name'] == name]
        assert len(selected) == 1 and selected[0]['source'] is None and Path(selected[0]['manifest_path']).parent == libraries[name]
    positive = build(output, env, 'library-positive', True)
    codecs.run([str(positive), '--nocapture', '--test-threads=1'], output / 'library-positive.log', cwd=output, env=env)
    assert '4 passed' in (output / 'library-positive.log').read_text()
    numeric = build(output, env, 'codec-positive', False)
    codecs.run([str(numeric), '--nocapture', '--test-threads=1'], output / 'codec-positive.log', cwd=output, env=env)
    assert '6 passed' in (output / 'codec-positive.log').read_text()
    controls = [
        ('crc-not-checked', 'page.rs', 'if header.crc != crc.crc()', 'if false', True, 'streaming::tests::truncated_corrupt_reserved_and_invalid_pages_fail_stickily'),
        ('partial-input-mutates-state', 'page.rs', '    if input.len() < OGG_PAGE_HEADER_SIZE {', '    packet_lens.clear();\n    if input.len() < OGG_PAGE_HEADER_SIZE {', True, 'streaming::tests::partial_input_is_transactional_and_never_touches_missing_fields'),
        ('page-body-copied', 'page.rs', 'page_buf: &input[table_end..end]', 'page_buf: Box::leak(input[table_end..end].to_vec().into_boxed_slice())', True, 'streaming::tests::complete_page_is_borrowed_and_matches_existing_pull_boundaries'),
        ('logical-failure-not-sticky', 'streaming/vorbis.rs', 'if self.failed {', 'if false {', False, 'contracts::logical_eos_sequence_serial_continuation_and_granules_are_strict'),
        ('complete-packet-quota-removed', 'streaming/vorbis.rs', 'if range.len() > limit', 'if false', False, 'contracts::complete_packets_obey_setup_and_audio_working_set_quotas'),
        ('missing-eos-accepted', 'streaming/vorbis.rs', 'decode_error("ogg: missing final Vorbis page")', 'Ok(PacketStep::End)', False, 'contracts::logical_eos_sequence_serial_continuation_and_granules_are_strict'),
        ('sequence-gap-accepted', 'streaming/vorbis.rs', 'if header.sequence != self.sequence.wrapping_add(1)', 'if false', False, 'contracts::logical_eos_sequence_serial_continuation_and_granules_are_strict'),
        ('end-trim-removed', 'logical.rs', 'packet.trim_end = page_end_ts.abs_delta(next_pkt_pts).min(packet.dur);', 'packet.trim_end = Duration::ZERO;', False, 'contracts::real_vorbis_partitions_equal_existing_pull_pcm'),
        ('pts-not-advanced', 'logical.rs', 'packet.pts = next_pkt_pts;', 'packet.pts = Timestamp::ZERO;', False, 'contracts::real_vorbis_partitions_equal_existing_pull_pcm'),
        ('metadata-prefix-not-skipped', 'mappings/vorbis.rs', 'self.metadata_options.is_disabled() && packet.len() >= 7', 'false && packet.len() >= 7', False, 'contracts::large_discarded_comment_has_the_same_rust_allocation_as_clean_audio'),
    ]
    for name, relative, before, after, library, test in controls:
        source = libraries['symphonia-format-ogg'] / 'src' / relative
        original = source.read_text()
        assert original.count(before) == 1, (name, 'mutation anchor changed')
        try:
            source.write_text(original.replace(before, after))
            binary = build(output, env, name, library)
            log = output / f'{name}-test.log'
            status = cpu.run([str(binary), '--exact', test, '--nocapture'], log, output, env, timeout=60)
            text = log.read_text()
            assert status == 101 and 'panicked at' in text and '1 failed' in text, (name, status)
        finally:
            source.write_text(original)
        print(f'compiled Ogg control {name}: runtime assertion rejected', flush=True)
    # Prove the standard EOF classification and legal codec consumers together.
    core = libraries['symphonia-core'] / 'src/io/bit.rs'
    original = core.read_text()
    assert original.count('Err(io::Error::from(io::ErrorKind::UnexpectedEof))') == 1
    try:
        core.write_text(original.replace('Err(io::Error::from(io::ErrorKind::UnexpectedEof))', 'Err(io::Error::new(io::ErrorKind::UnexpectedEof, "unexpected end of bitstream"))'))
        codecs.run(['cargo', 'clean', '--offline', '--locked', '-p', 'symphonia-core'], output / 'allocated-bit-eof-clean.log', cwd=output, env=env)
        binary = build(output, env, 'allocated-bit-eof', False)
        log = output / 'allocated-bit-eof-test.log'
        status = cpu.run([str(binary), '--exact', 'contracts::bitstream_eof_is_precise_and_has_no_rust_allocation', '--nocapture'], log, output, env)
        assert status == 101 and 'panicked at' in log.read_text() and '1 failed' in log.read_text()
    finally:
        core.write_text(original)
    print('compiled bit EOF control: runtime assertion rejected', flush=True)
    shared = shared_adapter(output, env)
    assert all(codecs.sha(ROOT / p) == digest for p, digest in checksums.items())
    for row in json.loads((output / 'library-source-provenance.json').read_text()):
        for name, digest in row['source_files'].items():
            assert codecs.sha(libraries[row['crate']] / name) == digest
    summary = {'library_tests': 4, 'real_codec_tests': 6, 'pull_push_pcm_comparisons': 30, 'compiled_controls': len(controls) + 1 + shared['shared_compiled_controls'], **shared,
               'library_binary_sha256': codecs.sha(positive), 'codec_binary_sha256': codecs.sha(numeric),
               'root_lock_sha256': codecs.sha(ROOT / 'engine/Cargo.lock'), 'root_inputs_unchanged': True, 'library_sources_restored': True,
               'scope': 'candidate borrowed Ogg/Vorbis library and real typed codec; Migo shared adapter and four-format regression covered; HTTP/DSP/player/product/device/performance acceptance pending'}
    (output / 'summary.json').write_text(json.dumps(summary, indent=2) + '\n')
    print(json.dumps(summary), flush=True)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--target-dir', type=Path, required=True)
    args = parser.parse_args()
    main(args.output.resolve(), args.target_dir.resolve())
