#!/usr/bin/env python3
"""Progressive MP3 drained linear SRC and EOF in an indivisible CPU request.

Verify the real raw MP3/sinc owner, bounded calls/copies, PCM batching, exact
reference output and terminal ownership. Only fresh exact runtime failures
count as rejected faults; device output remains a separate migration.
"""
from __future__ import annotations
import argparse
import json
from pathlib import Path

import check_audio_whole_decode as product

ROOT = product.whole.ROOT
PREFIX = 'streaming_mp3::tests::'


def declarations(original):
    rows = []
    def change(name, before, after, test, assertion):
        assert original.count(before) == 1, (name, 'mutation anchor drift')
        rows.append((name, before, after, PREFIX + test, assertion))
    pcm = 'complete_progressive_pcm_matches_frozen_raw_codec_and_independent_sinc'
    cancel = 'cancellation_at_codec_setup_dsp_and_eof_releases_every_owner_and_stays_terminal'
    capacity = 'invalid_target_nonempty_eof_and_insufficient_publication_capacity_are_sticky'
    change('stream-cancel-ignored', 'if cancel.load(Ordering::Acquire) {', 'if false {', cancel, 'Cancelled')
    change('stream-cancel-retains-owner', 'fn retire(&mut self, code: ErrorCode) {\n        self.owner = None;',
           'fn retire(&mut self, code: ErrorCode) {\n        let _ = &self.owner;', cancel, 'cancel retained stream allocations or PCM')
    change('stream-terminal-code-lost', 'self.error = Some(code);', 'self.error = Some(ErrorCode::Internal);', cancel, 'Cancelled')
    change('stream-input-quantum-doubled', 'const INPUT_BYTES: usize = 4096;', 'const INPUT_BYTES: usize = 8192;', 'long_music_matches_original_raw_minimp3_frames_and_independent_sinc', 'step.consumed <= input.len() && step.consumed <= 4096')
    before = '''            let step = self.pipeline.as_mut().unwrap().step(
                &self.scratch[self.head..],
                false,
                &mut self.output,
                cancel,
            )?;'''
    change('stream-two-src-kernels', before, before + '''
            if self.source_format.0 != self.target {
                let _ = self.pipeline.as_mut().unwrap().step(&self.scratch[self.head..], false, &mut self.output, cancel)?;
            }''', pcm, 'stream performed multiple kernels in one CPU turn')
    change('stream-src-bypassed', 'Stream::new(format.0, self.target, format.1, None)?',
           'Stream::new(format.0, format.0, format.1, None)?', pcm, 'progressive PCM duration changed')
    before = '''                self.dsp_finished = if let Some(pipeline) = &mut self.pipeline {
                    pipeline.step(&[], true, &mut self.output, cancel)?.progress
                        == bandlimited::Progress::Finished
                } else {
                    true
                };'''
    change('stream-src-tail-discarded', before, '                self.dsp_finished = true;', pcm, 'progressive PCM duration changed')
    change('stream-format-drift-accepted', '} else if self.source_format != format {', '} else if false {',
           'format_changes_are_errors_before_new_format_pcm_is_published', 'mixed-format stream was accepted')
    change('stream-pcm-batches-disabled', 'const PCM_BATCH_SAMPLES: usize = 4096;', 'const PCM_BATCH_SAMPLES: usize = 1;',
           'fixed_pcm_batching_preserves_a_useful_four_slot_decode_reserve', 'small SRC quantum consumed a transport slot')
    change('stream-processing-allocates', 'let result = self.advance(input, eof, pcm, cancel);',
           'std::hint::black_box(vec![0_u8; 64]);\n        let result = self.advance(input, eof, pcm, cancel);',
           'running_codec_sinc_and_eof_reuse_preallocated_buffers_without_heap_growth', 'stream processing allocated')
    change('stream-publication-grows', 'let count = self.output.len().min(STEP_SAMPLES);',
           'let count = self.output.len().min(STEP_SAMPLES);\n        pcm.reserve(count);', capacity, 'undersized PCM publication capacity was accepted')
    change('stream-eof-retains-owner', 'if progress == Progress::Finished {\n            self.owner = None;',
           'if progress == Progress::Finished {\n            let _ = &self.owner;', pcm, 'EOF retained decoder/SRC storage')
    change('stream-encoded-suffix-dropped', 'self.buffer.extend_from_slice(&input[..count]);',
           'self.buffer.extend_from_slice(&input[..count.saturating_sub(1)]);', pcm, 'progressive PCM duration changed')
    return rows


def main(work, target, preflight, controls_only=False):
    whole = product.whole
    package, env, frozen = whole.support.clock.prepare(work, target, whole.extra_inputs() + [Path(__file__)])
    source = package / 'src/streaming_mp3.rs'
    original = source.read_text()
    rows = declarations(original)
    product.src.verify_dsp(work, Path('/tmp/migo-w09-rubato-cargo'))
    programs, controls, profiles = [], [], {}
    for profile in ([] if controls_only else ['release'] if preflight else ['release', 'debug']):
        label = 'positive-' + profile
        binary = product.src.build(package, work, env, label, profile == 'release')
        log = work / (label + '-tests.log')
        command = [str(binary), PREFIX] if preflight else [str(binary)]
        assert whole.support.clock.common.run(command, log, package, env, timeout=900) == 0, log
        count = 9 if preflight else 567
        assert f'test result: ok. {count} passed; 0 failed;' in log.read_text(), log
        assert whole.support.clock.common.run([str(binary), '--list'], work / (label + '-inventory.log'), package, env) == 0
        inventory = (work / (label + '-inventory.log')).read_text()
        assert all(test + ': test' in inventory for _, _, _, test, _ in rows)
        programs.append(dict(name=label, path=str(binary), sha256=whole.support.codecs.sha(binary), fresh_rust=True, pruned=False))
        profiles[profile] = dict(passed=count, ignored=0 if preflight else 2)
    if not preflight:
        for name, before, after, test, assertion in rows:
            try:
                source.write_text(original.replace(before, after))
                binary = product.src.build(package, work, env, name, True)
                log = work / (name + '-test.log')
                status = whole.support.clock.common.run([str(binary), '--exact', test, '--nocapture'], log, package, env, timeout=120)
                text = log.read_text()
                assert status == 101 and all(t in text for t in ['running 1 test', test, 'panicked at', '1 failed', assertion]), (name, status, log)
                assert 'PermissionDenied' not in text and 'Operation not permitted' not in text
                digest = whole.support.codecs.sha(binary)
                programs.append(dict(name=name, path=str(binary), sha256=digest, fresh_rust=True, pruned=True))
                controls.append(dict(name=name, test=test, expected_assertion=assertion, exit=status, binary_sha256=digest))
                binary.unlink()
                (work / 'completed-controls.json').write_text(json.dumps(controls, indent=2) + '\n')
                print('actual progressive SRC fault rejected:', name, flush=True)
            finally:
                source.write_text(original)
    whole.verify_inputs(work, frozen)
    audio = ROOT / 'engine/crates/audio/src'
    assert all(p.read_bytes() == (package / 'src' / p.relative_to(audio)).read_bytes() for p in audio.rglob('*') if p.is_file())
    result = dict(profiles=profiles, controls=controls, compiled_controls=len(controls), programs=programs,
                  root_inputs_unchanged=True, source_copies_restored=True, legacy_progressive_sinc_migrated=True,
                  device_sinc_migrated=False, bounded_kernel_or_native_calls=1, pcm_batch_samples=4096,
                  max_pcm_publication_samples=8192, preflight=preflight, controls_only=controls_only, device_or_shipping_acceptance=False)
    (work / 'summary.json').write_text(json.dumps(result, indent=2) + '\n')
    print('Progressive SRC verified', profiles, len(controls), flush=True)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--target-dir', type=Path, required=True)
    parser.add_argument('--preflight', action='store_true')
    parser.add_argument('--controls-only', action='store_true')
    args = parser.parse_args()
    assert not (args.preflight and args.controls_only)
    main(args.output.resolve(), args.target_dir.resolve(), args.preflight, args.controls_only)
