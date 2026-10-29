#!/usr/bin/env python3
"""Reject retired PCM, metadata, EOF and errors on the actual audio path.

A source seek used to share one unversioned channel with the preceding traversal.
Freeze the audio crate, execute both ordinary and reusable EOF paths, and rebuild
faults in protocol, player ordering, storage recycling and allocation guards.
This is the revision prerequisite; the persistent source actor remains open.
"""
from __future__ import annotations
import argparse
import json
from pathlib import Path
import re

import check_audio_playback_clock as clock

ROOT = clock.ROOT


def controls(originals):
    rows = []

    def change(name, file, signature, before, after, test, module):
        original = originals[file]
        start = original.index(signature)
        end = original.index('\n    }', start) + len('\n    }')
        body = original[start:end]
        assert body.count(before) == 1, (name, 'anchor drift')
        rows.append((name, file, body, body.replace(before, after), module + test))

    def player(name, signature, before, after, test):
        change(name, 'inner_audio.rs', signature, before, after, test, 'inner_audio::stream_revision_tests::')

    accept = '    pub(crate) fn accept_stream_revision('
    poll = '    pub fn poll_stream('
    stale = 'every_old_message_and_unaccepted_future_message_is_discarded'
    storage = 'accepted_reposition_reuses_storage_and_waits_for_fresh_pcm_in_every_play_state'
    eof = 'reusable_eof_keeps_ownership_without_polling_or_cache_handoff_and_can_resume'
    player('retired-messages-accepted', poll, 'if msg.revision() != self.stream_revision {', 'if false {', stale)
    player('future-messages-admitted', poll, 'if msg.revision() != self.stream_revision {', 'if msg.revision() < self.stream_revision {', stale)
    player('source-identity-ignored', accept, '!Arc::ptr_eq(current, source)', 'false', 'a_delayed_or_foreign_source_acknowledgement_cannot_clear_current_pcm')
    player('duplicate-ack-accepted', accept, 'revision <= self.stream_revision', 'revision < self.stream_revision', 'duplicate_or_reordered_acknowledgements_preserve_the_active_traversal')
    player('old-pcm-retained', accept, '            samples.clear();', '', stale)
    player('fraction-survives-reposition', accept, '        self.position_phase = 0;', '', storage)
    player('seeked-before-fresh-pcm', accept, 'self.pending_seek = Some(target_frame);', 'self.pending_seek = None;', storage)
    player('seek-starts-paused-player', accept, '        self.position = 0;', '        self.shared.set_state(PlaybackState::Playing);\n        self.position = 0;', storage)
    player('eof-retires-reusable-source', poll, 'if !self.retains_stream_source() {', 'if true {', eof)
    player('eof-buffered-seek-waits', '    pub fn seek(', ' && !self.stream_complete', '', eof)
    player('eof-keeps-polling', '    pub(crate) fn needs_stream_poll(', ' && !self.stream_complete', '', eof)
    player('cache-steals-retained-source', '    pub fn take_streamed_audio(', ' || self.retains_stream_source()', '', eof)
    player('closed-source-ack-accepted', accept, '            || self.stream_rx.as_ref().is_none_or(|rx| rx.is_closed())\n', '', 'closed_cancelled_or_ordinary_source_rejects_reposition_before_clearing_pcm')
    player('load-keeps-old-revision', '    pub(crate) fn begin_load_for(', '        self.stream_revision = StreamRevision::INITIAL;\n', '', 'replacing_a_source_resets_revision_and_cancels_its_retained_eof')
    player('stale-work-unbounded', poll, 'for _ in 0..STREAM_CHANNEL_CAPACITY', 'for _ in 0..usize::MAX', 'discarded_pcm_returns_the_original_loan_and_polling_keeps_its_work_budget')
    player('revision-control-allocates', accept, '        self.push_event(InnerAudioEventType::Seeking);', '        std::hint::black_box(vec![0_u8; 64]);\n        self.push_event(InnerAudioEventType::Seeking);', 'accepted_revisions_and_discarded_loans_have_no_steady_heap_activity')
    change('pcm-return-lost', 'streaming.rs', '    fn drop(&mut self) {', '        let _ = self.home.try_send(buffer);', '', 'accepted_revisions_and_discarded_loans_have_no_steady_heap_activity', 'inner_audio::stream_revision_tests::')
    sequence = 'every_traversal_has_its_own_ready_pcm_and_exactly_one_done'
    for name, before, after, test in [
        ('revision-counter-not-reset', '        self.total_samples = 0;', '', 'rejected_revision_and_changed_format_preserve_publication_state'),
        ('eof-state-not-reset', '        self.finished = false;', '', sequence),
        ('source-format-forgotten', '        self.revision = revision;', '        self.format = None;\n        self.revision = revision;', 'rejected_revision_and_changed_format_preserve_publication_state'),
    ]:
        change(name, 'streaming.rs', '    pub(crate) fn begin(', before, after, test, 'streaming::revision_tests::')
    change('duplicate-done-published', 'streaming.rs', '    pub(crate) async fn finish(', 'self.finished || ', '', sequence, 'streaming::revision_tests::')
    change('eof-completion-flag-lost', 'streaming.rs', '    pub(crate) async fn finish(', '        state.download_complete.store(true, Ordering::Release);', '', sequence, 'streaming::revision_tests::')
    return rows


def main(output, target, positive_only):
    package, env, frozen = clock.prepare(output, target, [Path(__file__).resolve(), ROOT / 'scripts/testing/check_gitignore_contract.py'])
    files = ['inner_audio.rs', 'streaming.rs']
    originals = {f: (package / 'src' / f).read_text() for f in files}
    declarations = controls(originals)
    binary = clock.build(package, output, env, 'positive')
    log = output / 'positive-tests.log'
    assert clock.common.run([str(binary)], log, package, env) == 0
    match = re.search(r'test result: ok\. (\d+) passed; 0 failed; (\d+) ignored;', log.read_text())
    assert match, 'positive suite incomplete'
    available = set(re.findall(r'^test (\S+) \.\.\.', log.read_text(), re.M))
    assert all(test in available for _, _, _, _, test in declarations)
    receipts = []
    for name, file, before, after, test in ([] if positive_only else declarations):
        source = package / 'src' / file
        try:
            assert originals[file].count(before) == 1
            source.write_text(originals[file].replace(before, after))
            mutant = clock.build(package, output, env, name)
            test_log = output / f'{name}-test.log'
            status = clock.common.run([str(mutant), '--exact', test, '--nocapture'], test_log, package, env, timeout=30)
            observed = test_log.read_text()
            assert status == 101 and 'panicked at' in observed and '1 failed' in observed, (name, status, 'requires executed assertion')
            receipts.append({'name': name, 'test': test, 'exit': status, 'binary_sha256': clock.common.sha(mutant)})
        finally:
            source.write_text(originals[file])
        print('compiled stream revision control', name, ': runtime assertion rejected', flush=True)
    assert all(clock.common.sha(ROOT / p) == sha for p, sha in frozen.items()), 'Root changed'
    audio = ROOT / 'engine/crates/audio/src'
    assert all(p.read_bytes() == (package / 'src' / p.relative_to(audio)).read_bytes() for p in audio.rglob('*') if p.is_file()), 'copy not restored'
    summary = {'positive_tests': int(match[1]), 'ignored_tests': int(match[2]), 'compiled_controls': len(receipts), 'controls': receipts, 'positive_binary_sha256': clock.common.sha(binary), 'root_inputs_unchanged': True, 'source_copies_restored': True, 'root_lock_sha256': clock.common.sha(ROOT / 'engine/Cargo.lock'), 'scope': 'Canonical revision protocol, accepted audio-thread transition, stale/future filtering, reusable consumer EOF, bounded recycling and steady caller heap activity. Persistent producer/clock mapping/product candidate cutover/device/peak remain open.'}
    (output / 'summary.json').write_text(json.dumps(summary, indent=2) + '\n')
    print(json.dumps(summary, indent=2), flush=True)


if __name__ == '__main__':
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--output', type=Path, required=True)
    p.add_argument('--target-dir', type=Path, required=True)
    p.add_argument('--positive-only', action='store_true')
    args = p.parse_args()
    main(args.output.resolve(), args.target_dir.resolve(), args.positive_only)
