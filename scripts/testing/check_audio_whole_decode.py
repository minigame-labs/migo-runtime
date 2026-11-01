#!/usr/bin/env python3
"""Vorbis still drained parser/codec/copies together, blocking shared migration.

Verify the product's owned Vorbis and common format dispatcher. Only executed,
fresh Rust assertions reject faults; the persistent pool is a separate step.
"""
from __future__ import annotations
import argparse
import json
from pathlib import Path

import check_audio_product_rubato as src
whole = src.whole
ROOT = whole.ROOT
OGG = 'decoder::ogg::job::tests::'
PRODUCT = 'decoder::whole::tests::'


def declarations(originals):
    rows = []
    job, dispatcher = 'decoder/ogg_job.rs', 'decoder/whole.rs'
    def change(name, path, before, after, test, count=1):
        assert originals[path].count(before) == count, (name, 'mutation anchor drift', before)
        rows.append((name, path, before, after, test))
    cancel = OGG + 'cancellation_at_all_owned_phases_retires_input_pcm_and_stays_terminal'
    bounds = OGG + 'parser_codec_and_multichannel_pcm_copies_yield_separately'
    pcm = OGG + 'owned_steps_preserve_every_legacy_sample_and_submitted_rate'
    quota = OGG + 'parser_and_total_work_limits_are_sticky_and_release_every_owner'
    finish = OGG + 'incomplete_finish_and_output_errors_never_publish_a_partial_buffer'
    change('ogg-cancel-ignored', job, 'if cancel.load(Ordering::Acquire) {', 'if false {', cancel)
    change('ogg-cancel-retains-owner', job, 'self.phase = Phase::Failed(ErrorCode::Cancelled);',
           'return Err(EngineError::from_detail(ErrorCode::Cancelled,"injected"));', cancel)
    change('ogg-two-steps', job, '            } else {\n                self.advance(cancel)\n            }',
           '            } else {\n                self.advance(cancel).and_then(|done| if done { Ok(true) } else { self.advance(cancel) })\n            }', bounds)
    change('ogg-double-copy-budget', job, 'const COPY_SAMPLES: usize = 8192;', 'const COPY_SAMPLES: usize = 16384;', bounds)
    change('ogg-pcm-silenced', job, 'append_with(&scratch[..n], |v| v)', 'append_with(&scratch[..n], |_| 0.)', OGG + 'known_and_fallback_collection_preserve_pcm_and_reservation_steps')
    change('ogg-copy-repeats-prefix', job, '.slice(*frame..end)', '.slice(0..end-*frame)', OGG + 'nonzero_long_blocks_preserve_the_absolute_cursor_and_every_channel')
    change('ogg-gapless-disabled', job, '.gapless(true)', '.gapless(false)', pcm)
    change('ogg-total-work-unlimited', job, 'if self.steps > MAX_STEPS {', 'if false {', quota)
    change('ogg-parser-work-unlimited', job, 'if owner.calls > MAX_PARSER_CALLS {', 'if false {', quota)
    change('ogg-output-limit-ignored', job, '.filter(|n| *n <= self.limit)', '.filter(|_| true)', finish)
    change('ogg-finish-publishes-partial', job, 'Phase::Complete(audio) => Ok(audio),',
           'Phase::Decode(_) => Ok(DecodedAudio { samples:Vec::new(), sample_rate:48000, channels:1 }),\n            Phase::Complete(audio) => Ok(audio),', finish)
    change('ogg-target-fixed', job, 'ResampleJob::new(audio, rate)?', 'ResampleJob::new(audio, 44100)?', pcm)
    change('ogg-final-pcm-cloned', job, 'samples: collector.finish()?,', 'samples: collector.finish()?.clone(),',
           OGG + 'final_flatten_and_completion_transfer_the_reserved_pcm_without_cloning')
    change('ogg-input-never-retires', job, 'drop((input, parser, codec, scratch));',
           'std::mem::forget(input); drop((parser,codec,scratch));',
           OGG + 'input_retires_before_final_collection_copy_and_src')
    change('ogg-full-copy-heap', job, 'let block = owner.codec.as_ref().unwrap().last_decoded();',
           'std::hint::black_box(vec![0_u8; 100000]);\n                            let block = owner.codec.as_ref().unwrap().last_decoded();', bounds)
    change('ogg-terminal-code-lost', job, 'self.phase = Phase::Failed(error.code);',
           'self.phase = Phase::Failed(ErrorCode::Internal);', quota)
    change('product-cancel-not-forwarded', dispatcher, 'job.step(cancel)',
           'job.step(&AtomicBool::new(false))', PRODUCT + 'cancellation_and_incomplete_finish_release_each_product_owner', 3)
    change('product-target-fixed', dispatcher, 'if target == Some(0) ||', 'let target=Some(44100); if target == Some(0) ||',
           PRODUCT + 'all_product_formats_keep_complete_pcm_and_the_submitted_target')
    change('ogg-duration-preflight-disabled', job, 'scan: Some(duration::Scan::new()),', 'scan: None,', OGG + 'known_and_fallback_collection_preserve_pcm_and_reservation_steps')
    duration = 'decoder/ogg_duration.rs'
    original = originals[duration]
    guard = original[original.index('        if self.calls >= 4096'):original.index('        self.calls += 1;')]
    change('ogg-duration-fallback-disabled', duration, guard, '        if false { return Ok(Outcome::Fallback); }\n', OGG + 'known_and_fallback_collection_preserve_pcm_and_reservation_steps')
    change('ogg-duration-quota-disabled', duration, '.filter(|n| *n <= limit)', '.filter(|_| true)', OGG + 'duration_quota_rejects_before_codec_or_pcm_reservation')
    change('ogg-direct-fill-disabled', job, 'collector.append_direct(n, |out| view.copy_to_slice_interleaved(out))?', '{ let _ = &collector; false }', OGG + 'nonzero_long_blocks_preserve_the_absolute_cursor_and_every_channel')
    change('ogg-direct-fill-silenced', job, 'view.copy_to_slice_interleaved(out)', 'out.fill(0.)', pcm)
    return rows


def main(work, target, candidate, minimal, preflight, controls_only=False, positive_only=False):
    if candidate:
        factory = whole.prepare_candidate if minimal else src.wave.duration.bench.prepare
        package, env, frozen, _, _ = factory(work, target, Path('/tmp/migo-w09-rubato-cargo'))
        expected = 583 if minimal else 600
    else:
        package, env, frozen = whole.support.clock.prepare(work, target, whole.extra_inputs())
        expected = 567
    files = {name: package / 'src' / name for name in ['decoder/ogg_job.rs', 'decoder/whole.rs', 'decoder/ogg_duration.rs']}
    originals = {name: p.read_text() for name, p in files.items()}
    rows = declarations(originals)
    positives, controls, programs = {}, [], []
    for profile in ([] if controls_only else ['release'] if preflight else ['release', 'debug']):
        label = 'positive-' + profile
        binary = src.build(package, work, env, label, profile == 'release')
        log = work / (label + '-tests.log')
        assert whole.support.clock.common.run([str(binary)], log, package, env, timeout=900) == 0, log
        assert f'test result: ok. {expected} passed; 0 failed; 2 ignored;' in log.read_text(), log
        assert whole.support.clock.common.run([str(binary), '--list'], work / (label + '-inventory.log'), package, env) == 0
        inventory = (work / (label + '-inventory.log')).read_text()
        assert all(test + ': test' in inventory for _, _, _, _, test in rows)
        assert PRODUCT + 'actual_product_jobs_preserve_state_on_background_cpu_handoffs: test' in inventory
        positives[profile] = dict(passed=expected, ignored=2)
        programs.append(dict(name=label, sha256=whole.support.codecs.sha(binary), fresh_rust=True, pruned=False))
    if not candidate and not preflight and not positive_only:
        for name, path, before, after, test in rows:
            try:
                files[path].write_text(originals[path].replace(before, after))
                binary = src.build(package, work, env, name, True)
                log = work / (name + '-test.log')
                status = whole.support.clock.common.run([str(binary), '--exact', test, '--nocapture'], log, package, env, timeout=120)
                text = log.read_text()
                assert status == 101 and all(x in text for x in ['running 1 test', 'panicked at', '1 failed']), (name, status, log)
                digest = whole.support.codecs.sha(binary)
                programs.append(dict(name=name, sha256=digest, fresh_rust=True, pruned=True))
                controls.append(dict(name=name, test=test, exit=status, binary_sha256=digest))
                binary.unlink()
                print('executed whole-decode assertion rejected:', name, flush=True)
            finally:
                files[path].write_text(originals[path])
    whole.verify_inputs(work, frozen)
    expected_lib = (ROOT / 'engine/crates/audio/src/lib.rs').read_bytes()
    if candidate:
        expected_lib += (work / 'lib-addition.txt').read_bytes()
        assert all(whole.support.codecs.sha(work / p) == value for p, value in json.loads((work / 'retained-sources.json').read_text()).items())
    assert (package / 'src/lib.rs').read_bytes() == expected_lib
    assert all((package / 'src' / p.relative_to(ROOT / 'engine/crates/audio/src')).read_bytes() == p.read_bytes()
               for p in (ROOT / 'engine/crates/audio/src').rglob('*') if p.is_file() and p.name != 'lib.rs')
    result = dict(profiles=positives, controls=controls, compiled_controls=len(controls), programs=programs,
                  root_inputs_unchanged=True, source_copies_restored=True, candidate=candidate, minimal=minimal,
                  preflight=preflight, controls_only=controls_only, positive_only=positive_only,
                  owned_product_vorbis=True, unified_product_jobs=True, production_pool_migrated=True)
    (work / 'summary.json').write_text(json.dumps(result, indent=2) + '\n')
    print('whole product jobs verified', positives, len(controls), flush=True)


def allocation_errors(work, target):
    package, env, frozen = whole.support.clock.prepare(work, target, whole.extra_inputs())
    fixture = work / 'pcm_allocation_contracts.rs'
    fixture.write_bytes((ROOT / 'scripts/testing/fixtures/audio-codec-probe/pcm_allocation_contracts.rs').read_bytes())
    lib = package / 'src/lib.rs'
    before = ('static COUNTING_ALLOCATOR: migo_alloc_probe::CountingAllocator =\n'
              '    migo_alloc_probe::CountingAllocator::system();')
    after = ('static COUNTING_ALLOCATOR: pcm_allocation_contracts::Allocator =\n'
             '    pcm_allocation_contracts::Allocator::new();')
    original_lib = lib.read_text()
    assert original_lib.count(before) == 1
    expected_lib = original_lib.replace(before, after) + f'\n#[cfg(test)] #[path={json.dumps(str(fixture))}] mod pcm_allocation_contracts;\n'
    lib.write_text(expected_lib)
    pcm = package / 'src/decoder/pcm.rs'
    original = pcm.read_text()
    original_fixture = fixture.read_text()
    tests = 'pcm_allocation_contracts::'
    cases = [
        ('pcm-page-oom-code-lost', pcm, '"PCM page reservation refused"',
         'unknown_page_allocation_refusal_is_an_error_and_retires_pcm'),
        ('pcm-final-oom-code-lost', pcm, '"PCM final reservation refused"',
         'unknown_final_allocation_refusal_releases_the_existing_pages'),
        ('pcm-known-oom-code-lost', pcm, '"PCM reservation refused"',
         'known_reservation_keeps_its_allocation_error_and_releases_storage'),
        ('allocator-refusal-disabled', fixture, None,
         'unknown_page_allocation_refusal_is_an_error_and_retires_pcm'),
    ]
    programs, controls = [], []
    for profile in ['release', 'debug']:
        label = 'positive-' + profile
        binary = src.build(package, work, env, label, profile == 'release')
        log = work / (label + '-tests.log')
        assert whole.support.clock.common.run([str(binary), tests, '--nocapture'], log, package, env) == 0
        assert '4 passed; 0 failed;' in log.read_text()
        programs.append(dict(name=label, sha256=whole.support.codecs.sha(binary), fresh_rust=True, pruned=False))
    for name, path, anchor, test in cases:
        try:
            if anchor:
                point = original.index(anchor)
                at = original.rindex('ErrorCode::OutOfMemory', 0, point)
                path.write_text(original[:at] + original[at:].replace('ErrorCode::OutOfMemory', 'ErrorCode::InvalidArgument', 1))
            else:
                before = 'if target.get() != 0 && target.get() == bytes {'
                assert original_fixture.count(before) == 1
                path.write_text(original_fixture.replace(before, 'if false {'))
            binary = src.build(package, work, env, name, True)
            log = work / (name + '-test.log')
            status = whole.support.clock.common.run([str(binary), '--exact', tests + test, '--nocapture'], log, package, env)
            assert status == 101 and all(x in log.read_text() for x in ['running 1 test', 'panicked at', '1 failed'])
            digest = whole.support.codecs.sha(binary)
            controls.append(dict(name=name, test=tests + test, exit=status, binary_sha256=digest))
            programs.append(dict(name=name, sha256=digest, fresh_rust=True, pruned=True))
            binary.unlink()
            print('actual allocation-error assertion rejected:', name, flush=True)
        finally:
            pcm.write_text(original)
            fixture.write_text(original_fixture)
    whole.verify_inputs(work, frozen)
    assert lib.read_text() == expected_lib
    assert fixture.read_text() == original_fixture
    assert all((package / 'src' / p.relative_to(ROOT / 'engine/crates/audio/src')).read_bytes() == p.read_bytes()
               for p in (ROOT / 'engine/crates/audio/src').rglob('*') if p.is_file() and p.name != 'lib.rs')
    summary = dict(profiles={p: dict(passed=4, ignored=0) for p in ['release', 'debug']},
                   controls=controls, compiled_controls=4, programs=programs, root_inputs_unchanged=True,
                   source_copies_restored=True, actual_null_allocator_requests=True,
                   scope='only four isolated allocation-refusal tests; no claim of general OOM or shipping panic recovery')
    (work / 'summary.json').write_text(json.dumps(summary, indent=2) + '\n')
    print('PCM real allocator refusal verified: 4 tests each profile, 4 runtime controls', flush=True)


if __name__ == '__main__':
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--output', type=Path, required=True)
    p.add_argument('--target-dir', type=Path, required=True)
    p.add_argument('--candidate', action='store_true')
    p.add_argument('--minimal', action='store_true')
    p.add_argument('--preflight', action='store_true')
    p.add_argument('--allocation-only', action='store_true')
    p.add_argument('--controls-only', action='store_true')
    p.add_argument('--positive-only', action='store_true')
    a = p.parse_args()
    assert not a.minimal or a.candidate
    assert not (a.controls_only and (a.positive_only or a.candidate or a.preflight))
    if a.allocation_only:
        assert not (a.candidate or a.minimal or a.preflight or a.controls_only or a.positive_only)
        allocation_errors(a.output.resolve(), a.target_dir.resolve())
    else:
        main(a.output.resolve(), a.target_dir.resolve(), a.candidate, a.minimal, a.preflight, a.controls_only, a.positive_only)
