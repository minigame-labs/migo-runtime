#!/usr/bin/env python3
"""Inactive effect state may be nonzero below the graph's existing tail threshold.

The continuous device port must produce exact zeros there, retain real audible
tails, collect nodes immediately and publish ended on the local source fence.
Exercise real context/clock/control paths; only fresh exact failures count.
"""
from __future__ import annotations
import argparse
import json
from pathlib import Path
import check_audio_whole_decode as product

ROOT = product.whole.ROOT
PREFIX = 'device_prepare::pull::session::resources::owner::graph::tests::'


def declarations(originals):
    rows=[]
    def change(name,file,before,after,test,assertion):
        assert originals[file].count(before)==1,(name,'mutation anchor drift')
        rows.append((name,file,before,after,PREFIX+test,assertion))
    def graph(name,before,after,test,assertion): change(name,'device_graph.rs',before,after,test,assertion)
    ctx=originals['context.rs']; adapter=originals['device_graph.rs']; pull=originals['device_pull.rs']
    inactive='inactive_source_port_is_exact_zero_and_does_not_process_nodes'
    pcm='real_delay_and_biquad_tails_match_complete_mature_reference_pcm'
    immediate='immediate_stop_is_collected_and_ended_at_zero_fence_without_rendering'
    natural='natural_end_collection_is_immediate_but_notifications_wait_for_local_fences'
    late='a_new_control_fence_wakes_a_quiescent_stream_without_rebuilding_it'
    paused='suspend_partial_and_empty_calls_freeze_graph_controls_and_publication'
    change('graph-inactive-runs-node-dsp','context.rs','if self.has_active_sources() {\n            return self.process(output);\n        }','if true { return self.process(output); }',inactive,'inactive graph port leaked nonzero state')
    change('graph-inactive-scratch-not-zeroed','context.rs','        let output = &mut output[..frames * channels];\n        output.fill(0.0);','        let output = &mut output[..frames * channels];',inactive,'inactive graph port leaked nonzero state')

    graph('graph-control-sweep-skipped','        self.service_context_controls(context, &mut collected)?;\n','',immediate,'idle control event was hidden behind quiescence')
    changed=ctx.replace('device_controls_pending: true,','device_controls_pending: false,').replace('self.device_controls_pending = true;','self.device_controls_pending = false;');assert changed!=ctx
    change('graph-control-commands-never-marked','context.rs',ctx,changed,immediate,'idle control event was hidden behind quiescence')
    change('graph-watched-end-capacity-not-provisioned','context.rs','            self.ended.reserve(self.ended_watch.len());\n','','warmed_graph_first_and_steady_turns_including_watched_end_allocate_nothing','graph turn or watched end allocated')
    graph('graph-public-clock-replaced-by-local-clock','public_seconds: context.current_time(),','public_seconds: clocks.published_source as f64 / context.sample_rate() as f64,','graph_wall_stream_and_mix_clocks_remain_separate_across_idle_reactivation','public idle clock was replaced by the local SRC clock')
    start=pull.index('    fn control_ended(');end=pull.index('    fn advance(',start);block=pull[start:end]
    assert block.count('self.rendered,')==1
    change('graph-control-ended-adds-absolute-clock-offset','device_pull.rs',block,block.replace('self.rendered,','self.rendered.saturating_add(48000),'),immediate,'idle control event was hidden behind quiescence')
    change('graph-fence-rounded-before-end','device_pull.rs','.div_ceil(u128::from(f.source))','/ u128::from(f.source)',late,'quiescent stream did not flush pending control fence')
    graph('graph-control-overrides-event-budget','        let result = session.pull.control_ended(context.drain_ended());','        session.pull.limits.pending_events = MAX_EVENTS; let result = session.pull.control_ended(context.drain_ended());','control_event_overflow_is_terminal_without_partial_pcm_or_duplicate_collection','control events bypassed the pending-event budget')
    change('graph-effect-tail-excluded-from-activity','context.rs','self.nodes.values().any(|n| n.is_producing())','self.nodes.values().any(|n| n.is_producing() && !n.has_tail_audio())',pcm,'real effect graph produced no audible tail')
    graph('graph-continuous-port-submits-eof','eof: false','eof: true',pcm,'real graph step failed')
    start=adapter.index('        for &id in self.context.process_device_source(output)');end=adapter.index('        Ok(Block',start);block=adapter[start:end]
    graph('graph-collection-delayed-or-dropped',block,'        self.context.process_device_source(output);\n',natural,'graph collection waited behind SRC publication')
    graph('graph-natural-ended-not-drained','        self.context.drain_ended()','        self.context.drain_ended().take(0)',natural,'ended used absolute graph time or crossed its local fence')
    start=adapter.index('        if !bound');end=adapter.index('        let session = self.session()?;',start);block=adapter[start:end]
    graph('graph-foreign-context-binding-not-checked',block,'','foreign_context_and_revoked_epoch_are_rejected_before_graph_or_cached_output','graph adapter rendered a foreign context')
    suspend='''        if context.state() == AudioContextState::Suspended {
            session.suspend();
        } else {
            session.resume();
        }'''
    state='context.state() != AudioContextState::Running\n            || '
    assert adapter.count(suspend)==1 and adapter.count(state)==1
    changed=adapter.replace(suspend,'        session.resume();').replace(state,'')
    graph('graph-suspension-ignored',adapter,changed,paused,'suspended graph adapter advanced a turn')
    graph('graph-empty-output-services-controls','|| output.len() < context.channels().max(1) as usize','',paused,'empty/partial callback serviced graph controls')
    graph('graph-quiescent-pending-fence-not-woken','        if !events.is_empty() {\n            session.sleeping = false;\n        }\n','',late,'quiescent stream did not flush pending control fence')
    import check_audio_device_pull as prior
    for name,before,after,test,assertion in prior.declarations(pull):
        if name=='pull-event-budget-ignored': rows.append((name,'device_pull.rs',before,after,test,assertion))
    assert len(rows)==18
    return rows


def main(work, target, preflight, controls_only):
    whole = product.whole
    package, env, frozen = whole.support.clock.prepare(work, target, whole.extra_inputs() + [Path(__file__)])
    files = {name: package / 'src' / name for name in ['context.rs', 'device_graph.rs', 'device_pull.rs']}
    originals = {name: path.read_text() for name, path in files.items()}
    rows = declarations(originals)
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
                print('actual device-graph fault rejected:', name, flush=True)
            finally:
                path.write_text(original)
    whole.verify_inputs(work, frozen)
    root = ROOT / 'engine/crates/audio/src'
    assert all(p.read_bytes() == (package / 'src' / p.relative_to(root)).read_bytes() for p in root.rglob('*') if p.is_file())
    summary = dict(profiles=profiles, controls=controls, compiled_controls=len(controls), programs=programs,
                   root_inputs_unchanged=True, source_copies_restored=True, preflight=preflight, controls_only=controls_only,
                   real_context_graph_port_candidate=False, production_native_source_port=True, production_graph_idle_wiring=True,
                   production_device_sinc_migrated=True, device_or_shipping_acceptance=False)
    (work / 'summary.json').write_text(json.dumps(summary, indent=2) + '\n')
    print('Real context graph port verified', profiles, len(controls), flush=True)


if __name__ == '__main__':
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--output', type=Path, required=True)
    p.add_argument('--target-dir', type=Path, required=True)
    p.add_argument('--preflight', action='store_true')
    p.add_argument('--controls-only', action='store_true')
    a = p.parse_args()
    assert not (a.preflight and a.controls_only)
    main(a.output.resolve(), a.target_dir.resolve(), a.preflight, a.controls_only)
