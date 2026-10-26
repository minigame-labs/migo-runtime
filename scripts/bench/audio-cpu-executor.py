#!/usr/bin/env python3
"""Compare shared audio CPU admission with the previous spawn_blocking handoff.

Uses identical current codec/DSP/player/HTTP sources. Only the retained copy of
OffWorker changes for the baseline. Fresh-process ABBA/BAAB, exact PCM oracles,
cold/warm intervals and whole-process RSS are recorded. This is a single-host,
one-CPU virtual-consumer measurement, not device or multi-host acceptance.
"""
from __future__ import annotations

import argparse
import difflib
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import signal
import statistics
import subprocess

ROOT = Path(__file__).resolve().parents[2]


def sha(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def marked(text: str, prefix: str) -> list[dict]:
    return [json.loads(line.split(prefix, 1)[1]) for line in text.splitlines() if prefix in line]


def run(command: list[str], log: Path, environment: dict, cpu: int | None = None) -> None:
    with log.open('w') as output:
        child = subprocess.Popen(command, env=environment, stdout=output, stderr=subprocess.STDOUT,
                                 start_new_session=True,
                                 preexec_fn=(lambda: os.sched_setaffinity(0, {cpu})) if cpu is not None else None)
        try:
            status = child.wait(timeout=120)
        except subprocess.TimeoutExpired:
            os.killpg(child.pid, signal.SIGTERM)
            child.wait()
            raise AssertionError(f'timed out: {log}')
    assert status == 0, (log, status)


def main(cohort: Path, output: Path, target: Path, blocks: int, repeats: int) -> None:
    assert blocks >= 4 and blocks % 2 == 0 and repeats >= 3
    assert not output.exists(), 'use a fresh result directory'
    summary = json.loads((cohort / 'summary.json').read_text())
    candidate = cohort / 'positive-tests'
    assert sha(candidate) == summary['test_binary_sha256']
    inputs = json.loads((cohort / 'source-inputs.json').read_text())
    assert all(sha(ROOT / path) == checksum for path, checksum in inputs.items()), 'cohort inputs changed'
    retained = {p: sha(cohort / p) for p in summary['retained_sources']}
    output.mkdir(parents=True)
    driver = Path(__file__).resolve()
    (output / 'measurement-driver.py').write_bytes(driver.read_bytes())
    environment = os.environ.copy()
    environment.update(RUSTUP_TOOLCHAIN='1.95.0', CARGO_NET_OFFLINE='true', CARGO_TARGET_DIR=str(target),
                       CC='/usr/bin/clang', CXX='/usr/bin/clang++', W09_STREAM_FIXTURES=str(cohort / 'fixtures'))
    for key in ['ANDROID_NDK', 'ANDROID_NDK_HOME']:
        environment.pop(key, None)
    spec = importlib.util.spec_from_file_location('transport', ROOT / 'scripts/testing/check_audio_streaming_transport.py')
    transport = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(transport)
    source = cohort / 'source/off_worker.rs'
    original = source.read_text()
    before = '''let (value, outcome) = cpu
            .run(move || {
                let outcome = step(&mut value);
                (value, outcome)
            })
            .await?;'''
    after = '''let (value, outcome) = tokio::task::spawn_blocking(move || {
            let outcome = step(&mut value);
            (value, outcome)
        }).await.map_err(|error| shared::error::EngineError::from_detail(
            shared::error::ErrorCode::Internal, format!("baseline CPU step failed: {error}")))?;'''
    assert original.count(before) == 1
    baseline_source = original.replace(before, after)
    (output / 'off-worker-original.rs').write_text(original)
    (output / 'off-worker-baseline.rs').write_text(baseline_source)
    (output / 'baseline-source.diff').write_text(''.join(difflib.unified_diff(
        original.splitlines(keepends=True), baseline_source.splitlines(keepends=True),
        fromfile='shared-executor', tofile='previous-spawn-blocking')))
    settings = json.loads((cohort / 'build-settings.json').read_text())
    try:
        source.write_text(baseline_source)
        baseline = transport.build(cohort / 'probe', output, environment, 'baseline', settings['features'])
        run([str(baseline), 'transport_contracts::', '--nocapture', '--test-threads=1'],
            output / 'baseline-positive.log', environment)
    finally:
        source.write_text(original)
    assert all(sha(cohort / p) == checksum for p, checksum in retained.items()), 'retained source not restored'
    cases = {}
    for row in marked((cohort / 'tests-validated.log').read_text(), 'W09_PLAYER_ORACLE '):
        key = row['fixture'], row['target_rate']
        assert cases.get(key, row) == row
        cases[key] = row
    selected = [('long.mp3', 44100), ('long.mp3', 48000), ('cbr-44100-1.mp3', 48000)]
    allowed = sorted(os.sched_getaffinity(0))
    cpu = allowed[0]
    binary_hashes = {'baseline': sha(baseline), 'shared': sha(candidate)}
    (output / 'environment.json').write_text(json.dumps({
        'allowed_cpus': allowed, 'child_cpu': cpu, 'kernel': os.uname().release,
        'binaries': binary_hashes, 'driver_sha256': sha(driver), 'cohort': str(cohort),
        'baseline_positive_tests': settings['tests'], 'blocks': blocks, 'repeats_per_process': repeats,
        'scope': __doc__, 'clocks': 'CLOCK_PROCESS_CPUTIME_ID and monotonic Instant',
        'rust_allocation_scope': 'async HTTP executor thread only; CPU workers and native allocators excluded',
        'startup_scope': 'scheduler registration and lazy executor startup are inside the test helper interval; production registers at host creation'
    }, indent=2) + '\n')
    (output / 'cpuinfo.txt').write_text(Path('/proc/cpuinfo').read_text())
    raw = []
    for name, rate in selected:
        oracle = cases[name, rate]
        for block in range(blocks):
            order = ['baseline', 'shared', 'shared', 'baseline'] if block % 2 == 0 else ['shared', 'baseline', 'baseline', 'shared']
            for position, arm in enumerate(order):
                label = f'{Path(name).stem}-{rate}-block{block}-pos{position}-{arm}'
                env = environment.copy()
                env.update(W09_BENCH_FIXTURE=name, W09_BENCH_RATE=str(rate), W09_BENCH_CHANNELS=str(oracle['channels']),
                           W09_BENCH_FRAMES=str(oracle['frames']), W09_BENCH_BATCH='16', W09_BENCH_REPEATS=str(repeats))
                binary = candidate if arm == 'shared' else baseline
                run(['/usr/bin/time', '-f', '%U %S %M', '-o', str(output / f'{label}.time'), str(binary),
                     '--exact', 'transport_contracts::playback::pipeline_benchmark', '--ignored', '--nocapture'],
                    output / f'{label}.log', env, cpu)
                rows = marked((output / f'{label}.log').read_text(), 'W09_PIPELINE ')
                assert len(rows) == repeats
                for row in rows:
                    value = row['measurement']
                    assert value['pcm_hash'] == oracle['pcm_hash'] and value['frames'] == oracle['frames']
                user, system, peak = (output / f'{label}.time').read_text().split()
                raw.append({'fixture': name, 'rate': rate, 'block': block, 'position': position, 'arm': arm,
                            'runs': rows, 'process_user_s': float(user), 'process_system_s': float(system), 'peak_rss_kib': int(peak)})
            print(f'{name}/{rate} block {block + 1}/{blocks} same PCM', flush=True)
    (output / 'raw.json').write_text(json.dumps(raw, indent=2) + '\n')
    metrics = ['wall_ns', 'cpu_ns', 'first_pcm_ns', 'steps', 'io_thread_rust_allocations', 'player_peak_pcm_capacity_bytes']
    comparisons = []
    for name, rate in selected:
        group = [r for r in raw if r['fixture'] == name and r['rate'] == rate]
        comparison = {'fixture': name, 'rate': rate, 'phases': {}}
        for phase in ['cold', 'warm']:
            def value(item, metric):
                runs = item['runs'][:1] if phase == 'cold' else item['runs'][1:]
                return statistics.median(row['measurement'][metric] for row in runs)
            result = {}
            for metric in metrics:
                ratios = []
                for block in range(blocks):
                    a = statistics.median(value(r, metric) for r in group if r['block'] == block and r['arm'] == 'baseline')
                    b = statistics.median(value(r, metric) for r in group if r['block'] == block and r['arm'] == 'shared')
                    assert a > 0
                    ratios.append(b / a)
                result[metric] = {'shared_over_baseline_median': statistics.median(ratios), 'block_min': min(ratios), 'block_max': max(ratios)}
            comparison['phases'][phase] = result
        comparisons.append(comparison)
    assert sha(candidate) == binary_hashes['shared'] and sha(baseline) == binary_hashes['baseline']
    assert all(sha(ROOT / p) == checksum for p, checksum in inputs.items())
    result = {'fresh_processes': len(raw), 'playbacks': len(raw) * repeats, 'same_pcm_all_arms': True,
              'binaries': binary_hashes, 'root_inputs_unchanged': True, 'retained_sources_restored': True,
              'comparisons': comparisons, 'scope': __doc__}
    (output / 'summary.json').write_text(json.dumps(result, indent=2) + '\n')
    print(json.dumps({k: result[k] for k in ['fresh_processes', 'playbacks', 'same_pcm_all_arms', 'binaries']}), flush=True)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--cohort', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--target-dir', type=Path, required=True)
    parser.add_argument('--blocks', type=int, default=4)
    parser.add_argument('--repeats', type=int, default=3)
    args = parser.parse_args()
    main(args.cohort.resolve(), args.output.resolve(), args.target_dir.resolve(), args.blocks, args.repeats)
