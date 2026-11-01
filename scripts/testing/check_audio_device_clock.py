#!/usr/bin/env python3
"""The retired linear device owner must never return through a native bypass.

Native rendering accepts only the context rate, mixes channels additively,
bounds graph quanta and preserves stop/collection/idle state without callback
allocation. Managed sinc remains the sole device rate-conversion path.
"""
from __future__ import annotations
import argparse,json
from pathlib import Path
import check_audio_whole_decode as product
ROOT=product.whole.ROOT
PREFIX='context::device_tests::'


def verify_native_only(originals):
    for file in ['context.rs','resampler.rs']:
        for token in ['StreamResampler','process_for_device','resample_output','resampler_device_rate','resampler_device_channels','input_frames_to_clock']:
            assert token not in originals[file],(file,'obsolete device SRC returned',token)
    assert 'process_native_device' in originals['context.rs']


def declarations(originals):
    verify_native_only(originals)
    rows=[]
    def change(name,file,before,after,test,assertion):
        assert originals[file].count(before)==1,(name,'mutation anchor drift')
        rows.append((name,file,before,after,PREFIX+test,assertion))
    ctx=originals['context.rs'];start=ctx.index('    pub(crate) fn process_native_device(');end=ctx.index('    pub(crate) fn mix_device_channels(',start);body=ctx[start:end]
    def native(name,before,after,test,assertion):
        assert body.count(before)==1,(name,'native mutation anchor drift')
        change(name,'context.rs',body,body.replace(before,after),test,assertion)
    rate='native_rate_mismatch_is_rejected_without_graph_bus_or_epoch_changes'
    layout='native_channel_mapping_preserves_the_additive_mix_and_exact_clock'
    collection='native_collects_all_subquantum_nodes_and_ended_once'
    alloc='native_warmed_channel_layouts_do_not_allocate'
    burst='native_stop_burst_uses_command_reserved_collectors_without_allocation'
    native('native-rate-guard-bypassed','if device_rate != self.sample_rate {','if false {',rate,'native rate mismatch was accepted')
    native('native-clears-shared-mix','            if context_channels == device_channels {','            quantum.fill(0.0); if context_channels == device_channels {',layout,'native channel mapping changed the shared mix')
    native('native-channel-map-skipped','Self::mix_device_channels(&input, quantum, context_channels, device_channels);','for (to,from) in quantum.iter_mut().zip(&input) { *to+=*from; }',layout,'native channel mapping changed the shared mix')
    native('native-suspension-ignored','        self.native_collected.clear();','        self.resume(); self.native_collected.clear();','native_suspend_empty_and_partial_calls_preserve_clock_and_mix','native suspension modified the mix')
    native('native-collection-overwrites-earlier-quanta','self.native_collected.extend_from_slice(&self.collected);','self.native_collected.clone_from(&self.collected);',collection,'earlier native subquantum node ids were lost')
    native('native-collection-repeated','        self.native_collected.clear();\n','',collection,'native collection was repeated')
    native('native-callback-allocates','let mut input = std::mem::take(&mut self.native_input);','std::hint::black_box(vec![0_u8;64]); let mut input = std::mem::take(&mut self.native_input);',alloc,'native callback allocated')
    needle="""        self.native_collected
            .reserve(self.nodes.len().saturating_sub(self.native_collected.len()));"""
    change('native-stop-collector-not-provisioned','context.rs',needle,'',burst,'native stop burst allocated')
    needle="""        self.collected
            .reserve(self.nodes.len().saturating_sub(self.collected.len()));"""
    change('native-graph-collector-not-provisioned','context.rs',needle,'',burst,'native stop burst allocated')
    changed=body.replace(' || !self.has_active_sources()','').replace('            if !self.has_active_sources() {','            if false {');assert changed!=body
    change('native-inactive-runs-node-dsp','context.rs',body,changed,'inactive_native_graph_has_no_node_dsp_or_clock_work','inactive native graph executed node DSP')
    native('native-graph-quantum-unbounded','.chunks_mut(DEVICE_QUANTUM_FRAMES * device_channels)','.chunks_mut(frames * device_channels)','native_clock_and_pcm_are_partition_independent','native source render exceeded one quantum')
    change('native-route-staging-not-cleared','context.rs','        self.native_input.clear();\n','', 'native_route_replacement_clears_only_scratch_and_replaces_capability','native route replacement retained staging')
    # Retain the real production-call and native-route registration faults from
    # the shipping mixer, rather than resurrecting the old linear owner.
    import check_audio_device_mixer as prior
    for row in prior.declarations(originals):
        if row[0] in ['shipping-device-src-bypassed','mixer-native-registers-managed-route']:rows.append(row)
    assert len(rows)==14
    return rows


def main(work, target, preflight, controls_only):
    whole = product.whole
    package, env, frozen = whole.support.clock.prepare(work, target, whole.extra_inputs() + [Path(__file__)])
    files = {path.name: path for path in (package / 'src').glob('*.rs')}
    originals = {name: path.read_text() for name, path in files.items()}
    rows = declarations(originals)
    programs, controls, profiles = [], [], {}
    source_controls=[]
    for token in ['StreamResampler','process_for_device','resample_output','resampler_device_rate','resampler_device_channels','input_frames_to_clock']:
        mutated=dict(originals);mutated['context.rs']+='\n// '+token+'\n'
        try: verify_native_only(mutated)
        except AssertionError: source_controls.append(token)
        else: raise AssertionError(('source-only contract escaped',token))
    assert len(source_controls)==6
    for profile in ([] if controls_only else ['release'] if preflight else ['release', 'debug']):
        label = 'positive-' + profile
        binary = product.src.build(package, work, env, label, profile == 'release')
        log = work / (label + '-tests.log')
        command = [str(binary), PREFIX] if preflight else [str(binary)]
        assert whole.support.clock.common.run(command, log, package, env, timeout=900) == 0, log
        count = 10 if preflight else 567
        assert f'test result: ok. {count} passed; 0 failed;' in log.read_text(), log
        programs.append(dict(name=label, path=str(binary), sha256=whole.support.codecs.sha(binary), fresh_rust=True, pruned=False))
        profiles[profile] = dict(passed=count, ignored=0 if preflight else 2)
    if not preflight:
        for name, path, before, after, test, assertion in rows:
            try:
                files[path].write_text(originals[path].replace(before, after))
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
                print('actual native-device fault rejected:', name, flush=True)
            finally:
                files[path].write_text(originals[path])
    whole.verify_inputs(work, frozen)
    root = ROOT / 'engine/crates/audio/src'
    assert all(path.read_bytes() == (package / 'src' / path.relative_to(root)).read_bytes() for path in root.rglob('*') if path.is_file())
    summary = dict(profiles=profiles, controls=controls, compiled_controls=len(controls), source_only_controls=source_controls, programs=programs,
                   native_only_boundary=True, production_linear_device_removed=True,
                   root_inputs_unchanged=True, source_copies_restored=True, preflight=preflight, controls_only=controls_only,
                   device_sinc_migrated=True, existing_linear_pcm_preserved=False, source_quantum_frames=128,
                   exact_native_frame_clock=True, same_rate_device_layout_mapped=True, device_or_shipping_acceptance=False)
    (work / 'summary.json').write_text(json.dumps(summary, indent=2) + '\n')
    print('Native device boundary verified', profiles, len(controls), flush=True)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--target-dir', type=Path, required=True)
    parser.add_argument('--preflight', action='store_true')
    parser.add_argument('--controls-only', action='store_true')
    args = parser.parse_args()
    assert not (args.preflight and args.controls_only)
    main(args.output.resolve(), args.target_dir.resolve(), args.preflight, args.controls_only)
