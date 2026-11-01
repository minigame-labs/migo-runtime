#!/usr/bin/env python3
"""Source re-binding and curve replacement freed large resources on audio.

Check real CPU destruction, held credit, failed replacement and exact PCM.
Restore the old context with only a test-visible curve-command bridge;
actual old binding/curve setters must fail at real single-test assertions.
"""
from __future__ import annotations
import argparse
import json
from pathlib import Path
import check_audio_graph_capacity as capacity

ROOT = capacity.ROOT
DROP = 'audio_thread::tests::set_buffer_rejects_claimed_context_that_does_not_own_node'
CLOSE = 'audio_thread::tests::snapshot_buffer_commands_validate_context_and_node_ownership'
CREDIT = 'audio_thread::tests::a_steady_state_audio_thread_tick_never_reaches_the_heap'


def legacy_context(raw):
    assert 'fn set_wave_shaper_curve(' not in raw
    # The old command arm called this exact original setter inline. The bridge
    # exposes that path to the same test without adding retirement semantics.
    return raw + """
#[cfg(test)]
impl AudioContext {
    pub(crate) fn set_analyser_fft_size(&mut self, id: AudioNodeId, size: usize) -> bool { self.with_node_typed::<crate::nodes::AnalyserNode, _>(id, |node| node.set_fft_size(size)).is_some() }
    pub(crate) fn set_wave_shaper_curve(&mut self, id: AudioNodeId, curve: Option<Vec<f32>>) -> bool {
        self.with_node_typed::<crate::nodes::WaveShaperNode, _>(id, |node| node.set_curve(curve)).is_some()
    }
}
"""


def main(work, target, before_source=None, preflight=False):
    product = capacity.product
    package, env, frozen = capacity.prepare(work, target)
    assert 'scripts/testing/check_audio_binding_retirement.py' in frozen
    paths = {n: package / 'src' / n for n in ['node_retirement.rs', 'context.rs', 'nodes/buffer_source.rs', 'nodes/wave_shaper.rs']}
    originals = {n: p.read_text() for n, p in paths.items()}
    rows = [
        ('binding-destroyed-inline','node_retirement.rs','if self.lease.is_some() {','if false {',DROP,'legacy binding escaped CPU retirement'),
        ('binding-async-drop','node_retirement.rs','cpu.retirement().retire(value).await;','drop(value);',DROP,'legacy binding escaped CPU retirement'),
        ('legacy-binding-no-handoff','nodes/buffer_source.rs','crate::node_retirement::retire_replacement(&mut self.buffer, host, 0)','Ok(())',DROP,'legacy binding escaped CPU retirement'),
        ('snapshot-binding-no-handoff','nodes/buffer_source.rs','crate::node_retirement::retire_replacement(&mut self.buffer, host, 0)','Ok(())',CLOSE,'snapshot binding escaped CPU retirement'),
        ('curve-no-handoff','nodes/wave_shaper.rs','crate::node_retirement::retire_replacement(&mut self.curve, host, bytes)','Ok(())',CREDIT,'curve escaped CPU retirement'),
        ('curve-length-instead-of-capacity','nodes/wave_shaper.rs','curve.capacity() * std::mem::size_of::<f32>()','curve.len() * std::mem::size_of::<f32>()',CREDIT,'curve capacity admission bypassed physical ledger'),
        ('replacement-admission-uncharged','node_retirement.rs','try_reserve(bytes, "graph retained resource")','try_reserve(0, "graph retained resource")',DROP,'binding admission changed old state'),
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
    for index, test in enumerate([DROP, CLOSE, CREDIT]):
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
                for label,test,assertion in [('before-legacy-binding',DROP,'legacy binding escaped CPU retirement'),('before-snapshot-binding',CLOSE,'snapshot binding escaped CPU retirement'),('before-curve-replacement',CREDIT,'curve escaped CPU retirement')]:
                    red.append(capacity.run(binary, package, env, work, label, test, 101, assertion))
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
                print('Actual context retirement fault rejected', name, flush=True)
            finally:
                paths[file].write_text(originals[file])
        release = product.src.build(package, work, env, 'positive-release', True)
        record('positive-release', release)
        suites['isolated-release'] = capacity.run(release, package, env, work, 'full-release')
    product.whole.verify_inputs(work, frozen)
    root = ROOT / 'engine/crates/audio/src'
    assert all((package / 'src' / p.relative_to(root)).read_bytes() == p.read_bytes() for p in root.rglob('*') if p.is_file())
    result = dict(programs=programs, controls=controls, original_red=red, suites=suites, root_inputs_unchanged=True, source_copies_restored=True, root_dev_profile_verified=True, isolated_release_is_not_root_shipping=True, preflight=preflight, legacy_adaptation='Only cfg(test) curve-command bridge invokes the original inline setter; all old binding/curve/render implementations remain unchanged.')
    (work / 'summary.json').write_text(json.dumps(result, indent=2) + '\n')
    print('Binding retirement verified', len(controls), 'faults,', len(red), 'adapted-old-code red checks,', len(suites), 'full suites', flush=True)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--target-dir', type=Path, required=True)
    parser.add_argument('--before-source', type=Path)
    parser.add_argument('--preflight', action='store_true')
    args = parser.parse_args()
    main(args.output.resolve(), args.target_dir.resolve(), args.before_source, args.preflight)
