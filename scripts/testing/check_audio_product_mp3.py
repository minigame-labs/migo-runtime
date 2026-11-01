#!/usr/bin/env python3
"""MP3 product output included delay/padding; a codec switch would lose free-format.

Select the pinned existing native codec's mature presentation kernel in the
bounded product cursor/job. Prove complete frozen PCM, target rates, framing,
quotas and retirement, including real C heap release. Only executed fresh Rust
assertions count as fault rejection; production shipping/device claims stay open.
"""
from __future__ import annotations
import argparse
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import tarfile
import tempfile
import tomllib
import check_audio_whole_file as whole
import check_audio_duration_scan as duration

ROOT = whole.ROOT
BENCH = duration.bench
NATIVE = ROOT / 'engine/third_party/minimp3-sys-0.6.0'
PATCHES = ROOT / 'engine/third_party/minimp3-sys-patches'
EXTRA = ROOT / 'scripts/testing/fixtures/audio-codec-probe/product_mp3_native.rs'
TESTS = 'decoder::mp3::job::product_tests::'


def verify_native(work, copied):
    name, version, checksum = (PATCHES / 'UPSTREAM').read_text().split()
    cache = Path(os.environ.get('CARGO_HOME', str(Path.home() / '.cargo'))) / 'registry/cache'
    archive = next(cache.glob(f'*/{name}-{version}.crate'))
    assert whole.support.codecs.sha(archive) == checksum
    with tempfile.TemporaryDirectory(prefix='migo-native-mp3-') as directory:
        stage = Path(directory)
        with tarfile.open(archive) as stream:
            stream.extractall(stage, filter='data')
        for patch in sorted(PATCHES.glob('*.patch')):
            whole.support.codecs.run(['patch', '--batch', '--forward', '--fuzz=0', '-p0', '-i', str(patch)],
                work / (patch.stem + '.log'), cwd=stage)
        snapshot = lambda base: {str(p.relative_to(base)): whole.support.codecs.sha(p)
            for p in base.rglob('*') if p.is_file()}
        expected = snapshot(stage / f'{name}-{version}')
        assert snapshot(NATIVE) == snapshot(copied) == expected
    proof = dict(archive_sha256=checksum, files=expected,
        patches={p.name: whole.support.codecs.sha(p) for p in sorted(PATCHES.glob('*.patch'))})
    (work / 'native-source-proof.json').write_text(json.dumps(proof, indent=2) + '\n')


def declarations(originals):
    rows = []
    job = 'decoder/mp3/job.rs'
    cursor = 'decoder/mp3/file.rs'
    native = '@native/minimp3.c'
    pres = 'decoder/mp3/presentation.rs'
    pcm = TESTS + 'gapless_product_dispatch_preserves_full_frozen_pcm_and_exact_target_lengths'
    reserve = TESTS + 'known_presentation_reserves_once_in_its_own_step_and_never_copies_final_pcm'
    cancel = TESTS + 'gapless_cancellation_at_bootstrap_reservation_decode_dsp_and_completion_releases_input'
    def change(name, path, before, after, test):
        assert originals[path].count(before) == 1, (name, 'mutation anchor drift')
        rows.append((name, path, before, after, test))
    change('product-mp3-tag-frame-not-skipped', cursor, 'if skip_tag {', 'if false {', pcm)
    change('product-mp3-known-hint-lost', job, 'if expected.is_some() {', 'if false {', reserve)
    change('product-mp3-reservation-decodes-without-yield', job,
        '                    owner.collector.step()?;\n                    self.phase = Phase::Decode(owner);\n                    return Ok(false);',
        '                    owner.collector.step()?;', reserve)
    change('product-mp3-native-delay-ignored', native, '    *expected = p->state.detected_samples;',
        '    p->state.to_skip = 0;\n    *expected = p->state.detected_samples;', pcm)
    change('product-mp3-padding-retained', '@native/minimp3/minimp3_ex.h',
        'out_samples = dec->detected_samples - dec->cur_sample;', 'out_samples = out_samples;', pcm)
    change('product-mp3-cursor-publishes-partial', pres,
        'if unsafe { migo_mp3_presentation_finish(self.native.as_ptr()) } < 0 {', 'if false {',
        'decoder::mp3::file::tests::declared_gapless_cursor_eof_rejects_partial_pcm_and_releases_input')
    change('product-mp3-native-quota-ignored', native,
        'p->state.detected_samples > MINIMP3_MIN(limit, UINT64_C(16777216))', '0',
        'decoder::mp3::file::tests::native_metadata_quota_rejects_before_any_borrowed_pcm_can_escape')
    change('product-mp3-job-cancel-ignored', job, 'if cancel.load(Ordering::Acquire) {', 'if false {', cancel)
    change('product-mp3-cancel-retains-owner', job, 'self.phase = Phase::Failed(ErrorCode::Cancelled);',
        'return Err(EngineError::from_detail(ErrorCode::Cancelled, "injected"));', cancel)
    change('product-mp3-target-fixed', job, 'ResampleJob::new(audio, rate)?', 'ResampleJob::new(audio, 44100)?', pcm)
    change('product-mp3-source-rate-relabelled', job,
        'owner.metadata.get_or_insert((sample_rate, channels));',
        'owner.metadata.get_or_insert((sample_rate + 1, channels));', pcm)
    change('product-mp3-fallback-loses-presentation', cursor,
        'if let Some(frame) = boundary {', 'if let Some(frame) = boundary.filter(|_| false) {',
        'decoder::mp3::file::tests::native_fallback_bootstraps_gapless_before_publishing_any_pcm')
    change('product-mp3-total-native-credit-disabled', cursor, '    *total = next;', '    let _ = next;',
        'decoder::mp3::file::tests::native_call_quota_is_terminal_and_cannot_publish_complete_audio')
    change('product-mp3-native-view-doubled', cursor, 'const NATIVE_BYTES: usize = 8192;',
        'const NATIVE_BYTES: usize = 16384;',
        'decoder::mp3::file::tests::native_probes_and_fallback_obey_each_step_budget_on_invalid_input')
    change('product-mp3-pcm-normalization-shifted', 'decoder/pcm.rs', 'f32::from(sample) / 32768.0',
        'f32::from(sample) / 32767.0', pcm)
    return rows


def build(package, work, env, label, release=False):
    # C and Rust must both rebuild, including for Rust-only mutants. Never
    # infer a native recompile from a reused Rust test artifact.
    assert whole.support.clock.common.run(['cargo', 'clean', '--offline', '--locked', '-p', 'minimp3-sys'],
        work / (label + '-clean.log'), package, env) == 0
    path = package / 'src/lib.rs'
    path.write_text(path.read_text())
    binary = whole.build_candidate(package, work, env, label, release)
    rows = []
    for line in (work / (label + '-build.jsonl')).read_text().splitlines():
        try:
            row = json.loads(line)
        except ValueError:
            continue
        if row.get('reason') == 'compiler-artifact' and row['target']['name'] == 'minimp3_sys':
            rows.append(row)
    assert len(rows) == 1 and rows[0]['fresh'] is False
    return binary


def main(work, target, candidate, minimal, rubato, preflight):
    if candidate:
        if minimal:
            package, env, frozen, libraries, resampling = whole.prepare_candidate(work, target, rubato)
            expected = 422
        else:
            package, env, frozen, libraries, resampling = BENCH.prepare(work, target, rubato)
            expected = 439
    else:
        extras = whole.extra_inputs() + [p for p in (ROOT / 'scripts/testing/fixtures/minimp3-presentation').rglob('*') if p.is_file()]
        package, env, frozen = whole.support.clock.prepare(work, target, extras)
        expected = 406
    copied = work / 'native/minimp3-sys-0.6.0'
    assert str(copied) in (package / 'Cargo.toml').read_text()
    verify_native(work, copied)
    proof = json.loads((work / 'native-source-proof.json').read_text())
    files = {path: package / 'src' / path for path in ['decoder/mp3/job.rs', 'decoder/mp3/file.rs',
        'decoder/mp3/presentation.rs', 'decoder/pcm.rs']}
    files.update({'@native/minimp3.c': copied / 'minimp3.c',
        '@native/minimp3/minimp3_ex.h': copied / 'minimp3/minimp3_ex.h'})
    originals = {name: p.read_text() for name, p in files.items()}
    rows = declarations(originals)
    programs, rejected, positives = [], [], {}
    def record(label, binary, role, pruned=False):
        programs.append(dict(name=label, role=role, sha256=whole.support.codecs.sha(binary),
            fresh_rust=True, fresh_native=True, pruned=pruned))
        if pruned:
            binary.unlink()
    for profile in (['debug'] if preflight else ['release', 'debug']):
        label = 'positive-' + profile
        binary = build(package, work, env, label, profile == 'release')
        log = work / (label + '-tests.log')
        assert whole.support.clock.common.run([str(binary)], log, package, env, timeout=300) == 0, log
        assert f'test result: ok. {expected} passed; 0 failed; 2 ignored;' in log.read_text()
        inventory = whole.support.clock.common.run([str(binary), '--list'], work / (label + '-inventory.log'), package, env)
        assert inventory == 0
        names = (work / (label + '-inventory.log')).read_text()
        assert all(test + ': test' in names for _, _, _, _, test in rows), 'a control test is not compiled'
        record(label, binary, 'full-positive')
        positives[profile] = dict(passed=expected, ignored=2)
    if not candidate and not preflight:
        for name, path, before, after, test in rows:
            try:
                files[path].write_text(originals[path].replace(before, after))
                binary = build(package, work, env, name)
                log = work / (name + '-test.log')
                status = whole.support.clock.common.run([str(binary), '--exact', test, '--nocapture'], log, package, env, timeout=120)
                observed = log.read_text()
                assert status == 101 and 'panicked at' in observed and '1 failed' in observed and 'running 1 test' in observed, (name, status, log)
                record(name, binary, 'runtime-assertion', True)
                rejected.append(dict(name=name, test=test, exit=status, binary_sha256=programs[-1]['sha256']))
                print('actual product assertion rejected:', name, flush=True)
            finally:
                files[path].write_text(originals[path])
        # Instrumented C counters run only one selected Rust test at a time;
        # the complete product suite above has no global native test counters.
        lib = package / 'src/lib.rs'
        original_lib = lib.read_text()
        private_extra = work / 'source/product_mp3_native.rs'
        private_extra.parent.mkdir(exist_ok=True)
        private_extra.write_bytes(EXTRA.read_bytes())
        instrument = env.copy()
        instrument.update(CFLAGS='-DMIGO_MINIMP3_TEST', MIGO_MP3_TEST_FIXTURES=str(package / 'src/decoder/mp3/fixtures'))
        try:
            lib.write_text(original_lib + f'\n#[cfg(test)] #[path={json.dumps(str(private_extra))}] mod product_mp3_native;\n')
            exact = 'product_mp3_native::native_owner_releases_on_real_job_cancellation_eof_and_error'
            binary = build(package, work, instrument, 'native-owner-positive')
            log = work / 'native-owner-positive-tests.log'
            assert whole.support.clock.common.run([str(binary), '--exact', exact, '--test-threads=1', '--nocapture'], log, package, instrument, timeout=120) == 0
            assert '1 passed; 0 failed' in log.read_text()
            record('native-owner-positive', binary, 'native-ownership-positive')
            files['@native/minimp3.c'].write_text(originals['@native/minimp3.c'].replace(
                'void migo_mp3_presentation_free(migo_mp3_presentation *p) { free(p); }',
                'void migo_mp3_presentation_free(migo_mp3_presentation *p) { (void)p; }'))
            binary = build(package, work, instrument, 'native-owner-leaks')
            log = work / 'native-owner-leaks-test.log'
            status = whole.support.clock.common.run([str(binary), '--exact', exact, '--test-threads=1', '--nocapture'], log, package, instrument)
            assert status == 101 and 'panicked at' in log.read_text() and '1 failed' in log.read_text()
            record('native-owner-leaks', binary, 'runtime-assertion', True)
            rejected.append(dict(name='native-owner-leaks', test=exact, exit=101, binary_sha256=programs[-1]['sha256']))
            files['@native/minimp3.c'].write_text(originals['@native/minimp3.c'].replace(
                'malloc(sizeof(*p))', 'NULL'))
            binary = build(package, work, instrument, 'native-allocation-failure-positive')
            exact = 'product_mp3_native::injected_native_allocation_failure_retires_without_partial_pcm'
            log = work / 'native-allocation-failure-positive-tests.log'
            assert whole.support.clock.common.run([str(binary), '--exact', exact, '--test-threads=1', '--nocapture'], log, package, instrument) == 0
            assert '1 passed; 0 failed' in log.read_text()
            record('native-allocation-failure-positive', binary, 'resource-failure-positive')
        finally:
            lib.write_text(original_lib)
            files['@native/minimp3.c'].write_text(originals['@native/minimp3.c'])
    whole.verify_inputs(work, frozen)
    assert all(p.read_text() == originals[name] for name, p in files.items())
    assert all(whole.support.codecs.sha(copied / path) == value == whole.support.codecs.sha(NATIVE / path)
        for path, value in proof['files'].items())
    if candidate:
        assert all(whole.support.codecs.sha(work / p) == value for p, value in json.loads((work / 'retained-sources.json').read_text()).items())
    summary = dict(profiles=positives, runtime_controls=len(rejected), controls=rejected,
        formal_programs=len(programs), root_saved_inputs=len(frozen), source_copies_restored=True,
        root_inputs_unchanged=True, native_published_source_verified=True, candidate=candidate, minimal=minimal,
        preflight=preflight, product_mp3_presentation_selected=True, production_pool_migrated=False)
    (work / 'executed-programs.json').write_text(json.dumps(programs, indent=2) + '\n')
    (work / 'summary.json').write_text(json.dumps(summary, indent=2) + '\n')
    print('product MP3 checkpoint', positives, len(rejected), len(programs), flush=True)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--target-dir', type=Path, required=True)
    parser.add_argument('--candidate', action='store_true')
    parser.add_argument('--minimal', action='store_true')
    parser.add_argument('--rubato-cargo-home', type=Path)
    parser.add_argument('--preflight', action='store_true')
    args = parser.parse_args()
    assert not args.candidate or args.rubato_cargo_home
    main(args.output.resolve(), args.target_dir.resolve(), args.candidate, args.minimal,
        args.rubato_cargo_home.resolve() if args.rubato_cargo_home else None, args.preflight)
