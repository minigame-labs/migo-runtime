#!/usr/bin/env python3
"""Source AudioParams were sampled once per quantum despite their a-rate contract.

Check analytic offset curves and the unchanged scalar oscillator kernel.
Restore old source files and reject freshly compiled PCM, phase and first-use
allocation faults at actual Rust assertions.
"""
from __future__ import annotations
import argparse
import json
from pathlib import Path
import check_audio_graph_capacity as capacity

ROOT = capacity.ROOT
OFFSET, OSCILLATOR = capacity.TESTS


def declarations(originals):
    rows = [
        ('equal-time-events-reversed', 'param.rs', 'self.events.partition_point(|e| e.time() <= time)', 'self.events.partition_point(|e| e.time() < time)', OFFSET, 'equal-time events lost call order'),
        ('offset-frozen-at-block-start', 'nodes/constant_source.rs', 'if self.offset.has_automation() {', 'if false {', OFFSET, 'constant offset a-rate PCM changed'),
        ('frequency-frozen-at-block-start', 'nodes/oscillator.rs', 'let frequency_automated = self.frequency.has_automation();', 'let frequency_automated = false;', OSCILLATOR, 'oscillator a-rate PCM or phase changed'),
        ('detune-frozen-at-block-start', 'nodes/oscillator.rs', 'let detune_automated = self.detune.has_automation();', 'let detune_automated = false;', OSCILLATOR, 'oscillator a-rate PCM or phase changed'),
        ('offset-heap-scratch-reintroduced', 'nodes/constant_source.rs', 'let mut automation = [0.0; crate::audio_thread::RENDER_QUANTUM_FRAMES];', 'let mut automation = vec![0.0; crate::audio_thread::RENDER_QUANTUM_FRAMES];', OFFSET, 'source automation render allocated'),
        ('frequency-heap-scratch-reintroduced', 'nodes/oscillator.rs', 'let mut frequency_values = [0.0; crate::audio_thread::RENDER_QUANTUM_FRAMES];', 'let mut frequency_values = vec![0.0; crate::audio_thread::RENDER_QUANTUM_FRAMES];', OSCILLATOR, 'source automation render allocated'),
        ('detune-heap-scratch-reintroduced', 'nodes/oscillator.rs', 'let mut detune_values = [0.0; crate::audio_thread::RENDER_QUANTUM_FRAMES];', 'let mut detune_values = vec![0.0; crate::audio_thread::RENDER_QUANTUM_FRAMES];', OSCILLATOR, 'source automation render allocated'),
        ('automation-restarts-phase', 'nodes/oscillator.rs', 'if frequency_automated || detune_automated {', 'if frequency_automated || detune_automated {\n            self.phase = 0.0;', OSCILLATOR, 'oscillator a-rate PCM or phase changed'),
    ]
    for name, file, before, after, _, _ in rows:
        assert before != after and originals[file].count(before) == 1, (name, 'anchor drift')
    return rows


def main(work, target, before_directory=None, preflight=False):
    product = capacity.product
    package, env, frozen = capacity.prepare(work, target)
    assert 'scripts/testing/check_audio_source_automation.py' in frozen
    paths = {n: package / 'src' / n for n in ['nodes/constant_source.rs', 'nodes/oscillator.rs', 'param.rs']}
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
    for n, test in enumerate([OFFSET, OSCILLATOR]):
        capacity.run(positive, package, env, work, f'positive-targeted-{n}', test)
    assert product.whole.support.clock.common.run(['cargo', 'clippy', '--lib', '--tests', '--offline', '--locked', '--message-format=json'], work / 'clippy.jsonl', package, env, timeout=300) == 0
    if not preflight:
        if before_directory:
            for file, label, test, assertion in [
                ('nodes/constant_source.rs', 'before-constant', OFFSET, 'constant offset a-rate PCM changed'),
                ('nodes/oscillator.rs', 'before-oscillator', OSCILLATOR, 'oscillator a-rate PCM or phase changed'),
                ('param.rs', 'before-timeline', OFFSET, 'equal-time events lost call order'),
            ]:
                old_path = (before_directory if file.startswith('nodes/') else before_directory.parent) / Path(file).name
                old = old_path.read_bytes()
                (work / (label + '.rs')).write_bytes(old)
                try:
                    paths[file].write_bytes(old)
                    binary = product.src.build(package, work, env, label, False)
                    red.append(capacity.run(binary, package, env, work, label, test, 101, assertion))
                    record(label, binary, True)
                finally:
                    paths[file].write_text(originals[file])
        for name, file, before, after, test, assertion in rows:
            try:
                paths[file].write_text(originals[file].replace(before, after))
                binary = product.src.build(package, work, env, name, False)
                item = capacity.run(binary, package, env, work, name, test, 101, assertion)
                item['binary_sha256'] = record(name, binary, True)
                controls.append(item)
                (work / 'completed-controls.json').write_text(json.dumps(controls, indent=2) + '\n')
                print('Actual source automation fault rejected', name, flush=True)
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
    print('Source a-rate automation verified', len(controls), 'faults,', len(red), 'old-code red checks,', len(suites), 'full suites', flush=True)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--target-dir', type=Path, required=True)
    parser.add_argument('--before-directory', type=Path)
    parser.add_argument('--preflight', action='store_true')
    args = parser.parse_args()
    main(args.output.resolve(), args.target_dir.resolve(), args.before_directory, args.preflight)
