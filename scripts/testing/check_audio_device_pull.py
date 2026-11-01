#!/usr/bin/env python3
"""Filter lookahead must not publish ended early or discard device PCM suffixes.

Verify the bounded pull candidate against mature PCM and real AudioContext node
collection. Rendering and publication have separate rational clocks. Only fresh,
exact runtime assertions reject faults; production wiring remains separate.
"""
from __future__ import annotations
import argparse
import json
from pathlib import Path
import check_audio_whole_decode as product

ROOT = product.whole.ROOT
PREFIX = 'device_prepare::pull::tests::'


def declarations(original):
    rows = []
    def change(name, before, after, test, assertion, count=1):
        assert original.count(before) == count, (name, 'mutation anchor drift')
        rows.append((name, before, after, PREFIX + test, assertion))
    pcm = 'complete_pcm_and_ceil_endpoint_survive_varied_output_partitions'
    turns = 'each_turn_obeys_one_source_or_kernel_or_bounded_publication'
    budget = 'lookahead_and_event_budget_exhaustion_are_terminal_and_release_buffers'
    graph = 'graph_collection_is_immediate_but_ended_waits_for_device_fences'
    close = 'host_close_revokes_cached_pcm_and_already_due_events'
    bad = 'invalid_source_blocks_and_errors_are_sticky_without_partial_success'
    clock = 'clock_overflow_is_terminal_before_output_is_modified'
    pop = original[original.index('    fn pop_ended('):original.index('    fn step(')]
    step = original[original.index('    fn step('):original.index('    fn advance(')]
    change('pull-mix-overwrites-other-contexts', '*to += *from;', '*to = *from;', pcm,
           'device FIFO changed mature PCM or lost an input suffix')
    change('pull-output-suffix-dropped', 'self.output_head += count;', 'self.output_head = self.ready.output.len();', pcm,
           'device FIFO changed mature PCM or lost an input suffix')
    change('pull-input-suffix-dropped', 'buffers.head += step.consumed;', 'buffers.head = buffers.len;', pcm,
           'device FIFO changed mature PCM or lost an input suffix')
    change('pull-event-fence-rounded-down', '.div_ceil(u128::from(f.source))', '/ u128::from(f.source)', pcm,
           'EOF event lost its original ceil fence')
    change('pull-ended-published-during-lookahead', pop, pop.replace('event.fence <= self.published', 'true'), graph,
           'ended escaped during filter lookahead')
    change('pull-source-quantum-doubled', 'const QUANTUM: usize = 128;', 'const QUANTUM: usize = 256;', turns,
           'source render quantum exceeded one turn')
    change('pull-publication-quantum-removed', 'let quota = (output.len() / channels).min(QUANTUM);',
           'let quota = output.len() / channels;', turns, 'FIFO publication exceeded its quantum')
    change('pull-lookahead-budget-ignored', 'let frames = room.min(QUANTUM as u128) as usize;', 'let frames = QUANTUM;', budget,
           'device pull ignored a working-set budget')
    change('pull-event-budget-ignored', 'if buffers.events.len() == limits.pending_events {', 'if false {', budget,
           'device pull ignored a working-set budget')
    change('pull-render-turn-allocates', step, step.replace('self.check_live()?;', 'std::hint::black_box(vec![0_u8;64]); self.check_live()?;'),
           'first_and_steady_pull_turns_allocate_no_rust_heap', 'device pull turn allocated')
    change('pull-eof-not-forwarded', '.step(offered, self.source_eof && offered.is_empty())?', '.step(offered, false)?', pcm,
           'device pull never reached its exact endpoint')
    change('pull-published-clock-loses-frames', 'self.published = next_published;', 'self.published = next_published.saturating_sub(1);', pcm,
           'device FIFO clock drifted with callback partitioning')
    before = 'frames = frames.min((event.fence - self.published).min(QUANTUM as u64) as usize);'
    change('pull-publication-crosses-ended-fence', before, 'let _ = event;', graph,
           'ended did not stop at its exact device fence')
    change('pull-terminal-input-buffers-retained', 'self.buffers = None;', '', budget,
           'budget failure retained pull buffers')
    change('pull-terminal-error-code-lost', 'self.ready.retire(code);', 'self.ready.retire(ErrorCode::Internal);', bad,
           'device pull lost its terminal source error')
    change('pull-source-metadata-accepted', 'if block.frames > frames || (block.frames == 0 && !block.eof) {', 'if false {', bad,
           'invalid source metadata was accepted')
    change('pull-suspension-ignored', 'if self.suspended || quota == 0 {', 'if quota == 0 {',
           'suspend_and_partial_frame_calls_keep_fifo_phase_and_due_events', 'suspended device advanced a pull turn')
    change('pull-cached-pcm-cancel-check-lost', step, step.replace('        self.check_live()?;\n', ''),
           'cancellation_revokes_fifo_and_events_before_cached_data_can_escape', 'cancelled FIFO published stale PCM')
    change('pull-cached-pcm-host-check-lost', 'else if self.ready.request.cpu.is_closed() {', 'else if false {', close,
           'cached PCM crossed host close')
    change('pull-cached-event-host-check-lost', pop, pop.replace('        self.check_live()?;\n', ''), close,
           'cached event crossed host close')
    change('pull-rational-clock-wraps', '.unwrap_or(u64::MAX);', '.unwrap_or(0);', clock,
           'device clock wrapped during rational scaling')
    before = original[original.index('        let step = self\n'):original.index('        if step.consumed')]
    change('pull-two-src-calls-per-turn', before, before + '        self.ready.step(offered, self.source_eof && offered.is_empty())?;\n', turns,
           'device pull ran multiple kernels in one turn')
    begin = original.index('            let next_published =')
    split = original.index('            let count =', begin)
    end = original.index('            self.published = next_published;', split)
    before = original[begin:end]
    after = original[split:end] + original[begin:split]
    change('pull-overflow-writes-partial-pcm', before, after, clock,
           'overflow published partial device PCM')
    return rows


def main(work, target, preflight, controls_only):
    whole = product.whole
    package, env, frozen = whole.support.clock.prepare(work, target, whole.extra_inputs() + [Path(__file__)])
    path = package / 'src/device_pull.rs'
    original = path.read_text()
    rows = declarations(original)
    programs, controls, profiles = [], [], {}
    for profile in ([] if controls_only else ['release'] if preflight else ['release', 'debug']):
        label = 'positive-' + profile
        binary = product.src.build(package, work, env, label, profile == 'release')
        log = work / (label + '-tests.log')
        command = [str(binary), PREFIX] if preflight else [str(binary)]
        assert whole.support.clock.common.run(command, log, package, env, timeout=900) == 0, log
        count = 12 if preflight else 567
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
                print('actual device pull fault rejected:', name, flush=True)
            finally:
                path.write_text(original)
    whole.verify_inputs(work, frozen)
    root = ROOT / 'engine/crates/audio/src'
    assert all(p.read_bytes() == (package / 'src' / p.relative_to(root)).read_bytes() for p in root.rglob('*') if p.is_file())
    summary = dict(profiles=profiles, controls=controls, compiled_controls=len(controls), programs=programs,
                   root_inputs_unchanged=True, source_copies_restored=True, preflight=preflight, controls_only=controls_only,
                   candidate_only=True, production_device_sinc_migrated=False, max_source_frames_per_turn=128,
                   max_publication_frames_per_turn=128, max_native_kernels_per_turn=1,
                   pcm_fifo_reuses_prepared_output=True, separate_render_and_publication_clocks=True,
                   ended_fences_are_source_block_endpoints=True, device_or_shipping_acceptance=False)
    (work / 'summary.json').write_text(json.dumps(summary, indent=2) + '\n')
    print('Device pull candidate verified', profiles, len(controls), flush=True)


if __name__ == '__main__':
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--output', type=Path, required=True)
    p.add_argument('--target-dir', type=Path, required=True)
    p.add_argument('--preflight', action='store_true')
    p.add_argument('--controls-only', action='store_true')
    a = p.parse_args()
    assert not (a.preflight and a.controls_only)
    main(a.output.resolve(), a.target_dir.resolve(), a.preflight, a.controls_only)
