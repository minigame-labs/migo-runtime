#!/usr/bin/env python3
"""Admit AudioParam event capacity before mutation; retain it through CPU drop.

The frozen complete timeline is the numeric oracle. Source faults must compile
fresh test executables and reach the selected business assertion.
"""
from __future__ import annotations
import argparse
import json
from pathlib import Path
import re
import check_audio_command_ownership as base
import check_audio_waveshaper_storage as wave
import check_audio_waveshaper_capacity as cold

ROOT = base.ROOT
PREFIX = 'audio_thread::param_storage_tests::'


def declarations(originals):
    rows = []
    def add(name, changes, test, assertion):
        for key, before, after in changes:
            assert before != after and originals[key].count(before) == 1, (name, key, 'anchor drift')
        rows.append(dict(name=name, changes=changes, test=PREFIX + test, expected_assertion=assertion))
    refusal = 'refused_growth_is_atomic_for_every_scheduling_operation'
    adoption = 'all_twenty_two_parameters_adopt_existing_capacity'
    insertion = 'managed_context_and_generic_node_insertion_adopt_timelines'
    growth = 'event_capacity_growth_keeps_prefix_and_old_cpu_credit'
    closed = 'cancellation_gc_and_closed_host_reuse_keep_capacity_charge'
    threshold = 'small_large_boundary_and_closed_teardown_follow_original_cpu'
    foreign = 'foreign_adoption_refusal_and_detached_clone_preserve_owners'
    add('events-new-capacity-uncharged', [('storage', 'Admission::reserve(host, capacity_bytes(capacity)?)?', 'Admission::reserve(host, 0)?')], refusal, 'event growth bypassed physical admission')
    add('events-allocation-before-admission', [('storage', '        let reservation = Admission::reserve(host, capacity_bytes(capacity)?)?;\n        let mut data = Vec::with_capacity(capacity);', '        let mut data = Vec::with_capacity(capacity);\n        let reservation = Admission::reserve(host, capacity_bytes(capacity)?)?;')], refusal, 'event storage allocated before admission')
    add('events-adoption-length-for-capacity', [('storage', 'Admission::reserve(host, capacity_bytes(self.capacity())?)?', 'Admission::reserve(host, capacity_bytes(self.len())?)?')], adoption, 'node omitted AudioParam capacity adoption')
    add('events-adoption-copies-storage', [('storage', 'let data = std::mem::take(&mut **self);', 'let data = self.to_vec();')], adoption, 'AudioParam adoption copied original storage')
    add('events-empty-timeline-loses-host', [('param', '        self.retirement = Some(host.clone());', '        if !self.events.is_empty() { self.retirement = Some(host.clone()); }')], insertion, 'empty managed parameter lost physical host')
    add('events-generic-node-adoption-omitted', [('context', 'let Err(error) = node.prepare_params(host)', 'let Err(error) = Ok::<(), shared::error::EngineError>(())')], insertion, 'generic insertion omitted AudioParam capacity')
    add('events-existing-context-adoption-omitted', [('context', '            node.prepare_params(&host)?;', '            // fault: existing timelines were not adopted')], insertion, 'existing context omitted AudioParam capacity')
    original = wave.method(originals['param'], '    pub(crate) fn prepare_storage(')
    start = original.index('        if self\n')
    end = original.index('        self.events.prepare(host)?;', start)
    add('events-foreign-ledger-accepted', [('param', original[start:end], '')], foreign, 'AudioParam accepted a foreign physical ledger')
    add('events-growth-loses-prefix', [('storage', '        data.extend_from_slice(self);', '        data.resize(self.len(), AutomationEvent::SetValue { value: 0.0, time: 0.0 });')], growth, 'event growth lost scheduled prefix')
    add('events-old-credit-returned-early', [('retirement', '            let lease = self.lease.take().unwrap();', '            let mut lease = self.lease.take().unwrap();\n            lease.permit.shrink_to(0);')], growth, 'event growth returned old physical credit early')
    add('events-bulk-drop-on-caller', [('retirement', 'if self.lease.is_some() {', 'if false {')], threshold, 'bulk event storage freed on command caller')
    add('events-small-credit-omitted', [('storage', 'host.reserve_inline(bytes)', 'host.reserve_inline(0)')], threshold, 'small events acquired bulk handoff overhead')
    add('events-bulk-boundary-inline', [('storage', 'if bytes < 4096 {', 'if bytes < 8192 {')], threshold, 'bulk events lost CPU handoff charge')
    add('events-cancel-shrinks-capacity', [('param', '        self.events.retain(|e| e.time() < cancel_time);', '        self.events.retain(|e| e.time() < cancel_time);\n        self.events.shrink_to_fit();')], closed, 'event GC shrank physical capacity')
    add('events-reuse-requires-new-credit', [('storage', '        if required <= self.capacity() {\n            return Ok(());', '        if required <= self.capacity() {\n            if let Some(host) = host { let _ = host.reserve_inline(1)?; }\n            return Ok(());')], closed, 'event capacity reuse required fresh admission')
    original = wave.method(originals['param'], '    pub(crate) fn try_set_value_now(')
    changed = original.replace('        let clamped =', '        self.current_value = value;\n        let clamped =', 1)
    add('events-value-changed-before-anchor-admission', [('param', original, changed)], refusal, 'event refusal changed automation or static value')
    add('events-context-acknowledges-refusal', [('context', '            return param.try_set_value_now(value, now).is_ok();', '            let _ = param.try_set_value_now(value, now);\n            return true;')], 'context_setter_refusals_preserve_timeline_and_report_false', 'context reported rejected event as accepted')
    add('events-equal-time-order-reversed', [('param', 'self.events.partition_point(|e| e.time() <= time)', 'self.events.partition_point(|e| e.time() < time)')], 'owned_timeline_matches_complete_frozen_scalar_and_block_output', 'owned automation changed complete frozen output')
    for key, field in [('gain', 'gain'), ('delay', 'delay_time'), ('constant_source', 'offset'), ('oscillator', 'detune'), ('buffer_source', 'detune'), ('biquad_filter', 'gain'), ('dynamics_compressor', 'release'), ('panner', 'orientation_z')]:
        marker = wave.method(originals[key], '    fn prepare_params(')
        changed = marker.replace('self.' + field + '.prepare_storage(host)', 'Ok(())') if key in ['gain', 'delay', 'constant_source'] else marker.replace(', &mut self.' + field, '')
        # rustfmt writes longer arrays one field per line.
        if changed == marker:
            changed = marker.replace('            &mut self.' + field + ',\n', '')
        add('events-' + key + '-parameter-omitted', [(key, marker, changed)], adoption, 'node omitted AudioParam capacity adoption')
    for name, key, before, after, assertion in cold.declarations({'nodes/wave_shaper.rs': originals['wave'], 'context.rs': originals['context']}):
        rows.append(dict(name=name, changes=[('wave' if key == 'nodes/wave_shaper.rs' else 'context', before, after)], test=cold.TEST, expected_assertion=assertion))
    assert len(rows) == 30
    return rows


def before_timeline(original):
    """Adapt new private APIs only; original event Vec and setters stay intact."""
    assert 'events: Vec<AutomationEvent>' in original and 'retirement: Option<Host>' not in original
    bridge = '''
impl AudioParamTimeline {
    pub(crate) fn prepare_storage(&mut self, _host: &crate::node_retirement::Host) -> shared::error::EngineResult<()> { Ok(()) }
    #[cfg(test)]
    pub(crate) fn event_identity(&self) -> (usize, usize, usize) { (self.events.len(), self.events.capacity(), self.events.as_ptr() as usize) }
    #[cfg(test)]
    pub(crate) fn reserve_native_events(&mut self, capacity: usize) { self.events.reserve_exact(capacity.saturating_sub(self.events.len())); }
    #[cfg(test)]
    pub(crate) fn observe_events(&mut self, _hook: std::sync::Arc<dyn Fn()+Send+Sync>) {}
'''
    for name, params, args in [
        ('set_value_now', 'value: f32, current_time: f64', 'value, current_time'),
        ('set_value_at_time', 'value: f32, time: f64', 'value, time'),
        ('linear_ramp_to_value_at_time', 'value: f32, end_time: f64', 'value, end_time'),
        ('exponential_ramp_to_value_at_time', 'value: f32, end_time: f64', 'value, end_time'),
        ('set_target_at_time', 'target: f32, start_time: f64, time_constant: f64', 'target, start_time, time_constant'),
    ]:
        bridge += '    pub(crate) fn try_' + name + '(&mut self, ' + params + ') -> shared::error::EngineResult<()> { self.' + name + '(' + args + '); Ok(()) }\n'
    return original + bridge + '}\n'


def main(work, target, before_directory, preflight=False):
    package, env, frozen = base.prepare(work, target)
    product, common = base.capacity.product, base.capacity.product.whole.support.clock.common
    names = {'param': 'param.rs', 'storage': 'param_storage.rs', 'context': 'context.rs', 'retirement': 'node_retirement.rs', 'wave': 'nodes/wave_shaper.rs'}
    names.update({key: 'nodes/' + key + '.rs' for key in ['gain', 'delay', 'constant_source', 'oscillator', 'buffer_source', 'biquad_filter', 'dynamics_compressor', 'panner']})
    paths = {key: package / 'src' / name for key, name in names.items()}
    originals = {key: path.read_text() for key, path in paths.items()}
    rows = declarations(originals)
    programs, controls, red, suites = [], [], [], {}
    def build(label, release=False):
        binary = product.src.build(package, work, env, label, release)
        programs.append(dict(name=label, path=str(binary), sha256=product.whole.support.codecs.sha(binary), pruned=False))
        return binary
    def run(binary, label, selector='', expected=0, assertion=None):
        status = common.run([str(binary)] + (['--exact', selector] if selector else []) + ['--nocapture'], work / (label + '.log'), package, env, timeout=300)
        (work / (label + '-exit.txt')).write_text(str(status) + '\n')
        text = (work / (label + '.log')).read_text()
        assert status == expected, (label, status)
        if expected:
            assert all(marker in text for marker in ['running 1 test', 'panicked at', '1 failed', assertion]), label
        else:
            match = re.search(r'test result: ok\. ([1-9][0-9]*) passed; 0 failed; (\d+) ignored;', text)
            assert match, label
            suites[label] = dict(passed=int(match[1]), ignored=int(match[2]))
    positive = build('positive-debug')
    base.capacity.profile.verify(work / 'positive-debug-build.jsonl', json.loads((work / 'debug-profile-policy.json').read_text()), work / 'debug-profile-artifacts.json')
    run(positive, 'full-debug')
    assert common.run(['cargo', 'clippy', '--lib', '--tests', '--offline', '--locked', '--message-format=json'], work / 'clippy.jsonl', package, env, timeout=300) == 0
    old_manifest = json.loads((before_directory / 'root-sha256.json').read_text())
    changed = [n for n, h in old_manifest.items() if product.whole.support.codecs.sha(ROOT / n) != h and n.startswith('engine/crates/audio/src/')]
    saved = {n: (package / 'src' / n.removeprefix('engine/crates/audio/src/')).read_bytes() for n in changed}
    try:
        for n in changed: (package / 'src' / n.removeprefix('engine/crates/audio/src/')).write_bytes((before_directory / n).read_bytes())
        assert common.run(['cargo', 'clippy', '--lib', '--tests', '--offline', '--locked', '--message-format=json'], work / 'clippy-before.jsonl', package, env, timeout=300) == 0
    finally:
        for n, data in saved.items(): (package / 'src' / n.removeprefix('engine/crates/audio/src/')).write_bytes(data)
    old, current = wave.diagnostics(work / 'clippy-before.jsonl', package), wave.diagnostics(work / 'clippy.jsonl', package)
    added = current - old
    delta = dict(before=sum(old.values()), after=sum(current.values()), new_diagnostics=sum(added.values()), exact_multiset=old == current, actor_included=True, new=[dict(key=key, count=count) for key, count in added.items()])
    (work / 'clippy-delta.json').write_text(json.dumps(delta, indent=2) + '\n')
    assert not added, delta
    if not preflight:
        try:
            paths['param'].write_text(before_timeline((before_directory / 'engine/crates/audio/src/param.rs').read_text()))
            binary = build('before-timeline')
            for label, test, assertion in [('before-event-growth', 'refused_growth_is_atomic_for_every_scheduling_operation', 'event growth bypassed physical admission'), ('before-event-adoption', 'all_twenty_two_parameters_adopt_existing_capacity', 'node omitted AudioParam capacity adoption')]:
                run(binary, label, PREFIX + test, 101, assertion)
                red.append(dict(name=label, test=PREFIX + test, exit=101, expected_assertion=assertion, sha256=programs[-1]['sha256']))
            programs[-1]['pruned'] = True
            binary.unlink()
        finally: paths['param'].write_text(originals['param'])
        for row in rows:
            keys = set()
            try:
                for key, before, after in row['changes']:
                    assert key not in keys
                    keys.add(key)
                    paths[key].write_text(originals[key].replace(before, after))
                binary = build(row['name'])
                run(binary, row['name'], row['test'], 101, row['expected_assertion'])
                controls.append({key: value for key, value in row.items() if key != 'changes'} | dict(exit=101, sha256=programs[-1]['sha256']))
                programs[-1]['pruned'] = True
                binary.unlink()
                (work / 'completed-controls.json').write_text(json.dumps(controls, indent=2) + '\n')
                print('Actual AudioParam ownership fault rejected', row['name'], flush=True)
            finally:
                for key in keys: paths[key].write_text(originals[key])
        release = build('positive-release', True)
        run(release, 'full-release')
    product.whole.verify_inputs(work, frozen)
    assert all(path.read_text() == originals[key] for key, path in paths.items())
    root = ROOT / 'engine/crates/audio/src'
    assert all((package / 'src' / p.relative_to(root)).read_bytes() == p.read_bytes() for p in root.rglob('*') if p.is_file())
    summary = dict(programs=programs, controls=controls, original_red=red, suites=suites, root_inputs_unchanged=True, source_copies_restored=True, root_dev_profile_verified=True, isolated_release_is_not_root_shipping=True, preflight=preflight)
    (work / 'summary.json').write_text(json.dumps(summary, indent=2) + '\n')
    print('AudioParam event capacity verified', len(controls), 'fresh faults', flush=True)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--target-dir', type=Path, required=True)
    parser.add_argument('--before-directory', type=Path, required=True)
    parser.add_argument('--preflight', action='store_true')
    args = parser.parse_args()
    main(args.output.resolve(), args.target_dir.resolve(), args.before_directory.resolve(), args.preflight)
