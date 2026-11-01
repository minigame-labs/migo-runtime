#!/usr/bin/env python3
"""WaveShaper's missing lifetime state truncated FIR tails and suppressed DC.

Run the actual context/device-source contracts with an unchanged explicit-zero
DSP reference. Restore the real old node and execute freshly compiled lifetime,
PCM and connection faults; compilation failures are never red-test evidence.
"""
from __future__ import annotations
import argparse
import json
from pathlib import Path
import check_audio_graph_capacity as capacity

ROOT = capacity.ROOT
TAIL, DC = capacity.TESTS


def main(work, target, before_source=None, preflight=False):
    product = capacity.product
    package, env, frozen = capacity.prepare(work, target)
    assert 'scripts/testing/check_audio_waveshaper_lifecycle.py' in frozen
    paths = {n: package / 'src' / n for n in ['context.rs', 'nodes/wave_shaper.rs']}
    originals = {n: p.read_text() for n, p in paths.items()}
    rows = [
        ('tail-history-ignored', 'nodes/wave_shaper.rs',
         '        let baseline = self.zero_input_response();', '        return false;\n        let baseline = self.zero_input_response();', TAIL, 'waveshaper FIR tail PCM changed'),
        ('downstream-fir-history-ignored', 'nodes/wave_shaper.rs',
         'state.history.iter().any(|&sample| sample != baseline)', 'false', TAIL, 'waveshaper FIR tail PCM changed'),
        ('empty-input-tail-not-rendered', 'nodes/wave_shaper.rs',
         'output.len() / ch * ch', '0_usize', TAIL, 'waveshaper FIR tail PCM changed'),
        ('connected-dc-not-active', 'nodes/wave_shaper.rs',
         'self.has_tail_audio() || (self.output_connected && self.zero_input_response() != 0.0)', 'self.has_tail_audio()', DC, 'connected DC waveshaper did not become active'),
        ('output-connect-not-notified', 'context.rs',
         'node.set_output_connected(true);', 'let _ = node;', DC, 'connected DC waveshaper did not become active'),
        ('output-disconnect-not-notified', 'context.rs',
         'node.set_output_connected(connected);', 'let _ = (node, connected);', DC, 'disconnected DC waveshaper kept graph active'),
        ('pruned-output-not-notified', 'context.rs',
         'source.set_output_connected(connected);', 'let _ = (source, connected);', DC, 'pruned last output kept DC active'),
        ('replacement-output-not-notified', 'context.rs',
         'node.set_output_connected(self.connections.iter().any(|c| c.src == node_id));', 'let _ = node_id;', DC, 'replacement shaper lost output connection'),
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
    for n, test in enumerate([TAIL, DC]):
        capacity.run(positive, package, env, work, f'positive-targeted-{n}', test)
    assert product.whole.support.clock.common.run(['cargo', 'clippy', '--lib', '--tests', '--offline', '--locked', '--message-format=json'], work / 'clippy.jsonl', package, env, timeout=300) == 0
    if not preflight:
        if before_source:
            old = before_source.read_bytes()
            (work / 'before-wave_shaper.rs').write_bytes(old)
            try:
                paths['nodes/wave_shaper.rs'].write_bytes(old)
                binary = product.src.build(package, work, env, 'before-node', False)
                for label, test, assertion in [('before-tail', TAIL, 'waveshaper FIR tail PCM changed'), ('before-dc', DC, 'connected DC waveshaper did not become active')]:
                    red.append(capacity.run(binary, package, env, work, label, test, 101, assertion))
                record('before-node', binary, True)
            finally:
                paths['nodes/wave_shaper.rs'].write_text(originals['nodes/wave_shaper.rs'])
        for name, file, before, after, test, assertion in rows:
            try:
                paths[file].write_text(originals[file].replace(before, after))
                binary = product.src.build(package, work, env, name, False)
                item = capacity.run(binary, package, env, work, name, test, 101, assertion)
                item['binary_sha256'] = record(name, binary, True)
                controls.append(item)
                (work / 'completed-controls.json').write_text(json.dumps(controls, indent=2) + '\n')
                print('Actual WaveShaper lifetime fault rejected', name, flush=True)
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
    print('WaveShaper lifecycle verified', len(controls), 'faults,', len(red), 'old-code red checks,', len(suites), 'full suites', flush=True)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--target-dir', type=Path, required=True)
    parser.add_argument('--before-source', type=Path)
    parser.add_argument('--preflight', action='store_true')
    args = parser.parse_args()
    main(args.output.resolve(), args.target_dir.resolve(), args.before_source, args.preflight)
