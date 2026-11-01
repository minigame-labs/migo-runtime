#!/usr/bin/env python3
"""Check analyser snapshot admission, immutable capacity and actual CPU retirement.

Use the existing pinned native audio replica and original debug profile; fault
controls must rebuild and fail on a business assertion in an actual Rust test.
"""
from __future__ import annotations
import argparse
import json
import re
from pathlib import Path
import check_audio_command_ownership as base

ROOT = base.ROOT
TEST = 'audio_thread::analyser_output::tests::'


def main(work: Path, target: Path):
    package, env, frozen = base.prepare(work, target)
    product = base.capacity.product
    common = product.whole.support.clock.common
    source = package / 'src/audio_thread_analyser_output.rs'
    original = source.read_text()
    programs, controls = [], []

    def build(name, release=False):
        binary = product.src.build(package, work, env, name, release)
        programs.append(dict(name=name, path=str(binary), sha256=product.whole.support.codecs.sha(binary), pruned=False))
        return binary

    def run(binary, label, selector=TEST, expected=0, assertion=None):
        code = common.run([str(binary), selector, '--nocapture'], work/(label+'.log'), package, env, timeout=180)
        (work/(label+'-exit.txt')).write_text(str(code)+'\n')
        text = (work/(label+'.log')).read_text()
        assert code == expected, (label, code)
        if expected:
            assert all(x in text for x in ['panicked at', '1 failed', assertion]), label
        else:
            assert re.search(r'test result: ok\. [1-9][0-9]* passed; 0 failed;', text), label
        return code

    positive = build('positive-debug')
    base.capacity.profile.verify(work/'positive-debug-build.jsonl', json.loads((work/'debug-profile-policy.json').read_text()), work/'debug-profile-artifacts.json')
    run(positive, 'analyser-debug')
    run(positive, 'full-debug', '')
    getter = '''    let data = ctx
        .with_node_typed::<AnalyserNode, _>(node_id, getter)
        .expect("checked analyser disappeared");
'''
    admission = '''    let admission = ctx
        .response_retirement_host()
        .map(|host| Admission::new(host, bytes))
        .transpose()?;
'''
    rows = [
        ('missing-capacity', 'host.reserve_value_with_bytes::<Vec<T>>(bytes)', 'host.reserve_value_with_bytes::<Vec<T>>(0)', 'analyser output lacks physical capacity', 'all_analyser_shapes_keep_capacity_until_actual_storage_release'),
        ('early-release', 'AnalyserSamples::new(reservation.finish(data))', 'AnalyserSamples::new({ let reservation: crate::node_retirement::Reservation<Vec<T>> = reservation; drop(reservation); data })', 'analyser output lacks physical capacity', 'all_analyser_shapes_keep_capacity_until_actual_storage_release'),
        ('allocation-before-admission', admission+getter, getter+admission, 'analyser refusal allocated output before admission', 'analyser_refusal_precedes_allocation_and_preserves_smoothing_history'),
        ('inline-large', 'if bytes >= 4096 {', 'if bytes == 0 {', 'analyser credit returned before CPU storage release', 'all_analyser_shapes_keep_capacity_until_actual_storage_release'),
    ]
    for name, before, after, assertion, test in rows:
        assert original.count(before) == 1, (name, 'anchor drift')
        try:
            source.write_text(original.replace(before, after))
            binary = build(name)
            selector = TEST+test
            run(binary, name, selector, 101, assertion)
            text = (work/(name+'.log')).read_text()
            assert 'running 1 test' in text and '1 failed' in text
            controls.append(dict(name=name, test=selector, expected_assertion=assertion, exit=101, sha256=programs[-1]['sha256']))
            programs[-1]['pruned'] = True
            binary.unlink()
        finally:
            source.write_text(original)
    release = build('positive-release', True)
    run(release, 'analyser-release')
    run(release, 'full-release', '')
    for mode, is_release in [('debug', False), ('release', True)]:
        binary = base.build_shared(package, work, env, 'shared-'+mode, is_release)
        base.run_shared(binary, package, env, work, 'shared-'+mode+'-suite')
        programs.append(dict(name='shared-'+mode, path=str(binary), sha256=product.whole.support.codecs.sha(binary), pruned=True, target_name='shared'))
        binary.unlink()
    for package_name, args in [('audio', []), ('shared', ['-p', 'migo-shared', '--no-default-features', '--features', 'vfs,codec'])]:
        assert common.run(['cargo','clippy',*args,'--lib','--tests','--offline','--locked','--message-format=json'], work/(package_name+'-clippy.jsonl'), package, env, timeout=300) == 0
    product.whole.verify_inputs(work, frozen)
    assert source.read_text() == original
    result = dict(programs=programs, controls=controls, root_inputs_unchanged=True, source_copies_restored=True, root_dev_profile_verified=True)
    (work/'summary.json').write_text(json.dumps(result, indent=2)+'\n')
    print('Analyser native ownership verified', len(controls), 'actual source faults', flush=True)


if __name__ == '__main__':
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--output', type=Path, required=True)
    p.add_argument('--target-dir', type=Path, required=True)
    a = p.parse_args()
    main(a.output.resolve(), a.target_dir.resolve())
