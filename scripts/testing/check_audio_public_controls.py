#!/usr/bin/env python3
"""Validate public seek/restart through source-bound receipts on the audio thread.

Moving a cursor inside a reclaimed PCM window cannot rewind the actual source.
Freeze the real audio path and pinned candidate, exercise public commands, and
rebuild violations in admission, receipt identity, ordering and retained DSP.
"""
from __future__ import annotations
import argparse
import json
from pathlib import Path
import re
import shutil
import subprocess
import tomllib

import check_audio_seek_phase as phase
import check_audio_loop_phase as cyclic
import check_audio_stream_revisions as revisions

ROOT = phase.ROOT
codecs = phase.codecs
clock = revisions.clock


def prepare(work, target, rubato):
    env, frozen, _, libraries, resampling = phase.prepare(work, target, rubato)
    name = 'public_control_contracts.rs'
    path = ROOT/'scripts/testing/fixtures/audio-codec-probe'/name
    shutil.copy2(path, work/'source'/name)
    lib = work/'probe/src/lib.rs'
    lib.write_text(lib.read_text()+f'\n#[cfg(test)] #[path={json.dumps(str(work/"source"/name))}] mod public_control_contracts;\n')
    assert str(path.relative_to(ROOT)) in frozen
    retained = {str(p.relative_to(work)): codecs.sha(p)
                for tree in [work/'source', work/'probe/src'] for p in tree.rglob('*.rs')}
    (work/'retained-sources.json').write_text(json.dumps(retained, indent=2)+'\n')
    return env, frozen, retained, libraries, resampling


def native_declarations(originals):
    rows = []
    prefix = 'inner_audio::public_control_tests::'
    def change(name, path, signature, before, after, test):
        source = originals[path]
        start = source.index(signature)
        end = source.index('\n    }', start)+len('\n    }')
        body = source[start:end]
        assert body.count(before) == 1, (name, 'mutation anchor drift')
        rows.append((name, path, body, body.replace(before, after), prefix+test))
    player = 'inner_audio.rs'
    seek = '    pub fn seek('
    wait = 'polling_a_pending_public_control_never_allocates_or_renders_old_pcm'
    seek_test = 'public_seek_waits_for_source_then_for_fresh_pcm_in_every_play_state'
    stop = 'a_later_stop_cannot_be_reversed_by_an_older_seek_receipt'
    replay = 'public_play_restarts_a_reclaimed_completed_source_without_replaying_its_tail'
    bound = 'public_control_backlog_and_waiting_deadline_are_bounded'
    receipts = 'public_receipts_must_match_the_requested_target_deadline_and_source'
    ports = 'source_ports_and_public_time_conversion_reject_invalid_identity_or_clock'
    dispatch = '        if self.request_stream_control(time, ControlKind::Seek) {\n            return;\n        }'
    change('public-seek-bypasses-source', player, seek, dispatch, '', seek_test)
    change('request-clears-pcm-before-source-success', player, seek, dispatch,
           '        if let AudioSource::Owned(pcm) = &mut self.source { pcm.clear(); }\n'+dispatch, seek_test)
    change('pending-mixer-renders-old-pcm', player, '    pub fn process(',
           'if !self.stream_controls.is_empty() {', 'if false {', wait)
    change('completed-player-skips-source-receipt', player, '    pub fn poll_stream(',
           '        self.poll_stream_controls();', '', replay)
    change('completed-player-does-not-schedule-control-poll', player, '    pub(crate) fn needs_stream_poll(',
           '!self.stream_controls.is_empty()', 'false', replay)
    change('quiet-receipt-exposes-older-position', player, '    fn accept_stream_revision_as(',
           'if expose_position { target_frame } else { 0 }', 'target_frame', stop)
    change('public-stop-does-not-rewind-source', player, '    pub fn stop(',
           '        self.request_stream_control(0.0, ControlKind::Restart);', '', stop)
    change('ended-play-reuses-reclaimed-tail', player, '    pub fn play(',
           'if self.replay_needed', 'if false', replay)
    change('public-control-backlog-wraps', 'stream_control.rs', '    pub(crate) fn is_full(',
           'self.len == self.slots.len()', 'false', bound)
    change('public-control-wait-renews-expired-deadline', player, '    fn poll_stream_controls(',
           'tokio::time::Instant::now() < pending.deadline', 'true', bound)
    change('prepared-target-not-bound-to-request', player, '    fn poll_stream_controls(',
           'prepared.target != pending.target', 'false', receipts)
    change('prepared-deadline-not-bound-to-request', player, '    fn poll_stream_controls(',
           'prepared.deadline != pending.deadline', 'false', receipts)
    change('public-target-uses-output-rate', 'streaming.rs', '    pub(crate) fn seek_frame(',
           'seconds * self.input_rate as f64', 'seconds * self.output_rate as f64', seek_test)
    change('receipt-rounds-output-origin-down', 'stream_control.rs', '    pub(crate) fn mapped_output(',
           'position.frame + u64::from(position.phase != 0)', 'position.frame', seek_test)
    change('ordinary-source-installs-control-port', 'streaming.rs', '    pub(crate) fn install_controls(',
           '!self.retains_source()', 'false', ports)
    change('source-control-poll-allocates', player, '    fn poll_stream_controls(',
           '        use tokio::sync::oneshot::error::TryRecvError;',
           '        std::hint::black_box(vec![0_u8;64]);\n        use tokio::sync::oneshot::error::TryRecvError;', wait)
    change('replacement-keeps-old-control-receipt', player, '    fn cancel_stream(',
           '        self.stream_controls.clear();', '',
           'replacement_discards_pending_receipts_before_the_new_source_is_installed')
    change('rejected-public-target-clears-pcm', player, '    fn poll_stream_controls(',
           '                ControlReply::Rejected(code) | ControlReply::NotStarted(code) => {',
           '                ControlReply::Rejected(code) | ControlReply::NotStarted(code) => {\n                    if let AudioSource::Owned(pcm) = &mut self.source { pcm.clear(); }',
           'rejected_public_targets_preserve_the_previous_pcm_clock_and_play_state')
    change('public-stop-announces-a-seek', player, '    fn poll_stream_controls(',
           '!quiet\n                                && matches!(pending.kind, ControlKind::Seek | ControlKind::Start)', 'true', stop)
    return rows


def candidate_declarations(originals):
    path = 'producer_control.rs'
    source = originals[path]
    test = 'public_control_contracts::public_file_http_seek_stop_and_play_preserve_exact_pcm_and_source_clock'
    rows = []
    def change(name, before, after):
        assert source.count(before) == 1, (name, 'mutation anchor drift')
        rows.append((name, path, before, after, test))
    change('candidate-public-source-port-not-installed', '        state.install_controls(sender)?;', '        drop(sender);')
    change('candidate-public-target-lost', 'None => slot.seek(Timestamp::new(target)),', 'None => slot.seek(Timestamp::ZERO),')
    change('candidate-publication-output-origin-rounded-down',
           'position.frame + u64::from(position.phase != 0)', 'position.frame')
    start = source.index('                    let accepted = while_active(')
    check = source.index('                    if !matches!(accepted, Some(Ok(Ok(()))))', start)
    end = source.index('\n                    }', check)+len('\n                    }')
    change('candidate-publishes-before-audio-thread-acceptance', source[start:end], '                    drop(receipt);')
    terminal_test = 'timed_m4a::phase_tests::exact_terminal_seek_has_no_pcm_and_preserves_repeated_restart'
    timed = originals['timed_m4a.rs']
    for name, before, after in [
        ('candidate-rejects-exact-terminal-seek', 'frame > frames', 'frame >= frames'),
        ('candidate-terminal-publishes-bypass-pcm', 'if self.terminal_seek {', 'if false {'),
        ('candidate-terminal-state-survives-restart', 'self.terminal_seek = bypass_terminal;', 'self.terminal_seek |= bypass_terminal;'),
        ('candidate-terminal-forgets-bypass-count', '                self.terminal_remaining -= pcm.len();', ''),
        ('candidate-terminal-seeks-after-last-codec-frame', '            plan.input_origin = frame - 1;', ''),
    ]:
        assert timed.count(before) == 1, (name, 'mutation anchor drift')
        rows.append((name, 'timed_m4a.rs', before, after, terminal_test))
    return rows


def build_native(package, work, env, name):
    if name == 'positive':
        assert clock.common.run(['cargo', 'clean', '--offline', '--locked', '-p', 'migo-audio'],
                                work/'positive-clean.log', package, env) == 0
    log = work/(name+'-build.jsonl')
    assert clock.common.run(['cargo', 'test', '--lib', '--offline', '--locked', '--no-run',
                             '--message-format=json'], log, package, env) == 0, ('compile failed', log)
    artifacts = []
    for line in log.read_text().splitlines():
        try: row = json.loads(line)
        except ValueError: continue
        if row.get('reason') == 'compiler-artifact' and row.get('executable') and row['target']['name'] == 'audio':
            assert row['fresh'] is False, 'actual audio test artifact must be rebuilt'
            artifacts.append(Path(row['executable']))
    assert len(artifacts) == 1, artifacts
    binary = work/(name+'-tests')
    shutil.copy2(artifacts[0], binary)
    return binary


def faults(work, package, env, originals, rows, build, source_dir):
    receipts = []
    for name, path, before, after, test in rows:
        source = source_dir/path
        assert originals[path].count(before) == 1, name
        try:
            source.write_text(originals[path].replace(before, after))
            binary = build(name)
            log = work/(name+'-test.log')
            with log.open('w') as output:
                result = subprocess.run([str(binary), '--exact', test, '--nocapture'], cwd=package,
                                        env=env, stdout=output, stderr=subprocess.STDOUT, timeout=120)
            observed = log.read_text()
            assert result.returncode == 101 and 'panicked at' in observed and '1 failed' in observed, name
            receipts.append({'name': name, 'test': test, 'exit': result.returncode,
                             'binary_sha256': codecs.sha(binary)})
        finally:
            source.write_text(originals[path])
        print('compiled public-control fault', name, ': executed assertion rejected', flush=True)
    return receipts


def native(work, target, positive_only):
    package, env, frozen = clock.prepare(work, target, [Path(__file__).resolve(),
        ROOT/'scripts/testing/check_gitignore_contract.py', ROOT/'scripts/testing/check_audio_stream_revisions.py'])
    originals = {n: (package/'src'/n).read_text() for n in ['inner_audio.rs', 'streaming.rs', 'stream_control.rs']}
    rows = native_declarations(originals)+revisions.controls(originals)+phase.publication_declarations(originals['streaming.rs'])
    binary = build_native(package, work, env, 'positive')
    log = work/'positive-tests.log'
    assert clock.common.run([str(binary)], log, package, env) == 0
    assert re.search(r'test result: ok\. 304 passed; 0 failed; 2 ignored;', log.read_text())
    receipts = faults(work, package, env, originals, [] if positive_only else rows,
                      lambda name: build_native(package, work, env, name), package/'src')
    assert all(codecs.sha(ROOT/p) == h and codecs.sha(work/'source-inputs'/p) == h for p, h in frozen.items())
    audio = ROOT/'engine/crates/audio/src'
    assert all(p.read_bytes() == (package/'src'/p.relative_to(audio)).read_bytes() for p in audio.rglob('*.rs'))
    summary = {'positive_tests': 304, 'ignored_tests': 2, 'new_tests': 9,
               'compiled_controls': len(receipts), 'new_controls': 0 if positive_only else len(native_declarations(originals)),
               'retained_revision_controls': 0 if positive_only else 22, 'controls': receipts,
               'retained_publication_controls': 0 if positive_only else 8,
               'root_inputs_unchanged': True, 'source_copies_restored': True,
               'root_lock_sha256': codecs.sha(ROOT/'engine/Cargo.lock')}
    (work/'summary.json').write_text(json.dumps(summary, indent=2)+'\n')
    print(json.dumps(summary, indent=2), flush=True)


def candidate(work, target, rubato, positive_only):
    env, frozen, retained, libraries, resampling = prepare(work, target, rubato)
    package = work/'probe'
    codecs.run(['cargo', 'metadata', '--offline', '--format-version', '1'], work/'metadata.jsonl', cwd=package, env=env)
    identity = lambda p: (p['name'], p['version'], p.get('source'), p.get('checksum'))
    locked = tomllib.loads((ROOT/'engine/Cargo.lock').read_text())['package']
    known = {p['name'] for p in locked}
    assert all(p['name'] not in known or identity(p) in set(map(identity, locked))
               for p in tomllib.loads((package/'Cargo.lock').read_text())['package'])
    originals = {n: (work/'source'/n).read_text() for n in ['producer_control.rs', 'resampled_streaming.rs', 'timed_m4a.rs']}
    retained_loop = [row for row in cyclic.declarations(originals) if row[1] == 'timed_m4a.rs']
    assert len(retained_loop) == 4
    rows = candidate_declarations(originals)+retained_loop
    codecs.run(['cargo', 'check', '--offline', '--locked', '--lib'], work/'normal-library-check.log', cwd=package, env=env)
    positives = phase.positive_profiles(work, env, [
        ('cyclic', 'timed_m4a::loop_tests::', 7),
        ('cyclic-clock', 'resampled_streaming::loop_clock_tests::', 2),
        ('public-source', 'public_control_contracts::', 3),
    ])
    assert positives == {'release': 246, 'debug': 243}, positives
    receipts = faults(work, package, env, originals, [] if positive_only else rows,
                      lambda name: phase.file.dsp.source.build(work, env, name, release=False, clean=False), work/'source')
    assert all(codecs.sha(ROOT/p) == h and codecs.sha(work/'source-inputs'/p) == h for p, h in frozen.items())
    assert all(codecs.sha(work/p) == h for p, h in retained.items()), 'copy not restored'
    provenance = json.loads((work/'library-source-provenance.json').read_text())
    for row in provenance:
        assert phase.file.dsp.source.index_gate.timeline.source_tree(libraries[row['crate']]) == row['source_files']
    for name, row in resampling.items():
        assert all(codecs.sha(work/'resampling-vendor'/f"{name}-{row['version']}"/p) == h for p, h in row['source_files'].items())
    summary = {'positive_tests_per_profile': positives, 'new_tests': 4,
               'public_seek_restart_sequences_per_profile': 192, 'public_source_actors_per_profile': 32,
               'real_reclaimed_sources_per_profile': 2, 'real_full_queue_stop_sources_per_profile': 2,
               'terminal_configurations_per_profile': 72,
               'compiled_controls': len(receipts), 'new_controls': 0 if positive_only else 9, 'retained_loop_controls': 0 if positive_only else 4,
               'controls': receipts, 'root_inputs_unchanged': True, 'source_copies_restored': True,
               'root_lock_sha256': codecs.sha(ROOT/'engine/Cargo.lock'), 'libraries': len(provenance),
               'patches': sum(len(r['patches']) for r in provenance), 'resampling_source_files_unchanged': True,
               'scope': 'Shared native/audio-thread public seek/stop/play port connected to pinned candidate.File/HTTP clocked receipts,reclaimed replay,ordered stop under backpressure.No productcodec cutover,loop wrapping/live toggle or quota renewal completion.'}
    (work/'summary.json').write_text(json.dumps(summary, indent=2)+'\n')
    print(json.dumps(summary, indent=2), flush=True)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--target-dir', type=Path, required=True)
    parser.add_argument('--rubato-cargo-home', type=Path, required=True)
    parser.add_argument('--scope', choices=['native', 'candidate'], required=True)
    parser.add_argument('--positive-only', action='store_true')
    args = parser.parse_args()
    if args.scope == 'native': native(args.output.resolve(), args.target_dir.resolve(), args.positive_only)
    else: candidate(args.output.resolve(), args.target_dir.resolve(), args.rubato_cargo_home.resolve(), args.positive_only)
