#!/usr/bin/env python3
"""Own live WaveShaper capacities through their actual physical destruction.

Keep the existing native allocation and replacement-input matrices. Every fault
must compile a fresh executable and reach one named business assertion.
"""
from __future__ import annotations
import argparse
from collections import Counter
import json
from pathlib import Path
import re
import check_audio_command_ownership as base
import check_audio_replacement_input as transport
import check_audio_waveshaper_capacity as capacity

ROOT = base.ROOT
PREFIX = 'audio_thread::waveshaper_storage_tests::'


def method(text, marker):
    start = text.index(marker)
    left = text.index('{', start)
    depth, end = 1, left + 1
    while depth:
        depth += (text[end] == '{') - (text[end] == '}')
        end += 1
    return text[start:end]


def declarations(originals):
    rows = []
    def add(name, changes, test, assertion):
        for key, before, after in changes:
            assert before != after and originals[key].count(before) == 1, (name, key, 'anchor drift')
        rows.append(dict(name=name, changes=changes, test=test, expected_assertion=assertion))
    for name, key, before, after, assertion in capacity.declarations({
        'nodes/wave_shaper.rs': originals['node'], 'context.rs': originals['context']
    }):
        add(name, [('node' if key == 'nodes/wave_shaper.rs' else 'context', before, after)], capacity.TEST, assertion)
    for name, key, before, after, assertion in transport.declarations(originals):
        changes = [(key, before, after)]
        if name == 'replacement-small-offloaded':
            changes.append(('channel', 'if self.permit.is_none() || bytes < 4096 {', 'if self.permit.is_none() || bytes == 0 {'))
        add(name, changes, transport.TEST, assertion)

    first = PREFIX + 'history_admission_precedes_allocation_and_mode_change'
    adoption = PREFIX + 'adoption_counts_reserved_capacity_without_copying'
    growth = PREFIX + 'history_growth_refusal_preserves_original_pcm_and_owner'
    curve = PREFIX + 'curve_admission_preserves_input_and_clear_needs_no_new_credit'
    threshold = PREFIX + 'curve_capacity_threshold_keeps_small_values_inline'
    refused = PREFIX + 'refused_nodes_and_closed_hosts_keep_original_cpu_owners'
    add('history-capacity-uncharged', [('storage', 'host.reserve_value_with_bytes::<FirHistory>(bytes)', 'host.reserve_value_with_bytes::<FirHistory>(0)')], first, 'FIR history capacity omitted from ledger')
    start = originals['storage'].index('        let growth = if grows {')
    end = originals['storage'].index('\n        Ok(())', start)
    original = originals['storage'][start:end]
    changed = '        let allocated_first = if grows { Some(self.grown(up, down)) } else { None };\n' + original.replace('let data = self.grown(up, down);', 'let data = allocated_first.unwrap();')
    add('history-allocated-before-admission', [('storage', original, changed)], first, 'FIR history allocated before admission')
    add('history-length-instead-of-capacity', [('storage', '(self.up_state.capacity() + self.down_state.capacity())', '(self.up_state.len() + self.down_state.len())')], adoption, 'waveshaper adoption omitted actual capacity')
    original = method(originals['node'], '    pub(crate) fn try_set_oversample(')
    changed = original.replace('        if let Some(host)', '        self.set_mode(oversample);\n        if let Some(host)', 1)
    add('mode-changed-before-admission', [('node', original, changed)], first, 'FIR history refusal changed mode or storage')
    add('history-growth-loses-prefix', [('storage', 'up_state.extend_from_slice(&self.up_state);', 'up_state.resize(self.up_state.len(), FirState::default());')], growth, 'owned waveshaper changed complete PCM or history')
    add('generic-node-storage-adoption-omitted', [('context', 'shaper.prepare_storage(host, self.channels)', 'Ok::<(), shared::error::EngineError>(())')], adoption, 'waveshaper adoption omitted actual capacity')
    add('existing-context-storage-adoption-omitted', [('context', 'shaper.prepare_storage(&host, self.channels)?;', 'let _ = shaper;')], PREFIX + 'existing_context_adopts_preexisting_waveshaper_storage', 'existing context omitted waveshaper capacity')
    original = method(originals['node'], '    pub(crate) fn prepare_storage(')
    start = original.index('        if self\n')
    end = original.index('        // Hold prospective', start)
    node_guard = original[start:end]
    storage_guard = '''        if let Self::Owned(value) = self
            && !value.accepts_physical_ledger(host)
        {
            return Err(foreign_ledger());
        }
'''
    add('foreign-physical-owner-accepted', [('node', node_guard, ''), ('storage', storage_guard, '')], refused, 'waveshaper accepted foreign or refused node admission')
    add('curve-length-instead-of-capacity', [('node', 'self.retirement.as_ref(),\n                    data.capacity() * std::mem::size_of::<f32>(),', 'self.retirement.as_ref(),\n                    data.len() * std::mem::size_of::<f32>(),')], curve, 'live curve capacity omitted from ledger')
    add('owned-value-destroyed-inline', [('retirement', 'if self.lease.is_some() {', 'if false {')], refused, 'refused waveshaper data freed on caller')
    add('physical-credit-returned-before-data', [('retirement', '            let lease = self.lease.take().unwrap();', '            let mut lease = self.lease.take().unwrap();\n            lease.permit.shrink_to(0);')], curve, 'curve credit returned before actual CPU drop')
    add('small-curve-credit-omitted', [('storage', 'host.reserve_inline(bytes)', 'host.reserve_inline(0)')], threshold, 'small curve acquired artificial handoff credit')
    add('bulk-curve-destroyed-inline', [('storage', 'Some(host) if bytes < 4096', 'Some(host) if bytes <= 4096')], threshold, 'live curve capacity omitted from ledger')
    anchor = '        let admission = curve\n'
    duplicate = '''        if curve.is_none()
            && let Some(host) = self.retirement.as_ref()
            && let Some(old) = self.curve.as_ref()
        {
            let _reservation = host.reserve_value_with_bytes::<Vec<f32>>(old.capacity() * 4)?;
        }
'''
    add('curve-clear-double-charged', [('node', anchor, duplicate + anchor)], curve, 'curve clear acquired duplicate admission')
    assert len(rows) == 37, len(rows)
    return rows


def diagnostics(log, package):
    result = Counter()
    for line in log.read_text().splitlines():
        try: row = json.loads(line)
        except ValueError: continue
        if row.get('reason') != 'compiler-message': continue
        message = row['message']
        spans = [span for span in message.get('spans', []) if span.get('is_primary')]
        if not spans: continue
        files, lines = [], []
        for span in spans:
            path = Path(span['file_name'])
            files.append(str(path.relative_to(package)) if path.is_absolute() and path.is_relative_to(package) else str(path))
            lines.extend(text['text'].strip() for text in span.get('text', []))
        result[(tuple(files), message['level'], (message.get('code') or {}).get('code'), message['message'], tuple(lines))] += 1
    return result


def before_node(original):
    """Keep the original DSP/setters; adapt only the newer private API surface."""
    assert 'history: History' not in original and 'up_state: Vec<FirState>' in original
    original = original.replace('mod inline_tests;', 'pub(super) mod inline_tests;')
    original = original.replace('    output_connected: bool,', '    output_connected: bool,\n    before_host: Option<crate::node_retirement::Host>,', 1)
    original = original.replace('            output_connected: false,', '            output_connected: false,\n            before_host: None,', 1)
    return original + '''
impl WaveShaperNode {
    pub(crate) fn prepare_storage(&mut self, host: &crate::node_retirement::Host, _channels: u32) -> shared::error::EngineResult<()> {
        self.before_host = Some(host.clone());
        Ok(())
    }
    pub(crate) fn try_set_curve(&mut self, curve: &mut Option<Vec<f32>>) -> shared::error::EngineResult<()> {
        let host = self.before_host.clone();
        self.retire_curve(host.as_ref())?;
        self.set_curve(curve.take());
        Ok(())
    }
    pub(crate) fn try_set_oversample(&mut self, mode: OversampleType, channels: u32) -> shared::error::EngineResult<()> {
        self.set_oversample(mode);
        self.prepare_oversampling(channels);
        Ok(())
    }
    #[cfg(test)]
    pub(crate) fn history_identity(&self) -> (OversampleType, usize, usize, usize) {
        (self.oversample, self.up_state.as_ptr() as usize, self.down_state.as_ptr() as usize, (self.up_state.capacity()+self.down_state.capacity())*std::mem::size_of::<FirState>())
    }
    #[cfg(test)]
    pub(crate) fn curve_identity(&self) -> Option<(usize, usize)> {
        self.curve.as_ref().map(|data|(data.as_ptr() as usize,data.capacity()*4))
    }
    #[cfg(test)]
    pub(crate) fn coefficients(&self) -> (&[f32], &[f32]) { (&self.up_taps, &self.down_taps) }
    #[cfg(test)]
    pub(crate) fn observe_history(&mut self, _hook: std::sync::Arc<dyn Fn()+Send+Sync>) {}
    #[cfg(test)]
    pub(crate) fn observe_curve(&mut self, _hook: std::sync::Arc<dyn Fn()+Send+Sync>) {}
}
'''


def main(work, target, before_directory, preflight=False):
    package, env, frozen = base.prepare(work, target)
    product = base.capacity.product
    common = product.whole.support.clock.common
    paths = {key: package / name for key, name in [
        ('node', 'src/nodes/wave_shaper.rs'), ('storage', 'src/nodes/wave_shaper_storage.rs'),
        ('context', 'src/context.rs'), ('retirement', 'src/node_retirement.rs'),
        ('replacement', 'src/audio_thread_replacement_input.rs'), ('inbox', 'src/command_inbox.rs'),
        ('channel', 'local-deps/shared/src/audio_channel.rs'),
    ]}
    originals = {key: path.read_text() for key, path in paths.items()}
    rows = declarations(originals)
    programs, controls, red, suites = [], [], [], {}
    def build(label, release=False):
        binary = product.src.build(package, work, env, label, release)
        programs.append(dict(name=label, path=str(binary), sha256=product.whole.support.codecs.sha(binary), pruned=False))
        return binary
    def run(binary, label, selector='', expected=0, assertion=None):
        command = [str(binary)] + (['--exact', selector] if selector else []) + ['--nocapture']
        code = common.run(command, work / (label + '.log'), package, env, timeout=300)
        (work / (label + '-exit.txt')).write_text(str(code) + '\n')
        text = (work / (label + '.log')).read_text()
        assert code == expected, (label, code)
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
    policy = json.loads((before_directory / 'baseline-policy.json').read_text())
    source_base = Path(policy['immutable_full_source_base']) / 'engine/crates/audio/src'
    before_names = ['nodes/wave_shaper.rs', 'context.rs', 'audio_thread.rs', 'audio_thread_binding_retirement_tests.rs', 'audio_thread_parameter_retirement_tests.rs', 'audio_thread_command_parameters_tests.rs', 'nodes/mod.rs', 'nodes/wave_shaper_inline_tests.rs', 'nodes/wave_shaper_inline_reference.rs']
    before_paths = {name: package / 'src' / name for name in before_names}
    saved = {name: path.read_bytes() for name, path in before_paths.items()}
    try:
        for name, path in before_paths.items(): path.write_bytes((source_base / name).read_bytes())
        assert common.run(['cargo', 'clippy', '--lib', '--tests', '--offline', '--locked', '--message-format=json'], work / 'clippy-before.jsonl', package, env, timeout=300) == 0
    finally:
        for name, path in before_paths.items(): path.write_bytes(saved[name])
    old, current = diagnostics(work / 'clippy-before.jsonl', package), diagnostics(work / 'clippy.jsonl', package)
    added = current - old
    delta = dict(before=sum(old.values()), after=sum(current.values()), new_diagnostics=sum(added.values()), exact_multiset=old == current, actor_included=True, new=[dict(key=key, count=count) for key, count in added.items()])
    (work / 'clippy-delta.json').write_text(json.dumps(delta, indent=2) + '\n')
    assert not added, delta
    print('Clippy multiset verified before formal source controls', delta['before'], delta['after'], flush=True)
    if not preflight:
        adapted = before_node((source_base / 'nodes/wave_shaper.rs').read_text())
        (work / 'before-node-adapted.rs').write_text(adapted)
        try:
            paths['node'].write_text(adapted)
            binary = build('before-node')
            for label, test, assertion in [
                ('before-history-admission', 'history_admission_precedes_allocation_and_mode_change', 'FIR history admission bypassed physical ledger'),
                ('before-live-curve', 'curve_admission_preserves_input_and_clear_needs_no_new_credit', 'live curve capacity omitted from ledger'),
            ]:
                run(binary, label, PREFIX + test, 101, assertion)
                red.append(dict(name=label, test=PREFIX + test, exit=101, expected_assertion=assertion, sha256=programs[-1]['sha256']))
            programs[-1]['pruned'] = True
            binary.unlink()
        finally: paths['node'].write_text(originals['node'])
        for row in rows:
            changed_keys = set()
            try:
                for key, before, after in row['changes']:
                    assert key not in changed_keys
                    changed_keys.add(key)
                    paths[key].write_text(originals[key].replace(before, after))
                binary = build(row['name'])
                run(binary, row['name'], row['test'], 101, row['expected_assertion'])
                controls.append({key: value for key, value in row.items() if key != 'changes'} | dict(exit=101, sha256=programs[-1]['sha256']))
                programs[-1]['pruned'] = True
                binary.unlink()
                (work / 'completed-controls.json').write_text(json.dumps(controls, indent=2) + '\n')
                print('Actual WaveShaper ownership fault rejected', row['name'], flush=True)
            finally:
                for key in changed_keys: paths[key].write_text(originals[key])
        release = build('positive-release', True)
        run(release, 'full-release')
    product.whole.verify_inputs(work, frozen)
    assert all(path.read_text() == originals[key] for key, path in paths.items())
    for dependency in ['shared', 'io', 'services', 'frame-wire']:
        origin = ROOT / 'engine/crates' / dependency / 'src'
        assert all((package / 'local-deps' / dependency / path.relative_to(origin.parent)).read_bytes() == path.read_bytes() for path in origin.rglob('*') if path.is_file())
    origin = ROOT / 'engine/crates/audio/src'
    assert all((package / 'src' / path.relative_to(origin)).read_bytes() == path.read_bytes() for path in origin.rglob('*') if path.is_file())
    summary = dict(programs=programs, controls=controls, original_red=red, suites=suites, root_inputs_unchanged=True, source_copies_restored=True, root_dev_profile_verified=True, isolated_release_is_not_root_shipping=True, preflight=preflight, before_api_adapters='Carrier for the original context retirement host; original setters and FIR kernel unchanged; newer fallible APIs forward original setters, observations are read-only or no-op. No live capacity ownership added.')
    (work / 'summary.json').write_text(json.dumps(summary, indent=2) + '\n')
    print('WaveShaper capacity ownership verified', len(controls), 'fresh faults', flush=True)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--target-dir', type=Path, required=True)
    parser.add_argument('--before-directory', type=Path, required=True)
    parser.add_argument('--preflight', action='store_true')
    args = parser.parse_args()
    main(args.output.resolve(), args.target_dir.resolve(), args.before_directory.resolve(), args.preflight)
