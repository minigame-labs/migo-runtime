#!/usr/bin/env python3
"""Filter admission must cover preparation, live owners and deferred destruction.

Share the physical PCM ledger, retain credit with the actual CPU owner and use
one pre-reserved handoff per owner. Only fresh exact runtime assertions reject
faults; the same owner is used by production device wiring.
"""
from __future__ import annotations
import argparse
import json
from pathlib import Path
import check_audio_whole_decode as product

ROOT = product.whole.ROOT
PREFIX = 'device_prepare::pull::session::resources::tests::'


def declarations(originals):
    rows = []
    def change(name, file, before, after, test, assertion):
        assert originals[file].count(before) == 1, (name, 'mutation anchor drift')
        rows.append((name, file, before, after, PREFIX + test, assertion))
    def resource(name, before, after, test, assertion):
        change(name, 'device_resources.rs', before, after, test, assertion)
    quota = 'exact_host_process_limits_and_rejection_rollback_hold'
    shared = 'default_process_and_cloned_host_use_the_same_resource_authorities'
    threads = 'normal_drop_and_closed_host_destroy_resources_off_the_calling_thread'
    handoff = 'reserved_retirement_handoff_is_bounded_and_keeps_credit_during_backlog'
    source = 'terminal_source_errors_retain_cached_filters_until_the_reserved_handoff'
    internal = 'internal_src_cancel_race_retains_tables_for_off_render_retirement'
    overflow = 'invalid_formats_closed_hosts_and_counter_overflow_cannot_enter_admission'
    resource('resources-host-cap-bypassed', 'let host = self.usage.reserve()?;', 'let host = Usage::new(usize::MAX).reserve()?;', quota,
             'host accepted an owner beyond its exact quota')
    resource('resources-process-cap-bypassed', 'let process = self.process.reserve()?;', 'let process = Usage::new(usize::MAX).reserve()?;', quota,
             'process accepted an owner beyond its exact quota')
    resource('resources-physical-ledger-bypassed', '.try_reserve(OWNER_BYTES, "device filter")?', '.try_reserve(0, "device filter")?',
             'aggregate_pcm_competition_rejects_before_any_filter_construction', 'filter construction bypassed physical admission')
    resource('resources-credit-rollback-lost', 'self.0.owners.fetch_sub(1, Ordering::AcqRel);', '', quota,
             'failed process admission leaked host credit')
    resource('resources-physical-credit-returned-before-cpu-finish', 'lease.retired.as_mut().unwrap().state = State::Ready(ready);',
             'lease.retired.as_mut().unwrap().state = State::Ready(ready); lease.retired.as_mut().unwrap()._ticket._aggregate.shrink_to(0);',
             'aborted_running_preparation_holds_credit_until_its_actual_owner_dies',
             'aborting a running preparation returned credit before the CPU owner died')
    resource('resources-retirement-credit-returned-at-handoff', 'if let Some(send) = self.send.take() {',
             'if let Some(send) = self.send.take() { self.retired.as_mut().unwrap()._ticket._aggregate.shrink_to(0);', threads,
             'credit was released before actual destruction started')
    text = originals['device_resources.rs']
    before = text[text.index('impl Drop for Lease {'):text.index('struct Device {')]
    resource('resources-heavy-drop-on-render-caller', before,
             'impl Drop for Lease { fn drop(&mut self) { self.send.take(); drop(self.retired.take()); } }\n', threads,
             'heavy owner destruction ran on the caller')
    resource('resources-handoff-allocates', 'if let Some(send) = self.send.take() {',
             'if let Some(send) = self.send.take() { std::hint::black_box(vec![0_u8;64]);', handoff,
             'retirement handoff allocated')
    resource('resources-ready-retirement-not-deferred', 'ready.deferred_retirement = true;', 'ready.deferred_retirement = false;', source,
             'source error destroyed a filter or scratch on the render caller')
    change('resources-pull-scratch-destroyed-on-render', 'device_pull.rs',
           'if self.ready.deferred_retirement {\n            return;\n        }', 'if false { return; }', source,
           'source error destroyed a filter or scratch on the render caller')
    resource('resources-internal-src-not-deferred', 'ready.stream.as_mut().unwrap().deferred_retirement = true;',
             'ready.stream.as_mut().unwrap().deferred_retirement = false;', internal,
             'internal SRC cancellation destroyed live filter tables')
    resource('resources-terminal-error-code-lost', 'let result = self.session()?.step(graph, output);',
             'let result = self.session()?.step(graph, output).map_err(|_| invalid("mutant lost source error"));', source,
             'managed source error lost its typed code')
    resource('resources-owner-counter-wraps', 'used.checked_add(1)', 'Some(used.wrapping_add(1))', overflow,
             'owner count wrapped at usize limit')
    before = text[text.index('        if !(MIN_SAMPLE_RATE'):text.index('        let host =')]
    resource('resources-unbounded-format-admitted', before,
             '        if false { return Err(invalid("invalid managed device SRC format")); }\n', overflow,
             'unsupported managed format entered memory admission')
    resource('resources-peak-reservation-underestimated', 'const OWNER_BYTES: usize = 4 * 1024 * 1024;',
             'const OWNER_BYTES: usize = 1024 * 1024;', 'cumulative_native_construction_allocations_fit_the_reserved_peak',
             'mature native construction exceeds the reserved peak')
    before = '#[derive(Clone)]\nstruct Host {'
    after = 'impl Clone for Host { fn clone(&self) -> Self { Self { cpu:self.cpu.clone(), usage:Usage::new(HOST_OWNERS), process:self.process.clone(), aggregate:self.aggregate.clone(), retire_hook:self.retire_hook.clone() } } }\nstruct Host {'
    resource('resources-host-clone-duplicates-quota', before, after, shared,
             'host clone duplicated its resource quota')
    resource('resources-process-ledger-not-global', 'process: PROCESS.get_or_init(|| Usage::new(PROCESS_OWNERS)).clone(),',
             'process: Usage::new(PROCESS_OWNERS),', shared, 'hosts do not share the process filter quota')
    # Retained model whose old function-prefix anchor changed with the new
    # test-only retain-on-error guard. Exercise the repaired exact mutation.
    import check_audio_product_rubato as prior
    engine = ROOT / 'engine/crates/audio/src'
    models = prior.declarations({name: (engine / name).read_text() for name in ['resample_job.rs', 'bandlimited.rs', 'streaming_sinc.rs']})
    for name, file, before, after, test in models:
        if name == 'stream-src-cancel-retains-scratch':
            rows.append((name, 'streaming_sinc.rs', before, after, test, 'assertion failed: stream.layers.is_empty() && stream.layers.capacity() == 0'))
    assert len(rows) == 18
    return rows


def main(work, target, preflight, controls_only):
    whole = product.whole
    package, env, frozen = whole.support.clock.prepare(work, target, whole.extra_inputs() + [Path(__file__)])
    files = {name: package / 'src' / name for name in ['device_resources.rs', 'device_pull.rs', 'streaming_sinc.rs']}
    originals = {name: path.read_text() for name, path in files.items()}
    rows = declarations(originals)
    programs, controls, profiles = [], [], {}
    for profile in ([] if controls_only else ['release'] if preflight else ['release', 'debug']):
        label = 'positive-' + profile
        binary = product.src.build(package, work, env, label, profile == 'release')
        log = work / (label + '-tests.log')
        command = [str(binary), PREFIX] if preflight else [str(binary)]
        assert whole.support.clock.common.run(command, log, package, env, timeout=900) == 0, log
        count = 14 if preflight else 567
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
                print('actual device-resources fault rejected:', name, flush=True)
            finally:
                path.write_text(original)
    whole.verify_inputs(work, frozen)
    root = ROOT / 'engine/crates/audio/src'
    assert all(p.read_bytes() == (package / 'src' / p.relative_to(root)).read_bytes() for p in root.rglob('*') if p.is_file())
    summary = dict(profiles=profiles, controls=controls, compiled_controls=len(controls), programs=programs,
                   root_inputs_unchanged=True, source_copies_restored=True, preflight=preflight, controls_only=controls_only,
                   resource_lifetime_and_off_render_retirement=True, production_graph_idle_wiring=True,
                   production_device_sinc_migrated=True, device_or_shipping_acceptance=False)
    (work / 'summary.json').write_text(json.dumps(summary, indent=2) + '\n')
    print('Device resource lifetime candidate verified', profiles, len(controls), flush=True)


if __name__ == '__main__':
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--output', type=Path, required=True)
    p.add_argument('--target-dir', type=Path, required=True)
    p.add_argument('--preflight', action='store_true')
    p.add_argument('--controls-only', action='store_true')
    a = p.parse_args()
    assert not (a.preflight and a.controls_only)
    main(a.output.resolve(), a.target_dir.resolve(), a.preflight, a.controls_only)
