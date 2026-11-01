#!/usr/bin/env python3
"""A replacement backend can repeat its format, and context IDs can be reused.

Exercise the production context/route epoch and managed owner controller against
same-format/ABA/close/restart/foreign completions. Only fresh exact runtime
assertions reject faults; production device sinc wiring remains separate.
"""
from __future__ import annotations
import argparse
import json
from pathlib import Path
import check_audio_whole_decode as product

ROOT = product.whole.ROOT
PREFIX = 'device_prepare::pull::session::resources::owner::tests::'


def declarations(originals):
    rows = []
    owner_prefix = 'device_prepare::pull::session::resources::owner::tests::'
    context_prefix = 'context::route_tests::'
    def change(name, file, before, after, test, assertion):
        assert originals[file].count(before) == 1, (name, 'mutation anchor drift')
        rows.append((name, file, before, after, test, assertion))
    def owner(name, before, after, test, assertion):
        change(name, 'device_owner_slot.rs', before, after, owner_prefix + test, assertion)
    ctx = originals['context.rs']; route = originals['audio_thread.rs']; own = originals['device_owner_slot.rs']; resource = originals['device_resources.rs']
    block = '    for context in contexts.values_mut() {\n        context.renegotiate_device(replacement.0, replacement.1);\n    }'
    change('route-same-format-backend-keeps-old-epoch', 'audio_thread.rs', block,
           '    if current != replacement { ' + block + ' }', context_prefix + 'every_backend_replacement_revokes_the_previous_route_even_when_format_repeats',
           'same-format backend replacement kept the old route live')
    change('route-epoch-revocation-lost', 'context.rs', '.store(false, std::sync::atomic::Ordering::Release);',
           '.store(true, std::sync::atomic::Ordering::Release);', context_prefix + 'suspend_preserves_identity_but_close_drop_and_id_reuse_revoke_it',
           'context close retained route authority')
    start = ctx.index('impl Drop for AudioContext {'); end = ctx.index('#[cfg(test)]', start); block = ctx[start:end]
    change('route-context-drop-keeps-authority', 'context.rs', block,
           'impl Drop for AudioContext { fn drop(&mut self) {} }\n\n', context_prefix + 'suspend_preserves_identity_but_close_drop_and_id_reuse_revoke_it',
           'context drop retained route authority')
    route_reset = '    for context in contexts.values_mut() {\n        context.renegotiate_device(replacement.0, replacement.1);\n    }'
    change('route-channel-change-keeps-epoch', 'audio_thread.rs', route_reset,
           '    if current.0 != replacement.0 { ' + route_reset + ' }', context_prefix + 'backend_rate_and_channel_changes_have_new_epochs_without_aba_revival', 'channel change retained its route epoch')
    change('route-epoch-compares-numeric-equivalence', 'context.rs', 'Arc::ptr_eq(&self.0, &other.0)', 'true',
           context_prefix + 'suspend_preserves_identity_but_close_drop_and_id_reuse_revoke_it', 'numeric context reuse revived old authority')
    owner('owner-controller-duplicates-host-authority', 'Slot::new(self.host.clone(), context.id(), epoch, format)',
          'Slot::new(Host::new(self.host.cpu.clone()), context.id(), epoch, format)',
          'controller_uses_one_host_authority_and_ensure_does_not_restart_preparation', 'controller duplicated host authority per route')
    owner('owner-idempotent-ensure-restarts-request', '.is_some_and(|slot| slot.epoch.same(&epoch) && slot.format == format)',
          '.is_some_and(|_| false)', 'controller_uses_one_host_authority_and_ensure_does_not_restart_preparation',
          'idempotent ensure replaced its request')
    # Request identity and its live level are independent checks; remove both
    # to model accepting an old completion of an otherwise identical route.
    changed = own.replace('self.epoch.is_live() && self.operation.live.is_live()', 'self.epoch.is_live()').replace('            && Arc::ptr_eq(&self.operation, &binding.operation)\n', '')
    owner('owner-obsolete-request-accepted', own, changed,
          'same_format_replacement_and_aba_cannot_accept_old_request_results', 'same-format new request accepted a stale completion')
    meta = 'acceptance_rejects_foreign_authorities_missing_binding_and_wrong_metadata'
    owner('owner-foreign-host-accepted', '            && Arc::ptr_eq(&self.host.usage, &device.authority)\n', '', meta, 'foreign host owner was accepted')
    owner('owner-foreign-context-accepted', '            && ready.request.context == self.context\n', '', meta, 'wrong context owner was accepted')
    owner('owner-wrong-format-accepted', '            && ready.request.format == self.format\n', '', meta, 'wrong format owner was accepted')
    owner('owner-cancelled-ready-accepted', '            && !ready.request.cancel.load(Ordering::Acquire)\n', '', meta, 'cancelled prepared owner was accepted')
    cached = 'revoked_binding_blocks_cached_pcm_and_ended_without_heavy_render_destruction'
    owner('owner-bound-epoch-live-check-lost', 'self.epoch.is_live() && self.operation.live.is_live()', 'self.operation.live.is_live()', cached,
          'revoked route published cached PCM or ended')
    start = resource.index('        if self.error.is_none()', resource.index('    fn session(')); end = resource.index('        if let Some(code)', start); block = resource[start:end]
    change('owner-cached-delivery-skips-binding-guard', 'device_resources.rs', block, '', owner_prefix + cached,
           'revoked route published cached PCM or ended')
    change('owner-revocation-overwrites-terminal-error', 'device_resources.rs', 'if self.error.is_none()\n            && self', 'if self',
           owner_prefix + 'active_bound_device_retains_reference_pcm_and_its_first_terminal_code',
           'later route revocation replaced a terminal source error')
    owner('owner-stale-take-retains-cached-session', '            if !slot.epoch.is_live() {\n                slot.invalidate();\n            }', '',
          'cached_take_revalidates_epoch_and_suspension_keeps_the_same_request', 'stale take retained a cached owner')
    owner('owner-restart-keeps-escaped-owner-live', '        self.slots.clear();', '', 'restart_revokes_escaped_devices_and_keeps_other_hosts_live',
          'restart left an escaped owner live')
    owner('owner-old-context-cleanup-removes-new-slot', '.is_some_and(|slot| slot.epoch.same(&context.device_epoch()))', '.is_some_and(|_| true)',
          'numeric_context_reuse_and_late_remove_do_not_revoke_the_new_owner', 'late old-context cleanup removed the new owner')
    change('native-rate-guard-bypassed','context.rs','if device_rate != self.sample_rate {','if false {',
           'context::device_tests::native_rate_mismatch_is_rejected_without_graph_bus_or_epoch_changes',
           'native rate mismatch was accepted')
    assert len(rows) == 19
    return rows


def main(work, target, preflight, controls_only):
    whole = product.whole
    package, env, frozen = whole.support.clock.prepare(work, target, whole.extra_inputs() + [Path(__file__)])
    files = {name: package / 'src' / name for name in ['context.rs', 'audio_thread.rs', 'device_resources.rs', 'device_owner_slot.rs']}
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
                print('actual device-owner fault rejected:', name, flush=True)
            finally:
                path.write_text(original)
    whole.verify_inputs(work, frozen)
    root = ROOT / 'engine/crates/audio/src'
    assert all(p.read_bytes() == (package / 'src' / p.relative_to(root)).read_bytes() for p in root.rglob('*') if p.is_file())
    summary = dict(profiles=profiles, controls=controls, compiled_controls=len(controls), programs=programs,
                   root_inputs_unchanged=True, source_copies_restored=True, preflight=preflight, controls_only=controls_only,
                   managed_route_identity_candidate=True, production_route_epochs=True, production_graph_idle_wiring=False,
                   production_device_sinc_migrated=False, device_or_shipping_acceptance=False)
    (work / 'summary.json').write_text(json.dumps(summary, indent=2) + '\n')
    print('Managed device route identity verified', profiles, len(controls), flush=True)


if __name__ == '__main__':
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--output', type=Path, required=True)
    p.add_argument('--target-dir', type=Path, required=True)
    p.add_argument('--preflight', action='store_true')
    p.add_argument('--controls-only', action='store_true')
    a = p.parse_args()
    assert not (a.preflight and a.controls_only)
    main(a.output.resolve(), a.target_dir.resolve(), a.preflight, a.controls_only)
