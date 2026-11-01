#!/usr/bin/env python3
"""Bound MP3 PCM collection, final copying and DSP under one exclusive owner.

Check real copy and allocator deltas, exact nonzero PCM and every cancellation
phase. Faults must rebuild and execute to a Rust assertion; build errors,
timeouts, signals and zero tests never count as rejection.
"""
from __future__ import annotations

import argparse
import json
from pathlib import Path
import re

import check_audio_mp3_steps as mp3
import check_audio_decode_rate as rate
import check_audio_io_renewal as support

ROOT = support.ROOT


def declarations(originals):
    rows = []
    pcm = 'decoder/pcm.rs'
    job = 'decoder/mp3/job.rs'
    tests = 'decoder::pcm::tests::'
    jobs = 'decoder::mp3::job::tests::'
    exact = tests + 'every_partition_preserves_sample_order_and_keeps_existing_pages_in_place'
    heap = tests + 'append_and_flatten_bound_allocations_and_never_reallocate_the_final_prefix'
    protocol = tests + 'incomplete_or_invalid_protocol_never_publishes_partial_pcm'
    cancel = jobs + 'cancellation_at_every_owned_phase_releases_storage_and_cannot_resume'
    target = jobs + 'raw_and_targeted_jobs_preserve_exact_nonzero_pcm_and_metadata'
    dispatch = jobs + 'actual_rate_dispatch_uses_collection_and_dsp_in_the_same_mp3_job'

    def change(name, file, before, after, test):
        assert originals[file].count(before) == 1, (name, 'anchor drift')
        rows.append((name, file, before, after, test))

    def region(name, file, start, end, before, after, test):
        text = originals[file]
        begin = text.index(start)
        finish = text.index(end, begin)
        old = text[begin:finish]
        assert old.count(before) == 1
        change(name, file, old, old.replace(before, after), test)

    change('pcm-unbounded-final-copy', pcm, 'let mut remaining = STEP_SAMPLES;',
           'let mut remaining = usize::MAX;', exact)
    change('pcm-doubled-step-budget', pcm, 'const STEP_SAMPLES: usize = 8192;',
           'const STEP_SAMPLES: usize = 16384;', exact)
    change('pcm-doubled-page-allocation', pcm, 'const PAGE_SAMPLES: usize = 16_384;',
           'const PAGE_SAMPLES: usize = 32_768;', exact)
    change('pcm-wrong-normalization', pcm, 'f32::from(sample) / 32768.0',
           'f32::from(sample) / 32767.0', exact)
    change('pcm-loses-collected-length', pcm, '*samples = needed.unwrap();', '*samples = 0;', exact)
    change('pcm-repeats-page-prefix', pcm, '&page[*head..*head + count]', '&page[..count]', exact)
    change('pcm-does-not-advance-page-head', pcm, '*head += count;', 'let _ = count;', heap)
    change('pcm-leaks-retired-pages', pcm, 'pages.pop_front();',
           'std::mem::forget(pages.pop_front());',
           tests + 'pages_release_as_their_last_samples_are_copied')
    change('pcm-initializes-complete-output-in-seal', pcm, 'Vec::with_capacity(samples)',
           'vec![0.0; samples]', heap)
    change('pcm-publishes-partial-output', pcm, 'Phase::Complete(output) => Ok(output),',
           'Phase::Complete(output) | Phase::Flattening { output, .. } => Ok(output),', protocol)
    change('pcm-admits-oversized-frame', pcm, 'pcm.len() > STEP_SAMPLES', 'false', protocol)
    change('pcm-allocates-whole-ceiling-for-each-page', pcm, 'Vec::with_capacity(PAGE_SAMPLES)',
           'Vec::with_capacity(crate::limits::MAX_AUDIO_PCM_SAMPLES as usize)', heap)
    region('pcm-failure-retains-pages', pcm, '        let needed =', '        let mut remaining = pcm;',
           '            self.phase = Phase::Failed;', '',
           tests + 'exact_pcm_limit_is_accepted_and_limit_plus_one_releases_the_whole_prefix')

    change('mp3-job-ignores-cancellation', job, 'cancel.load(Ordering::Acquire)', 'false', cancel)
    change('mp3-job-cancellation-retains-storage', job,
           'self.phase = Phase::Failed(ErrorCode::Cancelled);',
           'return Err(EngineError::from_detail(ErrorCode::Cancelled, "injected"));', cancel)
    change('mp3-job-loses-terminal-error', job, 'self.phase = Phase::Failed(error.code);',
           'self.phase = Phase::Failed(ErrorCode::Internal);',
           jobs + 'codec_and_dsp_errors_retire_the_job_without_returning_partial_pcm')
    change('mp3-job-publishes-before-dsp', job, 'Phase::Complete(audio) => Ok(audio),',
           'Phase::Complete(audio) | Phase::Decoded(audio) => Ok(audio),',
           jobs + 'complete_pcm_is_unavailable_before_all_collection_and_dsp_phases_finish')
    change('mp3-job-uses-fixed-target', job, 'ResampleJob::new(audio, rate)?',
           'ResampleJob::new(audio, 44_100)?', target)
    region('mp3-dispatch-bypasses-complete-job', 'decoder/mod.rs',
           'pub(crate) fn decode_for_rate', '\n}', 'if is_mp3(data) {', 'if false {', dispatch)
    change('mp3-job-relabels-input-rate', job,
           'owner.metadata.get_or_insert((sample_rate, channels));',
           'owner.metadata.get_or_insert((sample_rate + 1, channels));', target)
    change('mp3-job-drains-dsp-without-yielding', job, 'if resampler.step(cancel)? {',
           'if { while !resampler.step(cancel)? {} true } {', cancel)
    change('mp3-job-drains-final-copy-without-yielding', job, 'if collector.step()? {',
           'if { while !collector.step()? {} true } {', dispatch)
    change('mp3-job-clones-complete-finalized-pcm', job, 'samples: collector.finish()?,',
           'samples: collector.finish()?.clone(),', target)
    assert len(rows) == 23 and len({r[0] for r in rows}) == 23
    return rows


def main(work: Path, target_dir: Path, positive_only: bool):
    extra = list((ROOT / 'scripts/testing').glob('check_audio_*.py'))
    extra += list(mp3.FIXTURES.iterdir())
    extra += [ROOT / 'scripts/testing/generate_mp3_step_fixtures.py',
              ROOT / 'scripts/testing/check_gitignore_contract.py']
    package, env, frozen = support.clock.prepare(work, target_dir, extra)
    names = ['decoder/pcm.rs', 'decoder/mp3/job.rs', 'decoder/mod.rs',
             'decoder/mp3.rs', 'decoder/mp3/file.rs', 'audio_thread.rs']
    originals = {p: (package / 'src' / p).read_text() for p in names}
    new_rows = declarations(originals)
    rows = new_rows + mp3.declarations(originals) + rate.declarations(originals)
    assert len(rows) == 52 and len({r[0] for r in rows}) == 52
    binary = support.public.build_native(package, work, env, 'positive')
    log = work / 'positive-tests.log'
    assert support.clock.common.run([str(binary)], log, package, env) == 0
    assert re.search(r'test result: ok\. 397 passed; 0 failed; 2 ignored;', log.read_text())
    support.program(work, 'positive', 'native')
    receipts = support.faults(
        work, package, env, originals, [] if positive_only else rows,
        lambda name: support.public.build_native(package, work, env, name),
        package / 'src', 'native')
    assert all(support.codecs.sha(ROOT / p) == h
               and support.codecs.sha(work / 'source-inputs' / p) == h for p, h in frozen.items())
    audio = ROOT / 'engine/crates/audio/src'
    assert all(p.read_bytes() == (package / 'src' / p.relative_to(audio)).read_bytes()
               for p in audio.rglob('*') if p.is_file()), 'source copy was not restored'
    summary = {
        'positive_tests': 397, 'ignored_tests': 2, 'new_tests': 13,
        'compiled_controls': len(receipts), 'new_controls': 0 if positive_only else len(new_rows),
        'retained_controls': 0 if positive_only else 29, 'controls': receipts,
        'root_inputs_unchanged': True, 'source_copies_restored': True,
        'root_lock_sha256': support.codecs.sha(ROOT / 'engine/Cargo.lock'),
        'scope': 'Paged PCM collection, bounded final copy, complete MP3 codec/DSP owner; '
                 'production DecodePool still drains synchronously; WAV/Vorbis and scheduler migration open.',
    }
    (work / 'summary.json').write_text(json.dumps(summary, indent=2) + '\n')
    print('native complete', summary['positive_tests'], len(receipts), flush=True)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--target-dir', type=Path, required=True)
    parser.add_argument('--positive-only', action='store_true')
    args = parser.parse_args()
    main(args.output.resolve(), args.target_dir.resolve(), args.positive_only)
