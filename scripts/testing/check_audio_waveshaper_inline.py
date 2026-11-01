#!/usr/bin/env python3
"""Remove WaveShaper fixed tap/scratch heaps and retain the full old kernel.

Retain preparation, history, partial native blocks and tail/DC semantics. Only a fresh
executable reaching the named business assertion counts as a rejected fault.
"""
from __future__ import annotations
import argparse
import json
from pathlib import Path
import re
import check_audio_command_ownership as base
import check_audio_waveshaper_capacity as legacy

ROOT=base.ROOT
PREFIX='nodes::wave_shaper::inline_tests::'


def declarations(originals):
    rows=[(n,k,a,z,legacy.TEST,message)for n,k,a,z,message in legacy.declarations(originals)]
    coefficients='coefficients_and_mode_setter_are_exact_without_heap'
    pcm='modes_and_partial_frames_match_frozen_full_block_kernel'
    long='long_blocks_and_changing_channels_keep_history_without_scratch_heap'
    node='nodes/wave_shaper.rs'
    rows += [
        ('FIR-normalization-lost',node,'*tap = *tap / dc * gain;','*tap *= gain;',coefficients,'FIR coefficient bits changed'),
        ('FIR-interpolation-gain-lost',node,'design_lowpass(cutoff, factor as f64)','design_lowpass(cutoff, 1.0)',coefficients,'FIR coefficient bits changed'),
        ('chunk-restarts-history',node,'                let count = (frames - offset).min(crate::audio_thread::RENDER_QUANTUM_FRAMES);','                self.up_state[channel] = FirState::default();\n                let count = (frames - offset).min(crate::audio_thread::RENDER_QUANTUM_FRAMES);',pcm,'bounded waveshaper PCM or history changed'),
        ('chunk-output-offset-lost',node,'output[(offset + frame) * ch + channel] = kept;','output[frame * ch + channel] = kept;',long,'bounded waveshaper PCM or history changed'),
        ('chunk-shaping-span-lost',node,'scratch[..count * factor].iter_mut()','scratch[..factor].iter_mut()',pcm,'bounded waveshaper PCM or history changed'),
        ('decimation-phase-changed',node,'if phase == 0 {\n                            kept = filtered;','if phase == 1 {\n                            kept = filtered;',pcm,'bounded waveshaper PCM or history changed'),
        ('empty-curve-silenced',node,'if curve.is_empty() {\n            return input;','if curve.is_empty() {\n            return 0.0;',pcm,'bounded waveshaper PCM or history changed'),
    ]
    for name,key,a,z,_,_ in rows:assert a!=z and originals[key].count(a)==1,(name,'anchor drift')
    return rows


def main(work: Path, target: Path, before_directory: Path, preflight=False):
    package, env, frozen = base.prepare(work, target)
    product = base.capacity.product
    common = product.whole.support.clock.common
    paths={name:package/'src'/name for name in ['nodes/wave_shaper.rs','context.rs']}
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
        key='nodes/wave_shaper.rs'
        try:
            hook='\n#[cfg(test)]\n#[path = "wave_shaper_inline_tests.rs"]\nmod inline_tests;\n'
            paths[key].write_text((before_directory/'engine/crates/audio/src'/key).read_text()+hook)
            binary=build('before-node')
            for label,test,message in [('before-coefficients','coefficients_and_mode_setter_are_exact_without_heap','FIR coefficient design allocated'),('before-long-block','long_blocks_and_changing_channels_keep_history_without_scratch_heap','long waveshaper render allocated scratch')]:
                run(binary,label,PREFIX+test,101,message)
                red.append(dict(name=label,program='before-node',test=PREFIX+test,exit=101,expected_assertion=message,sha256=programs[-1]['sha256']))
            programs[-1]['pruned']=True;binary.unlink()
            print('Actual unchanged WaveShaper before source rejected at both heap assertions',flush=True)
        finally:paths[key].write_text(originals[key])

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
                print('Actual WaveShaper inline fault rejected', name, flush=True)
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
    print('WaveShaper inline taps/scratch verified', len(controls), 'actual source faults', flush=True)


if __name__ == '__main__':
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--output', type=Path, required=True)
    p.add_argument('--target-dir', type=Path, required=True)
    p.add_argument('--before-directory', type=Path, required=True)
    p.add_argument('--preflight', action='store_true')
    a = p.parse_args()
    main(a.output.resolve(), a.target_dir.resolve(), a.before_directory.resolve(), a.preflight)
