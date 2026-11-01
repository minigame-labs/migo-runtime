#!/usr/bin/env python3
"""The foreground queue probe did not exercise real source/codec/PCM ownership.

Drive the production HTTP MP3 entry over a gated loopback source with whole-file
jobs on the same executor. Gates prove overlap and backpressure, not real-time
playback deadlines. Only fresh executables' exact Rust failures reject faults.
All source mutations stay in the retained probe; Root engine inputs stay fixed.
"""
from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import re

import check_audio_whole_decode as product

ROOT = product.whole.ROOT
FIXTURE = ROOT / 'scripts/testing/fixtures/audio-codec-probe/streaming_whole_contention.rs'
PREFIX = 'audio_thread::decode_pool::real_stream_probe::'
CONTENDED = PREFIX + 'real_http_refills_overlap_whole_jobs_and_preserve_every_sample'
PAUSED = PREFIX + 'full_real_pcm_channel_stops_cpu_and_cancel_retires_the_producer'


def declarations():
    return [
        ('stream-pcm-muted', 'streaming_mp3.rs', '        Ok(Step {\n            consumed,',
         '        pcm.fill(0.);\n        Ok(Step {\n            consumed,', CONTENDED, 'silent reference/workload', None, 0),
        ('whole-pcm-muted', 'decode_pool.rs', 'let audio = self.job.take().unwrap().finish()?;',
         'let mut audio = self.job.take().unwrap().finish()?; audio.samples.fill(0.);', CONTENDED, 'whole PCM changed', 'MIGO_REAL_STREAM_INITIAL_PCM', 1),
        ('stream-cpu-demoted', 'streaming.rs', '        cpu,\n        client,\n        url,',
         '        cpu.background(),\n        client,\n        url,', CONTENDED, 'actual pipeline made no progress', 'MIGO_REAL_STREAM_INITIAL_PCM', 1),
        ('shared-memory-reservation-replaced', 'decode_pool.rs', '            process_decode_in_flight_budget(),',
         '            Arc::new(DecodeInFlightUsage::new(2, 1)),', CONTENDED, 'shared whole memory reservation', 'MIGO_REAL_STREAM_INITIAL_PCM', 1),
        ('pcm-channel-capacity-reduced', 'streaming.rs', 'const STREAM_CHANNEL_CAPACITY: usize = 4;',
         'const STREAM_CHANNEL_CAPACITY: usize = 3;', PAUSED, 'actual pipeline made no progress', 'MIGO_REAL_HTTP_BODY', 1),
        ('cancel-wakeup-lost', 'streaming.rs', 'self.cancel_notify.notify_waiters();',
         'let _ = &self.cancel_notify;', PAUSED, 'actual pipeline made no progress', 'MIGO_REAL_HTTP_BODY', 1),
        ('consumer-drop-not-latched', 'streaming_transport.rs', '                context.state.cancel();\n                return ProducerEvent::Cancelled;',
         '                return ProducerEvent::Cancelled;', PAUSED, 'actual pipeline made no progress', 'MIGO_REAL_HTTP_BODY', 2),
    ]


def fixture_clippy(package, work, env, fixture):
    common = product.whole.support.clock.common
    log = work / 'fixture-clippy.jsonl'
    args = ['cargo', 'clippy', '--lib', '--tests', '--offline', '--locked', '--message-format=json']
    assert common.run(args, log, package, env, timeout=300) == 0, log
    diagnostics = []
    for line in log.read_text().splitlines():
        try:
            row = json.loads(line)
        except ValueError:
            continue
        if row.get('reason') == 'compiler-message':
            message = row['message']
            if message['level'] in ['warning', 'error'] and any(
                span.get('is_primary') and span['file_name'] == str(fixture)
                for span in message['spans']
            ):
                diagnostics.append(message)
    (work / 'fixture-clippy.json').write_text(json.dumps(dict(diagnostics=diagnostics), indent=2) + '\n')
    assert not diagnostics, log


def main(work: Path, target: Path):
    common = product.whole.support.clock.common
    extra = product.whole.extra_inputs() + [Path(__file__), FIXTURE, ROOT / 'scripts/testing/check_gitignore_contract.py']
    package, env, frozen = product.whole.support.clock.prepare(work, target, extra)
    fixture = work / FIXTURE.name
    fixture.write_bytes(FIXTURE.read_bytes())
    addition = '\n#[cfg(test)]\n#[path=' + json.dumps(str(fixture)) + ']\nmod real_stream_probe;\n'
    source = package / 'src/decode_pool.rs'
    source.write_text(source.read_text() + addition)
    (work / 'module-addition.txt').write_text(addition)
    originals = {name: (package / 'src' / name).read_text() for name in ['decode_pool.rs', 'streaming.rs', 'streaming_mp3.rs', 'streaming_transport.rs']}
    for name, path, before, _after, _test, _message, _marker, _count in declarations():
        assert originals[path].count(before) == 1, (name, 'anchor drift')
    programs, positives, controls = [], [], []
    cpus = sorted(os.sched_getaffinity(0))
    assert len(cpus) >= 6, 'the declared topology matrix needs six permitted CPUs'
    topologies = [(1, 1, 1), (1, 2, 2), (6, 1, 2), (6, 2, 2)]
    for profile in ['release', 'debug']:
        label = 'positive-' + profile
        binary = product.src.build(package, work, env, label, profile == 'release')
        programs.append(dict(name=label, path=str(binary), sha256=product.whole.support.codecs.sha(binary), fresh_rust=True, pruned=False))
        if profile == 'release':
            fixture_clippy(package, work, env, fixture)
        for cpu_count, hosts, streams in topologies:
            for rate in [48000, 44100]:
                case = f'{profile}-{cpu_count}cpu-{hosts}host-{streams}stream-{rate}'
                log = work / (case + '.log')
                run_env = env | dict(MIGO_REAL_HOSTS=str(hosts), MIGO_REAL_STREAMS=str(streams), MIGO_REAL_RATE=str(rate))
                command = ['taskset', '-c', ','.join(map(str, cpus[:cpu_count])), str(binary), '--exact', CONTENDED, '--nocapture']
                status = common.run(command, log, package, run_env, timeout=180)
                text = log.read_text()
                assert status == 0 and '1 passed; 0 failed' in text, log
                match = re.search(r'MIGO_REAL_STREAM (\{.*\})', text)
                assert match, log
                row = json.loads(match[1])
                assert (row['rate'], row['hosts'], row['streams'], row['io_workers']) == (rate, hosts, streams, max(2, cpu_count)), log
                assert row['complete_pcm_bits_checked'] and row['overlap_rounds'] == 39
                assert row['background_jobs'] == 39 * (1 if cpu_count == 1 else 2)
                positives.append(dict(profile=profile, test=CONTENDED, case=case, **row))
                print('real pipeline verified:', case, flush=True)
        for cpu_count in [1, 6]:
            label = f'{profile}-{cpu_count}cpu-pause-cancel'
            log = work / (label + '.log')
            command = ['taskset', '-c', ','.join(map(str, cpus[:cpu_count])), str(binary), '--exact', PAUSED, '--nocapture']
            assert common.run(command, log, package, env, timeout=60) == 0, log
            assert '1 passed; 0 failed' in log.read_text() and log.read_text().count('MIGO_REAL_HTTP_BODY') == 2, log
            positives.append(dict(profile=profile, test=PAUSED, case=label, cpu_count=cpu_count, cancel_and_consumer_drop=True))
        (work / 'completed-positive-cases.json').write_text(json.dumps(positives, indent=2) + '\n')
    for profile in ['release', 'debug']:
        for rate in [48000, 44100]:
            rows = [row for row in positives if row['profile'] == profile and row.get('rate') == rate]
            assert len({(row['stream_samples'], row['pcm_fingerprint']) for row in rows}) == 1
        native = next(row for row in positives if row['profile'] == profile and row.get('rate') == 48000)
        converted = next(row for row in positives if row['profile'] == profile and row.get('rate') == 44100)
        expected_samples = (((native['stream_samples'] // 2) * 44100 + 47999) // 48000) * 2
        assert converted['stream_samples'] == expected_samples, 'exact streaming SRC output frame count'
    for name, path, before, after, test, expected, marker, marker_count in declarations():
        changed = package / 'src' / path
        try:
            changed.write_text(originals[path].replace(before, after))
            binary = product.src.build(package, work, env, name, True)
            log = work / (name + '-test.log')
            command = ['taskset', '-c', ','.join(map(str, cpus[:6])), str(binary), '--exact', test, '--nocapture']
            status = common.run(command, log, package, env | dict(MIGO_REAL_RATE='44100'), timeout=60)
            text = log.read_text()
            assert status == 101 and all(item in text for item in ['running 1 test', 'panicked at', '1 failed', expected]), (name, status, log)
            assert marker is None or text.count(marker) >= marker_count, (name, 'failed before real HTTP/PCM work', log)
            assert 'PermissionDenied' not in text and 'Operation not permitted' not in text, log
            digest = product.whole.support.codecs.sha(binary)
            controls.append(dict(name=name, test=test, exit=status, expected_assertion=expected, real_work_marker=marker, marker_count=marker_count, binary_sha256=digest))
            programs.append(dict(name=name, path=str(binary), sha256=digest, fresh_rust=True, pruned=True))
            binary.unlink()
            (work / 'completed-controls.json').write_text(json.dumps(controls, indent=2) + '\n')
            print('fresh assertion rejected:', name, flush=True)
        finally:
            changed.write_text(originals[path])
    product.whole.verify_inputs(work, frozen)
    root = ROOT / 'engine/crates/audio/src'
    for path in root.rglob('*'):
        if path.is_file():
            expected = path.read_bytes() + (addition.encode() if path.name == 'decode_pool.rs' else b'')
            assert (package / 'src' / path.relative_to(root)).read_bytes() == expected, path
    assert fixture.read_bytes() == FIXTURE.read_bytes()
    result = dict(positive_cases=positives, positive_processes=len(positives), compiled_controls=len(controls), controls=controls,
                  programs=programs, root_inputs_unchanged=True, source_copies_restored=True, real_http_codec_pcm=True,
                  controlled_supply_and_gated_overlap=True, device_or_playback_deadline_acceptance=False,
                  exact_conversion_frame_counts_checked=True,
                  latency_is_gated_observation_not_benchmark=True, global_strict_clippy_passed=False)
    (work / 'summary.json').write_text(json.dumps(result, indent=2) + '\n')
    print('real streaming/whole contention gate passed', len(positives), len(controls), flush=True)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--target-dir', type=Path, required=True)
    args = parser.parse_args()
    main(args.output, args.target_dir)
