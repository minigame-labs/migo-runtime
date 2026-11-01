#!/usr/bin/env python3
"""Replace retained a-rate heap buffers with bounded original-clock stack chunks.

Retain gain, delay and source automation semantics. Only a fresh
executable reaching the named business assertion counts as a rejected fault.
"""
from __future__ import annotations
import argparse
import json
from pathlib import Path
import re
import check_audio_command_ownership as base
import check_audio_automation_capacity as gain_delay
import check_audio_source_automation as sources

ROOT = base.ROOT
PREFIX = 'inline_automation_tests::'


def declarations(originals):
    rows = [(n, 'nodes/'+k, a, z, gain_delay.TEST, message)
            for n, k, a, z, message in gain_delay.declarations({k.removeprefix('nodes/'):v for k,v in originals.items() if k.startswith('nodes/')} )]
    rows += sources.declarations(originals)
    timeline='offset_timeline_matches_independent_before_without_rebasing_clock'
    rows += [
        ('timeline-frame-offset-lost','param.rs','let time = clock.at(frame_offset + index);','let time = clock.at(index);',timeline,'chunked timeline changed original frame clock'),
        ('timeline-chunk-clock-rebased','param.rs','let mut block = self.values_block(start_time, sample_rate);\n        self.compute_values_from(&mut block, frame_offset, buffer);','let mut block = self.values_block(crate::render_time::RenderTime::new(start_time, sample_rate).at(frame_offset), sample_rate);\n        self.compute_values_from(&mut block, 0, buffer);',timeline,'chunked timeline changed original frame clock'),
        ('oscillator-phase-restarted-per-chunk','nodes/oscillator.rs','                if !self.phase_started {','                if true {','oscillator_matches_frozen_phase_for_all_shapes_and_automation_modes','chunked oscillator PCM or phase changed'),
    ]
    for name,key,before,after,_,_ in rows:
        assert before!=after and originals[key].count(before)==1,(name,'anchor drift')
    return rows


def main(work: Path, target: Path, before_directory: Path, preflight=False):
    package, env, frozen = base.prepare(work, target)
    product = base.capacity.product
    common = product.whole.support.clock.common
    paths = {name:package/'src'/name for name in ['nodes/gain.rs','nodes/delay.rs','nodes/constant_source.rs','nodes/oscillator.rs','param.rs']}
    originals = {key: path.read_text() for key, path in paths.items()}
    rows = declarations(originals)
    programs, controls, suites, red = [], [], {}, []

    def build(name, release=False):
        binary = product.src.build(package, work, env, name, release)
        programs.append(dict(name=name, path=str(binary), sha256=product.whole.support.codecs.sha(binary), pruned=False))
        return binary

    def run(binary, label, selector='', expected=0, assertion=None):
        cmd = [str(binary)] + (['--exact', selector] if selector else []) + ['--nocapture']
        code = common.run(cmd, work / (label + '.log'), package, env, timeout=300)
        (work / (label + '-exit.txt')).write_text(str(code) + '\n')
        text = (work / (label + '.log')).read_text()
        assert code == expected, (label, code)
        if expected:
            assert all(x in text for x in ['running 1 test', 'panicked at', '1 failed', assertion]), label
        else:
            match = re.search(r'test result: ok\. ([1-9][0-9]*) passed; 0 failed; (\d+) ignored;', text)
            assert match, label
            suites[label] = dict(passed=int(match[1]), ignored=int(match[2]))

    positive = build('positive-debug')
    base.capacity.profile.verify(work / 'positive-debug-build.jsonl', json.loads((work / 'debug-profile-policy.json').read_text()), work / 'debug-profile-artifacts.json')
    for test in sorted({row[4] for row in rows}):
        run(positive, 'positive-' + test.rsplit('::', 1)[-1], PREFIX + test if '::' not in test else test)
    assert common.run(['cargo', 'clippy', '--lib', '--tests', '--offline', '--locked', '--message-format=json'], work / 'clippy.jsonl', package, env, timeout=300) == 0
    if not preflight:
        run(positive, 'full-debug')
        for key,message in [('nodes/gain.rs','gain command retained scratch heap'),('nodes/delay.rs','delay retained scratch beyond circular buffer'),('nodes/constant_source.rs','constant command retained scratch heap'),('nodes/oscillator.rs','oscillator command retained scratch heap')]:
            name='before-'+Path(key).stem
            try:
                paths[key].write_bytes((before_directory/'engine/crates/audio/src'/key).read_bytes())
                binary=build(name)
                run(binary,name,PREFIX+'constructors_and_parameter_access_have_no_scratch_heap',101,message)
                red.append(dict(name=name,test=PREFIX+'constructors_and_parameter_access_have_no_scratch_heap',exit=101,expected_assertion=message,sha256=programs[-1]['sha256']))
                programs[-1]['pruned']=True;binary.unlink()
                print('Actual before automation scratch rejected',name,flush=True)
            finally: paths[key].write_text(originals[key])

        for name, key, before, after, test, assertion in rows:
            try:
                paths[key].write_text(originals[key].replace(before, after))
                binary = build(name)
                selector = PREFIX + test if '::' not in test else test
                run(binary, name, selector, 101, assertion)
                controls.append(dict(name=name, test=selector, exit=101, expected_assertion=assertion, sha256=programs[-1]['sha256']))
                programs[-1]['pruned'] = True
                binary.unlink()
                (work / 'completed-controls.json').write_text(json.dumps(controls, indent=2) + '\n')
                print('Actual bounded automation fault rejected', name, flush=True)
            finally:
                paths[key].write_text(originals[key])
        release = build('positive-release', True)
        run(release, 'full-release')
    product.whole.verify_inputs(work, frozen)
    assert all(path.read_text() == originals[key] for key, path in paths.items())
    for dep in ['shared', 'io', 'services', 'frame-wire']:
        root = ROOT / 'engine/crates' / dep / 'src'
        assert all((package / 'local-deps' / dep / p.relative_to(root.parent)).read_bytes() == p.read_bytes() for p in root.rglob('*') if p.is_file())
    assert all((package / 'src' / p.relative_to(ROOT / 'engine/crates/audio/src')).read_bytes() == p.read_bytes() for p in (ROOT / 'engine/crates/audio/src').rglob('*') if p.is_file())
    result = dict(programs=programs, controls=controls, original_red=red, suites=suites, root_inputs_unchanged=True, source_copies_restored=True, root_dev_profile_verified=True, isolated_release_is_not_root_shipping=True, preflight=preflight)
    (work / 'summary.json').write_text(json.dumps(result, indent=2) + '\n')
    print('Bounded stack automation verified', len(controls), 'actual source faults', flush=True)


if __name__ == '__main__':
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--output', type=Path, required=True)
    p.add_argument('--target-dir', type=Path, required=True)
    p.add_argument('--before-directory', type=Path, required=True)
    p.add_argument('--preflight', action='store_true')
    a = p.parse_args()
    main(a.output.resolve(), a.target_dir.resolve(), a.before_directory.resolve(), a.preflight)
