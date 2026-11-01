#!/usr/bin/env python3
"""A single pinned CPU concealed shared audio's six-worker handoff cost.

Compare exact accepted before/legacy sources and a bounded-turn candidate with
complete PCM across CPU/owner counts, conversion and foreground probe bursts.
No result changes codec, DSP, input ownership, memory budgets or turn elapsed cap.
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

FIXTURE = ROOT / 'scripts/testing/fixtures/audio-codec-probe/decode_topology_benchmark.rs'
TEST = 'audio_thread::scheduling_benchmark::complete_pcm_across_topologies'


def accepted_sources(baseline, receipt):
    values = json.loads(receipt.read_text())['root_saved_inputs']['sha256']
    expected = {name: value for name, value in values.items() if name.startswith('engine/crates/audio/src/')}
    assert expected and all(gate.whole.support.codecs.sha(baseline / name) == h for name, h in expected.items())
    actual = {str(p.relative_to(baseline)) for p in (baseline / 'engine/crates/audio/src').rglob('*') if p.is_file()}
    assert actual == set(expected)
    return expected


def prepare(work, target, before, legacy, cap, prototype=False):
    before_hashes = accepted_sources(before, ROOT / 'docs/implementation/2026-10-25-runtime-modernization/results/W09-shared-decode-checkpoint.json')
    legacy_hashes = accepted_sources(legacy, ROOT / 'docs/implementation/2026-10-25-runtime-modernization/results/W09-whole-owner-checkpoint.json')
    package, env, frozen = gate.whole.support.clock.prepare(work, target, gate.whole.extra_inputs() + [Path(__file__), FIXTURE])
    fixture = work / FIXTURE.name
    fixture.write_bytes(FIXTURE.read_bytes())
    (work / 'fixtures').mkdir()
    # The original music fixture is 48 kHz: labeling its native bypass as
    # "convert" never exercised SRC. Both arms now get the same generated
    # 44.1 kHz encoded input, and the native/converted output rates are asserted.
    (work / 'fixtures/cross.ogg').write_bytes((ROOT / 'engine/crates/audio/src/decoder/ogg-fixtures/cross.ogg').read_bytes())
    encoded = work / 'fixtures/music.mp3'
    command = ['ffmpeg', '-v', 'error', '-nostdin', '-i', str(ROOT / 'engine/crates/audio/src/decoder/mp3/fixtures/music.mp3'), '-ar', '44100', '-c:a', 'libmp3lame', '-q:a', '2', str(encoded)]
    common = gate.whole.support.clock.common
    assert common.run(command, work / 'input-generation.log', work, env) == 0
    probe = work / 'input-probe.json'
    assert common.run(['ffprobe', '-v', 'error', '-show_entries', 'stream=sample_rate,duration', '-of', 'json', str(encoded)], probe, work, env) == 0
    assert [stream['sample_rate'] for stream in json.loads(probe.read_text())['streams']] == ['44100']
    assert common.run(['ffmpeg', '-version'], work / 'ffmpeg-version.log', work, env) == 0
    (work / 'input-generation.json').write_text(json.dumps(dict(command=command, input_source_sha256=gate.whole.support.codecs.sha(ROOT / 'engine/crates/audio/src/decoder/mp3/fixtures/music.mp3'), encoded_sha256=gate.whole.support.codecs.sha(encoded), native_rate=44100, converted_rate=48000), indent=2) + '\n')
    version = next(row['version'] for row in tomllib.loads((ROOT / 'engine/Cargo.lock').read_text())['package'] if row['name'] == 'libc')
    manifest = (package / 'Cargo.toml').read_text().replace('[dev-dependencies]\n', f'[dev-dependencies]\nlibc="={version}"\n')
    (package / 'Cargo.toml').write_text(manifest)
    common = gate.whole.support.clock.common
    assert common.run(['cargo', 'metadata', '--offline', '--format-version', '1'], work / 'benchmark-metadata.jsonl', package, env) == 0
    root = {str(p.relative_to(package / 'src')): p.read_bytes() for p in (package / 'src').rglob('*') if p.is_file()}
    programs = {}
    for arm in ['before', 'candidate', 'legacy']:
        shutil.rmtree(package / 'src')
        origin = legacy if arm == 'legacy' else ROOT if arm == 'candidate' and not prototype else before
        shutil.copytree(origin / 'engine/crates/audio/src', package / 'src')
        if arm == 'candidate' and prototype:
            source = package / 'src/decode_pool.rs'
            text = source.read_text()
            assert text.count('const MAX_STEPS_PER_TURN: usize = 32;') == 1
            source.write_text(text.replace('const MAX_STEPS_PER_TURN: usize = 32;', f'const MAX_STEPS_PER_TURN: usize = {cap};'))
        if arm == 'legacy':
            source = package / 'src/audio_thread.rs'
            text = source.read_text()
            signature = 'fn run_decode_with_panic_boundary('
            begin = text.index(signature)
            at = text.index(') -> EngineResult<DecodedAudio> {', begin) + len(') -> EngineResult<DecodedAudio> {')
            source.write_text(text[:at] + '\n    #[cfg(test)] scheduling_benchmark::background_started();' + text[at:])
        else:
            source = package / 'src/decode_pool.rs'
            text = source.read_text()
            begin = text.index('    fn turn(&mut self')
            at = text.index('{', begin) + 1
            source.write_text(text[:at] + '\n        #[cfg(test)] if self.input.is_some() { super::scheduling_benchmark::background_started(); }' + text[at:])
        source = package / 'src/audio_thread.rs'
        host = '_host' if arm == 'legacy' else 'host'
        adapter = f'\n#[cfg(test)] fn benchmark_pool(tx: DecodeQueueSender<DecodeResult>, rate:u32, {host}:&migo_io::scheduler::IoScheduler)->LazyDecodePool {{ LazyDecodePool::new(tx,rate,ThreadWakeup::new()'
        adapter += ('' if arm == 'legacy' else ',host.audio_cpu()') + ') }\n'
        adapter += f'#[cfg(test)] #[path={json.dumps(str(fixture))}] mod scheduling_benchmark;\n'
        source.write_text(source.read_text() + adapter)
        binary = gate.src.build(package, work, env, arm + '-benchmark', True)
        programs[arm] = dict(path=str(binary), sha256=gate.whole.support.codecs.sha(binary), fresh_rust=True)
        saved = work / (arm + '-source')
        shutil.copytree(package / 'src', saved)
        if arm == 'candidate':
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
                    if msg['level'] in ['warning', 'error'] and any(s.get('is_primary') and s['file_name'] == str(fixture) for s in msg['spans']):
                        diagnostics.append(msg)
            (work / 'fixture-clippy.json').write_text(json.dumps(dict(diagnostics=diagnostics), indent=2) + '\n')
            assert not diagnostics
    shutil.rmtree(package / 'src')
    for name, value in root.items():
        destination = package / 'src' / name
        destination.parent.mkdir(parents=True, exist_ok=True)
        destination.write_bytes(value)
    gate.whole.verify_inputs(work, frozen)
    config = dict(programs=programs, source_inputs=frozen, before_sha256=before_hashes, legacy_sha256=legacy_hashes,
                  candidate_step_cap=cap, prototype=prototype, common_io_current_for_all_arms=True, elapsed_ms=1, target=str(target), profile='O3/noLTO/cg16/unwind,cfg(test)')
    (work / 'run-config.json').write_text(json.dumps(config, indent=2) + '\n')
    print('three fresh topology programs prepared', flush=True)


def scenarios(smoke):
    cases = [('tiny', 80), ('wave', 8), ('mp3', 4), ('vorbis', 4), ('convert', 4)]
    if smoke:
        return [(6, 1, case, 2, False) for case, _ in cases]
    rows = [(cpus, hosts, case, loops, False) for cpus in [1, 6] for hosts in [1, 2] for case, loops in cases]
    rows += [(6, 2, case, 4, True) for case in ['wave', 'convert']]
    return rows


def verify_probe(work):
    config = json.loads((work / 'run-config.json').read_text())
    package, fixture = work / 'probe', work / FIXTURE.name
    env = os.environ | dict(RUSTUP_TOOLCHAIN='1.95.0', CARGO_TARGET_DIR=config['target'], CARGO_NET_OFFLINE='true', CC='/usr/bin/clang', CXX='/usr/bin/clang++')
    originals = {str(p.relative_to(package / 'src')): p.read_bytes() for p in (package / 'src').rglob('*') if p.is_file()}
    source = fixture.read_text()
    rows = [
        ('probe-single-owner', 'for id in 0..host_count {', 'for id in 0..1 {', False),
        ('probe-pcm-corrupted', 'for audio in decoded {', 'for mut audio in decoded { audio.samples[0] = 1.;', False),
        ('probe-background-overlap-lost', 'BACKGROUND_ENTERED.fetch_add(1, Ordering::AcqRel);', 'let _ = &BACKGROUND_ENTERED;', True),
    ]
    receipts = []
    try:
        shutil.rmtree(package / 'src')
        shutil.copytree(work / 'candidate-source', package / 'src')
        for name, before, after, refill in rows:
            try:
                assert source.count(before) == 1, (name, 'anchor drift')
                fixture.write_text(source.replace(before, after))
                binary = gate.src.build(package, work, env, name, True)
                log = work / (name + '-test.log')
                run_env = env | dict(MIGO_TOPOLOGY_CASE='wave', MIGO_TOPOLOGY_LOOPS='1', MIGO_TOPOLOGY_HOSTS='2', MIGO_TOPOLOGY_REFILL=str(int(refill)))
                status = gate.whole.support.clock.common.run(['taskset', '-c', ','.join(map(str, sorted(os.sched_getaffinity(0))[:6])), str(binary), '--ignored', '--exact', TEST, '--nocapture'], log, package, run_env, timeout=30)
                assert status == 101 and all(x in log.read_text() for x in ['running 1 test', 'panicked at', '1 failed']), (name, status, log)
                receipts.append(dict(name=name, exit=status, test=TEST, binary_sha256=gate.whole.support.codecs.sha(binary), fresh_rust=True))
                binary.unlink()
                print('executed probe assertion rejected:', name, flush=True)
            finally:
                fixture.write_text(source)
    finally:
        shutil.rmtree(package / 'src')
        for name, value in originals.items():
            target = package / 'src' / name
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_bytes(value)
    gate.whole.verify_inputs(work, config['source_inputs'])
    (work / 'probe-controls.json').write_text(json.dumps(receipts, indent=2) + '\n')


def measure(work, smoke=False):
    config = json.loads((work / 'run-config.json').read_text())
    gate.whole.verify_inputs(work, config['source_inputs'])
    for row in config['programs'].values():
        assert gate.whole.support.codecs.sha(Path(row['path'])) == row['sha256']
    cpus = sorted(os.sched_getaffinity(0))
    assert len(cpus) >= 6
    samples = []
    for cpu_count, hosts, case, loops, refill in scenarios(smoke):
        scenario = f'{cpu_count}cpu-{hosts}host-{case}-refill{int(refill)}'
        for block in range(1 if smoke else 6):
            # Symmetric blocks rotate the middle arm instead of always placing
            # one implementation at the warmest position.
            names = ['before', 'candidate', 'legacy']
            k = block % 3
            names = names[k:] + names[:k]
            order = names + names[::-1]
            for offset, arm in enumerate(order):
                label = f'{scenario}-{block}-{offset}-{arm}'
                log, metrics = work / (label + '.log'), work / (label + '-time.json')
                env = os.environ | dict(MIGO_TOPOLOGY_CASE=case, MIGO_TOPOLOGY_LOOPS=str(loops), MIGO_TOPOLOGY_HOSTS=str(hosts), MIGO_TOPOLOGY_REFILL=str(int(refill)))
                command = ['/usr/bin/time', '-f', '{"peak_rss_kib":%M}', '-o', str(metrics), 'taskset', '-c', ','.join(map(str, cpus[:cpu_count])), config['programs'][arm]['path'], '--ignored', '--exact', TEST, '--test-threads=1', '--nocapture']
                with log.open('w') as output:
                    status = subprocess.run(command, cwd=work, env=env, stdout=output, stderr=subprocess.STDOUT, timeout=120).returncode
                assert status == 0 and '1 passed; 0 failed' in log.read_text(), log
                match = re.search(r'MIGO_TOPOLOGY (\{.*\})', log.read_text())
                assert match, log
                row = json.loads(match[1])
                assert row['pcm_checked'] and row['io_workers'] == max(2, cpu_count)
                assert (row['case'], row['hosts'], row['loops'], row['refill']) == (case, hosts, loops, refill)
                if case in ['mp3', 'convert']:
                    assert row['rate'] == (44100 if case == 'mp3' else 48000), 'native bypass mislabeled as conversion'
                assert row['foreground_count'] == (loops * 16 if refill else 0)
                row.update(arm=arm, block=block, offset=offset, cpu_count=cpu_count, scenario=scenario, peak_rss_kib=json.loads(metrics.read_text())['peak_rss_kib'], log=log.name)
                samples.append(row)
        print('topology measured', scenario, flush=True)
    rng = random.Random(20261031)
    summary = {}
    for scenario in dict.fromkeys(row['scenario'] for row in samples):
        selected = [row for row in samples if row['scenario'] == scenario]
        assert len({(row['pcm_bits'], row['samples'], row['rate']) for row in selected}) == 1
        comparisons = {}
        for base in ['before', 'legacy']:
            comparisons[base] = {}
            for metric in ['cpu_ns', 'wall_ns']:
                ratios = []
                for block in sorted({row['block'] for row in selected}):
                    values = {arm: statistics.mean(row[metric] for row in selected if row['block'] == block and row['arm'] == arm) for arm in ['candidate', base]}
                    ratios.append(values['candidate'] / values[base])
                boot = sorted(statistics.mean(rng.choices(ratios, k=len(ratios))) for _ in range(10000))
                comparisons[base][metric] = dict(ratio=statistics.mean(ratios), ci95=[boot[249], boot[9749]], block_ratios=ratios)
        summary[scenario] = dict(comparisons=comparisons,
            rss_kib={arm: statistics.median(row['peak_rss_kib'] for row in selected if row['arm'] == arm) for arm in ['candidate', 'before', 'legacy']},
            foreground_p99_ns={arm: statistics.median(row['foreground_p99_ns'] for row in selected if row['arm'] == arm) for arm in ['candidate', 'before', 'legacy']})
    result = dict(summary=summary, samples=samples, fresh_processes=len(samples), complete_pcm_bits_checked=True, warm_common_executor=True, smoke=smoke, device_or_shipping_acceptance=False, whole_game_peak=False)
    (work / ('smoke.json' if smoke else 'performance.json')).write_text(json.dumps(result, indent=2) + '\n')
    print(json.dumps({name: row['comparisons']['before']['cpu_ns'] for name, row in summary.items()}, indent=2), flush=True)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--target-dir', type=Path)
    parser.add_argument('--before-sources', type=Path)
    parser.add_argument('--legacy-sources', type=Path)
    parser.add_argument('--step-cap', type=int, default=128)
    parser.add_argument('--prototype', action='store_true')
    parser.add_argument('--prepare-only', action='store_true')
    parser.add_argument('--run-prepared', action='store_true')
    parser.add_argument('--verify-probe', action='store_true')
    parser.add_argument('--smoke', action='store_true')
    args = parser.parse_args()
    assert not (args.prepare_only and args.run_prepared)
    if not args.run_prepared:
        assert args.target_dir and args.before_sources and args.legacy_sources
        prepare(args.output, args.target_dir, args.before_sources, args.legacy_sources, args.step_cap, args.prototype)
    if args.verify_probe:
        verify_probe(args.output)
    elif not args.prepare_only:
        measure(args.output, args.smoke)
