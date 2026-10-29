#!/usr/bin/env python3
"""Prove idle seek completion and command ordering without driving the mixer.

Seek once waited in a shared atomic mailbox until audible rendering resumed.
Exercise the owning-thread control path, frozen real audio sources and freshly
rebuilt runtime assertions. Compile errors, signals and zero tests are rejected.
"""
from __future__ import annotations

import argparse
import json
from pathlib import Path
import re

import check_audio_playback_clock as clock

ROOT = clock.ROOT


def controls(original: str):
    start = original.index('    pub fn seek(&mut self, time: f64) {')
    end = original.index('    /// Stop the audio and reset position', start)
    body = original[start:end]
    rows = []

    def add(name, before, after, test):
        assert body.count(before) == 1, (name, 'anchor drift')
        rows.append((name, body, body.replace(before, after), 'inner_audio::seek_tests::' + test))

    paused = 'paused_seeks_complete_before_rendering_and_preserve_the_backing'
    add('idle-seek-waits-for-play', '!time.is_finite() || sample_rate == 0',
        'self.shared.state() != PlaybackState::Playing || !time.is_finite() || sample_rate == 0', paused)
    add('seek-not-applied-on-command',
        '        self.seek_frame(target_frame.min(self.shared.duration_frames()));', '',
        'stopped_seek_stop_and_play_observe_command_order')
    add('seek-retains-fraction', '        self.position_phase = 0;\n', '', paused)
    add('seek-retains-timeupdate-backlog', '        self.frames_since_time_update = 0;\n', '', paused)
    add('seek-retains-waiting-latch', '        self.waiting_notified = false;\n', '', paused)
    add('seeking-observes-new-position', '        self.push_event(InnerAudioEventType::Seeking);\n', '', paused)
    # Keep both events, but deliberately snapshot Seeking after applying the cursor.
    name, before, after, test = rows.pop()
    after = after.replace('        self.push_event(InnerAudioEventType::Seeked);',
                          '        self.push_event(InnerAudioEventType::Seeking);\n'
                          '        self.push_event(InnerAudioEventType::Seeked);')
    rows.append((name, before, after, test))
    add('seeked-event-missing', '        self.push_event(InnerAudioEventType::Seeked);\n', '', paused)
    add('seek-starts-paused-playback', '        self.waiting_notified = false;',
        '        self.shared.set_state(PlaybackState::Playing);\n        self.waiting_notified = false;', paused)
    add('nonfinite-seek-accepted', '!time.is_finite() || ', '',
        'seek_rejects_nonfinite_and_unloaded_targets_and_clamps_finite_bounds')
    add('stream-base-ignored', 'target_frame.saturating_sub(self.stream_base_frames)', 'target_frame',
        'retained_stream_seek_uses_absolute_frames_while_paused')
    add('sample-index-confused-with-frame', '.saturating_mul(channels)', '.saturating_mul(1)',
        'stereo_seek_targets_frames_and_keeps_seeked_before_play')
    add('seek-control-allocates', '        let sample_rate = self.shared.sample_rate();',
        '        std::hint::black_box(vec![0_u8; 64]);\n        let sample_rate = self.shared.sample_rate();',
        'buffered_seek_and_event_delivery_have_no_steady_heap_activity')
    return rows


def main(output: Path, target: Path):
    package, env, frozen = clock.prepare(output, target, [Path(__file__).resolve(),
        ROOT / 'scripts/testing/check_gitignore_contract.py',
        ROOT / 'scripts/testing/check_audio_source_epochs.py'])
    source = package / 'src/inner_audio.rs'
    original = source.read_text()
    rows = controls(original)
    binary = clock.build(package, output, env, 'positive')
    positive_log = output / 'positive-tests.log'
    assert clock.common.run([str(binary)], positive_log, package, env) == 0
    observed = positive_log.read_text()
    match = re.search(r'test result: ok\. (\d+) passed; 0 failed; (\d+) ignored;', observed)
    assert match, 'positive audio suite did not finish'
    available = set(re.findall(r'^test (\S+) \.\.\.', observed, re.MULTILINE))
    assert all(test in available for _, _, _, test in rows), 'missing fault test in positive suite'
    receipts = []
    for name, before, after, test in rows:
        try:
            source.write_text(original.replace(before, after))
            mutant = clock.build(package, output, env, name)
            log = output / f'{name}-test.log'
            status = clock.common.run([str(mutant), '--exact', test, '--nocapture'], log,
                                      package, env, timeout=30)
            observed = log.read_text()
            assert status == 101 and 'panicked at' in observed and '1 failed' in observed, (
                name, status, 'must reject an executed test assertion')
            receipts.append({'name': name, 'test': test, 'exit': status,
                             'binary_sha256': clock.common.sha(mutant)})
        finally:
            source.write_text(original)
        print(f'compiled buffered seek control {name}: runtime assertion rejected', flush=True)
    assert all(clock.common.sha(ROOT / p) == sha for p, sha in frozen.items()), 'Root changed'
    audio = ROOT / 'engine/crates/audio'
    assert all(p.read_bytes() == (package / 'src' / p.relative_to(audio / 'src')).read_bytes()
               for p in (audio / 'src').rglob('*') if p.is_file()), 'copy not restored'
    summary = {'positive_tests': int(match[1]), 'ignored_tests': int(match[2]),
               'compiled_controls': len(receipts), 'controls': receipts,
               'positive_binary_sha256': clock.common.sha(binary), 'root_inputs_unchanged': True,
               'source_copies_restored': True, 'existing_dependency_versions_preserved': True,
               'root_lock_sha256': clock.common.sha(ROOT / 'engine/Cargo.lock'),
               'scope': 'Owning-thread buffered seek for idle/playing/stream/cache sources, '
                        'event snapshots, control ordering and actual heap activity. '
                        'Outside-window decoder/transport repositioning remains open.'}
    (output / 'summary.json').write_text(json.dumps(summary, indent=2) + '\n')
    print(json.dumps(summary, indent=2), flush=True)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--target-dir', type=Path, required=True)
    args = parser.parse_args()
    main(args.output.resolve(), args.target_dir.resolve())
