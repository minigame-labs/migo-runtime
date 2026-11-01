#!/usr/bin/env python3
"""Graph collection destroyed node/PCM and excess render buffers in the mix.

Compile actual production-drop, held-credit and closed-host checks. Exercise
fresh CPU/inline/free/admission faults; adapt only the old command return types
and a test-only preparation shim when proving the previous render path red.
"""
from __future__ import annotations
import argparse
import json
from pathlib import Path
import check_audio_graph_capacity as capacity

ROOT = capacity.ROOT
DROP = 'audio_thread::tests::production_context_mix_uses_mature_sinc_and_unregisters_real_node_ids'
CREDIT = 'audio_thread::tests::a_steady_state_audio_thread_tick_never_reaches_the_heap'


def legacy_context(raw):
    source = raw
    source = source.replace('pub fn create_buffer_source(&mut self, node_id: AudioNodeId) {\n        self.add_node(Box::new(BufferSourceNode::new(node_id, self.sample_rate)));', 'pub fn create_buffer_source(&mut self, node_id: AudioNodeId) -> bool {\n        self.add_node(Box::new(BufferSourceNode::new(node_id, self.sample_rate)))', 1)
    source = source.replace('pub fn create_gain(&mut self, node_id: AudioNodeId) {\n        self.add_node(Box::new(GainNode::new(node_id)));', 'pub fn create_gain(&mut self, node_id: AudioNodeId) -> bool {\n        self.add_node(Box::new(GainNode::new(node_id)))', 1)
    start = source.index('    pub fn add_node(')
    end = source.index('    /// Grow graph scratch', start)
    body = source[start:end].replace('pub fn add_node(&mut self, mut node: Box<dyn AudioNodeProcessor>) {', 'pub fn add_node(&mut self, mut node: Box<dyn AudioNodeProcessor>) -> bool {', 1)
    anchor = '        self.device_controls_pending = true;\n    }'
    assert body.count(anchor) == 1
    source = source[:start] + body.replace(anchor, '        self.device_controls_pending = true;\n        true\n    }') + source[end:]
    source = source.replace('    fn new_with_pcm_budget(', '    pub(crate) fn new_with_pcm_budget(', 1)
    bridges = []
    if 'pub(crate) fn set_analyser_fft_size(' not in source:
        bridges.append('pub(crate) fn set_analyser_fft_size(&mut self, id: AudioNodeId, size: usize) -> bool { self.with_node_typed::<crate::nodes::AnalyserNode, _>(id, |node| node.set_fft_size(size)).is_some() }')
    if 'pub(crate) fn set_wave_shaper_curve(' not in source:
        bridges.append('pub(crate) fn set_wave_shaper_curve(&mut self, id: AudioNodeId, curve: Option<Vec<f32>>) -> bool { self.with_node_typed::<crate::nodes::WaveShaperNode, _>(id, |node| node.set_curve(curve)).is_some() }')
    if 'pub(crate) fn prepare_node_retirement(' not in source:
        bridges.append('pub(crate) fn prepare_node_retirement(&mut self, _: migo_io::audio_cpu::AudioCpu, _: Arc<crate::limits::AudioAggregateLedger>) -> EngineResult<()> { Ok(()) }')
    if 'pub(crate) fn observe_buffer_retirement(' not in source:
        bridges.append('pub(crate) fn observe_buffer_retirement(&mut self, id: AudioBufferId, _: Arc<dyn Fn() + Send + Sync>) -> bool { self.buffers.contains_key(&id) }')
    if 'pub(crate) fn observe_context_retirement(' not in source:
        bridges.append('pub(crate) fn observe_context_retirement(&mut self, _: Arc<dyn Fn() + Send + Sync>) {}')
    source += '\n#[cfg(test)]\nimpl AudioContext {\n' + '\n'.join(bridges) + '\n}\n'
    # Render, pruning, recycling and resource ownership remain the real old code.
    for marker, ending in [('    fn prune(', '    /// Whether a node'), ('    /// Take a collected', '    /// Downcast')]:
        assert raw[raw.index(marker):raw.index(ending, raw.index(marker))] == source[source.index(marker):source.index(ending, source.index(marker))]
    return source


def main(work, target, before_source=None, preflight=False):
    product = capacity.product
    package, env, frozen = capacity.prepare(work, target)
    assert 'scripts/testing/check_audio_node_retirement.py' in frozen
    paths = {n: package / 'src' / n for n in ['node_retirement.rs', 'context.rs']}
    originals = {n: p.read_text() for n, p in paths.items()}
    rows = [
        ('node-destroyed-inline', 'node_retirement.rs', 'if let Some(lease) = self.lease.take() {', 'if false {\n            let lease = self.lease.take().unwrap();', DROP, 'node or PCM owner was destroyed outside CPU workers'),
        ('node-destroyed-on-async', 'node_retirement.rs', 'cpu.retirement().retire(value).await;', 'drop(value);', DROP, 'node or PCM owner was destroyed outside CPU workers'),
        ('render-buffer-destroyed-inline', 'node_retirement.rs', 'self.buffer = buffer;', 'drop(buffer);', DROP, 'excess render buffers were freed in mix'),
        ('physical-credit-returned-first', 'node_retirement.rs', '    _resource: Box<dyn Send>,\n    _buffer: Option<Vec<f32>>,\n    _permit: AudioAggregatePermit,', '    _permit: AudioAggregatePermit,\n    _resource: Box<dyn Send>,\n    _buffer: Option<Vec<f32>>,', CREDIT, 'node retirement returned credit before actual destructor'),
        ('retirement-admission-not-charged', 'node_retirement.rs', 'try_reserve(bytes, "graph node retirement")', 'try_reserve(0, "graph node retirement")', CREDIT, 'retirement backlog bypassed physical admission'),
        ('closed-host-admits-node', 'node_retirement.rs', 'if self.cpu.is_closed() {', 'if false {', CREDIT, 'closed host admitted new graph owner'),
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
    for index, test in enumerate([DROP, CREDIT]):
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
                binary = product.src.build(package, work, env, 'before-node-owner', False)
                red.append(capacity.run(binary, package, env, work, 'before-node-owner', DROP, 101, 'node or PCM owner was destroyed outside CPU workers'))
                record('before-node-owner', binary, True)
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
                print('Actual node retirement fault rejected', name, flush=True)
            finally:
                paths[file].write_text(originals[file])
        release = product.src.build(package, work, env, 'positive-release', True)
        record('positive-release', release)
        suites['isolated-release'] = capacity.run(release, package, env, work, 'full-release')
    product.whole.verify_inputs(work, frozen)
    root = ROOT / 'engine/crates/audio/src'
    assert all((package / 'src' / p.relative_to(root)).read_bytes() == p.read_bytes() for p in root.rglob('*') if p.is_file())
    result = dict(programs=programs, controls=controls, original_red=red, suites=suites, root_inputs_unchanged=True, source_copies_restored=True, root_dev_profile_verified=True, isolated_release_is_not_root_shipping=True, preflight=preflight, legacy_adaptation='Only command return signatures, test-constructor visibility and cfg(test) no-op preparation/observation bridges; original pruning/recycling/processing bytes verified unchanged.')
    (work / 'summary.json').write_text(json.dumps(result, indent=2) + '\n')
    print('Node retirement verified', len(controls), 'faults,', len(red), 'adapted-old-code red checks,', len(suites), 'full suites', flush=True)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--target-dir', type=Path, required=True)
    parser.add_argument('--before-source', type=Path)
    parser.add_argument('--preflight', action='store_true')
    args = parser.parse_args()
    main(args.output.resolve(), args.target_dir.resolve(), args.before_source, args.preflight)
