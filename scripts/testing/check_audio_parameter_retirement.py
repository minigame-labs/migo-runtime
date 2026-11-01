#!/usr/bin/env python3
"""Bulk FFT table replacement destroyed owners on the audio caller.

Check bulk CPU destruction, capacity credit, refusal and bounded FIR/FFT history.
Restore the old context with only test additions and an analyser command bridge;
original setters must reach the precise table ownership assertions.
"""
from __future__ import annotations
import argparse
import json
from pathlib import Path
import check_audio_graph_capacity as capacity

ROOT = capacity.ROOT
DROP, CLOSE = capacity.TESTS


def legacy_context(raw):
    assert 'fn set_analyser_fft_size(' not in raw
    source=raw
    for name, helper in [('steady_state_audio_quantum_never_reaches_the_heap','analyser_tables'),('a_graph_change_on_the_render_path_never_reaches_the_heap','oversampling_tables')]:
        before=f'    fn {name}() {{';assert source.count(before)==1
        source=source.replace(before,before+f'\n        crate::audio_thread::parameter_retirement_tests::{helper}();',1)
    return source + """
#[cfg(test)]
impl AudioContext {
    pub(crate) fn set_analyser_fft_size(&mut self, id: AudioNodeId, size: usize) -> bool {
        self.with_node_typed::<crate::nodes::AnalyserNode, _>(id, |node| node.set_fft_size(size)).is_some()
    }
}
"""


def main(work, target, before_source=None, preflight=False):
    if 'retire_fft_tables' not in (ROOT/'engine/crates/audio/src/nodes/analyser.rs').read_text():
        if before_source is not None:
            raise ValueError('A context-only before-source cannot bypass node-owned FFT storage; use the frozen analyser reference and source controls in the current workspace gate.')
        from check_audio_analyser_workspace import main as workspace_main
        return workspace_main(work, target, preflight)
    product = capacity.product
    package, env, frozen = capacity.prepare(work, target)
    assert 'scripts/testing/check_audio_parameter_retirement.py' in frozen
    paths = {n: package / 'src' / n for n in ['node_retirement.rs', 'context.rs', 'nodes/analyser.rs', 'nodes/wave_shaper.rs']}
    originals = {n: p.read_text() for n, p in paths.items()}
    rows = [
        ('FFT-tables-inline','node_retirement.rs','if self.lease.is_some() {','if false {',DROP,'FFT tables escaped CPU retirement'),
        ('FFT-tables-async','node_retirement.rs','cpu.retirement().retire(value).await;','drop(value);',DROP,'FFT tables escaped CPU retirement'),
        ('FFT-handoff-omitted','context.rs','node.retire_fft_tables(size, retirement.as_ref())','Ok::<(), EngineError>(())',DROP,'FFT tables escaped CPU retirement'),
        ('FFT-capacity-uncharged','nodes/analyser.rs','host.retire_with(bytes, || {','host.retire_with(0, || {',DROP,'FFT table capacity escaped physical ledger'),
        ('FFT-admission-free','node_retirement.rs','try_reserve(bytes, "graph retained resource")','try_reserve(0, "graph retained resource")',DROP,'FFT table capacity escaped physical ledger'),
        ('storage-taken-before-admission','node_retirement.rs','        let reservation = self.reserve_value_with_bytes::<T>(retained_bytes)?;\n        let retired = reservation.finish(take());','        let taken = take();\n        let reservation = self.reserve_value_with_bytes::<T>(retained_bytes)?;\n        let retired = reservation.finish(taken);',DROP,'FFT tables escaped CPU retirement'),
        ('unchanged-FFT-tables-retired','nodes/analyser.rs','if size.next_power_of_two().clamp(32, 32768) == self.fft_size {','if false {',DROP,'unchanged/refused FFT retired live tables'),
        ('repeated-FIR-loses-history','nodes/wave_shaper.rs','    pub(crate) fn prepare_oversampling(&mut self, channels: u32) {','    pub(crate) fn prepare_oversampling(&mut self, channels: u32) {\n        self.up_state.clear();\n        self.down_state.clear();',CLOSE,'parameter replacement PCM or filter history changed'),
        ('small-FFT-unnecessary-handoff','nodes/analyser.rs','if bytes < crate::node_retirement::HANDOFF_BYTES {','if false {',DROP,'unchanged/refused FFT retired live tables'),
    ]
    for name, file, before, after, _, _ in rows:
        assert before != after and originals[file].count(before) == 1, (name, 'anchor drift')
    programs, controls, red, suites = [], [], [], {}

    def record(name, binary, pruned=False):
        digest = product.whole.support.codecs.sha(binary)
        programs.append(dict(name=name, path=str(binary), sha256=digest, pruned=pruned))
        if pruned:
            binary.unlink()
        return digest

    positive = product.src.build(package, work, env, 'positive-debug', False)
    capacity.profile.verify(work / 'positive-debug-build.jsonl', json.loads((work / 'debug-profile-policy.json').read_text()), work / 'debug-profile-artifacts.json')
    record('positive-debug', positive)
    for index, test in enumerate([DROP, CLOSE]):
        capacity.run(positive, package, env, work, f'positive-targeted-{index}', test)
    assert product.whole.support.clock.common.run(['cargo', 'clippy', '--lib', '--tests', '--offline', '--locked', '--message-format=json'], work / 'clippy.jsonl', package, env, timeout=300) == 0
    if not preflight:
        suites['debug'] = capacity.run(positive, package, env, work, 'full-debug')
        if before_source:
            raw = before_source.read_text()
            (work / 'before-context-original.rs').write_text(raw)
            adapted = legacy_context(raw)
            (work / 'before-context-adapted.rs').write_text(adapted)
            try:
                paths['context.rs'].write_text(adapted)
                binary = product.src.build(package, work, env, 'before-context-resources', False)
                for label,test,assertion in [('before-FFT-tables',DROP,'FFT tables escaped CPU retirement')]:
                    red.append(capacity.run(binary, package, env, work, label, test, 101, assertion))
                capacity.run(binary, package, env, work, 'before-bounded-FIR-preserved', CLOSE)
                record('before-context-resources', binary, True)
            finally:
                paths['context.rs'].write_text(originals['context.rs'])
        for name, file, before, after, test, assertion in rows:
            try:
                paths[file].write_text(originals[file].replace(before, after))
                binary = product.src.build(package, work, env, name, False)
                item = capacity.run(binary, package, env, work, name, test, 101, assertion)
                item['binary_sha256'] = record(name, binary, True)
                controls.append(item)
                (work / 'completed-controls.json').write_text(json.dumps(controls, indent=2) + '\n')
                print('Actual parameter retirement fault rejected', name, flush=True)
            finally:
                paths[file].write_text(originals[file])
        release = product.src.build(package, work, env, 'positive-release', True)
        record('positive-release', release)
        suites['isolated-release'] = capacity.run(release, package, env, work, 'full-release')
    product.whole.verify_inputs(work, frozen)
    root = ROOT / 'engine/crates/audio/src'
    assert all((package / 'src' / p.relative_to(root)).read_bytes() == p.read_bytes() for p in root.rglob('*') if p.is_file())
    result = dict(programs=programs, controls=controls, original_red=red, suites=suites, root_inputs_unchanged=True, source_copies_restored=True, root_dev_profile_verified=True, isolated_release_is_not_root_shipping=True, preflight=preflight, legacy_adaptation='Only cfg(test) analyser-command bridge and two current test calls; original setters, FFT/history and render bytes unchanged.')
    (work / 'summary.json').write_text(json.dumps(result, indent=2) + '\n')
    print('Parameter retirement verified', len(controls), 'faults,', len(red), 'adapted-old-code red checks,', len(suites), 'full suites', flush=True)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--target-dir', type=Path, required=True)
    parser.add_argument('--before-source', type=Path)
    parser.add_argument('--preflight', action='store_true')
    args = parser.parse_args()
    main(args.output.resolve(), args.target_dir.resolve(), args.before_source, args.preflight)
