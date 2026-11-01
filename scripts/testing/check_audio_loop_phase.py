#!/usr/bin/env python3
"""Reject cyclic padding/reset and per-cycle rounding in the pinned DSP path.

A new sinc per repetition loses filter history and rounds each short track.
Compare the continuous stream with one library render of concatenated PCM,
then rebuild actual faults in cycle state, clocks, source replay and retirement.
This is a candidate host checkpoint; public loop controls are not wired here.
"""
from __future__ import annotations
import argparse
import json
from pathlib import Path
import shutil
import subprocess
import tomllib
import check_audio_seek_phase as phase
ROOT = phase.ROOT
codecs = phase.codecs


def declarations(originals):
    rows = []
    wave = 'timed_m4a::loop_tests::loop_keeps_one_filter_grid_across_seams'
    clock = 'resampled_streaming::loop_clock_tests::only_exact_cycles_advance_and_a_final_boundary_is_idempotent'
    overflow = 'resampled_streaming::loop_clock_tests::loop_counters_reject_overflow_without_wrapping'
    config = 'timed_m4a::loop_tests::loop_configuration_and_clock_are_validated_before_publication'
    def scoped(name, path, signature, before, after, test):
        source = originals[path]
        start = source.index(signature)
        end = source.index('\n    }', start)+len('\n    }')
        body = source[start:end]
        assert body.count(before) == 1, (name, 'mutation anchor drift')
        rows.append((name, path, body, body.replace(before, after), test))
    sampler = 'resampled_streaming.rs'
    step = '    fn step_inner('
    anchor = '                self.decoder\n                    .seek(symphonia::core::units::Timestamp::ZERO)'
    scoped('loop-resets-retained-sinc', sampler, step, anchor,
        '                if let Some(s) = &mut self.resampler { s.reset(); }\n'+anchor, wave)
    scoped('loop-flushes-before-next-cycle', sampler, step, anchor,
        '                if let Some(s) = &mut self.resampler { s.append_step(&[], true, pcm).map_err(error)?; }\n'+anchor, wave)
    before = anchor+'\n                    .map_err(|cause| error(cause.to_string()))?;'
    scoped('loop-never-rewinds-its-source', sampler, step, before, '', wave)
    scoped('loop-keeps-decoder-eof', sampler, step,
        '                self.decoder_done = false;', '', wave)
    scoped('loop-restart-keeps-old-frame-count', sampler, '    fn restart(',
        '        self.frames = 0;', '', clock)
    scoped('loop-restart-keeps-sealed-boundary', sampler, '    fn restart(',
        '        self.sealed = false;', '', clock)
    scoped('loop-accepts-incomplete-cycle', sampler, '    fn boundary(',
        'self.expected != Some(self.frames)', 'false', clock)
    scoped('loop-frame-counter-wraps', sampler, '    fn observe(',
        '.checked_add(frames)', '.checked_add(frames).or(Some(0))', overflow)
    scoped('loop-cycle-counter-wraps', sampler, '    fn boundary(',
        '.checked_add(1)', '.checked_add(1).or(Some(0))', overflow)
    scoped('loop-final-boundary-counted-twice', sampler, '    fn boundary(',
        'if !self.sealed {', 'if true {', clock)
    scoped('loop-stops-one-cycle-early', sampler, '    fn boundary(',
        'self.completed < n', 'self.completed.saturating_add(1) < n', clock)
    scoped('loop-accepts-zero-repetitions', sampler, '    pub(crate) fn with_repetitions(',
        'if matches!(repetitions, Repetitions::Finite(0)) {',
        'if false && matches!(repetitions, Repetitions::Finite(0)) {', config)
    scoped('loop-accepts-empty-presentation', sampler, '    pub(crate) fn bind_loop_frames(',
        'if frames == 0', 'if false && frames == 0', config)
    scoped('loop-rebinds-presentation-length', sampler, '    pub(crate) fn bind_loop_frames(',
        'state.expected.is_some_and(|old| old != frames)', 'false', config)
    scoped('loop-allocates-at-each-boundary', sampler, step, anchor,
        '                std::hint::black_box(vec![0_u8;64]);\n'+anchor,
        'timed_m4a::loop_tests::cached_continuous_loop_steps_have_no_steady_heap_activity')
    timed = 'timed_m4a.rs'
    scoped('loop-duration-saturates-instead-of-rejecting-overflow', timed, '    fn timeline(',
        '.checked_mul(cycles)', '.checked_mul(cycles).or(Some(u64::MAX))', config)
    scoped('loop-clock-error-does-not-retire-decoder', timed, '    fn step(',
        '            self.inner.retire_loop_error();', '', config)
    scoped('continuous-loop-claims-single-track-end', timed, '    fn timeline(',
        'Some(Repetitions::Continuous) => Ok(None)',
        'Some(Repetitions::Continuous) => Ok(Some(timeline))',
        'timed_m4a::loop_tests::continuous_cancelled_and_failed_loops_do_not_reopen')
    scoped('loop-seek-discards-unfinished-cyclic-pcm', timed, '    fn seek(',
        'if self.inner.repetitions().is_some() {', 'if false {', config)
    return rows


def product_reference():
    checkpoint = ROOT/'docs/implementation/2026-10-25-runtime-modernization/results/W09-seek-phase-final-v3/merged-source-inputs.json'
    frozen = json.loads(checkpoint.read_text())
    selected = {p: h for p, h in frozen.items() if p.startswith('engine/')}
    assert selected and all(codecs.sha(ROOT/p) == h for p, h in selected.items()), 'product inputs changed'
    return {'reference': str(checkpoint), 'unchanged_engine_inputs': len(selected),
            'audio_tests_from_previous_checkpoint': {'passed': 295, 'ignored': 2},
            'fresh_product_tests_this_checkpoint': False}


def main(work, target, rubato, positive_only):
    reference = product_reference()
    env, frozen, retained, libraries, resampling = phase.prepare(work, target, rubato)
    package = work/'probe'
    codecs.run(['cargo', 'metadata', '--offline', '--format-version', '1'],
               work/'metadata.jsonl', cwd=package, env=env)
    identity = lambda p: (p['name'], p['version'], p.get('source'), p.get('checksum'))
    locked = tomllib.loads((ROOT/'engine/Cargo.lock').read_text())['package']
    known = {p['name'] for p in locked}
    pins = {identity(p) for p in locked}
    assert all(p['name'] not in known or identity(p) in pins
               for p in tomllib.loads((package/'Cargo.lock').read_text())['package'])
    files = ['resampled_streaming.rs', 'timed_m4a.rs', 'bandlimited.rs',
             'producer_control.rs', 'streaming_transport.rs', 'inner_audio.rs']
    originals = {n: (work/'source'/n).read_text() for n in files}
    rows = declarations(originals)+phase.declarations(originals)
    codecs.run(['cargo', 'check', '--offline', '--locked', '--lib'],
               work/'normal-library-check.log', cwd=package, env=env)
    positives = phase.positive_profiles(work, env, [
        ('cyclic', 'timed_m4a::loop_tests::', 7),
        ('cyclic-clock', 'resampled_streaming::loop_clock_tests::', 2),
    ])
    assert positives == {'release': 233, 'debug': 230}, positives
    receipts = []
    for name, path, before, after, test in ([] if positive_only else rows):
        source = work/'source'/path
        assert originals[path].count(before) == 1, name
        try:
            source.write_text(originals[path].replace(before, after))
            binary = phase.file.dsp.source.build(work, env, name, release=False, clean=False)
            log = work/f'{name}-test.log'
            with log.open('w') as out:
                r = subprocess.run([str(binary), '--exact', test, '--nocapture'],
                    cwd=package, env=env, stdout=out, stderr=subprocess.STDOUT, timeout=120)
            observed = log.read_text()
            assert r.returncode == 101 and 'panicked at' in observed and '1 failed' in observed, name
            receipts.append({'name': name, 'test': test, 'exit': r.returncode,
                             'binary_sha256': codecs.sha(binary)})
        finally:
            source.write_text(originals[path])
        print('compiled cyclic fault', name, ': executed assertion rejected', flush=True)
    assert all(codecs.sha(ROOT/p) == h and codecs.sha(work/'source-inputs'/p) == h
               for p, h in frozen.items()), 'Root/frozen drift'
    assert all(codecs.sha(work/p) == h for p, h in retained.items()), 'copy not restored'
    provenance = json.loads((work/'library-source-provenance.json').read_text())
    for row in provenance:
        assert phase.file.dsp.source.index_gate.timeline.source_tree(libraries[row['crate']]) == row['source_files']
    for name, row in resampling.items():
        assert all(codecs.sha(work/'resampling-vendor'/f"{name}-{row['version']}"/p) == h
                   for p, h in row['source_files'].items())
    assert product_reference() == reference
    summary = {'positive_tests_per_profile': positives, 'new_tests': 9,
               'loop_pcm_matrix_comparisons': 217, 'tiny_257cycle_sequences': 6,
               'actual_file_http_player_sequences': 16, 'continuous_cancellation_sources': 2,
               'compiled_controls': len(receipts), 'new_controls': 0 if positive_only else 19,
               'retained_seek_controls': 0 if positive_only else 19, 'controls': receipts,
               'root_inputs_unchanged': True, 'source_copies_restored': True,
               'product_reference': reference, 'root_lock_sha256': codecs.sha(ROOT/'engine/Cargo.lock'),
               'libraries': len(provenance), 'patches': sum(len(r['patches']) for r in provenance),
               'resampling_source_files_unchanged': True,
               'scope': 'Candidate cyclic codec/source replay with retained sinc history/partial input/phase,final-only flush,finite aggregate clocks and continuous cancellation.No public loop clock wrapping/live toggle/native cutover,productcodec/device/performance completion.'}
    (work/'summary.json').write_text(json.dumps(summary, indent=2)+'\n')
    print(json.dumps(summary, indent=2), flush=True)


if __name__ == '__main__':
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--output', type=Path, required=True)
    p.add_argument('--target-dir', type=Path, required=True)
    p.add_argument('--rubato-cargo-home', type=Path, required=True)
    p.add_argument('--positive-only', action='store_true')
    a = p.parse_args()
    main(a.output.resolve(), a.target_dir.resolve(), a.rubato_cargo_home.resolve(), a.positive_only)
