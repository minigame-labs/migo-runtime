#!/usr/bin/env python3
"""Whole-file candidates must compose all existing codec/source/DSP ports.

WAV/Vorbis alone did not cover full AAC/M4A ownership or external library
requests. Reuse the bounded collector and library framing/index/ReadAt APIs;
prove complete PCM, exact presentation, quotas, cancellation and real CPU
handoffs. This gate does not select a product codec or change the Root lock.
"""
from __future__ import annotations
import argparse
import json
from pathlib import Path
import re
import shutil
import struct
import tomllib

import check_audio_whole_file as whole
import check_audio_mp4_index as mp4
import check_audio_mp4_source as source

ROOT = whole.ROOT
NAMES = ['whole_file_m4a', 'whole_file_codecs_contracts', 'isomp4_streaming']


def fixtures(work, env):
    directory = work / 'complete-fixtures'
    mp4.fixtures(directory)
    source.source_fixtures(directory)
    rows = json.loads((directory / 'manifest.json').read_text())
    rows += [dict(file=name, frames=240137, channels=1, rate=48000)
             for name in json.loads((directory / 'source-layouts.json').read_text())['layouts']]
    data = (directory / '1-tail.m4a').read_bytes()
    timeline = mp4.timeline
    (directory / 'edit-raw.m4a').write_bytes(timeline.rewrite(data, {timeline.ELST: struct.pack('>II', 0, 0)}))
    for name, start, frames, version in [('edit-211', 211, 5000, 0), ('edit-1537', 1537, 5000, 0),
                                          ('edit-version-one', 1024, 9737, 1)]:
        file = name + '.m4a'
        (directory / file).write_bytes(timeline.rewrite(data, {timeline.ELST: timeline.edit(frames, start, version=version)}))
        rows.append(dict(file=file, frames=frames, channels=2, rate=44100, slice_start=start))
    assert len(rows) == 19
    (directory / 'complete-m4a.json').write_text(json.dumps(rows, indent=2) + '\n')
    tail = (directory / '2-tail.m4a').read_bytes()
    # Fixture surgery only. Encoded media stays at its original offsets.
    movie = tail.index(b'moov') - 4
    atom = lambda tag, body: struct.pack('>I4s', len(body) + 8, tag) + body
    (directory / 'valid-many.m4a').write_bytes(tail[:movie] + atom(b'free', b'') * 24 + tail[movie:])
    malformed = {
        'bad-many': tail[:movie] + atom(b'free', b'') * 40 + tail[movie:],
        'bad-metadata': tail[:movie] + atom(b'free', bytes(4 * 1024 * 1024)) + tail[movie:],
        'bad-index': tail[:movie] + atom(b'moov', bytes(2 * 1024 * 1024)),
        'bad-header': tail[:movie] + struct.pack('>I4s', 1000000, b'free'),
        'bad-fragment': tail[:movie] + atom(b'moof', b'') + tail[movie:],
        'bad-missing': tail[:movie],
    }
    for name, payload in malformed.items():
        (directory / (name + '.m4a')).write_bytes(payload)
    elementary = []
    for name in ['stereo-cbr', 'mono-vbr']:
        original = whole.pcm.mp3.FIXTURES
        shutil.copyfile(original / (name + '.mp3'), directory / (name + '.mp3'))
        # minimp3's frozen PCM describes the same original source framing.
        channels = 2 if name == 'stereo-cbr' else 1
        elementary.append(dict(file=name + '.mp3', rate=44100 if channels == 2 else 22050, channels=channels,
                               frames=(original / (name + '.f32le')).stat().st_size // (4 * channels)))
    for number, (rate, channels, frames) in enumerate([(32000, 1, 6437), (44100, 2, 9737), (48000, 1, 17)]):
        wav = directory / f'adts-{number}.wav'
        whole.support.codecs.fixture_pcm(wav, rate, channels, frames)
        file = directory / f'adts-{number}.aac'
        whole.support.codecs.run(['ffmpeg', '-nostdin', '-v', 'error', '-i', str(wav), '-c:a', 'aac',
            '-aac_pns', '0', '-f', 'adts', str(file)], work / f'adts-{number}-encode.log', cwd=work)
        pcm = directory / f'adts-{number}.f32le'
        whole.support.codecs.run(['ffmpeg', '-nostdin', '-v', 'error', '-i', str(file), '-f', 'f32le', str(pcm)],
            work / f'adts-{number}-decode.log', cwd=work)
        elementary.append(dict(file=file.name, rate=rate, channels=channels, frames=pcm.stat().st_size // (4 * channels)))
    (directory / 'elementary.json').write_text(json.dumps(elementary, indent=2) + '\n')
    whole.support.codecs.run(['ffmpeg', '-nostdin', '-v', 'error', '-i', str(directory / '1.wav'),
        '-c:a', 'libmp3lame', '-b:a', '128k', str(directory / 'known-gapless.mp3')],
        work / 'known-gapless-encode.log', cwd=work)
    env['W09_COMPLETE_FIXTURES'] = str(directory)
    (work / 'complete-fixture-sha256.json').write_text(json.dumps({str(p.relative_to(directory)): whole.support.codecs.sha(p)
        for p in directory.iterdir() if p.is_file()}, indent=2) + '\n')


def prepare(work, target, rubato):
    package, env, frozen, libraries, resampling = whole.prepare_candidate(work, target, rubato)
    manifest = (package / 'Cargo.toml').read_text()
    manifest = manifest.replace('"mp3","wav"', '"aac","isomp4","mp3","wav"')
    serde_version = next(p['version'] for p in tomllib.loads((ROOT / 'engine/Cargo.lock').read_text())['package'] if p['name'] == 'serde_json')
    manifest = manifest.replace('[dependencies]\n', f'[dependencies]\nserde_json="={serde_version}"\n' + 'symphonia-codec-aac={version="=0.6.1",features=["streaming"]}\n'
        'symphonia-format-isomp4={version="=0.6.1",default-features=false,features=["streaming"]}\n', 1)
    manifest = manifest.replace('default=["wave-streaming-probe"', 'default=["adts-streaming-probe","m4a-streaming-probe","wave-streaming-probe"')
    (package / 'Cargo.toml').write_text(manifest)
    (package / 'Cargo.lock').write_bytes((ROOT / 'engine/Cargo.lock').read_bytes())
    for name in NAMES:
        (work / 'source' / (name + '.rs')).write_bytes((whole.FIXTURES / (name + '.rs')).read_bytes())
    whole.support.codecs.run(['cargo', 'metadata', '--offline', '--format-version', '1'], work / 'complete-metadata.jsonl', cwd=package, env=env)
    locked = tomllib.loads((ROOT / 'engine/Cargo.lock').read_text())['package']
    selected = tomllib.loads((package / 'Cargo.lock').read_text())['package']
    identity = lambda p: (p['name'], p['version'], p.get('source'), p.get('checksum'))
    known = {p['name'] for p in locked}
    pins = set(map(identity, locked))
    assert all(p['name'] not in known or identity(p) in pins for p in selected)
    fixtures(work, env)
    (work / 'retained-sources.json').write_text(json.dumps({str(p.relative_to(work)): whole.support.codecs.sha(p)
        for tree in [package / 'src', work / 'source'] for p in tree.rglob('*') if p.is_file()}, indent=2) + '\n')
    return package, env, frozen, libraries, resampling


def declarations(originals):
    rows = whole.declarations(originals['whole_file.rs'])
    test = 'whole_file_candidate::codecs_tests::'
    matrix = test + 'm4a_jobs_match_exact_presentation_across_front_tail_reordered_and_multi_media'
    elementary = test + 'mp3_and_adts_jobs_match_pull_pcm_independent_counts_and_sinc_tail'
    invalid = test + 'invalid_m4a_sources_fail_with_bounded_work_and_release_all_storage'
    def change(name, path, before, after, selected):
        assert originals[path].count(before) == 1, (name, 'mutation anchor drift')
        rows.append((name, path, before, after, selected))
    change('complete-mp3-target-fixed', 'whole_file.rs', 'Format::Mp3 => Codec::Mp3(Box::new(Resampled::new(target_rate)))', 'Format::Mp3 => Codec::Mp3(Box::new(Resampled::new(44_100)))', elementary)
    change('complete-adts-target-fixed', 'whole_file.rs', 'AdtsResampled::new(\n                target_rate,', 'AdtsResampled::new(\n                44_100,', elementary)
    change('complete-m4a-target-fixed', 'whole_file.rs', 'm4a::File::new(target_rate,', 'm4a::File::new(44_100,', matrix)
    change('complete-m4a-source-index-length-mismatched', 'whole_file_m4a.rs', 'object_len: Some(data.len() as u64)', 'object_len: Some(data.len() as u64 + 1)', matrix)
    change('complete-m4a-source-atom-budget-expanded', 'whole_file_m4a.rs', 'const MAX_ATOMS: u32 = 32;', 'const MAX_ATOMS: u32 = 64;', invalid)
    change('complete-m4a-source-metadata-budget-disabled', 'whole_file_m4a.rs', '*n <= MAX_SKIPPED', 'true', invalid)
    change('complete-m4a-source-index-cap-deferred', 'whole_file_m4a.rs', 'end - start > IndexOptions::default().max_index_bytes as u64', 'false', invalid)
    change('complete-m4a-source-resume-offset-shifted', 'whole_file_m4a.rs', '.resume_at(span, packet.offset)', '.resume_at(span, packet.offset + 1)', matrix)
    change('complete-m4a-source-caller-offset-not-restored', 'whole_file_m4a.rs', '*offset = usize::try_from(packet.offset)', '*offset = usize::try_from(0_u64)', matrix)
    change('complete-m4a-source-scans-drained-without-yield', 'whole_file_m4a.rs', 'self.source = next;\n        Ok(true)',
        'self.source = next;\n        if matches!(self.source, Some(Source::Index(_))) { return self.source_step(data, offset); }\n        Ok(true)',
        test + 'm4a_index_scans_yield_after_each_library_atom_without_copying_the_body')
    return rows


def main(work, target, rubato, preflight):
    package, env, frozen, libraries, resampling = prepare(work, target, rubato)
    originals = {name: (work / 'source' / name).read_text() for name in ['whole_file.rs', 'whole_file_m4a.rs']}
    profiles = {}
    for profile in (['debug'] if preflight else ['release', 'debug']):
        name = 'positive-' + profile
        binary = whole.build_candidate(package, work, env, name, release=profile == 'release')
        log = work / (name + '-tests.log')
        args = [str(binary)] + (['whole_file_candidate::codecs_tests::', '--nocapture'] if preflight else [])
        assert whole.support.clock.common.run(args, log, package, env, timeout=300) == 0, log
        match = re.search(r'test result: ok\. (\d+) passed; 0 failed; (\d+) ignored;', log.read_text())
        assert match
        profiles[profile] = dict(passed=int(match[1]), ignored=int(match[2]))
        assert profiles[profile] == dict(passed=7 if preflight else 427, ignored=0 if preflight else 2), profiles
        whole.support.program(work, name, 'native')
    rows = declarations(originals)
    receipts = whole.support.faults(work, package, env, originals, [] if preflight else rows,
        lambda name: whole.build_candidate(package, work, env, name), work / 'source', 'native')
    whole.verify_inputs(work, frozen)
    assert all(whole.support.codecs.sha(work / p) == h for p, h in json.loads((work / 'retained-sources.json').read_text()).items())
    for row in json.loads((work / 'library-source-provenance.json').read_text()):
        assert all(whole.support.codecs.sha(libraries[row['crate']] / p) == h for p, h in row['source_files'].items())
    for name, row in resampling.items():
        assert all(whole.support.codecs.sha(work / 'resampling-vendor' / f"{name}-{row['version']}" / p) == h for p, h in row['source_files'].items())
    summary = dict(profiles=profiles, preflight=preflight, compiled_controls=len(receipts), controls=receipts,
        root_inputs_unchanged=True, source_copies_restored=True, m4a_job_comparisons=38, elementary_job_comparisons=10,
        product_codec_switched=False, new_job_tests=7, retained_wave_job_comparisons=80,
        retained_vorbis_job_comparisons=12, retained_wave_pcm_comparisons=300,
        library_index_parse_max_bytes=2097152, library_index_parse_preemptible=False)
    (work / 'summary.json').write_text(json.dumps(summary, indent=2) + '\n')
    print(json.dumps({k: v for k, v in summary.items() if k != 'controls'}), flush=True)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--target-dir', type=Path, required=True)
    parser.add_argument('--rubato-cargo-home', type=Path, required=True)
    parser.add_argument('--preflight', action='store_true')
    args = parser.parse_args()
    main(args.output.resolve(), args.target_dir.resolve(), args.rubato_cargo_home.resolve(), args.preflight)
