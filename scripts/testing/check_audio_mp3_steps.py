#!/usr/bin/env python3
"""Whole-file MP3 framing must yield before a large scan or probe loop.

Observe actual FFI slices, compare nonzero PCM to the original implementation,
and execute rebuilt faults; a compile failure or timeout is never rejection.
"""
from __future__ import annotations

import argparse
import json
from pathlib import Path
import re

import check_audio_decode_rate as rate
import check_audio_io_renewal as support

ROOT = support.ROOT
FIXTURES = ROOT / 'engine/crates/audio/src/decoder/mp3/fixtures'


def declarations(originals):
    rows = []
    path = 'decoder/mp3/file.rs'
    tests = 'decoder::mp3::file::tests::'
    bounds = tests + 'native_probes_and_fallback_obey_each_step_budget_on_invalid_input'
    golden = tests + 'nonzero_cbr_and_vbr_match_original_complete_pcm_with_owned_or_borrowed_input'
    tags = tests + 'metadata_and_scan_window_boundaries_keep_exact_nonzero_pcm'
    free = tests + 'free_format_uses_native_boundaries_bootstrap_and_isolated_tagged_tail'
    cancel = tests + 'cancellation_retires_owned_input_at_every_suspended_phase_without_native_work'
    heap = tests + 'productive_frame_steps_preserve_decoder_state_without_heap_activity'
    capacity = tests + 'whole_mp3_pcm_growth_respects_the_capacity_and_sample_ceiling'

    def change(name, file, before, after, test):
        assert originals[file].count(before) == 1, (name, 'anchor drift')
        rows.append((name, file, before, after, test))

    def region(name, start, end, edits, test, file=path):
        source = originals[file]
        begin = source.index(start)
        finish = source.index(end, begin)
        before = source[begin:finish]
        after = before
        for old, new in edits:
            assert after.count(old) == 1, (name, old)
            after = after.replace(old, new)
        change(name, file, before, after, test)

    region('mp3-unbounded-rust-sync-scan', '                Phase::Search => {',
           '                Phase::Probe {', [
               ('let end = (state.position + scans).min(data.len());', 'let end = data.len();'),
               ('scans -= window.len();', 'scans = scans.saturating_sub(window.len());')], bounds)
    change('mp3-unbounded-probe-ffi-slice', path,
           '.measured_frame(&data[state.position..state.position + size])',
           '.measured_frame(&data[state.position..])', bounds)
    region('mp3-unbounded-fallback-ffi-slice', '                Phase::Fallback => {',
           '                Phase::End =>', [
               ('state.frames.decode(&data[start..start + size])', 'state.frames.decode(&data[start..])')], bounds)
    region('mp3-fallback-discards-native-consumed-offset', '                Phase::Fallback => {',
           '                Phase::End =>', [('state.position += consumed;', 'state.position += 1;')], bounds)
    region('mp3-tagged-tail-gets-lookahead-instead-of-isolation', '                Phase::Decode(frame) => {',
           '                Phase::Fallback => {', [('frame.length\n                    };',
               '(data.len() - state.position).min(NATIVE_BYTES)\n                    };')], tags)
    change('mp3-free-bootstrap-gets-only-one-frame', path, '                        frame.view',
           '                        frame.length', free)
    change('mp3-free-bootstrap-expands-successful-view', path, '                        frame.view',
           '                        (data.len() - state.position).min(NATIVE_BYTES)', free)
    change('mp3-metadata-body-becomes-audio', path,
           'id3v2_tag_length(&data[state.position..])', 'id3v2_tag_length(&[])', tags)
    change('mp3-steady-boundary-hint-discarded', path,
           'state.previous = Some(frame.length);', 'state.previous = None;', heap)
    change('mp3-failed-probe-destroys-free-format-state', 'decoder/mp3.rs',
           '            *self.state = saved;', '            let _ = saved;', free)
    region('mp3-native-decoder-rebuilt-for-each-frame', '                Phase::Decode(frame) => {',
           '                Phase::Fallback => {', [('let start = state.position;',
               'state.frames = Mp3FrameDecoder::new();\n                    let start = state.position;')], golden)
    region('mp3-step-call-admission-disabled', 'fn admit_native(', 'fn invalid(', [
        ('*calls == 0 || size > *bytes', 'size > *bytes'),
        ('*calls -= 1;', '*calls = calls.saturating_sub(1);')],
        tests + 'a_lone_tagged_frame_and_an_unavailable_reservoir_do_not_lose_later_frames')
    region('mp3-step-byte-admission-disabled', 'fn admit_native(', 'fn invalid(', [
        ('*calls == 0 || size > *bytes', '*calls == 0'),
        ('*bytes -= size;', '*bytes = bytes.saturating_sub(size);')], bounds)
    region('mp3-total-native-work-quota-disabled', 'fn admit_native(', 'fn invalid(', [
        ('''let next = total
        .checked_sub(1)
        .ok_or_else(|| invalid("MP3 native-call budget exceeded"))?;''',
         'let next = total.saturating_sub(1);')],
        tests + 'native_call_quota_is_terminal_and_cannot_publish_complete_audio')
    change('mp3-cancellation-ignored', path, 'cancel.load(Ordering::Acquire)', 'false', cancel)
    region('mp3-cancellation-retains-owned-input', '        if cancel.load(',
           '        if self.cancelled {', [('            self.state = None;', '')], cancel)
    region('mp3-eof-retains-owned-input', '        if self\n            .state',
           '        let Some(state)', [('self.state = None;', 'return Ok(FileStep::Finished);')], golden)
    change('mp3-probe-reallocates-on-every-frame', path,
           '                    if let Some(frame) = state\n                        .probe',
           '                    state.probe = Mp3FrameDecoder::new();\n                    if let Some(frame) = state\n                        .probe', heap)
    region('mp3-empty-reservoir-ends-the-file', '                Phase::Decode(frame) => {',
           '                Phase::Fallback => {', [('Mp3Step::NeedMoreData => FileStep::Yield,',
               'Mp3Step::NeedMoreData => { state.phase = Phase::End; FileStep::Yield },')],
        tests + 'a_lone_tagged_frame_and_an_unavailable_reservoir_do_not_lose_later_frames')
    change('mp3-pcm-capacity-limit-removed', 'decoder/pcm.rs',
           'Vec::with_capacity(samples)', 'Vec::with_capacity(samples.saturating_mul(2))', capacity)
    change('mp3-pcm-sample-limit-removed', 'decoder/pcm.rs',
           '!needed.is_some_and(crate::limits::pcm_samples_within_budget)', 'false', capacity)
    for name, before, after, test in [
        ('mp3-step-call-budget-expanded', 'const NATIVE_CALLS: usize = 4;', 'const NATIVE_CALLS: usize = 8;',
         tests + 'a_lone_tagged_frame_and_an_unavailable_reservoir_do_not_lose_later_frames'),
        ('mp3-step-byte-budget-expanded', 'const NATIVE_BYTES: usize = 8192;', 'const NATIVE_BYTES: usize = 16384;', bounds),
        ('mp3-step-scan-budget-expanded', 'const SCAN_BYTES: usize = 4096;', 'const SCAN_BYTES: usize = 8192;', bounds),
    ]:
        change(name, path, before, after, test)
    return rows


def main(work: Path, target: Path, positive_only: bool):
    extra = list((ROOT / 'scripts/testing').glob('check_audio_*.py')) + list(FIXTURES.iterdir())
    extra += [ROOT / 'scripts/testing/generate_mp3_step_fixtures.py',
              ROOT / 'scripts/testing/check_gitignore_contract.py']
    package, env, frozen = support.clock.prepare(work, target, extra)
    originals = {p: (package / 'src' / p).read_text()
                 for p in ['decoder/mp3.rs', 'decoder/mp3/file.rs', 'decoder/pcm.rs', 'audio_thread.rs']}
    new_rows = declarations(originals)
    rows = new_rows + rate.declarations(originals)
    assert len(new_rows) == 24 and len(rows) == 29
    binary = support.public.build_native(package, work, env, 'positive')
    log = work / 'positive-tests.log'
    assert support.clock.common.run([str(binary)], log, package, env) == 0
    assert re.search(r'test result: ok\. 397 passed; 0 failed; 2 ignored;', log.read_text())
    support.program(work, 'positive', 'native')
    receipts = support.faults(
        work, package, env, originals, [] if positive_only else rows,
        lambda name: support.public.build_native(package, work, env, name), package / 'src', 'native')
    assert all(support.codecs.sha(ROOT / p) == h
               and support.codecs.sha(work / 'source-inputs' / p) == h for p, h in frozen.items())
    audio = ROOT / 'engine/crates/audio/src'
    assert all(p.read_bytes() == (package / 'src' / p.relative_to(audio)).read_bytes()
               for p in audio.rglob('*') if p.is_file()), 'source copy was not restored'
    summary = {
        'positive_tests': 397, 'ignored_tests': 2, 'new_tests': 10,
        'compiled_controls': len(receipts), 'new_controls': 0 if positive_only else len(new_rows),
        'retained_rate_controls': 0 if positive_only else 5, 'controls': receipts,
        'root_inputs_unchanged': True, 'source_copies_restored': True,
        'root_lock_sha256': support.codecs.sha(ROOT / 'engine/Cargo.lock'),
        'scope': 'Bounded whole-file MP3 codec cursor, native state and complete PCM; '
                 'PCM collection/allocation steps, WAV/Vorbis and shared scheduler migration remain open.',
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
