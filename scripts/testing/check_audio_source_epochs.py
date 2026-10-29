#!/usr/bin/env python3
"""Execute source-load ownership contracts and freshly compiled fault controls.

Numeric IDs alone let late decodes overwrite a replacement or recreated player.
Freeze actual audio/IO/service/shared inputs, copy the audio crate, and require
runtime assertions at admission, publication and installation. A compile error,
timeout or killed process cannot count as a detected source-load regression.
"""
from __future__ import annotations

import argparse
import json
from pathlib import Path
import re

import check_audio_playback_clock as clock

ROOT = clock.ROOT


def function(source: str, signature: str) -> str:
    start = source.index(signature)
    end = source.index('\n    }', start) + len('\n    }')
    return source[start:end]


def controls(originals: dict[str, str]):
    declarations = []

    def add(name, file, before, after, test, module='audio_thread::source_load_tests::'):
        assert originals[file].count(before) == 1, (name, 'mutation anchor drift')
        declarations.append((name, file, before, after, module + test))

    def change(name, file, signature, before, after, test):
        body = function(originals[file], signature)
        assert body.count(before) == 1, (name, 'function anchor drift')
        add(name, file, body, body.replace(before, after), test)

    file = 'inner_audio.rs'
    add('older-source-tag-admitted', file,
        'source_id != 0 && source_id <= self.last_source_id', 'false',
        'older_source_tags_cannot_retire_a_newer_preparation_or_loaded_source')
    add('source-generation-lags', file,
        'self.source_generation.load(Ordering::Acquire)', 'self.source_generation.load(Ordering::Acquire).saturating_sub(1)',
        'superseded_queued_jobs_are_cancelled_before_peak_admission')
    add('allocation-identity-ignored', file,
        'load.matches(self)', 'load.is_current()',
        'equal_generations_from_different_live_players_do_not_match')
    add('destruction-keeps-token-valid', file,
        'self.shared.source_generation.store(0, Ordering::Release);', '',
        'destruction_invalidates_tokens_even_if_atomic_state_is_retained')
    begin = '    pub(crate) fn begin_load_for('
    for name, before, after, test in [
        ('old-stream-not-cancelled', '        self.cancel_stream();\n', '',
         'source_switch_retires_old_pcm_stream_seek_and_events'),
        ('old-pcm-retained', '        self.source = AudioSource::Owned(Vec::new());\n', '',
         'source_switch_retires_old_pcm_stream_seek_and_events'),
        ('old-seek-retained', '        self.shared.take_seek_target();\n', '',
         'source_switch_retires_old_pcm_stream_seek_and_events'),
        ('old-events-retained', '        self.pending_events.clear();\n', '',
         'source_switch_retires_old_pcm_stream_seek_and_events'),
        ('old-playing-state-retained', '        self.shared.set_state(PlaybackState::Stopped);\n', '',
         'source_switch_retires_old_pcm_stream_seek_and_events'),
        ('old-pcm-credit-retained', '        self.stream_permit = None;\n', '',
         'source_replacement_returns_old_pcm_credit_before_new_admission'),
        ('epoch-change-allocates', '        let load = self.shared.begin_source(source_id)?;',
         '        std::hint::black_box(vec![0_u8; 64]);\n        let load = self.shared.begin_source(source_id)?;',
         'source_epoch_changes_reuse_the_existing_atomic_allocation'),
    ]:
        change(name, file, begin, before, after, test)
    guard = ('        if !self.shared.owns_load(&load) || self.shared.is_loaded() {\n'
             '            return Err(InnerAudioLoadToken::cancelled_error());\n        }\n')
    for name, method in [('owned-completion-accepts-stale', 'complete_load'),
                         ('cached-completion-accepts-stale', 'complete_cached_load')]:
        change(name, file, f'    pub(crate) fn {method}(', guard, '',
               'stale_preparation_tokens_are_rejected_for_all_backings')
    stream_guard = guard.replace('            return Err', '            state.cancel();\n            return Err')
    change('stream-completion-accepts-stale', file, '    pub(crate) fn complete_stream_load(',
           stream_guard, '', 'stale_preparation_tokens_are_rejected_for_all_backings')
    change('rejected-stream-does-not-cancel', file, '    pub(crate) fn complete_stream_load(',
           '            .inspect_err(|_| state.cancel())?', '?',
           'rejected_stream_preparation_cancels_its_incoming_producer')
    body = function(originals[file], '    pub(crate) fn begin_source(')
    assert body.count('generation.checked_add(1)') == body.count('previous + 1') == 1
    wrapped = body.replace('generation.checked_add(1)', 'Some(generation.wrapping_add(1))').replace(
        'previous + 1', 'previous.wrapping_add(1)')
    add('source-generation-wraps', file, body, wrapped,
        'source_generation_exhaustion_never_wraps_or_revalidates_a_token', 'inner_audio::tests::')
    file = 'audio_thread.rs'
    add('publication-accepts-retired-pcm', file,
        '&& (!load.is_current() || resp.is_closed())', '&& false',
        'superseded_decoded_pcm_never_enters_the_result_queue')
    add('worker-admits-cancelled-peak', file,
        '\n                if job.is_cancelled() {', '\n                if false {',
        'superseded_queued_jobs_are_cancelled_before_peak_admission')
    add('lazy-pool-starts-for-abandoned-reply', file,
        '\n        if job.is_cancelled() {', '\n        if false {',
        'abandoned_replies_do_not_start_a_decode_pool')
    add('install-does-not-check-stale-errors', file,
        'Some(player) if player.shared.owns_load(&load) && !resp.is_closed() => {',
        'Some(player) if !resp.is_closed() => {',
        'stale_errors_are_cancelled_without_changing_the_new_source')
    return declarations


def main(output: Path, target: Path):
    package, env, frozen = clock.prepare(output, target, [Path(__file__).resolve(),
        ROOT / 'scripts/testing/check_gitignore_contract.py'])
    originals = {name: (package / 'src' / name).read_text()
                 for name in ['inner_audio.rs', 'audio_thread.rs']}
    declarations = controls(originals)
    binary = clock.build(package, output, env, 'positive')
    assert clock.common.run([str(binary)], output / 'positive-tests.log', package, env) == 0
    match = re.search(r'test result: ok\. (\d+) passed; 0 failed; (\d+) ignored;',
                      (output / 'positive-tests.log').read_text())
    assert match, 'positive audio suite did not finish'
    receipts = []
    for name, relative, before, after, test in declarations:
        source = package / 'src' / relative
        try:
            source.write_text(originals[relative].replace(before, after))
            mutant = clock.build(package, output, env, name)
            log = output / f'{name}-test.log'
            status = clock.common.run([str(mutant), '--exact', test, '--nocapture'], log,
                                      package, env, timeout=20)
            observed = log.read_text()
            assert status == 101 and 'panicked at' in observed and '1 failed' in observed, (
                name, status, 'must fail an executed test assertion')
            receipts.append({'name': name, 'test': test, 'exit': status,
                             'binary_sha256': clock.common.sha(mutant)})
        finally:
            source.write_text(originals[relative])
        print(f'compiled source epoch control {name}: runtime assertion rejected', flush=True)
    assert all(clock.common.sha(ROOT / path) == checksum for path, checksum in frozen.items())
    audio = ROOT / 'engine/crates/audio'
    assert all(path.read_bytes() == (package / 'src' / path.relative_to(audio / 'src')).read_bytes()
               for path in (audio / 'src').rglob('*') if path.is_file())
    summary = {'positive_tests': int(match[1]), 'ignored_tests': int(match[2]),
               'compiled_controls': len(receipts), 'controls': receipts,
               'positive_binary_sha256': clock.common.sha(binary), 'root_inputs_unchanged': True,
               'source_copies_restored': True, 'existing_dependency_versions_preserved': True,
               'root_lock_sha256': clock.common.sha(ROOT / 'engine/Cargo.lock'),
               'scope': 'Source-load generation/allocation identity, retirement and decode boundaries. '
                        'Not active whole-file decoder interruption, yielding DecodePool, '
                        'outside-window transport controls, native/device/peak-performance acceptance.'}
    (output / 'summary.json').write_text(json.dumps(summary, indent=2) + '\n')
    print(json.dumps(summary, indent=2), flush=True)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--target-dir', type=Path, required=True)
    args = parser.parse_args()
    main(args.output.resolve(), args.target_dir.resolve())
