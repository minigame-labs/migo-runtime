#!/usr/bin/env python3
"""Prewarmed owner churn did not exercise the first production graph mix.

Build real AudioThread/device code, preserve Root dev package roles, and check
all first-use PCM, ended IDs and closed-host credit. Timings are host samples,
with process-first-use and service-reuse cycles reported separately.
"""
from __future__ import annotations
import argparse
import json
import os
from pathlib import Path
import re
import check_audio_whole_decode as product
import audio_debug_profile as profile

ROOT = product.whole.ROOT
FIXTURE = ROOT / 'scripts/testing/fixtures/audio-codec-probe/device_cold_start.rs'
TEST = 'audio_thread::cold_graph_probe::first_use_large_graph_preserves_complete_pcm_events_and_closed_host_credit'
ADAPTER = '''
#[cfg(test)]
impl DeviceMixer {
    pub(crate) fn cold_probe_active(&self) -> usize {
        self.routes.values().filter(|route| matches!(route.state, RouteState::Active(_))).count()
    }
    pub(crate) fn cold_probe_usage(&self) -> (usize, usize, usize) {
        let host = &self.controller.host;
        (host.usage.owners.load(Ordering::Acquire), host.process.owners.load(Ordering::Acquire), host.aggregate.used_bytes())
    }
    pub(crate) fn cold_probe_position(&self, context: &AudioContext) -> u64 {
        if context.sample_rate() == 48_000 { return context.device_graph_frame(); }
        let Some(route) = self.routes.get(&context.id()) else { return 0; };
        let RouteState::Active(device) = &route.state else { return 0; };
        let Some(lease) = &device.lease else { return 0; };
        let State::Session(session) = &lease.retired.as_ref().unwrap().state else { return 0; };
        session.pull.clocks().published_output
    }
    pub(crate) fn cold_probe_retirement(&mut self, started: Arc<std::sync::atomic::AtomicUsize>, ended: Arc<std::sync::atomic::AtomicUsize>, bad: Arc<std::sync::atomic::AtomicBool>) {
        self.controller.host.retire_hook = Some(Arc::new(move |before| {
            if !std::thread::current().name().unwrap_or("").starts_with("Migo-IO-") { bad.store(true, Ordering::Release); }
            if before { started.fetch_add(1, Ordering::Release); } else { ended.fetch_add(1, Ordering::Release); }
        }));
    }
}
'''


def prepare(work, target):
    whole = product.whole
    extra = [Path(__file__), FIXTURE, *profile.inputs(ROOT)]
    extra += [p for p in (ROOT / 'engine/crates/frame-wire').rglob('*') if p.is_file()]
    extra += [ROOT / 'engine/.cargo/config.toml']
    package, env, frozen = whole.support.clock.prepare(work, target, whole.extra_inputs() + extra)
    env['CARGO_INCREMENTAL'] = '0'
    profile.apply(ROOT, work, package, env, whole.support.clock.common)
    fixture = work / FIXTURE.name
    fixture.write_bytes(FIXTURE.read_bytes())
    additions = {
        'audio_thread.rs': '\n#[cfg(test)]\n#[path=' + json.dumps(str(fixture)) + ']\nmod cold_graph_probe;\n',
        'device_mixer.rs': ADAPTER,
    }
    for name, addition in additions.items():
        p = package / 'src' / name
        p.write_text(p.read_text() + addition)
    (work / 'module-additions.json').write_text(json.dumps(additions, indent=2) + '\n')
    return package, env, frozen


def controls(originals):
    audio = originals['audio_thread.rs']
    start = audio.index('            device_mixer.mix(', audio.index('fn mix_contexts('))
    end = audio.index('            );', start) + len('            );')
    block = audio[start:end]
    return [
        ('cold-shipping-mix-muted', 'audio_thread.rs', block, block + '\n            quantum.fill(0.125);', 'cold graph complete PCM changed'),
        ('cold-shipping-ended-lost', 'audio_thread.rs', block, block.replace('&mut ended,', '&mut |_| {},'), 'cold graph ended set changed'),
        ('cold-owner-drop-on-async', 'device_resources.rs', 'retirement.retire(retired).await;', 'drop(retired);', 'cold graph destroyed an owner outside CPU workers'),
    ]


def execute(binary, package, env, work, label, cpus, hosts, contexts, rate, automation=False, waveshaper_factor=0, source_frames=2048, source_kind='buffer', start_frame=0.0, stop_frame=None):
    run_env = env | dict(MIGO_COLD_HOSTS=str(hosts), MIGO_COLD_CONTEXTS=str(contexts), MIGO_COLD_RATE=str(rate), MIGO_COLD_AUTOMATION=str(int(automation)), MIGO_COLD_WAVESHAPER=str(waveshaper_factor), MIGO_COLD_FRAMES=str(source_frames), MIGO_COLD_SOURCE=source_kind, MIGO_COLD_START=str(start_frame), MIGO_COLD_STOP='' if stop_frame is None else str(stop_frame))
    allowed = sorted(os.sched_getaffinity(0))
    assert len(allowed) >= cpus
    command = ['/usr/bin/time', '-f', '{"user_seconds":%U,"system_seconds":%S,"peak_rss_kib":%M}',
        '-o', str(work / (label + '-time.json')), 'taskset', '-c', ','.join(map(str, allowed[:cpus])),
        str(binary), '--exact', TEST, '--nocapture']
    log = work / (label + '.log')
    status = product.whole.support.clock.common.run(command, log, package, run_env, timeout=120)
    (work / (label + '-exit.txt')).write_text(str(status) + '\n')
    return status, log


def main(work, target, preflight=False, controls_only=False, automation=False, waveshaper_factor=0, source_frames=2048, source_kind='buffer', start_frame=0.0, stop_frame=None):
    package, env, frozen = prepare(work, target)
    files = {name: package / 'src' / name for name in ['audio_thread.rs', 'device_resources.rs']}
    originals = {name: path.read_text() for name, path in files.items()}
    rows = controls(originals)
    for name, file, before, after, _ in rows:
        assert before != after and originals[file].count(before) == 1, (name, 'anchor drift')
    programs, cases, negatives = [], [], []
    if not controls_only:
        binary = product.src.build(package, work, env, 'positive-debug', False)
        profile.verify(work / 'positive-debug-build.jsonl', json.loads((work / 'debug-profile-policy.json').read_text()), work / 'debug-profile-artifacts.json')
        programs.append(dict(name='positive-debug', path=str(binary), sha256=product.whole.support.codecs.sha(binary), pruned=False))
        clippy = work / 'fixture-clippy.jsonl'
        assert product.whole.support.clock.common.run(['cargo', 'clippy', '--lib', '--tests', '--offline', '--locked', '--message-format=json'], clippy, package, env, timeout=300) == 0
        warnings = []
        for line in clippy.read_text().splitlines():
            try: row = json.loads(line)
            except ValueError: continue
            if row.get('reason') != 'compiler-message': continue
            m = row['message']
            if m['level'] in ['warning', 'error'] and any(s.get('is_primary') and s['file_name'] == str(work / FIXTURE.name) for s in m['spans']): warnings.append(m)
        (work / 'fixture-clippy.json').write_text(json.dumps(warnings, indent=2) + '\n')
        assert not warnings, warnings
        scenarios = [(1, 1, 1, 44_100)] if preflight else [(cpu, hosts, contexts, rate) for cpu in [1, 6] for hosts, contexts in [(1, 1), (4, 8)] for rate in [48_000, 44_100]]
        for cpu, hosts, contexts, rate in scenarios:
            label = f'{cpu}cpu-{hosts}host-{contexts}contexts-{rate}'
            status, log = execute(binary, package, env, work, label, cpu, hosts, contexts, rate, automation, waveshaper_factor, source_frames, source_kind, start_frame, stop_frame)
            text = log.read_text()
            assert status == 0 and 'running 1 test' in text and '1 passed; 0 failed;' in text, log
            values = [json.loads(s) for s in re.findall(r'MIGO_COLD (\{.*\})', text)]
            assert len(values) == 2 and all(v['complete_mixed_pcm_bits_checked'] for v in values)
            assert all(v['automation'] == automation and v['nodes_per_context'] == (66 + int(automation) + int(waveshaper_factor != 0)) for v in values)
            assert all(v['waveshaper_factor'] == waveshaper_factor and v['source_frames'] == source_frames and v['source_kind'] == source_kind and v['start_frame'] == start_frame and v['stop_frame'] == (stop_frame if stop_frame is not None else None if source_kind == 'buffer' else source_frames) for v in values)
            cases.append(dict(case=label, cpus=cpu, automation=automation, waveshaper_factor=waveshaper_factor, observations=values,
                process_metrics=json.loads((work / (label + '-time.json')).read_text())))
            (work / 'completed-positive-cases.json').write_text(json.dumps(cases, indent=2) + '\n')
            print('Cold production graph verified', label, flush=True)
    if not preflight:
        for name, file, before, after, assertion in rows:
            try:
                files[file].write_text(originals[file].replace(before, after))
                binary = product.src.build(package, work, env, name, False)
                status, log = execute(binary, package, env, work, name, 1, 1, 1, 44_100, automation, waveshaper_factor, source_frames, source_kind, start_frame, stop_frame)
                text = log.read_text()
                assert status == 101 and all(t in text for t in ['running 1 test', 'panicked at', '1 failed', assertion]), (name, status, log)
                digest = product.whole.support.codecs.sha(binary)
                programs.append(dict(name=name, path=str(binary), sha256=digest, pruned=True))
                negatives.append(dict(name=name, expected_assertion=assertion, exit=status, binary_sha256=digest))
                binary.unlink()
                (work / 'completed-controls.json').write_text(json.dumps(negatives, indent=2) + '\n')
                print('Actual cold graph fault rejected', name, flush=True)
            finally:
                files[file].write_text(originals[file])
    product.whole.verify_inputs(work, frozen)
    additions = json.loads((work / 'module-additions.json').read_text())
    root = ROOT / 'engine/crates/audio/src'
    assert all((package / 'src' / p.relative_to(root)).read_bytes() == p.read_bytes() + additions.get(p.name, '').encode() for p in root.rglob('*') if p.is_file())
    (work / 'summary.json').write_text(json.dumps(dict(cases=cases, controls=negatives, programs=programs, root_inputs_unchanged=True, source_copies_restored=True, root_dev_profile_verified=not controls_only, device_or_shipping_acceptance=False, preflight=preflight, controls_only=controls_only, automation=automation, waveshaper_factor=waveshaper_factor, source_frames=source_frames, source_kind=source_kind, start_frame=start_frame, stop_frame=stop_frame), indent=2) + '\n')
    print('Cold graph gate complete', len(cases), len(negatives), flush=True)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--target-dir', type=Path, required=True)
    parser.add_argument('--preflight', action='store_true')
    parser.add_argument('--controls-only', action='store_true')
    parser.add_argument('--automation', action='store_true')
    parser.add_argument('--waveshaper-factor', type=int, choices=[0, 2, 4], default=0)
    parser.add_argument('--source-frames', type=int, default=2048)
    parser.add_argument('--source-kind', choices=['buffer', 'constant', 'oscillator-frequency', 'oscillator-detune', 'oscillator-both'], default='buffer')
    parser.add_argument('--start-frame', type=float, default=0.0)
    parser.add_argument('--stop-frame', type=float)
    args = parser.parse_args()
    assert 0 <= args.start_frame <= 16384
    assert args.stop_frame is None or 0 <= args.stop_frame <= 32768
    assert 1 <= args.source_frames <= 16384
    assert not (args.preflight and args.controls_only)
    main(args.output.resolve(), args.target_dir.resolve(), args.preflight, args.controls_only, args.automation, args.waveshaper_factor, args.source_frames, args.source_kind, args.start_frame, args.stop_frame)
