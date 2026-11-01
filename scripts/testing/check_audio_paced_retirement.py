#!/usr/bin/env python3
"""Held destructors did not prove paced-player progress during natural teardown.

Run real managed-device churn beside production HTTP MP3/player and ordinary
whole-file work, keeping exact PCM and lifetime assertions. Setup/retirement,
host wake jitter and source starvation are separate observations. No CPU entry
barrier or source gate is used during the timed interval; this is host evidence.
"""
from __future__ import annotations
import argparse
import json
import os
from pathlib import Path
import re
import check_audio_paced_pressure as paced
import audio_debug_profile as debug_profile

ROOT = paced.ROOT
PRODUCT = paced.PRODUCT
FIXTURES = ROOT / 'scripts/testing/fixtures/audio-codec-probe'
DEVICE = FIXTURES / 'device_retirement_load.rs'
PLAYER = FIXTURES / 'streaming_retirement_pressure.rs'
TEST = paced.TEST.rsplit('::', 1)[0] + '::retirement::paced_player_and_whole_jobs_survive_device_owner_churn'


def prepare(work, target, debug_policy='root'):
    common = PRODUCT.whole.support.clock.common
    extras = PRODUCT.whole.extra_inputs() + [Path(__file__), DEVICE, PLAYER, paced.FIXTURE,
        paced.shared_gate.FIXTURE, Path(paced.__file__), Path(paced.shared_gate.__file__), *debug_profile.inputs(ROOT)]
    package, env, frozen = PRODUCT.whole.support.clock.prepare(work, target, extras)
    env['CARGO_INCREMENTAL'] = '0'
    debug_profile.apply(ROOT, work, package, env, common, debug_policy)
    encoded = work / 'source-30s.mp3'
    source = ROOT / 'engine/crates/audio/src/decoder/mp3/fixtures/music.mp3'
    command = ['ffmpeg', '-v', 'error', '-nostdin', '-stream_loop', '2', '-i', str(source),
               '-t', '30', '-ar', '48000', '-c:a', 'libmp3lame', '-q:a', '2', str(encoded)]
    assert common.run(command, work / 'input-generation.log', package, env) == 0
    assert common.run(['ffprobe', '-v', 'error', '-show_entries', 'stream=sample_rate,channels,duration',
        '-of', 'json', str(encoded)], work / 'input-probe.json', package, env) == 0
    metadata = json.loads((work / 'input-probe.json').read_text())['streams']
    assert len(metadata) == 1 and (metadata[0]['sample_rate'], metadata[0]['channels']) == ('48000', 2)
    assert 30 <= float(metadata[0]['duration']) < 31
    assert common.run(['ffmpeg', '-version'], work / 'ffmpeg-version.log', package, env) == 0
    (work / 'input-generation.json').write_text(json.dumps(dict(command=command,
        source_sha256=PRODUCT.whole.support.codecs.sha(source), encoded_sha256=PRODUCT.whole.support.codecs.sha(encoded),
        metadata=metadata), indent=2) + '\n')
    device, player, parent, helper = [work / p.name for p in [DEVICE, PLAYER, paced.FIXTURE, paced.shared_gate.FIXTURE]]
    device.write_bytes(DEVICE.read_bytes())
    player.write_bytes(PLAYER.read_bytes())
    text = paced.FIXTURE.read_text()
    replacements = [
        ('#[test]\nfn sample_clock_consumption_preserves_pcm_under_unbarriered_pressure()',
         'fn sample_clock_consumption_preserves_pcm_under_unbarriered_pressure()'),
        ('    println!("MIGO_PACED_PLAYING");', '    retirement::on_playing();\n    println!("MIGO_PACED_PLAYING");'),
        ('        let reserved = process_decode_in_flight_budget()', '        retirement::on_tick();\n        let reserved = process_decode_in_flight_budget()'),
        ('    let elapsed = start.elapsed();', '    let elapsed = start.elapsed();\n    retirement::on_finished();'),
    ]
    for before, after in replacements:
        assert text.count(before) == 1
        text = text.replace(before, after)
    text += '\n#[path=' + json.dumps(str(player)) + ']\nmod retirement;\n'
    parent.write_text(text)
    helper_text = paced.shared_gate.FIXTURE.read_text()
    start = helper_text.index('const MUSIC: &[u8] = include_bytes!(')
    end = helper_text.index('));', start) + 3
    helper_text = helper_text[:start] + 'const MUSIC: &[u8] = include_bytes!(' + json.dumps(str(encoded)) + ');' + helper_text[end:]
    helper_text += '\n#[path=' + json.dumps(str(parent)) + ']\nmod paced;\n'
    helper.write_text(helper_text)
    aliases = 'DeviceRetirementBatch, DeviceRetirementLoad'
    additions = {
        'decode_pool.rs': '\n#[cfg(test)]\n#[path=' + json.dumps(str(helper)) + ']\nmod real_stream_probe;\n',
        'device_resources.rs': '\n#[cfg(test)]\n#[path=' + json.dumps(str(device)) + ']\nmod paced_device_probe;\n#[cfg(test)]\npub(crate) use paced_device_probe::{Batch as DeviceRetirementBatch, Load as DeviceRetirementLoad};\n',
        'device_session.rs': '\n#[cfg(test)]\npub(crate) use resources::{' + aliases + '};\n',
        'device_pull.rs': '\n#[cfg(test)]\npub(crate) use session::{' + aliases + '};\n',
        'device_prepare.rs': '\n#[cfg(test)]\npub(crate) use pull::{' + aliases + '};\n',
    }
    for name, addition in additions.items():
        p = package / 'src' / name
        p.write_text(p.read_text() + addition)
    (work / 'module-additions.json').write_text(json.dumps(additions, indent=2) + '\n')
    (work / 'adapter-rewrites.json').write_text(json.dumps(replacements, indent=2) + '\n')
    files = {'device': device, 'player': player, 'parent': parent, 'helper': helper,
             'device_resources.rs': package / 'src/device_resources.rs', 'decode_pool.rs': package / 'src/decode_pool.rs',
             'inner_audio.rs': package / 'src/inner_audio.rs'}
    return package, env, frozen, files


def declarations(originals):
    rows = []
    def add(name, file, before, after, assertion, marker):
        assert originals[file].count(before) == 1, (name, 'anchor drift')
        rows.append((name, file, before, after, assertion, marker))
    add('churn-not-started', 'player', '    if state.enabled {\n        let first',
        '    if false {\n        let first', 'device churn worker was not started', 'MIGO_PACED_PLAYING')
    add('device-destructor-on-async-worker', 'device_resources.rs', 'retirement.retire(retired).await;',
        'drop(retired);', 'device destruction left existing CPU workers', 'MIGO_PACED_PLAYING')
    add('player-render-muted', 'inner_audio.rs', 'let volume = self.shared.volume();', 'let volume = 0.;',
        'silent render reference', None)
    add('whole-pcm-muted', 'decode_pool.rs', 'let audio = self.job.take().unwrap().finish()?;',
        'let mut audio = self.job.take().unwrap().finish()?; audio.samples.fill(0.);',
        'whole PCM changed', 'MIGO_PACED_PLAYING')
    return rows


def fixture_clippy(package, work, env, files):
    log = work / 'fixture-clippy.jsonl'
    assert PRODUCT.whole.support.clock.common.run(['cargo', 'clippy', '--lib', '--tests', '--offline',
        '--locked', '--message-format=json'], log, package, env, timeout=300) == 0, log
    warnings = []
    paths = {str(files[n]) for n in ['device', 'player', 'parent']}
    for line in log.read_text().splitlines():
        try: row = json.loads(line)
        except ValueError: continue
        if row.get('reason') != 'compiler-message': continue
        message = row['message']
        if message['level'] in ['warning', 'error'] and any(s.get('is_primary') and s['file_name'] in paths for s in message['spans']):
            warnings.append(message)
    (work / 'fixture-clippy.json').write_text(json.dumps(dict(diagnostics=warnings), indent=2) + '\n')
    assert not warnings, warnings


def main(work, target, preflight=False, controls_only=False, debug_policy='root', profiles=None):
    package, env, frozen, files = prepare(work, target, debug_policy)
    originals = {n: p.read_text() for n, p in files.items()}
    rows = declarations(originals)
    common = PRODUCT.whole.support.clock.common
    programs, controls, positives = [], [], []
    binaries = {}
    for profile in ([] if controls_only else profiles or (['release'] if preflight else ['release', 'debug'])):
        name = 'positive-' + profile
        binary = PRODUCT.src.build(package, work, env, name, profile == 'release')
        binaries[profile] = binary
        if profile == 'debug':
            debug_profile.verify(work / (name + '-build.jsonl'), json.loads((work / 'debug-profile-policy.json').read_text()),
                work / 'debug-profile-artifacts.json')
        programs.append(dict(name=name, path=str(binary), sha256=PRODUCT.whole.support.codecs.sha(binary), pruned=False))
    fixture_clippy(package, work, env, files)
    cpus = sorted(os.sched_getaffinity(0)); assert len(cpus) >= 6
    scenarios = [(6, 2, 2, 48000, 'wave')] if preflight else paced.SCENARIOS
    for profile, binary in binaries.items():
        for index, (count, hosts, streams, rate, kind) in enumerate(scenarios):
            modes = [True] if preflight else ([False, True] if (index + (profile == 'debug')) % 2 == 0 else [True, False])
            for churn in modes:
                label = f'{profile}-{count}cpu-{hosts}host-{streams}stream-{rate}-{kind}-churn{int(churn)}'
                log, metrics = work / (label + '.log'), work / (label + '-time.json')
                run_env = env | dict(MIGO_PACED_RATE=str(rate), MIGO_PACED_HOSTS=str(hosts), MIGO_PACED_STREAMS=str(streams),
                    MIGO_PACED_KIND=kind, MIGO_PACED_PRESSURE='1', MIGO_DEVICE_CHURN=str(int(churn)))
                args = ['/usr/bin/time', '-f', '{"user_seconds":%U,"system_seconds":%S,"peak_rss_kib":%M}', '-o', str(metrics),
                    'taskset', '-c', ','.join(map(str, cpus[:count])), str(binary), '--exact', TEST, '--nocapture']
                status = common.run(args, log, package, run_env, timeout=120)
                (work / (label + '-exit.txt')).write_text(str(status) + '\n')
                text = log.read_text(); assert status == 0 and 'running 1 test' in text and '1 passed; 0 failed;' in text, log
                a = re.search(r'MIGO_PACED (\{.*\})', text); b = re.search(r'MIGO_DEVICE_CHURN (\{.*\})', text); assert a and b, log
                player, device = json.loads(a[1]), json.loads(b[1])
                assert (player['rate'], player['hosts'], player['streams'], player['background_kind'], player['pressure']) == (rate, hosts, streams, kind, True)
                assert player['complete_mixed_pcm_bits_checked'] and player['media_frames'] >= 29 * rate
                assert player['io_workers'] == max(2, count) and player['foreground_requests'] > 0
                assert player['background_completed'] >= 2 and player['background_progress_windows'] >= 2
                assert device['enabled'] == churn
                if churn: assert device['completed_during_playback'] >= 3 and not device['device']['bad_destructor_thread']
                positives.append(dict(case=label, profile=profile, cpus=count, player=player, device=device,
                    process_metrics=json.loads(metrics.read_text())))
                (work / 'completed-positive-cases.json').write_text(json.dumps(positives, indent=2) + '\n')
                print('Paced device churn verified:', label, 'missing', player['player_missing_frames'], flush=True)
    if not preflight:
        for name, file, before, after, assertion, marker in rows:
            try:
                files[file].write_text(originals[file].replace(before, after))
                binary = PRODUCT.src.build(package, work, env, name, True)
                log = work / (name + '-test.log')
                run_env = env | dict(MIGO_PACED_RATE='48000', MIGO_PACED_HOSTS='2', MIGO_PACED_STREAMS='2',
                    MIGO_PACED_KIND='wave', MIGO_PACED_PRESSURE='1', MIGO_DEVICE_CHURN='1')
                status = common.run(['taskset', '-c', ','.join(map(str, cpus[:6])), str(binary), '--exact', TEST,
                    '--nocapture'], log, package, run_env, timeout=120)
                text = log.read_text()
                assert status == 101 and all(s in text for s in ['running 1 test', 'panicked at', '1 failed', assertion]), (name, status, log)
                assert marker is None or marker in text
                digest = PRODUCT.whole.support.codecs.sha(binary)
                programs.append(dict(name=name, path=str(binary), sha256=digest, pruned=True))
                controls.append(dict(name=name, test=TEST, exit=status, expected_assertion=assertion, binary_sha256=digest))
                binary.unlink(); (work / 'completed-controls.json').write_text(json.dumps(controls, indent=2) + '\n')
                print('Actual paced-retirement fault rejected:', name, flush=True)
            finally: files[file].write_text(originals[file])
    PRODUCT.whole.verify_inputs(work, frozen)
    additions = json.loads((work / 'module-additions.json').read_text())
    root = ROOT / 'engine/crates/audio/src'
    for p in root.rglob('*'):
        if p.is_file(): assert (package / 'src' / p.relative_to(root)).read_bytes() == p.read_bytes() + additions.get(p.name, '').encode()
    assert all(p.read_text() == originals[n] for n, p in files.items())
    result = dict(positive_cases=positives, positive_processes=len(positives), controls=controls, compiled_controls=len(controls),
        programs=programs, root_inputs_unchanged=True, source_copies_restored=True, preflight=preflight, controls_only=controls_only,
        actual_production_player=True, real_managed_device_owners=True, no_cpu_entry_barriers=True,
        debug_policy=debug_policy,
        no_source_gates_during_playback=True, setup_and_retirement_separately_recorded=True,
        device_or_shipping_acceptance=False, fixed_work_cpu_speedup_claim=False)
    (work / 'summary.json').write_text(json.dumps(result, indent=2) + '\n')
    print('Paced retirement gate verified', len(positives), len(controls), flush=True)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--target-dir', type=Path, required=True)
    parser.add_argument('--preflight', action='store_true')
    parser.add_argument('--controls-only', action='store_true')
    parser.add_argument('--debug-policy', choices=['root','unoptimized'], default='root')
    parser.add_argument('--profile', choices=['release','debug'], action='append')
    args = parser.parse_args()
    assert not (args.preflight and args.controls_only)
    main(args.output, args.target_dir, args.preflight, args.controls_only, args.debug_policy, args.profile)
