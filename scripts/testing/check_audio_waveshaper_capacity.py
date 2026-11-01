#!/usr/bin/env python3
"""Oversampling commands left WaveShaper's first quantum allocating scratch.

Keep the original filter kernel and compare its complete PCM/history against
prepared commands, including mode changes. Prove missing preparation and lost
history through freshly compiled, executed allocation/PCM assertions.
"""
from __future__ import annotations
import argparse
import json
from pathlib import Path
import check_audio_graph_capacity as capacity

ROOT = capacity.ROOT
TEST, COMPAT = capacity.TESTS


def declarations(originals):
    rows = [
        ('shaper-command-preparation-omitted', 'context.rs',
         'node.prepare_oversampling(channels);', 'let _ = channels;', 'cold context render allocated'),
        ('shaper-only-one-channel-prepared', 'nodes/wave_shaper.rs',
         'let channels = channels.max(1) as usize;', 'let channels = 1_usize;', 'cold context render allocated'),
        ('shaper-heap-scratch-reintroduced', 'nodes/wave_shaper.rs',
         'let mut scratch = [0.0; crate::audio_thread::RENDER_QUANTUM_FRAMES * 4];', 'let mut scratch = vec![0.0; crate::audio_thread::RENDER_QUANTUM_FRAMES * 4];', 'cold context render allocated'),
        ('shaper-repeated-command-loses-history', 'nodes/wave_shaper.rs',
         '    pub(crate) fn prepare_oversampling(&mut self, channels: u32) {',
         '    pub(crate) fn prepare_oversampling(&mut self, channels: u32) {\n        self.up_state.clear();\n        self.down_state.clear();',
         'prepared waveshaper PCM or history changed'),
    ]
    for name, file, before, after, _ in rows:
        assert before != after and originals[file].count(before) == 1, (name, 'anchor drift')
    return rows


def main(work, target, before_source=None, preflight=False):
    product = capacity.product
    package, env, frozen = capacity.prepare(work, target)
    assert 'scripts/testing/check_audio_waveshaper_capacity.py' in frozen
    paths = {n: package / 'src' / n for n in ['context.rs', 'nodes/wave_shaper.rs']}
    originals = {n: p.read_text() for n, p in paths.items()}
    rows = declarations(originals)
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
    for n, test in enumerate([TEST, COMPAT]):
        capacity.run(positive, package, env, work, f'positive-targeted-{n}', test)
    assert product.whole.support.clock.common.run(['cargo', 'clippy', '--lib', '--tests', '--offline', '--locked', '--message-format=json'], work / 'clippy.jsonl', package, env, timeout=300) == 0
    if not preflight:
        if before_source:
            old = before_source.read_text()
            # Only a no-op compatibility method is appended for the new private
            # command API. All original state, setters and DSP remain verbatim.
            shim = '\nimpl WaveShaperNode { pub(crate) fn prepare_oversampling(&mut self, _channels: u32) {} }\n'
            (work / 'before-wave_shaper.rs').write_text(old)
            (work / 'before-api-shim.rs').write_text(shim)
            try:
                paths['nodes/wave_shaper.rs'].write_text(old + shim)
                binary = product.src.build(package, work, env, 'before-node-with-noop-api', False)
                red.append(capacity.run(binary, package, env, work, 'before-node-with-noop-api', TEST, 101, 'cold context render allocated'))
                capacity.run(binary, package, env, work, 'before-node-long-block-compat', COMPAT)
                record('before-node-with-noop-api', binary, True)
            finally:
                paths['nodes/wave_shaper.rs'].write_text(originals['nodes/wave_shaper.rs'])
        for name, file, before, after, assertion in rows:
            try:
                paths[file].write_text(originals[file].replace(before, after))
                binary = product.src.build(package, work, env, name, False)
                item = capacity.run(binary, package, env, work, name, TEST, 101, assertion)
                item['binary_sha256'] = record(name, binary, True)
                controls.append(item)
                (work / 'completed-controls.json').write_text(json.dumps(controls, indent=2) + '\n')
                print('Actual WaveShaper preparation fault rejected', name, flush=True)
            finally:
                paths[file].write_text(originals[file])
        suites['debug'] = capacity.run(positive, package, env, work, 'full-debug')
        release = product.src.build(package, work, env, 'positive-release', True)
        record('positive-release', release)
        suites['isolated-release'] = capacity.run(release, package, env, work, 'full-release')
    product.whole.verify_inputs(work, frozen)
    root = ROOT / 'engine/crates/audio/src'
    assert all((package / 'src' / p.relative_to(root)).read_bytes() == p.read_bytes() for p in root.rglob('*') if p.is_file())
    result = dict(programs=programs, controls=controls, original_red=red, suites=suites, root_inputs_unchanged=True, source_copies_restored=True, root_dev_profile_verified=True, isolated_release_is_not_root_shipping=True, preflight=preflight)
    (work / 'summary.json').write_text(json.dumps(result, indent=2) + '\n')
    print('WaveShaper preparation verified', len(controls), 'faults,', len(red), 'old-code red checks,', len(suites), 'full suites', flush=True)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--target-dir', type=Path, required=True)
    parser.add_argument('--before-source', type=Path)
    parser.add_argument('--preflight', action='store_true')
    args = parser.parse_args()
    main(args.output.resolve(), args.target_dir.resolve(), args.before_source, args.preflight)
