#!/usr/bin/env python3
"""Verify complete PCM before measuring bounded whole jobs against pull+sinc.

The product is recorded separately: codec presentation and the selected SRC
profile can differ. Ratios use only bit-identical complete outputs.
Use a single fresh release binary, pinned CPU, rotated ABBA/BAAB, complete result
destruction and process RSS. No shipping-profile/device/default-codec claim.
"""
from __future__ import annotations
import argparse
import importlib.util
import json
import os
from pathlib import Path
import shutil
import statistics
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / 'scripts/testing'))
import check_audio_complete_codecs as complete

codecs = complete.whole.support.codecs
FIXTURE = 'scripts/testing/fixtures/audio-codec-probe/whole_file_benchmark.rs'


def prepare(work, target, rubato):
    package, env, frozen, libraries, resampling = complete.prepare(work, target, rubato)
    for relative in ['scripts/bench/audio-whole-file-performance.py', 'scripts/bench/audio-codec-performance.py',
                     'scripts/bench/audio-codec-startup.py']:
        frozen[relative] = codecs.sha(ROOT / relative)
        saved = work / 'source-inputs' / relative
        saved.parent.mkdir(parents=True, exist_ok=True)
        saved.write_bytes((ROOT / relative).read_bytes())
    (work / 'source-inputs.json').write_text(json.dumps(frozen, indent=2) + '\n')
    source = work / 'source/whole_file_benchmark.rs'
    source.write_bytes((ROOT / FIXTURE).read_bytes())
    addition = '\n#[path=' + json.dumps(str(source)) + '] pub mod codec_benchmark;\n'
    lib = package / 'src/lib.rs'
    lib.write_text(lib.read_text() + addition)
    old = work / 'lib-addition.txt'
    old.write_text(old.read_text() + addition)
    binaries = package / 'src/bin'
    binaries.mkdir()
    (binaries / 'audio-whole-file-benchmark.rs').write_text('fn main() { audio::codec_benchmark::main(); }\n')
    # Cargo's isolated release profile is explicit; this is not Root's z/fat/abort profile.
    manifest = package / 'Cargo.toml'
    manifest.write_text(manifest.read_text() + '\n[profile.release]\nopt-level=3\nlto=false\ncodegen-units=16\ndebug=0\npanic="unwind"\n')
    (work / 'retained-sources.json').write_text(json.dumps({str(p.relative_to(work)): codecs.sha(p)
        for tree in [package / 'src', work / 'source'] for p in tree.rglob('*') if p.is_file()}, indent=2) + '\n')
    return package, env, frozen, libraries, resampling


def build(work, package, env, label):
    log = work / (label + '-build.jsonl')
    assert complete.whole.support.clock.common.run(['cargo', 'build', '--release', '--offline', '--locked',
        '--bin', 'audio-whole-file-benchmark', '--message-format=json'], log, package, env, timeout=300) == 0, log
    artifacts = []
    for line in log.read_text().splitlines():
        try:
            row = json.loads(line)
        except ValueError:
            continue
        if row.get('reason') == 'compiler-artifact' and row.get('executable') and row['target']['name'] == 'audio-whole-file-benchmark':
            assert row['fresh'] is False
            artifacts.append(Path(row['executable']))
    assert len(artifacts) == 1
    binary = work / (label + '-benchmark')
    shutil.copy2(artifacts[0], binary)
    return binary


def cases(work):
    dest = work / 'music'
    dest.mkdir()
    wav = dest / 'source.wav'
    codecs.fixture_pcm(wav, 48000, 2, 480137)
    rows = []
    for name, suffix, args in [('mp3', 'mp3', ['-c:a', 'libmp3lame', '-b:a', '128k']),
        ('vorbis', 'ogg', ['-c:a', 'libvorbis', '-q:a', '4']),
        ('adts', 'aac', ['-c:a', 'aac', '-aac_pns', '0', '-f', 'adts']),
        ('m4a', 'm4a', ['-c:a', 'aac', '-aac_pns', '0', '-movie_timescale', '48000'])]:
        codecs.run(['ffmpeg', '-nostdin', '-v', 'error', '-i', str(wav), *args, str(dest / ('music.' + suffix))],
            dest / (name + '-encode.log'), cwd=work)
    for name, format, path, rate, channels, frames in [
        ('short-wave', 'wave', work / 'complete-fixtures/1.wav', 44100, 2, 9737),
        ('short-cbr', 'mp3', work / 'complete-fixtures/stereo-cbr.mp3', 44100, 2, 6912),
        ('short-vbr', 'mp3', work / 'complete-fixtures/mono-vbr.mp3', 22050, 1, 5184),
        ('short-vorbis', 'vorbis', work / 'vorbis/fixtures/1.ogg', 48000, 2, 9737),
        ('short-adts', 'adts', work / 'complete-fixtures/adts-0.aac', 32000, 1, 8192),
        ('tiny-m4a', 'm4a', work / 'complete-fixtures/0-front.m4a', 48000, 1, 17),
        ('reverse-tail-m4a', 'm4a', work / 'complete-fixtures/2-tail-reverse.m4a', 48000, 1, 240137),
        ('music-wave', 'wave', wav, 48000, 2, 480137),
        ('music-mp3-gapless', 'mp3', dest / 'music.mp3', 48000, 2, 480137),
        ('music-vorbis', 'vorbis', dest / 'music.ogg', 48000, 2, 480137),
        ('music-adts', 'adts', dest / 'music.aac', 48000, 2, 481280),
        ('music-m4a-tail', 'm4a', dest / 'music.m4a', 48000, 2, 480137),
    ]:
        for target in [rate, 44100 if rate == 48000 else 48000]:
            rows.append(dict(name=f'{name}-{target}', format=format, source=str(path), rate=rate, channels=channels,
                frames=(frames * target + rate - 1) // rate, target=target,
                iterations=32 if frames < 20000 else 2,
                arms=['pull', 'job', 'product'] if format in ['wave', 'mp3', 'vorbis'] else ['pull', 'job']))
    assert len(rows) == 24
    (work / 'cases.json').write_text(json.dumps(rows, indent=2) + '\n')
    return rows


def verify(work, binary, rows, env, setup):
    records = []
    directory = work / 'verification'
    directory.mkdir()
    for row in rows:
        results = {}
        for arm in row['arms']:
            label = row['name'] + '-' + arm
            pcm = directory / (label + '.f32le')
            command = [str(binary), arm, row['format'], row['source'], str(row['target']), '1', str(pcm)]
            result = subprocess.run(command, env=env, text=True, capture_output=True, timeout=90, preexec_fn=setup)
            (directory / (label + '.log')).write_text(result.stdout + result.stderr)
            assert result.returncode == 0, (label, 'verification failed')
            shape = json.loads(result.stdout)
            assert shape['verification_only'] is True and shape['sample_rate'] == row['target'] and shape['channels'] == row['channels']
            assert pcm.stat().st_size == shape['samples'] * 4
            if arm != 'product':
                assert shape['frames'] == row['frames'], (label, shape, row)
            results[arm] = dict(pcm_sha256=codecs.sha(pcm), pcm_bytes=pcm.stat().st_size, pcm_pruned_after_verification=True, **shape)
        assert results['pull']['pcm_sha256'] == results['job']['pcm_sha256'], (row['name'], 'complete PCM differs')
        product = results.get('product')
        classification = None if not product else ('different_presentation' if product['frames'] != results['job']['frames']
            else 'different_dsp' if row['target'] != row['rate'] else 'exact_pcm' if product['pcm_sha256'] == results['job']['pcm_sha256'] else 'different_pcm')
        records.append(dict(case=row['name'], results=results, candidate_complete_pcm_identical=True,
            product_comparison=classification, input_sha256=codecs.sha(Path(row['source']))))
        (work / 'verification.json').write_text(json.dumps(records, indent=2) + '\n')
        for arm in row['arms']:
            (directory / (row['name'] + '-' + arm + '.f32le')).unlink()
    return records


def mutations(work, package, env):
    support = complete.whole.support
    source = work / 'source/whole_file_benchmark.rs'
    original = source.read_text()
    prefix = 'codec_benchmark::tests::'
    equivalence = prefix + 'real_complete_controls_keep_all_five_formats_and_sinc_tails_bit_exact'
    quality = prefix + 'product_and_candidate_sinc_controls_share_alias_rejection'
    rows = []
    def change(name, before, after, test):
        assert original.count(before) == 1, (name, 'mutation anchor drift')
        rows.append((name, source.name, before, after, test))
    change('bench-pull-bypasses-rate-conversion', 'if raw.sample_rate == target {', 'if true {', equivalence)
    change('bench-pull-discards-sinc-output', 'samples.extend_from_slice(&scratch);', 'samples.resize(samples.len() + scratch.len(), 0.0);', equivalence)
    change('bench-pull-marks-final-offer-eof', 'position == raw.samples.len(),', 'true,', equivalence)
    change('bench-pull-adds-one-tail-frame', 'let mut samples = Vec::with_capacity(count as usize);', 'let mut samples = vec![0.0; raw.channels as usize];', equivalence)
    change('bench-job-uses-fixed-target', 'WholeFileJob::new(data, format, target, LIMIT)', 'WholeFileJob::new(data, format, 44100, LIMIT)', equivalence)
    # Product and pull now share the selected sinc quality; replacing one with
    # the other is no longer a fault. Keep actual quality/PCM-loss controls.
    change('bench-product-returns-silence', '"product" => crate::decoder::decode_for_rate(data, target).map_err(|error| error.to_string()),', '"product" => crate::decoder::decode_for_rate(data, target).map(|mut a| { a.samples.fill(0.0); a }).map_err(|error| error.to_string()),', quality)
    return support.faults(work, package, env, {source.name: original}, rows,
        lambda name: complete.whole.build_candidate(package, work, env, name), work / 'source', 'native')


def measure(work, binary, rows, env, setup, blocks):
    records = []
    for block in range(blocks):
        ordered = rows[block % len(rows):] + rows[:block % len(rows)]
        for row in ordered:
            arms = ['pull', 'job', 'job', 'pull'] if block % 2 == 0 else ['job', 'pull', 'pull', 'job']
            if 'product' in row['arms']:
                arms.insert(0 if block % 2 == 0 else len(arms), 'product')
            for position, arm in enumerate(arms):
                label = f'{block}-{row["name"]}-{position}-{arm}'
                process = work / (label + '-process.json')
                result = subprocess.run(['/usr/bin/time', '-f', '{"user_s":%U,"system_s":%S,"peak_rss_kib":%M,"wall_s":%e}', '-o', str(process),
                    str(binary), arm, row['format'], row['source'], str(row['target']), str(row['iterations'])],
                    env=env, capture_output=True, text=True, preexec_fn=setup, timeout=90)
                (work / (label + '.log')).write_text(result.stdout + result.stderr)
                assert result.returncode == 0, label
                data = json.loads(result.stdout)
                assert data['verification_only'] is False and data['sample_rate'] == row['target'] and data['channels'] == row['channels']
                if arm != 'product':assert data['frames_per_iteration'] == row['frames']
                records.append(dict(case=row['name'], arm=arm, block=block, position=position, **data,
                    process=json.loads(process.read_text())))
        (work / 'raw.json').write_text(json.dumps(records, indent=2) + '\n')
        print(f'whole-file ABBA/BAAB {block + 1}/{blocks}', flush=True)
    results = []
    for row in rows:
        group = [r for r in records if r['case'] == row['name']]
        arms = {arm: dict(samples=sum(r['arm'] == arm for r in group),
            decode_and_drop_ns=statistics.median(r['decode_and_drop_total_ns'] / r['iterations'] for r in group if r['arm'] == arm),
            process_peak_rss_kib=statistics.median(r['process']['peak_rss_kib'] for r in group if r['arm'] == arm)) for arm in row['arms']}
        paired = []
        for block in range(blocks):
            ns = {arm: statistics.median(r['decode_and_drop_total_ns'] / r['iterations'] for r in group if r['arm'] == arm and r['block'] == block) for arm in ['pull', 'job']}
            paired.append(ns['job'] / ns['pull'])
        results.append(dict(case=row['name'], arms=arms, job_over_pull_block_ratios=paired,
            job_over_pull=statistics.median(paired), complete_pcm_equivalence=True))
    (work / 'performance.json').write_text(json.dumps(results, indent=2) + '\n')
    return results


def standalone(work, env):
    directory = work / 'standalone'
    directory.mkdir()
    original_lock = (ROOT / 'engine/Cargo.lock').read_bytes()
    programs = []
    selected = None
    wave = work / 'complete-fixtures/1.wav'
    for label, simd, patched, benchmark, phases in [
        ('published', False, False, False, False), ('scalar', False, True, True, False),
        ('simd', True, True, True, False), ('phases', True, True, False, True),
    ]:
        sub = directory / label
        sub.mkdir()
        binary = codecs.make_package(ROOT, sub, env, simd, patched, benchmark, phases)
        programs.append(dict(name=label, path=str(binary), sha256=codecs.sha(binary), fresh=False, pruned_after_execution=False))
        if benchmark:
            for arm in ['legacy', 'candidate']:
                result = subprocess.run([str(binary), arm, str(wave), '2'], capture_output=True, text=True, env=env, timeout=30)
                (sub / (arm + '-test.log')).write_text(result.stdout + result.stderr)
                assert result.returncode == 0
                row = json.loads(result.stdout)
                assert (row['sample_rate'], row['channels'], row['frames_per_iteration']) == (44100, 2, 9737)
            if simd:selected = sub
        else:
            result = codecs.probe(binary, 'candidate', wave, sub)
            assert result['ok'] and result['frames'] == 9737
            if phases:
                text = (sub / 'candidate.log').read_text()
                markers = [line.removeprefix('W09_PHASES ') for line in text.splitlines() if line.startswith('W09_PHASES ')]
                assert len(markers) == 1 and len(json.loads(markers[0])) == 8
    package = selected / 'probe'
    def build_test(label):
        log = directory / (label + '-build.jsonl')
        assert complete.whole.support.clock.common.run(['cargo', 'test', '--bin', 'migo-codec-candidate', '--offline', '--locked',
            '--features', 'simd,metadata-budgets', '--no-run', '--message-format=json'], log, package, env, timeout=300) == 0, log
        artifacts = []
        for line in log.read_text().splitlines():
            try:r = json.loads(line)
            except ValueError:continue
            if r.get('reason') == 'compiler-artifact' and r.get('executable') and r['target']['name'] == 'migo-codec-candidate':
                assert r['fresh'] is False
                artifacts.append(Path(r['executable']))
        assert len(artifacts) == 1
        binary = directory / (label + '-tests')
        shutil.copy2(artifacts[0], binary)
        return binary
    binary = build_test('positive')
    log = directory / 'positive-tests.log'
    assert complete.whole.support.clock.common.run([str(binary)], log, package, env, timeout=300) == 0, log
    import re
    match = re.search(r'test result: ok\. (\d+) passed; 0 failed; (\d+) ignored;', log.read_text())
    assert match and (int(match[1]), int(match[2])) == (72, 0)
    programs.append(dict(name='positive',path=str(binary),sha256=codecs.sha(binary),fresh=False,pruned_after_execution=False))
    main = package / 'src/main.rs'
    original = main.read_text()
    copied = directory / 'private-audio-src'
    shutil.copytree(ROOT / 'engine/crates/audio/src', copied)
    decoder = copied / 'decoder/mod.rs'
    old_decoder = decoder.read_text()
    controls = [
        ('standalone-no-counting-allocator', 'decoder::pcm::tests::pages_release_as_their_last_samples_are_copied'),
        ('standalone-raw-rate-dispatch', 'decoder::mp3::job::tests::actual_rate_dispatch_uses_collection_and_dsp_in_the_same_mp3_job'),
    ]
    receipts = []
    for name, test in controls:
        try:
            if name == 'standalone-no-counting-allocator':
                assert original.count('#[global_allocator]') == 1
                main.write_text(original.replace('#[global_allocator]', ''))
            else:
                before = 'whole::WholeDecodeJob::new(data, Some(target_rate))?'
                assert old_decoder.count(before) == 1
                decoder.write_text(old_decoder.replace(before, 'whole::WholeDecodeJob::new(data, Some(44100))?'))
                before = json.dumps(str(ROOT / 'engine/crates/audio/src/decoder/mod.rs'))
                assert original.count(before) == 1
                main.write_text(original.replace(before, json.dumps(str(decoder))))
            mutant = build_test(name)
            textlog = directory / (name + '-test.log')
            status = complete.whole.support.clock.common.run([str(mutant), '--exact', test, '--nocapture'], textlog, package, env, timeout=30)
            text = textlog.read_text()
            assert status == 101 and all(token in text for token in ['running 1 test', 'panicked at', '1 failed'])
            digest = codecs.sha(mutant)
            programs.append(dict(name=name,path=str(mutant),sha256=digest,fresh=False,pruned_after_execution=True))
            receipts.append(dict(name=name,test=test,exit=status,binary_sha256=digest))
            mutant.unlink()
        finally:
            main.write_text(original)
            decoder.write_text(old_decoder)
    for label in ['published','scalar','simd','phases']:
        sub = directory / label
        assert all(codecs.sha(ROOT / p) == h and codecs.sha(sub / 'source-inputs' / p) == h for p,h in json.loads((sub / 'source-inputs.json').read_text()).items())
    assert (ROOT / 'engine/Cargo.lock').read_bytes() == original_lock
    (directory / 'executed-programs.json').write_text(json.dumps(programs, indent=2) + '\n')
    summary = dict(release_configurations=4,passed=int(match[1]),ignored=int(match[2]),controls=receipts,compiled_controls=2,programs=len(programs),source_copies_restored=True,root_inputs_unchanged=True)
    (directory / 'summary.json').write_text(json.dumps(summary, indent=2) + '\n')
    print('standalone scaffold complete',summary,flush=True)
    return summary


def main(work, target, rubato, blocks, preflight):
    package, env, frozen, libraries, resampling = prepare(work, target, rubato)
    cpu = min(os.sched_getaffinity(0))
    def setup():
        codecs.limits()
        os.sched_setaffinity(0, {cpu})
    (work / 'environment.json').write_text(json.dumps(dict(cpu=cpu, allowed_cpus=sorted(os.sched_getaffinity(0)),
        kernel=os.uname().release, cpuinfo=Path('/proc/cpuinfo').read_text(), profile=dict(opt_level=3,lto=False,codegen_units=16,panic='unwind'),scope=__doc__), indent=2) + '\n')
    standalone_summary = None if preflight else standalone(work, dict(env, CARGO_TARGET_DIR='/tmp/migo-w09-codec-target'))
    for profile in (['debug'] if preflight else ['release', 'debug']):
        label = 'positive-' + profile
        binary = complete.whole.build_candidate(package, work, env, label, release=profile == 'release')
        log = work / (label + '-tests.log')
        args = [str(binary)] + (['codec_benchmark::tests::'] if preflight else [])
        assert complete.whole.support.clock.common.run(args, log, package, env, timeout=300) == 0, log
        assert f'{3 if preflight else 430} passed; 0 failed; {0 if preflight else 2} ignored;' in log.read_text()
        complete.whole.support.program(work, label, 'native')
    binary = build(work, package, env, 'positive')
    rows = cases(work)
    verification = verify(work, binary, rows, env, setup)
    controls = [] if preflight else mutations(work, package, env)
    performance = [] if preflight else measure(work, binary, rows, env, setup, blocks)
    complete.whole.verify_inputs(work, frozen)
    assert all(codecs.sha(work / p) == h for p, h in json.loads((work / 'retained-sources.json').read_text()).items())
    for row in json.loads((work / 'library-source-provenance.json').read_text()):
        assert all(codecs.sha(libraries[row['crate']] / p) == h for p, h in row['source_files'].items())
    for name, row in resampling.items():
        assert all(codecs.sha(work / 'resampling-vendor' / f"{name}-{row['version']}" / p) == h for p, h in row['source_files'].items())
    summary = dict(preflight=preflight, benchmark_binary_sha256=codecs.sha(binary), source_inputs=len(frozen),
        test_profiles=['debug'] if preflight else ['release','debug'], tests_per_profile=3 if preflight else 430,
        ignored_per_profile=0 if preflight else 2, controls=controls, compiled_controls=len(controls),
        cases=len(rows), candidate_pcm_comparisons=len(verification), blocks=0 if preflight else blocks,
        source_copies_restored=True, root_inputs_unchanged=True, product_codec_switched=False,
        production_profile_measured=False, standalone=standalone_summary, performance=performance)
    (work / 'summary.json').write_text(json.dumps(summary, indent=2) + '\n')
    print('whole-file complete:',len(rows),'cases;',len(controls),'executed assertion controls',flush=True)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--target-dir', type=Path, required=True)
    parser.add_argument('--rubato-cargo-home', type=Path, required=True)
    parser.add_argument('--blocks', type=int, default=6, choices=range(4, 17))
    parser.add_argument('--preflight', action='store_true')
    args = parser.parse_args()
    main(args.output.resolve(), args.target_dir.resolve(), args.rubato_cargo_home.resolve(), args.blocks, args.preflight)
