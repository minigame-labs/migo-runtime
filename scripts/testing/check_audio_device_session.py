#!/usr/bin/env python3
"""Persistent device-filter idle must preserve tail PCM and restart phase.

Inactive graphs need no render or SRC work after a bounded zero tail. Waking
must preserve mature PCM and phase. Only fresh exact assertions reject faults.
"""
from __future__ import annotations
import argparse
import json
from pathlib import Path
import check_audio_whole_decode as product

ROOT = product.whole.ROOT
PREFIX = 'device_prepare::pull::session::tests::'


def declarations(originals):
    rows = []
    def change(name, file, before, after, test, assertion):
        assert originals[file].count(before) == 1, (name, 'mutation anchor drift')
        rows.append((name, file, before, after, PREFIX + test, assertion))
    def session(name, before, after, test, assertion):
        change(name, 'device_session.rs', before, after, test, assertion)
    startup = 'inactive_startup_does_no_source_or_filter_work'
    pcm = 'multiple_bursts_preserve_mature_pcm_phase_and_complete_tail'
    bounded = 'first_active_tail_idle_and_wake_turns_are_bounded_and_allocate_nothing'
    metadata = 'quiet_bound_uses_prepared_mature_history_and_all_cascade_rates'
    clock = 'quiet_clock_overflow_and_cached_nonzero_pcm_cannot_disappear'
    events = 'unread_ended_fences_prevent_quiescence_and_are_delivered_once'
    session('session-startup-renders-idle', 'sleeping: true,', 'sleeping: false,', startup,
            'inactive startup ran the continuous graph')
    session('session-idle-keeps-running', 'return Ok(SessionTurn::Quiescent);', 'self.sleeping = false;', startup,
            'inactive startup ran the continuous graph')
    session('session-quiet-bound-zeroed', 'quiet_span,', 'quiet_span: 0,', metadata,
            'quiet bound lost a mature history window or source-rate projection')
    change('session-history-window-lost', 'bandlimited.rs', 'sinc_points\n            .checked_mul(2)',
           'sinc_points\n            .checked_mul(1)', metadata,
           'quiet bound lost a mature history window or source-rate projection')
    change('session-cascade-source-projection-lost', 'streaming_sinc.rs',
           '.div_ceil(u128::from(source_rate))', '.div_ceil(u128::from(self.source))', metadata,
           'quiet bound lost a mature history window or source-rate projection')
    session('session-eof-accepted', 'if block.eof {', 'if false {',
            'continuous_graph_eof_is_a_sticky_terminal_error', 'continuous graph EOF was accepted as an idle flush')
    session('session-sleep-skips-cancel-and-close', '        self.pull.check_live()?;\n', '',
            'cancellation_and_host_close_revoke_even_a_sleeping_owner', 'sleeping session bypassed owner revocation')
    before = 'self.sleeping = false;\n            self.quiet_at = None;'
    wake = originals['device_session.rs']
    session('session-wake-keeps-old-tail-fence', wake, wake.replace('self.quiet_at = None;', ''),
            'reactivation_clears_the_old_quiet_deadline_and_control_work_stays_active',
            'reactivation retained the old quiet deadline')
    session('session-wake-rewinds-publication-clock', before,
            before + '\n            self.pull.published = 0;', pcm,
            'last publication was replaced by an idle notification')
    session('session-unread-events-hidden', '&& self.pull.buffers.as_ref().unwrap().events.is_empty()', '', events,
            'idle hid an unread ended event')
    session('session-cached-nonzero-pcm-hidden',
            '&& self.pull.ready.output[self.pull.output_head..]\n                .iter()\n                .all(|&sample| sample == 0.0)', '', clock,
            'quiescence discarded cached nonzero PCM')
    session('session-final-publication-lost', '        Ok(SessionTurn::Work(turn))',
            '        Ok(if self.sleeping { SessionTurn::Quiescent } else { SessionTurn::Work(turn) })', pcm,
            'last publication was replaced by an idle notification')
    session('session-quiet-clock-wraps', 'self.pull.rendered.checked_add(self.quiet_span)',
            'Some(self.pull.rendered.wrapping_add(self.quiet_span))', clock,
            'quiet source fence wrapped instead of retiring')
    session('session-turn-allocates', '        self.pull.check_live()?;\n',
            '        std::hint::black_box(vec![0_u8;64]); self.pull.check_live()?;\n', bounded,
            'session turn allocated')
    session('session-wake-rebuilds-scratch', before,
            before + '\n            let buffers = self.pull.buffers.as_mut().unwrap(); let mut scratch = Vec::with_capacity(1000000); scratch.extend_from_slice(&buffers.input); buffers.input = scratch;', pcm,
            'idle wake replaced the prepared owner or scratch')
    return rows


def main(work, target, preflight, controls_only):
    whole = product.whole
    package, env, frozen = whole.support.clock.prepare(work, target, whole.extra_inputs() + [Path(__file__)])
    files = {name: package / 'src' / name for name in ['device_session.rs', 'streaming_sinc.rs', 'bandlimited.rs']}
    originals = {name: path.read_text() for name, path in files.items()}
    rows = declarations(originals)
    programs, controls, profiles = [], [], {}
    for profile in ([] if controls_only else ['release'] if preflight else ['release', 'debug']):
        label = 'positive-' + profile
        binary = product.src.build(package, work, env, label, profile == 'release')
        log = work / (label + '-tests.log')
        command = [str(binary), PREFIX] if preflight else [str(binary)]
        assert whole.support.clock.common.run(command, log, package, env, timeout=900) == 0, log
        count = 10 if preflight else 567
        assert f'test result: ok. {count} passed; 0 failed;' in log.read_text(), log
        profiles[profile] = dict(passed=count, ignored=0 if preflight else 2)
        programs.append(dict(name=label, path=str(binary), sha256=whole.support.codecs.sha(binary), fresh_rust=True, pruned=False))
    if not preflight:
        for name, file, before, after, test, assertion in rows:
            path = files[file]
            original = originals[file]
            try:
                path.write_text(original.replace(before, after))
                binary = product.src.build(package, work, env, name, True)
                log = work / (name + '-test.log')
                status = whole.support.clock.common.run([str(binary), '--exact', test, '--nocapture'], log, package, env, timeout=120)
                text = log.read_text()
                assert status == 101 and all(t in text for t in ['running 1 test', test, 'panicked at', '1 failed', assertion]), (name, status, log)
                digest = whole.support.codecs.sha(binary)
                controls.append(dict(name=name, test=test, expected_assertion=assertion, exit=status, binary_sha256=digest))
                programs.append(dict(name=name, path=str(binary), sha256=digest, fresh_rust=True, pruned=True))
                binary.unlink()
                (work / 'completed-controls.json').write_text(json.dumps(controls, indent=2) + '\n')
                print('actual device-session fault rejected:', name, flush=True)
            finally:
                path.write_text(original)
    whole.verify_inputs(work, frozen)
    root = ROOT / 'engine/crates/audio/src'
    assert all(p.read_bytes() == (package / 'src' / p.relative_to(root)).read_bytes() for p in root.rglob('*') if p.is_file())
    summary = dict(profiles=profiles, controls=controls, compiled_controls=len(controls), programs=programs,
                   root_inputs_unchanged=True, source_copies_restored=True, preflight=preflight, controls_only=controls_only,
                   persistent_phase_and_quiet_tail_candidate=True, production_graph_idle_wiring=False,
                   production_device_sinc_migrated=False, device_or_shipping_acceptance=False)
    (work / 'summary.json').write_text(json.dumps(summary, indent=2) + '\n')
    print('Continuous device session candidate verified', profiles, len(controls), flush=True)


if __name__ == '__main__':
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--output', type=Path, required=True)
    p.add_argument('--target-dir', type=Path, required=True)
    p.add_argument('--preflight', action='store_true')
    p.add_argument('--controls-only', action='store_true')
    a = p.parse_args()
    assert not (a.preflight and a.controls_only)
    main(a.output.resolve(), a.target_dir.resolve(), a.preflight, a.controls_only)
