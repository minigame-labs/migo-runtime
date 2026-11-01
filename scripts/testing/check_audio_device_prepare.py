#!/usr/bin/env python3
"""Heavy device sinc setup cannot be copied into the render worker unchanged.

Exercise the owned preparation candidate on the existing AudioCpu and streaming
runtime before production wiring. Identical formats/context IDs are not route
identities. Only fresh, exact runtime assertions count as rejected faults.
"""
from __future__ import annotations
import argparse
import json
from pathlib import Path
import check_audio_whole_decode as product

ROOT = product.whole.ROOT
PREFIX = 'device_prepare::tests::'


def declarations(original):
    rows = []
    def change(name, before, after, test, assertion, count=1):
        assert original.count(before) == count, (name, 'mutation anchor drift')
        rows.append((name, before, after, PREFIX + test, assertion))
    accept = 'acceptance_rejects_live_foreign_cancelled_and_unprepared_results'
    route = 'same_format_and_aba_replacement_cancel_taken_owners_and_keep_new_identity'
    turns = 'actual_cpu_turns_prepare_one_layer_and_leave_the_shared_async_worker_free'
    pcm = 'prepared_handoff_preserves_every_library_sample_and_the_original_eof_clock'
    change('prepare-foreign-generation-accepted', 'Arc::ptr_eq(request, &ready.request)', 'true', accept,
           'a live same-id same-format foreign generation was accepted')
    change('prepare-cancelled-generation-accepted', '&& !request.cancel.load(Ordering::Acquire)', '', accept,
           'a cancelled matching generation was accepted')
    change('prepare-unfinished-filter-accepted', '&& ready.is_ready()', '&& ready.error.is_none()', accept,
           'an unprepared filter reached the render owner')
    change('prepare-route-does-not-cancel', 'request.cancel.store(true, Ordering::Release);', 'let _ = request;', route,
           'superseded route kept rendering')
    change('prepare-cancel-retains-scratch', 'self.output = Vec::new();', '', route,
           'cancelled route retained filter scratch')
    change('prepare-two-layers-per-turn', 'ready.step(&[], false)?;', 'ready.step(&[], false)?; ready.step(&[], false)?;', turns,
           'preparation ran more than one layer per CPU turn')
    change('prepare-precancelled-constructor', 'if request.cancel.load(Ordering::Acquire) {', 'if false {',
           'precancelled_setup_and_closed_hosts_never_construct_a_filter', 'cancelled or closed setup constructed a filter')
    change('prepare-render-allocates', 'self.output.clear();', 'std::hint::black_box(vec![0_u8;64]); self.output.clear();',
           'handed_off_filter_allocates_neither_on_first_processing_step_nor_steady_state', 'prepared render step allocated')
    change('prepare-target-rate-fixed', 'Stream::new(f.source, f.target, f.channels, None)?', 'Stream::new(f.source, 48000, f.channels, None)?', pcm,
           'prepared handoff changed mature PCM or the submitted format')
    change('prepare-eof-not-forwarded', 'stream.step(input, eof, &mut self.output, &self.request.cancel)',
           'stream.step(input, false, &mut self.output, &self.request.cancel)', pcm, 'prepared handoff lost its terminal clock')
    before = '''            let _ = send.send(result);
            wakeup.notify();'''
    change('prepare-result-does-not-wake', before, '            let _ = send.send(result);',
           'publication_wakes_the_owner_and_pending_polls_do_not_construct_filters', 'preparation result did not wake the render owner')
    change('prepare-terminal-code-lost', 'self.error = Some(code);', 'self.error = Some(ErrorCode::Internal);', route,
           'superseded route kept rendering')
    change('prepare-native-route-uses-cpu', 'if format.source == format.target {', 'if false {',
           'native_bypass_uses_no_cpu_and_invalid_formats_preserve_the_live_route', 'native bypass started an async preparation')
    change('prepare-initial-owner-on-async-worker', 'let mut ready = cpu.run(initialize).await??;', 'let mut ready = initialize()?;', turns,
           'setup ran outside the existing AudioCpu executor')
    close = 'completed_and_taken_results_observe_host_close_before_render_use'
    change('prepare-closed-completion-accepted', '&& !request.cpu.is_closed()', '', close,
           'completed result crossed host close before acceptance')
    change('prepare-closed-owner-renders', 'else if self.request.cpu.is_closed() {', 'else if false {', close,
           'handed-off filter ran after host close')
    return rows


def main(work, target, preflight, controls_only):
    whole = product.whole
    package, env, frozen = whole.support.clock.prepare(work, target, whole.extra_inputs() + [Path(__file__)])
    path = package / 'src/device_prepare.rs'
    original = path.read_text()
    rows = declarations(original)
    programs, controls, profiles = [], [], {}
    for profile in ([] if controls_only else ['release'] if preflight else ['release', 'debug']):
        label = 'positive-' + profile
        binary = product.src.build(package, work, env, label, profile == 'release')
        log = work / (label + '-tests.log')
        command = [str(binary), PREFIX] if preflight else [str(binary)]
        assert whole.support.clock.common.run(command, log, package, env, timeout=900) == 0, log
        count = 11 if preflight else 567
        assert f'test result: ok. {count} passed; 0 failed;' in log.read_text(), log
        profiles[profile] = dict(passed=count, ignored=0 if preflight else 2)
        programs.append(dict(name=label, path=str(binary), sha256=whole.support.codecs.sha(binary), fresh_rust=True, pruned=False))
    if not preflight:
        for name, before, after, test, assertion in rows:
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
                print('actual device preparation fault rejected:', name, flush=True)
            finally:
                path.write_text(original)
    whole.verify_inputs(work, frozen)
    root = ROOT / 'engine/crates/audio/src'
    assert all(p.read_bytes() == (package / 'src' / p.relative_to(root)).read_bytes() for p in root.rglob('*') if p.is_file())
    summary = dict(profiles=profiles, controls=controls, compiled_controls=len(controls), programs=programs,
                   root_inputs_unchanged=True, source_copies_restored=True, preflight=preflight, controls_only=controls_only,
                   candidate_only=True, production_device_sinc_migrated=False, existing_executor=True,
                   max_setup_layers_per_turn=1, opaque_per_request_identity=True, per_context_pending_results=1,
                   device_or_shipping_acceptance=False)
    (work / 'summary.json').write_text(json.dumps(summary, indent=2) + '\n')
    print('Device preparation candidate verified', profiles, len(controls), flush=True)


if __name__ == '__main__':
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--output', type=Path, required=True)
    p.add_argument('--target-dir', type=Path, required=True)
    p.add_argument('--preflight', action='store_true')
    p.add_argument('--controls-only', action='store_true')
    a = p.parse_args()
    assert not (a.preflight and a.controls_only)
    main(a.output.resolve(), a.target_dir.resolve(), a.preflight, a.controls_only)
