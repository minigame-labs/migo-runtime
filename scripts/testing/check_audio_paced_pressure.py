#!/usr/bin/env python3
"""Entered-job barriers did not establish ordinary paced-player coexistence.

Drive the production InnerAudioPlayer at 128-frame host-clock intervals while
ordinary whole-file queues remain busy. Use a 30-second loopback MP3 and an idle
control for each topology. Host wake jitter, player starvation and background
completion are distinct observations, never device-deadline acceptance.
"""
from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import re

import check_audio_real_stream_contention as shared_gate

ROOT = shared_gate.ROOT
PRODUCT = shared_gate.product
FIXTURE = ROOT / 'scripts/testing/fixtures/audio-codec-probe/streaming_paced_pressure.rs'
TEST = shared_gate.PREFIX + 'paced::sample_clock_consumption_preserves_pcm_under_unbarriered_pressure'
SCENARIOS = [(1, 1, 1, 48000, 'wave'), (1, 2, 2, 44100, 'convert'),
             (6, 1, 2, 48000, 'wave'), (6, 2, 2, 44100, 'convert')]


def declarations():
    return [
        ('clock-sleep-bypassed', 'fixture', '        if now < deadline {', '        if false {',
         'consumer ran ahead of sample clock', 'MIGO_PACED_PLAYING'),
        ('background-not-started', 'fixture', '    let mut load = if pressure {', '    let mut load = if false {',
         'background work did not progress', 'MIGO_PACED_PLAYING'),
        ('player-render-muted', 'inner_audio.rs', 'let volume = self.shared.volume();', 'let volume = 0.;',
         'silent render reference', None),
        ('whole-pcm-muted', 'decode_pool.rs', 'let audio = self.job.take().unwrap().finish()?;',
         'let mut audio = self.job.take().unwrap().finish()?; audio.samples.fill(0.);',
         'whole PCM changed', 'MIGO_PACED_PLAYING'),
    ]


def main(work: Path, target: Path):
    common = PRODUCT.whole.support.clock.common
    extras = PRODUCT.whole.extra_inputs() + [Path(__file__), FIXTURE, shared_gate.FIXTURE,
             Path(shared_gate.__file__), ROOT / 'scripts/testing/check_gitignore_contract.py']
    package, env, frozen = PRODUCT.whole.support.clock.prepare(work, target, extras)
    encoded = work / 'source-30s.mp3'
    source = ROOT / 'engine/crates/audio/src/decoder/mp3/fixtures/music.mp3'
    command = ['ffmpeg', '-v', 'error', '-nostdin', '-stream_loop', '2', '-i', str(source),
               '-t', '30', '-ar', '48000', '-c:a', 'libmp3lame', '-q:a', '2', str(encoded)]
    assert common.run(command, work / 'input-generation.log', package, env) == 0
    probe = work / 'input-probe.json'
    assert common.run(['ffprobe', '-v', 'error', '-show_entries', 'stream=sample_rate,channels,duration',
                       '-of', 'json', str(encoded)], probe, package, env) == 0
    metadata = json.loads(probe.read_text())['streams']
    assert len(metadata) == 1 and (metadata[0]['sample_rate'], metadata[0]['channels']) == ('48000', 2)
    assert 30 <= float(metadata[0]['duration']) < 31
    assert common.run(['ffmpeg', '-version'], work / 'ffmpeg-version.log', package, env) == 0
    (work / 'input-generation.json').write_text(json.dumps(dict(command=command, source_sha256=PRODUCT.whole.support.codecs.sha(source),
        encoded_sha256=PRODUCT.whole.support.codecs.sha(encoded), metadata=metadata), indent=2) + '\n')
    fixture = work / FIXTURE.name
    fixture.write_bytes(FIXTURE.read_bytes())
    helper = work / shared_gate.FIXTURE.name
    helper_text = shared_gate.FIXTURE.read_text()
    begin = helper_text.index('const MUSIC: &[u8] = include_bytes!(')
    end = helper_text.index('));', begin) + 3
    helper_text = helper_text[:begin] + 'const MUSIC: &[u8] = include_bytes!(' + json.dumps(str(encoded)) + ');' + helper_text[end:]
    helper_text += '\n#[path=' + json.dumps(str(fixture)) + ']\nmod paced;\n'
    helper.write_text(helper_text)
    addition = '\n#[cfg(test)]\n#[path=' + json.dumps(str(helper)) + ']\nmod real_stream_probe;\n'
    owner = package / 'src/decode_pool.rs'
    owner.write_text(owner.read_text() + addition)
    (work / 'module-addition.txt').write_text(addition)
    (work / 'retained-helper.txt').write_text(helper_text)
    files = {'fixture': fixture, 'decode_pool.rs': owner, 'inner_audio.rs': package / 'src/inner_audio.rs'}
    originals = {name: path.read_text() for name, path in files.items()}
    for name, path, before, *_ in declarations():
        assert originals[path].count(before) == 1, (name, 'anchor drift')
    programs, positives, controls = [], [], []
    binaries = {}
    for profile in ['release', 'debug']:
        name = 'positive-' + profile
        binary = PRODUCT.src.build(package, work, env, name, profile == 'release')
        binaries[profile] = binary
        programs.append(dict(name=name, path=str(binary), sha256=PRODUCT.whole.support.codecs.sha(binary), fresh_rust=True, pruned=False))
    shared_gate.fixture_clippy(package, work, env, fixture)
    cpus = sorted(os.sched_getaffinity(0))
    assert len(cpus) >= 6
    for profile in ['release', 'debug']:
        for index, (count, hosts, streams, rate, kind) in enumerate(SCENARIOS):
            order = [False, True] if (index + (profile == 'debug')) % 2 == 0 else [True, False]
            for pressure in order:
                label = f'{profile}-{count}cpu-{hosts}host-{streams}stream-{rate}-{kind}-load{int(pressure)}'
                log, metrics = work / (label + '.log'), work / (label + '-time.json')
                run_env = env | dict(MIGO_PACED_RATE=str(rate), MIGO_PACED_HOSTS=str(hosts), MIGO_PACED_STREAMS=str(streams),
                                     MIGO_PACED_KIND=kind, MIGO_PACED_PRESSURE=str(int(pressure)))
                args = ['/usr/bin/time', '-f', '{"user_seconds":%U,"system_seconds":%S,"peak_rss_kib":%M}', '-o', str(metrics),
                        'taskset', '-c', ','.join(map(str, cpus[:count])), str(binaries[profile]), '--exact', TEST, '--nocapture']
                assert common.run(args, log, package, run_env, timeout=100) == 0, log
                text = log.read_text()
                assert '1 passed; 0 failed' in text and 'MIGO_PACED_PLAYING' in text, log
                match = re.search(r'MIGO_PACED (\{.*\})', text)
                assert match, log
                row = json.loads(match[1])
                assert (row['rate'], row['hosts'], row['streams'], row['pressure'], row['io_workers'], row['background_kind']) == (rate, hosts, streams, pressure, max(2, count), kind)
                assert row['complete_mixed_pcm_bits_checked'] and row['media_frames'] >= 29 * rate
                assert row['elapsed_ns'] >= row['media_frames'] * 1_000_000_000 // rate
                assert row['foreground_requests'] > 0
                assert not pressure or (row['background_completed'] >= 2 and row['background_progress_windows'] >= 2 and row['coexist_observations'] >= 2)
                row.update(case=label, profile=profile, cpu_count=count, process_metrics=json.loads(metrics.read_text()))
                positives.append(row)
                (work / 'completed-positive-cases.json').write_text(json.dumps(positives, indent=2) + '\n')
                print('paced consumer verified:', label, 'missing frames', row['player_missing_frames'], flush=True)
    for name, path, before, after, expected, marker in declarations():
        try:
            files[path].write_text(originals[path].replace(before, after))
            binary = PRODUCT.src.build(package, work, env, name, True)
            log = work / (name + '-test.log')
            args = ['taskset', '-c', ','.join(map(str, cpus[:6])), str(binary), '--exact', TEST, '--nocapture']
            run_env = env | dict(MIGO_PACED_RATE='48000', MIGO_PACED_HOSTS='2', MIGO_PACED_STREAMS='2', MIGO_PACED_KIND='wave', MIGO_PACED_PRESSURE='1')
            status = common.run(args, log, package, run_env, timeout=100)
            text = log.read_text()
            assert status == 101 and all(value in text for value in ['running 1 test', 'panicked at', '1 failed', expected]), (name, status, log)
            assert marker is None or marker in text, log
            assert 'PermissionDenied' not in text and 'Operation not permitted' not in text, log
            digest = PRODUCT.whole.support.codecs.sha(binary)
            controls.append(dict(name=name, test=TEST, exit=status, expected_assertion=expected, real_work_marker=marker, binary_sha256=digest))
            programs.append(dict(name=name, path=str(binary), sha256=digest, fresh_rust=True, pruned=True))
            binary.unlink()
            (work / 'completed-controls.json').write_text(json.dumps(controls, indent=2) + '\n')
            print('fresh paced assertion rejected:', name, flush=True)
        finally:
            files[path].write_text(originals[path])
    PRODUCT.whole.verify_inputs(work, frozen)
    root = ROOT / 'engine/crates/audio/src'
    for path in root.rglob('*'):
        if path.is_file():
            expected = path.read_bytes() + (addition.encode() if path.name == 'decode_pool.rs' else b'')
            assert (package / 'src' / path.relative_to(root)).read_bytes() == expected, path
    assert fixture.read_bytes() == FIXTURE.read_bytes() and helper.read_text() == helper_text
    assert PRODUCT.whole.support.codecs.sha(encoded) == json.loads((work / 'input-generation.json').read_text())['encoded_sha256']
    result = dict(positive_cases=positives, positive_processes=len(positives), compiled_controls=len(controls), controls=controls,
                  programs=programs, root_inputs_unchanged=True, source_copies_restored=True, actual_production_player=True,
                  callback_frames=128, no_cpu_entry_barriers=True, no_source_gates_during_playback=True, complete_mixed_pcm_bits_checked=True,
                  host_timer_observation_not_device_deadline=True, fixed_work_cpu_speedup_claim=False, device_or_shipping_acceptance=False)
    (work / 'summary.json').write_text(json.dumps(result, indent=2) + '\n')
    print('paced-pressure gate passed', len(positives), len(controls), flush=True)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--target-dir', type=Path, required=True)
    args = parser.parse_args()
    main(args.output, args.target_dir)
