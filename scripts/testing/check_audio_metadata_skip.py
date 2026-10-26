#!/usr/bin/env python3
"""The probe formerly applied MetadataOptions only to external tags.

Exercise library-owned skip paths for the selected audio containers, actual
allocations/PCM, and compiled fixture failures. No product dependency changes.
"""
from __future__ import annotations

import argparse
import importlib.util
import json
import os
from pathlib import Path
import resource
import shutil
import struct
import subprocess

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location('timeline', ROOT / 'scripts/testing/check_audio_codec_timeline.py')
timeline = importlib.util.module_from_spec(spec)
spec.loader.exec_module(timeline)
codecs = timeline.codecs
FEATURES = 'simd,metadata-budgets,metadata-skip'


def normalize_ogg_fixture(clean: Path, remuxed: Path, output: Path) -> None:
    def pages(data):
        pos = 0
        result = []
        while pos < len(data):
            assert data[pos:pos + 4] == b'OggS'
            count = data[pos + 26]
            body = pos + 27 + count
            end = body + sum(data[pos + 27:body])
            assert end <= len(data)
            result.append((pos, end))
            pos = end
        return result

    original, generated = clean.read_bytes(), remuxed.read_bytes()
    a, b = pages(original)[-1], pages(generated)[-1]
    assert original[a[0] + 5] & 4 and generated[b[0] + 5] & 4
    assert original[a[0] + 26:a[1]] == generated[b[0] + 26:b[1]], 'EOS audio bytes/lacing must match'
    data = bytearray(generated)
    old = struct.unpack_from('<Q', data, b[0] + 6)[0]
    new = struct.unpack_from('<Q', original, a[0] + 6)[0]
    data[b[0] + 6:b[0] + 14] = original[a[0] + 6:a[0] + 14]
    data[b[0] + 22:b[0] + 26] = bytes(4)
    # Fixture-only independent Ogg CRC. Production uses the library parser.
    crc = 0
    for byte in data[b[0]:b[1]]:
        crc ^= byte << 24
        for _ in range(8):
            crc = ((crc << 1) ^ (0x04c11db7 if crc & 0x80000000 else 0)) & 0xffffffff
    struct.pack_into('<I', data, b[0] + 22, crc)
    output.write_bytes(data)
    (output.parent / 'preparation.json').write_text(json.dumps({
        'source_eos_granule': new, 'ffmpeg_remux_eos_granule': old,
        'audio_packet_bytes_and_lacing_identical': True,
        'normalized_to_source_granule': old != new,
        'scope': 'fixture-only metadata remux normalization, not a product timestamp heuristic',
    }, indent=2) + '\n')


def fixtures(output: Path) -> Path:
    directory = output / 'fixtures'
    directory.mkdir()
    wave = directory / 'source.wav'
    codecs.fixture_pcm(wave, 48000, 2, 9737)
    codecs.run(['ffmpeg', '-version'], output / 'ffmpeg-version.txt', cwd=output)
    for args, name in [(['-c:a', 'aac', '-b:a', '128k', '-movie_timescale', '48000'], 'mp4-clean.m4a'),
                       (['-c:a', 'libvorbis'], 'vorbis-clean.ogg')]:
        codecs.run(['ffmpeg', '-nostdin', '-v', 'error', '-i', str(wave), *args, str(directory / name)],
                   output / f'{name}-encode.log', cwd=output)
    mp4 = (directory / 'mp4-clean.m4a').read_bytes()
    assert mp4.index(b'mdat') < mp4.index(b'moov'), 'fixture editing requires media before moov'

    def atom(kind, body):
        return struct.pack('>I4s', len(body) + 8, kind) + body

    meta = atom(b'meta', bytes(4) + atom(b'ilst', atom(b'\xa9nam',
                atom(b'data', struct.pack('>II', 1, 0) + b'x' * (2 * 1024 * 1024)))))
    moov = timeline.payloads(mp4, (b'moov',))[0]
    (directory / 'mp4-nested.m4a').write_bytes(timeline.rewrite(mp4, {(b'moov',): moov + atom(b'udta', meta)}))
    (directory / 'mp4-top.m4a').write_bytes(mp4 + meta)
    (directory / 'comments.txt').write_text(';FFMETADATA1\ntitle=' + 'x' * (2 * 1024 * 1024) + '\n')
    codecs.run(['ffmpeg', '-nostdin', '-v', 'error', '-i', str(directory / 'vorbis-clean.ogg'),
                '-f', 'ffmetadata', '-i', str(directory / 'comments.txt'), '-map', '0:a', '-map_metadata', '1',
                '-c:a', 'copy', str(directory / 'vorbis-remuxed.ogg')], output / 'vorbis-remux.log', cwd=output)
    normalize_ogg_fixture(directory / 'vorbis-clean.ogg', directory / 'vorbis-remuxed.ogg', directory / 'vorbis-large.ogg')
    codecs.run(['ffmpeg', '-nostdin', '-v', 'error', '-i', str(directory / 'vorbis-large.ogg'), '-f', 'null', '-'],
               output / 'normalized-vorbis-oracle.log', cwd=output)
    (output / 'fixture-checksums.json').write_text(json.dumps({p.name: codecs.sha(p) for p in directory.iterdir()}, indent=2) + '\n')
    return directory


def controls(output: Path, env: dict) -> int:
    vendor = output / 'vendor'
    ape = (vendor / 'symphonia-metadata-0.6.1/src/ape.rs').read_text()
    start = ape.index('            if footer.is_header')
    end = ape.index('                return decode_error', start)
    footer_condition = ape[start:end]
    declarations = [
        ('probe-policy-lost', 'symphonia-core', 'src/formats/probe.rs',
         'fmt_opts.metadata_options = meta_opts;', '', 'wave_info_is_retained_by_default_and_omitted_when_disabled'),
        ('id3v1-policy-ignored', 'symphonia-metadata', 'src/id3v1.rs',
         'if self.options.is_disabled() {', 'if false {', 'id3v1_zero_retention_preserves_the_following_media'),
        ('ape-policy-ignored', 'symphonia-metadata', 'src/ape.rs',
         'if !self.options.is_disabled() {', 'if true {', 'ape_zero_retention_skips_before_large_value_allocation'),
        ('ape-footer-bit-reversed', 'symphonia-metadata', 'src/ape.rs',
         'let has_footer = flags & 0x4000_0000 == 0;', 'let has_footer = flags & 0x4000_0000 != 0;',
         'ape_header_only_tag_does_not_consume_following_media'),
        ('ape-item-extent-unbounded', 'symphonia-metadata', 'src/ape.rs',
         'ScopedStream::new(&mut self.reader, item_bytes)', 'ScopedStream::new(&mut self.reader, u64::MAX)',
         'ape_item_cannot_borrow_bytes_from_the_footer_or_media'),
        ('ape-footer-mismatch-accepted', 'symphonia-metadata', 'src/ape.rs',
         footer_condition, '            if false {\n', 'ape_skip_still_checks_footer_identity_and_minimum_extent'),
        ('ape-unparsed-items-accepted', 'symphonia-metadata', 'src/ape.rs',
         'if items.bytes_available() != 0 {', 'if false {',
         'ape_default_rejects_unparsed_items_but_disabled_values_remain_opaque'),
        ('compressed-tag-rejected-before-skip', 'symphonia-metadata', 'src/id3v2/mod.rs',
         'let header = read_id3v2_header(reader)?;',
         'let header = read_id3v2_header(reader)?;\n    if header.is_compressed { return unsupported_error("control: rejected opaque metadata"); }',
         'compressed_id3v22_skip_uses_encoded_extent_without_an_inflater'),
        ('wave-info-parsed-when-disabled', 'symphonia-format-riff', 'src/wave/mod.rs',
         'b"INFO" if !opts.metadata_options.is_disabled()', 'b"INFO" if true',
         'wave_large_optional_info_keeps_pcm_without_parsing_its_body'),
        ('wave-collected-metadata-discarded', 'symphonia-format-riff', 'src/wave/mod.rs',
         '                        metadata,', '                        metadata: Default::default(),',
         'wave_info_is_retained_by_default_and_omitted_when_disabled'),
        ('mp4-policy-not-forwarded', 'symphonia-format-isomp4', 'src/demuxer.rs',
         '.with_metadata_options(opts.metadata_options)', '', 'mp4_nested_and_top_level_metadata_follow_the_same_policy'),
        ('mp4-ilst-parsed-when-disabled', 'symphonia-format-isomp4', 'src/atoms/meta.rs',
         'if it.metadata_options().is_disabled() {', 'if false {', 'mp4_skip_does_not_allocate_large_ilst_values'),
        ('ogg-mapper-policy-lost', 'symphonia-format-ogg', 'src/mappings/mod.rs',
         'mapper.set_metadata_options(metadata_options);', '',
         'ogg_vorbis_zero_retention_preserves_setup_and_decoded_pcm'),
        ('vorbis-comments-decoded-when-disabled', 'symphonia-metadata', 'src/embedded/vorbis.rs',
         'if options.is_disabled() {', 'if false {',
         'embedded_vorbis_skip_checks_lengths_without_value_or_base64_allocation'),
        ('ogg-empty-revisions-retained', 'symphonia-format-ogg', 'src/mappings/vorbis.rs',
         'if self.metadata_options.is_disabled() {', 'if false {',
         'ogg_vorbis_zero_retention_preserves_setup_and_decoded_pcm'),
    ]
    for name, crate, relative, before, _, _ in declarations:
        assert (vendor / f'{crate}-0.6.1' / relative).read_text().count(before) == 1, (name, 'mutation anchor changed')
    for name, crate, relative, before, after, test in declarations:
        source = vendor / f'{crate}-0.6.1' / relative
        original = source.read_text()
        source.write_text(original.replace(before, after))
        work = output / name
        work.mkdir()
        try:
            binary = timeline.executable_tests(output / 'probe', work, env, features=FEATURES)
            log = work / 'observed-red.log'
            with log.open('w') as stream:
                status = subprocess.run([str(binary), '--exact', f'metadata_skip_contracts::{test}', '--nocapture'],
                                        env=env, stdout=stream, stderr=subprocess.STDOUT,
                                        timeout=30, preexec_fn=worker_limits).returncode
            text = log.read_text()
            assert status == 101 and '1 failed' in text, (name, status, 'expected an executed fixture failure')
            assert f"panicked at {output / 'source/metadata_skip_contracts.rs'}:" in text, (name, 'failure outside contract fixture')
        finally:
            source.write_text(original)
        print(f'actual metadata-policy mutation {name}: fixture contract rejected', flush=True)
    return len(declarations)


def worker_limits():
    resource.setrlimit(resource.RLIMIT_AS, (512 * 1024 * 1024, 512 * 1024 * 1024))
    resource.setrlimit(resource.RLIMIT_CORE, (0, 0))
    resource.setrlimit(resource.RLIMIT_CPU, (5, 5))


def main(output: Path, target: Path) -> None:
    assert not output.exists(), 'use a fresh result directory'
    output.mkdir(parents=True)
    env = os.environ.copy()
    env.update(RUSTUP_TOOLCHAIN='1.95.0', CARGO_TARGET_DIR=str(target), CARGO_NET_OFFLINE='true')
    original_lock = (ROOT / 'engine/Cargo.lock').read_bytes()
    binary = codecs.make_package(ROOT, output, env, True, True)
    inputs = fixtures(output)
    env['W09_METADATA_SKIP_FIXTURES'] = str(inputs)
    manifest = output / 'probe/Cargo.toml'
    manifest.write_text(manifest.read_text().replace('metadata-budgets=[]', 'metadata-budgets=[]\nmetadata-skip=[]')
        .replace('[dev-dependencies]', '[dev-dependencies]\nsymphonia-metadata={version="=0.6.1",default-features=false,features=["ape","id3v1","id3v2","vorbis"]}\nsymphonia-format-riff={version="=0.6.1",default-features=false,features=["wav"]}'))
    source = output / 'source'
    source.mkdir()
    for name in ['metadata_contracts', 'metadata_skip_contracts']:
        shutil.copyfile(ROOT / f'scripts/testing/fixtures/audio-codec-probe/{name}.rs', source / f'{name}.rs')
        with (output / 'probe/src/main.rs').open('a') as entry:
            entry.write(f'\n#[cfg(test)] #[path={json.dumps(str(source / (name + ".rs")))}] mod {name};\n')
    shutil.copyfile(Path(__file__), output / 'validation-driver.py')
    codecs.run(['cargo', 'metadata', '--offline', '--format-version', '1'], output / 'test-resolution.jsonl', cwd=output / 'probe', env=env)
    positive = output / 'positive'
    positive.mkdir()
    tests = timeline.executable_tests(output / 'probe', positive, env, features=FEATURES)
    codecs.run([str(tests), 'metadata_', '--nocapture', '--test-threads=1'], output / 'tests.log', cwd=output, env=env)
    assert '35 passed; 0 failed; 1 ignored' in (output / 'tests.log').read_text()
    test_hash = codecs.sha(tests)
    negative = controls(output, env)
    proof = output / 'verification'
    proof.mkdir()
    timeline.provenance(output, proof)
    assert codecs.sha(tests) == test_hash
    assert (ROOT / 'engine/Cargo.lock').read_bytes() == original_lock
    frozen = json.loads((output / 'source-inputs.json').read_text())
    assert all(codecs.sha(ROOT / path) == value for path, value in frozen.items())
    assert all(codecs.sha(inputs / name) == value for name, value in json.loads((output / 'fixture-checksums.json').read_text()).items())
    summary = {'tests_passed': 35, 'prior_id3_tests': 17, 'new_metadata_policy_tests': 18,
               'explicit_fuzz_entry_ignored': 1, 'compiled_negative_controls': negative,
               'test_binary_sha256': test_hash, 'probe_sha256': codecs.sha(binary),
               'patched_libraries': len(json.loads((output / 'library-source-provenance.json').read_text())), 'root_lock_unchanged': True, 'source_mutations_restored': True,
               'scope': 'Selected WAV/Ogg-Vorbis/M4A and external ID3/APE zero-retention paths; nonzero-retention limits outside ID3 remain separate. No product codec/DSP cutover or device/performance acceptance.'}
    (output / 'summary.json').write_text(json.dumps(summary, indent=2) + '\n')
    print(json.dumps(summary, indent=2), flush=True)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--target-dir', type=Path, required=True)
    args = parser.parse_args()
    main(args.output.resolve(), args.target_dir.resolve())
