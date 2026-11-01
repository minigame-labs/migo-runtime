#!/usr/bin/env python3
"""Device sinc preparation must never fall back to rendering the old linear route.

Exercise the shipping AudioThread and bounded managed mixer: pending/failed
admission freezes graph PCM, native routes bypass filters, and published PCM,
events and retirement keep their real owner. Fresh exact runtime faults only.
"""
from __future__ import annotations
import argparse,json
from pathlib import Path
import check_audio_whole_decode as product
ROOT=product.whole.ROOT
PREFIX='device_prepare::pull::session::resources::owner::mixer::tests::'


def declarations(originals):
    rows=[]
    def change(name,file,before,after,test,assertion):
        assert originals[file].count(before)==1,(name,'mutation anchor drift')
        rows.append((name,file,before,after,test if test.startswith('audio_thread::')else PREFIX+test,assertion))
    def mix(name,before,after,test,assertion):change(name,'device_mixer.rs',before,after,test,assertion)
    m=originals['device_mixer.rs'];audio=originals['audio_thread.rs']
    native='native_rate_bypasses_filter_admission_and_preserves_additive_channel_mapping'
    pcm='real_managed_mix_matches_complete_mature_pcm_with_channel_mapping_and_partial_callbacks'
    pending='pending_preparation_does_not_render_or_mutate_the_shared_bus'
    admission='source_collection_and_ended_survive_preparation_admission_failure_without_audio'
    stopped='cold_unused_contexts_and_stopped_pending_sources_do_not_hold_filter_credit'
    close='close_id_reuse_and_same_format_route_reset_revoke_cached_pcm_and_release_slots'
    start=audio.index('            device_mixer.mix(',audio.index('fn mix_contexts('));end=audio.index('            );',start)+len('            );');block=audio[start:end]
    change('shipping-device-src-bypassed','audio_thread.rs',block,'ctx.process(quantum); for id in ctx.drain_ended() { ended(id); }','audio_thread::tests::production_context_mix_uses_mature_sinc_and_unregisters_real_node_ids','shipping context mixer bypassed mature device SRC')
    start=m.index('        if context.sample_rate() == target {',m.index('    fn mix_bounded'));end=m.index('        let Some(route)',start);block=m[start:end]
    mix('mixer-native-clears-shared-bus',block,block.replace('            match context.process_native_device(','            output.fill(0.0); match context.process_native_device('),native,'native bypass corrupted the shared mix or channel mapping')
    mix('mixer-native-registers-managed-route','            if context.sample_rate() == target {\n                Self::control_only','            if false {\n                Self::control_only',native,'native bypass allocated a managed route')
    needle="""        let RouteState::Active(device) = &mut route.state else {
            return;
        };"""
    mix('mixer-pending-renders-graph',needle,needle.replace('            return;','            context.process(output); return;'),pending,'pending preparation mutated the shared mix')
    needle="""                        AudioContext::mix_device_channels(
                            &scratch[..n * source_channels],
                            &mut output[written * target_channels..(written + n) * target_channels],
                            source_channels,
                            target_channels,
                        );"""
    mix('mixer-publication-overwrites-shared-bus',needle,'output[written * target_channels..(written+n)*target_channels].fill(0.0); '+needle,'native_and_managed_contexts_add_into_one_bus_without_cross_route_authority','native and sinc contributions did not share one additive mix')
    mix('mixer-publication-skips-channel-map',needle,'for (to,from) in output[written*target_channels..(written+n)*target_channels].iter_mut().zip(&scratch[..n*source_channels]) { *to+=*from; }',pcm,'managed device mix differed from complete mature PCM')
    mix('mixer-publication-offset-reused','&mut output[written * target_channels..(written + n) * target_channels]','&mut output[..n * target_channels]',pcm,'managed device mix differed from complete mature PCM')
    mix('mixer-hides-buffered-sinc-tail','                || !session.sleeping\n','',pcm,'mixer hid buffered sinc tail after graph became inactive')
    start=m.index('        if context.has_device_controls()');end=m.index('\n    }\n    /// Command',start);block=m[start:end]
    mix('mixer-control-only-collection-skipped',block,'',admission,'background stop collection or event deferral failed')
    mix('mixer-admission-retry-is-idempotent','                    slot.request(wakeup.clone(), None);\n','', 'failed_admission_retries_a_new_operation_on_the_same_host_authority','admission retry reused an exhausted operation')
    mix('mixer-admission-retry-has-no-wakeup','Some(at.saturating_duration_since(now))','None',admission,'admission failure had no retry wake deadline')
    start=audio.index('pub(crate) fn device_retry_wait(');end=audio.index('\nfn wait_for_audio_work(',start);block=audio[start:end]
    change('mixer-admission-deadline-lost-in-sleep','audio_thread.rs',block,'pub(crate) fn device_retry_wait(mode: AudioWaitMode, _retry: Option<Duration>) -> AudioWaitMode { mode }\n','preparation_retry_shortens_idle_sleep_without_overriding_commands_or_earlier_deadlines','idle sleep lost the admission retry deadline')
    needle="""            if context.state() == AudioContextState::Running
                && !context.has_active_sources()
                && self
                    .routes
                    .get(&id)
                    .is_some_and(|r| !matches!(r.state, RouteState::Active(_)))"""
    mix('mixer-stopped-pending-retained',needle,'            if false',stopped,'stopped pending source retained its preparation')
    start=m.index('        self.routes.retain(');end=m.index('        for (&id, context)',start);block=m[start:end]
    mix('mixer-closed-slots-not-pruned',block,'        self.routes.retain(|_,_|true); self.controller.slots.retain(|_,_|true);\n',close,'closed context retained a route slot')
    mix('mixer-foreign-target-activity-accepted','            if route.format.target != target\n                || !route.epoch.same(&context.device_epoch())','            if !route.epoch.same(&context.device_epoch())',close,'foreign target route was reported as audible')
    start=m.index('    fn mix_bounded(');block=m[start:];changed=block.replace('                    ended(id);','                    let _ = id;');assert changed!=block
    mix('mixer-publication-ended-dropped',block,changed,pcm,'production mix lost or repeated a fenced ended event')
    mix('mixer-immediate-collection-dropped','device.step_context(context, scratch, &mut collected)?','device.step_context(context, scratch, |_| {})?',pcm,'production mix did not collect the graph node immediately')
    mix('mixer-turn-budget-ignored','for _ in 0..budget {','for _ in 0..MAX_MIX_TURNS {','bounded_mix_exhaustion_retires_the_owner_and_preserves_the_existing_bus','zero turn budget did not retire its route')
    mix('mixer-background-ended-published','if !paused && context.state() == AudioContextState::Running {','if context.state() == AudioContextState::Running {',admission,'background stop collection or event deferral failed')
    import check_audio_device_owner as owner,check_audio_device_resources as resources
    for row in owner.declarations(originals):
        if row[0]=='owner-controller-duplicates-host-authority':rows.append(row)
    for row in resources.declarations(originals):
        if row[0]=='resources-internal-src-not-deferred':rows.append(row)
    assert len(rows)==21
    return rows


def main(work, target, preflight, controls_only):
    whole = product.whole
    package, env, frozen = whole.support.clock.prepare(work, target, whole.extra_inputs() + [Path(__file__)])
    files = {name: package / 'src' / name for name in ['context.rs','audio_thread.rs','device_owner_slot.rs','device_resources.rs','device_pull.rs','device_mixer.rs','streaming_sinc.rs','device_prepare.rs']}
    originals = {name: path.read_text() for name, path in files.items()}
    rows = declarations(originals)
    programs, controls, profiles = [], [], {}
    for profile in ([] if controls_only else ['release'] if preflight else ['release', 'debug']):
        label = 'positive-' + profile
        binary = product.src.build(package, work, env, label, profile == 'release')
        log = work / (label + '-tests.log')
        command = [str(binary), PREFIX] if preflight else [str(binary)]
        assert whole.support.clock.common.run(command, log, package, env, timeout=900) == 0, log
        count = 13 if preflight else 567
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
                print('actual device-mixer fault rejected:', name, flush=True)
            finally:
                path.write_text(original)
    whole.verify_inputs(work, frozen)
    root = ROOT / 'engine/crates/audio/src'
    assert all(p.read_bytes() == (package / 'src' / p.relative_to(root)).read_bytes() for p in root.rglob('*') if p.is_file())
    summary = dict(profiles=profiles, controls=controls, compiled_controls=len(controls), programs=programs,
                   root_inputs_unchanged=True, source_copies_restored=True, preflight=preflight, controls_only=controls_only,
                   production_managed_controller_wired=True, production_device_graph_wired=True, bounded_mix_driver=True,
                   production_device_sinc_migrated=True, device_or_shipping_acceptance=False)
    (work / 'summary.json').write_text(json.dumps(summary, indent=2) + '\n')
    print('Production managed device mixer verified', profiles, len(controls), flush=True)


if __name__ == '__main__':
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--output', type=Path, required=True)
    p.add_argument('--target-dir', type=Path, required=True)
    p.add_argument('--preflight', action='store_true')
    p.add_argument('--controls-only', action='store_true')
    a = p.parse_args()
    assert not (a.preflight and a.controls_only)
    main(a.output.resolve(), a.target_dir.resolve(), a.preflight, a.controls_only)
