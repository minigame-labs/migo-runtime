#!/usr/bin/env python3
"""Compare shared dispatch with the last accepted two-thread pool.

Same codec, input, full PCM and sample rate. O3 Rust-test traces on this host;
process CPU and child GNU-time RSS are not device or shipping acceptance.
The baseline tree must match the retained whole-owner checkpoint's hashes.
"""
from __future__ import annotations
import argparse
import json
import os
from pathlib import Path
import random
import re
import shutil
import statistics
import subprocess
import sys
import tomllib

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / 'scripts/testing'))
import check_audio_whole_decode as gate

FIXTURE = ROOT / 'scripts/testing/fixtures/audio-codec-probe/decode_pool_benchmark.rs'
TEST = 'audio_thread::scheduling_benchmark::same_codec_whole_file_dispatch'


def prepare(work, target, baseline):
    previous = json.loads((ROOT / 'docs/implementation/2026-10-25-runtime-modernization/results/W09-whole-owner-checkpoint.json').read_text())
    legacy = baseline / 'engine/crates/audio/src'
    expected = {name: value for name, value in previous['root_saved_inputs']['sha256'].items()
                if name.startswith('engine/crates/audio/src/')}
    assert expected and all(gate.whole.support.codecs.sha(baseline / name) == value for name, value in expected.items())
    assert {str(p.relative_to(baseline)) for p in legacy.rglob('*') if p.is_file()} == set(expected)
    package, env, frozen = gate.whole.support.clock.prepare(work, target, gate.whole.extra_inputs() + [Path(__file__), FIXTURE])
    fixture = work / FIXTURE.name
    fixture.write_bytes(FIXTURE.read_bytes())
    (work / 'fixtures').mkdir()
    for name, source in [('music.mp3', 'decoder/mp3/fixtures/music.mp3'), ('cross.ogg', 'decoder/ogg-fixtures/cross.ogg')]:
        (work / 'fixtures' / name).write_bytes((ROOT / 'engine/crates/audio/src' / source).read_bytes())
    version = next(row['version'] for row in tomllib.loads((ROOT / 'engine/Cargo.lock').read_text())['package'] if row['name'] == 'libc')
    manifest = (package / 'Cargo.toml').read_text().replace('[dev-dependencies]\n', f'[dev-dependencies]\nlibc="={version}"\n')
    (package / 'Cargo.toml').write_text(manifest)
    common = gate.whole.support.clock.common
    assert common.run(['cargo', 'metadata', '--offline', '--format-version', '1'], work / 'benchmark-metadata.jsonl', package, env) == 0
    root = {str(p.relative_to(package / 'src')): p.read_bytes() for p in (package / 'src').rglob('*') if p.is_file()}
    programs = {}
    for arm in ['shared', 'legacy']:
        if arm == 'legacy':
            shutil.rmtree(package / 'src')
            shutil.copytree(legacy, package / 'src')
        source = package / 'src/audio_thread.rs'
        host = 'host' if arm == 'shared' else '_host'
        adapter = f'\n#[cfg(test)] fn benchmark_pool(tx: DecodeQueueSender<DecodeResult>, rate:u32, {host}:&migo_io::scheduler::IoScheduler)->LazyDecodePool {{ LazyDecodePool::new(tx,rate,ThreadWakeup::new()'
        adapter += (',host.audio_cpu()' if arm == 'shared' else '') + ') }\n'
        adapter += f'#[cfg(test)] #[path={json.dumps(str(fixture))}] mod scheduling_benchmark;\n'
        source.write_text(source.read_text() + adapter)
        binary = gate.src.build(package, work, env, arm + '-benchmark', True)
        programs[arm] = dict(path=str(binary), sha256=gate.whole.support.codecs.sha(binary), fresh_rust=True)
        if arm == 'shared':
            log = work / 'fixture-clippy.jsonl'
            assert common.run(['cargo', 'clippy', '--lib', '--tests', '--offline', '--locked', '--message-format=json'], log, package, env, timeout=300) == 0
            diagnostics = []
            for line in log.read_text().splitlines():
                try:
                    row = json.loads(line)
                except ValueError:
                    continue
                if row.get('reason') == 'compiler-message':
                    msg = row['message']
                    if msg['level'] in ['warning', 'error'] and any(span.get('is_primary') and span['file_name'] == str(fixture) for span in msg['spans']):
                        diagnostics.append(msg)
            (work / 'fixture-clippy.json').write_text(json.dumps(dict(diagnostics=diagnostics, ordinary_exit=0), indent=2) + '\n')
            assert not diagnostics
    shutil.rmtree(package / 'src')
    for name, value in root.items():
        destination = package / 'src' / name
        destination.parent.mkdir(parents=True, exist_ok=True)
        destination.write_bytes(value)
    gate.whole.verify_inputs(work, frozen)
    shutil.copytree(legacy, work / 'legacy-source/engine/crates/audio/src')
    config = dict(programs=programs, source_inputs=frozen, legacy_source_sha256=expected,
                  target=str(target), profile='O3/noLTO/cg16/unwind, cfg(test)', source_copies_restored=True)
    (work / 'run-config.json').write_text(json.dumps(config, indent=2) + '\n')
    print('same-codec dispatch programs freshly compiled', flush=True)


def measure(work):
    config = json.loads((work / 'run-config.json').read_text())
    gate.whole.verify_inputs(work, config['source_inputs'])
    for row in config['programs'].values():
        assert gate.whole.support.codecs.sha(Path(row['path'])) == row['sha256']
    cpu = min(os.sched_getaffinity(0))
    samples = []
    for case, loops in [('wave', 12), ('tiny', 80), ('mp3', 6), ('vorbis', 8)]:
        for block in range(6):
            order = ['shared', 'legacy', 'legacy', 'shared'] if block % 2 == 0 else ['legacy', 'shared', 'shared', 'legacy']
            for offset, arm in enumerate(order):
                label = f'{case}-{block}-{offset}-{arm}'
                log, metrics = work / (label + '.log'), work / (label + '-time.json')
                env = os.environ | dict(MIGO_DISPATCH_CASE=case, MIGO_DISPATCH_LOOPS=str(loops))
                command = ['/usr/bin/time', '-f', '{"peak_rss_kib":%M}', '-o', str(metrics), 'taskset', '-c', str(cpu), config['programs'][arm]['path'], '--ignored', '--exact', TEST, '--test-threads=1', '--nocapture']
                with log.open('w') as output:
                    status = subprocess.run(command, cwd=work, env=env, stdout=output, stderr=subprocess.STDOUT, timeout=120).returncode
                assert status == 0 and '1 passed; 0 failed' in log.read_text(), log
                match = re.search(r'MIGO_DISPATCH (\{.*\})', log.read_text())
                assert match, log
                row = json.loads(match[1])
                assert row['pcm_checked'] and row['loops'] == loops
                row.update(arm=arm, block=block, offset=offset, peak_rss_kib=json.loads(metrics.read_text())['peak_rss_kib'], log=log.name)
                samples.append(row)
            print('paired dispatch block', case, block, flush=True)
    rng = random.Random(20261031)
    summary = {}
    for case in ['wave', 'tiny', 'mp3', 'vorbis']:
        selected = [row for row in samples if row['case'] == case]
        assert len({(row['pcm_bits'], row['samples'], row['rate']) for row in selected}) == 1
        result = {}
        for metric in ['cpu_ns', 'wall_ns']:
            ratios = []
            for block in range(6):
                arm = {name: statistics.mean(row[metric] for row in selected if row['block'] == block and row['arm'] == name) for name in ['shared', 'legacy']}
                ratios.append(arm['shared'] / arm['legacy'])
            boot = sorted(statistics.mean(rng.choices(ratios, k=6)) for _ in range(10000))
            result[metric] = dict(shared_over_legacy=statistics.mean(ratios), ci95=[boot[249], boot[9749]], block_ratios=ratios)
        peaks = {arm: statistics.median(row['peak_rss_kib'] for row in selected if row['arm'] == arm) for arm in ['shared', 'legacy']}
        result.update(peak_rss_kib=peaks, rss_delta_kib=peaks['shared'] - peaks['legacy'])
        summary[case] = result
    result = dict(summary=summary, samples=samples, fresh_processes=len(samples), affinity_cpu=cpu,
                  complete_pcm_bits_checked=True, same_codec_and_src=True, profile=config['profile'],
                  device_or_shipping_acceptance=False, whole_game_peak=False)
    (work / 'performance.json').write_text(json.dumps(result, indent=2) + '\n')
    print(json.dumps(summary, indent=2), flush=True)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--target-dir', type=Path)
    parser.add_argument('--baseline-sources', type=Path)
    parser.add_argument('--prepare-only', action='store_true')
    parser.add_argument('--run-prepared', action='store_true')
    args = parser.parse_args()
    assert not (args.prepare_only and args.run_prepared)
    if not args.run_prepared:
        assert args.target_dir and args.baseline_sources
        prepare(args.output, args.target_dir, args.baseline_sources)
    if not args.prepare_only:
        measure(args.output)
