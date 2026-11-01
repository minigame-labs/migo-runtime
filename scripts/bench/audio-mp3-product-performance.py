#!/usr/bin/env python3
"""Measure bounded product MP3 against the same mature native full reader.

Both paths use the pinned minimp3 core/trim kernel and verify every frozen PCM
sample before timing. Native-rate only; this host experiment is not a scheduler,
shipping-profile/device, playbackRate DSP or whole-game peak-memory acceptance.
"""
from __future__ import annotations
import argparse
import json
import os
from pathlib import Path
import random
import re
import statistics
import subprocess
import sys
import tomllib

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / 'scripts/testing'))
import check_audio_product_mp3 as product

FIXTURE = ROOT / 'scripts/testing/fixtures/audio-codec-probe/product_mp3_benchmark.rs'


def environment(work, target):
    env = os.environ.copy()
    env.update(RUSTUP_TOOLCHAIN='1.95.0', CARGO_TARGET_DIR=str(target), CARGO_NET_OFFLINE='true',
        CC='/usr/bin/clang', CXX='/usr/bin/clang++', CFLAGS='-DMIGO_MINIMP3_TEST',
        MIGO_MP3_TEST_FIXTURES=str(work / 'probe/src/decoder/mp3/fixtures'))
    for key in ['RUSTFLAGS', 'CARGO_ENCODED_RUSTFLAGS', 'CARGO_BUILD_TARGET']:
        env.pop(key, None)
    return env


def prepare(work, target):
    package, env, frozen = product.whole.support.clock.prepare(work, target,
        product.whole.extra_inputs() + [Path(__file__), FIXTURE])
    copied = work / 'native/minimp3-sys-0.6.0'
    product.verify_native(work, copied)
    rows = tomllib.loads((ROOT / 'engine/Cargo.lock').read_text())['package']
    version = next(r['version'] for r in rows if r['name'] == 'libc')
    manifest = (package / 'Cargo.toml').read_text()
    manifest = manifest.replace('[dev-dependencies]\n', f'[dev-dependencies]\nlibc="={version}"\n', 1)
    (package / 'Cargo.toml').write_text(manifest)
    lib = package / 'src/lib.rs'
    extra = work / 'product_mp3_benchmark.rs'
    extra.write_bytes(FIXTURE.read_bytes())
    lib.write_text(lib.read_text() + f'\n#[cfg(test)] #[path={json.dumps(str(extra))}] mod product_mp3_benchmark;\n')
    env = environment(work, target)
    assert product.whole.support.clock.common.run(['cargo', 'metadata', '--offline', '--format-version', '1'],
        work / 'benchmark-metadata.jsonl', package, env) == 0
    binary = product.build(package, work, env, 'same-codec-benchmark', True)
    config = dict(binary=str(binary), binary_sha256=product.whole.support.codecs.sha(binary), target=str(target),
        fixture_sha256=product.whole.support.codecs.sha(FIXTURE), source_inputs=frozen,
        profile='isolated O3/noLTO/cg16/unwind, cfg(test) Rust tracing and native allocation test hooks; no codec entry hooks',
        scope='same minimp3 core and trim kernel; product direct f32 collection vs mature full i16 buffer then f32 conversion')
    (work / 'run-config.json').write_text(json.dumps(config, indent=2) + '\n')
    print('benchmark binary prepared', config['binary_sha256'], flush=True)


def measure(work):
    config = json.loads((work / 'run-config.json').read_text())
    binary = Path(config['binary'])
    assert product.whole.support.codecs.sha(binary) == config['binary_sha256']
    frozen = config['source_inputs']
    assert all(product.whole.support.codecs.sha(ROOT / name) == value == product.whole.support.codecs.sha(work / 'source-inputs' / name)
        for name, value in frozen.items())
    cpu = min(os.sched_getaffinity(0))
    env = environment(work, Path(config['target']))
    samples = []
    for case, loops in [('long', 12), ('tiny', 300), ('mpeg2', 60), ('untagged', 100)]:
        for block in range(6):
            order = ['product', 'mature', 'mature', 'product'] if block % 2 == 0 else ['mature', 'product', 'product', 'mature']
            for offset, mode in enumerate(order):
                label = f'{case}-{block}-{offset}-{mode}'
                process_env = env | dict(MIGO_MP3_BENCH_CASE=case, MIGO_MP3_BENCH_MODE=mode, MIGO_MP3_BENCH_LOOPS=str(loops))
                log = work / (label + '.log')
                metrics = work / (label + '-time.json')
                command = ['/usr/bin/time', '-f', '{"peak_rss_kib":%M}', '-o', str(metrics), 'taskset', '-c', str(cpu), str(binary), '--ignored', '--exact',
                    'product_mp3_benchmark::same_codec_product_and_mature_reader', '--test-threads=1', '--nocapture']
                with log.open('w') as stream:
                    child = subprocess.Popen(command, cwd=work / 'probe', env=process_env, stdout=stream, stderr=subprocess.STDOUT)
                    pid, status, usage = os.wait4(child.pid, 0)
                    child.returncode = os.waitstatus_to_exitcode(status)
                assert child.returncode == 0 and pid == child.pid, log
                text = log.read_text()
                assert '1 passed; 0 failed' in text
                match = re.search(r'MIGO_MP3_BENCH (\{.*\})', text)
                assert match
                row = json.loads(match[1]);assert row['pcm_checked'] and row['loops'] == loops
                row.update(block=block, offset=offset, peak_rss_kib=json.loads(metrics.read_text())['peak_rss_kib'],
                    launcher_peak_rss_kib=usage.ru_maxrss,
                    process_cpu_seconds=usage.ru_utime + usage.ru_stime, log=log.name)
                samples.append(row)
            print('paired block', case, block, flush=True)
    report = {}
    rng = random.Random(20261031)
    for case in ['long', 'tiny', 'mpeg2', 'untagged']:
        ratios = []
        peaks = {}
        for mode in ['product', 'mature']:
            peaks[mode] = statistics.median(r['peak_rss_kib'] for r in samples if r['case'] == case and r['mode'] == mode)
        for block in range(6):
            time = {mode: statistics.mean(r['cpu_ns'] / r['loops'] for r in samples
                if r['case'] == case and r['block'] == block and r['mode'] == mode) for mode in ['product', 'mature']}
            ratios.append(time['product'] / time['mature'])
        bootstrap = sorted(statistics.median(rng.choices(ratios, k=6)) for _ in range(5000))
        report[case] = dict(product_over_mature_cpu_median=statistics.median(ratios),
            paired_block_ratios=ratios, bootstrap_median_95=[bootstrap[125], bootstrap[4874]], peak_rss_kib=peaks)
    assert product.whole.support.codecs.sha(binary) == config['binary_sha256']
    assert all(product.whole.support.codecs.sha(ROOT / name) == value for name, value in frozen.items())
    result = dict(binary_sha256=config['binary_sha256'], cpu=cpu, fresh_processes=len(samples),
        complete_pcm_verified_processes=len(samples), blocks_per_case=6, samples=samples, report=report,
        profile=config['profile'], scope=config['scope'], shipping_or_device_claim=False)
    (work / 'summary.json').write_text(json.dumps(result, indent=2) + '\n')
    print(json.dumps(report, indent=2), flush=True)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--target-dir', type=Path)
    parser.add_argument('--prepare-only', action='store_true')
    parser.add_argument('--run-prepared', action='store_true')
    args = parser.parse_args()
    if not args.run_prepared:
        assert args.target_dir
        prepare(args.output.resolve(), args.target_dir.resolve())
    if not args.prepare_only:
        measure(args.output.resolve())
