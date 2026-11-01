#!/usr/bin/env python3
"""Scheduled source start/stop only took effect at quantum boundaries.

Check exact sample windows, fractional onset and terminal source state.
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


def main(work, target, before_directory=None, preflight=False):
    product = capacity.product
    package, env, frozen = capacity.prepare(work, target)
    assert 'scripts/testing/check_audio_source_scheduling.py' in frozen
    paths = {n: package / 'src' / n for n in ['nodes/constant_source.rs', 'nodes/oscillator.rs', 'nodes/buffer_source.rs', 'render_time.rs', 'param.rs']}
    originals = {n: p.read_text() for n, p in paths.items()}
    rows = [
        ('constant-window-ignored','nodes/constant_source.rs','clock.active(self.start_when, self.stop_when, frames)','0..frames',OFFSET,'constant scheduled PCM changed'),
        ('oscillator-window-ignored','nodes/oscillator.rs','clock.active(origin, self.stop_when, frames)','0..frames',OSCILLATOR,'oscillator scheduled PCM or initial phase changed'),
        ('buffer-window-ignored','nodes/buffer_source.rs','clock.active(self.start_when, stop, frames)','0..frames',OSCILLATOR,'buffer source scheduled PCM or fractional offset changed'),
        ('clock-origin-not-canonical','render_time.rs','let grid = frame / rate == time;','let grid = false;',OFFSET,'sample clock changed across block boundaries'),
        ('parameter-grid-not-canonical','param.rs','let time = clock.at(index);','let time = start_time + index as f64 * (1.0 / sample_rate.max(1) as f64);',OFFSET,'automation event missed its exact sample'),
        ('constant-stop-not-terminal','nodes/constant_source.rs','let stopped = clock.stopped(self.stop_when, frames);','let stopped = false;',OFFSET,'constant stop fence changed'),
        ('buffer-offset-truncated','nodes/buffer_source.rs','self.position = self.start_offset.max(0.0) * sample_rate;','self.position = (self.start_offset.max(0.0) * sample_rate).floor();',OSCILLATOR,'buffer source scheduled PCM or fractional offset changed'),
        ('oscillator-onset-rounded','nodes/oscillator.rs','(current_time - origin).max(0.0)','0.0_f64',OSCILLATOR,'oscillator scheduled PCM or initial phase changed'),
    ]
    for name, file, before, after, _, _ in rows:
        assert before != after and originals[file].count(before) == (2 if name=='oscillator-onset-rounded' else 1), (name, 'anchor drift')
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
        suites['debug'] = capacity.run(positive, package, env, work, 'full-debug')
        if before_directory:
            for file, label, test, assertion in [
                ('nodes/constant_source.rs','before-constant',OFFSET,'constant scheduled PCM changed'),
                ('nodes/oscillator.rs','before-oscillator',OSCILLATOR,'oscillator scheduled PCM or initial phase changed'),
                ('nodes/buffer_source.rs','before-buffer',OSCILLATOR,'buffer source scheduled PCM or fractional offset changed'),
                ('param.rs','before-timeline',OFFSET,'automation event missed its exact sample'),
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
                print('Actual source scheduling fault rejected', name, flush=True)
            finally:
                paths[file].write_text(originals[file])
        release = product.src.build(package, work, env, 'positive-release', True)
        record('positive-release', release)
        suites['isolated-release'] = capacity.run(release, package, env, work, 'full-release')
    product.whole.verify_inputs(work, frozen)
    root = ROOT / 'engine/crates/audio/src'
    assert all((package / 'src' / p.relative_to(root)).read_bytes() == p.read_bytes() for p in root.rglob('*') if p.is_file())
    result = dict(programs=programs, controls=controls, original_red=red, suites=suites, root_inputs_unchanged=True, source_copies_restored=True, root_dev_profile_verified=True, isolated_release_is_not_root_shipping=True, preflight=preflight)
    (work / 'summary.json').write_text(json.dumps(result, indent=2) + '\n')
    print('Source scheduling verified', len(controls), 'faults,', len(red), 'old-code red checks,', len(suites), 'full suites', flush=True)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--target-dir', type=Path, required=True)
    parser.add_argument('--before-directory', type=Path)
    parser.add_argument('--preflight', action='store_true')
    args = parser.parse_args()
    main(args.output.resolve(), args.target_dir.resolve(), args.before_directory, args.preflight)
