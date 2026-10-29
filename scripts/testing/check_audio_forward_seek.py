#!/usr/bin/env python3
"""Verify forward seek through undiscovered sequential PCM with real faults.

The downloaded prefix is not the track's duration. Keep the original source,
decoder history and bounded loans; require actual target PCM or EOF before
Seeked. Reuse the frozen-copy/rebuilt-assertion runner, not source-only checks.
"""
from __future__ import annotations

import argparse
from pathlib import Path

import check_audio_buffered_seek as runner


def controls(original):
    rows = []

    def change(name, signature, before, after, test):
        start = original.index(signature)
        end = original.index('\n    }', start) + len('\n    }')
        body = original[start:end]
        assert body.count(before) == 1, (name, 'anchor drift')
        rows.append((name, body, body.replace(before, after), 'inner_audio::forward_seek_tests::' + test))

    paused = 'paused_seek_reaches_undecoded_stereo_pcm_without_replacing_the_source'
    seek = '    pub fn seek(&mut self, time: f64) {'
    poll = '    pub fn poll_stream(&mut self) {'
    prefix = "    fn seek_stream_prefix<'a>("
    process = '    pub fn process(&mut self, output: &mut [f32]) -> bool {'
    change('target-clamped-to-prefix', seek,
           'self.is_streaming()\n            && !self.stream_complete\n            && target_frame >= self.shared.duration_frames()', 'false', paused)
    change('seeked-before-first-target-frame', poll,
           'target < self.shared.duration_frames()', 'target <= self.shared.duration_frames()', paused)
    change('pending-seek-renders-old-audio', process,
           '        if self.pending_seek.is_some() {', '        if false {',
           'pending_seek_preserves_the_mix_and_emits_one_waiting_event')
    change('seek-discards-without-a-work-bound', poll,
           'for _ in 0..STREAM_CHANNEL_CAPACITY', 'for _ in 0..usize::MAX',
           'poll_budget_bounds_paused_seek_work_and_preserves_pcm_credit')
    change('seek-retains-old-pcm', prefix, '            pcm.clear();', '', paused)
    change('seek-prefix-not-discarded', prefix,
           'Ok(&samples[skip as usize * channels..])', 'Ok(samples)', paused)
    change('seek-source-base-incorrect', prefix,
           'self.stream_base_frames = base + skip;', 'self.stream_base_frames = base;', paused)
    change('seek-skip-confuses-samples-and-frames', prefix,
           'skip as usize * channels', 'skip as usize', paused)
    change('seek-uses-requested-cursor-as-decoded-length', prefix,
           'let base = self.shared.duration_frames();', 'let base = self.shared.position_frames();', paused)
    change('discarded-input-rearms-waiting', poll,
           '                    if self.pending_seek.is_none() {', '                    if true {',
           'pending_seek_preserves_the_mix_and_emits_one_waiting_event')
    change('eof-keeps-requested-time', poll,
           'self.shared\n                            .set_position_frames(self.shared.duration_frames());', '',
           'eof_clamps_a_pending_seek_and_ended_waits_for_play')
    change('empty-eof-never-ends', process,
           '                self.finish_playback(0, 0);', '',
           'eof_clamps_a_pending_seek_and_ended_waits_for_play')
    for name, signature in [('stop-keeps-pending-seek', '    pub fn stop(&mut self) {'),
                            ('error-keeps-pending-seek', '    fn fail_stream(&mut self) {'),
                            ('source-switch-keeps-pending-seek', '    pub(crate) fn begin_load_for(')]:
        change(name, signature, '        self.pending_seek = None;\n', '',
               'stop_error_and_source_replacement_retire_a_pending_seek')
    change('malformed-seek-pcm-accepted', prefix,
           'channels == 0 || !samples.len().is_multiple_of(channels)', 'channels == 0',
           'malformed_or_overflowing_seek_pcm_fails_without_seeked')
    before = '''        let end = base.checked_add(frames).ok_or_else(|| {
            EngineError::from_detail(ErrorCode::IoError, "seek PCM duration overflow")
        })?;'''
    change('seek-duration-overflow-hidden', prefix, before,
           '        let end = base.saturating_add(frames);',
           'malformed_or_overflowing_seek_pcm_fails_without_seeked')
    change('discarded-seek-pcm-allocates', prefix,
           '        let Some(target) = self.pending_seek else {',
           '        std::hint::black_box(vec![0_u8; 64]);\n        let Some(target) = self.pending_seek else {',
           'discarded_seek_pcm_has_no_steady_heap_activity')
    return rows


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--target-dir', type=Path, required=True)
    args = parser.parse_args()
    runner.main(args.output.resolve(), args.target_dir.resolve(), controls,
                extra_inputs=[Path(__file__).resolve()],
                scope='Sequential forward seek beyond the decoded prefix, bounded discard, '
                      'target/EOF completion, stale control retirement, PCM and heap invariants. '
                      'Backward/indexed reposition, restart/loop and native/device/peak acceptance remain open.')
