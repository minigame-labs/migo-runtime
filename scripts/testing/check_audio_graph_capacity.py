#!/usr/bin/env python3
"""Warm allocation tests missed first rendering and growth beyond warmed sizes.

Exercise the existing allocation contracts without prewarming their new cold
graphs, compare an optional old implementation with the same tests, then prove
node/edge growth and mix-buffer faults through fresh executed Rust assertions.
"""
from __future__ import annotations
import argparse
import json
import os
from pathlib import Path
import re
import check_audio_whole_decode as product
import audio_debug_profile as profile

ROOT = product.whole.ROOT
TESTS = [
    'context::steady_state_allocation::steady_state_audio_quantum_never_reaches_the_heap',
    'context::steady_state_allocation::a_graph_change_on_the_render_path_never_reaches_the_heap',
]


def prepare(work, target):
    whole = product.whole
    extra = [Path(__file__), *profile.inputs(ROOT), ROOT / 'engine/.cargo/config.toml']
    extra += [p for p in (ROOT / 'engine/crates/frame-wire').rglob('*') if p.is_file()]
    package, env, frozen = whole.support.clock.prepare(work, target, whole.extra_inputs() + extra)
    env['CARGO_INCREMENTAL'] = '0'
    for key in ['RUSTFLAGS', 'CARGO_ENCODED_RUSTFLAGS', 'CARGO_BUILD_TARGET']:
        env.pop(key, None)
    profile.apply(ROOT, work, package, env, whole.support.clock.common)
    return package, env, frozen


def original_with_new_tests(before):
    replacements = [
        ('#[cfg(test)]\nmod tests {', '#[cfg(test)]\n#[path = "context_capacity_tests.rs"]\nmod capacity_tests;\n\n#[cfg(test)]\nmod tests {'),
        ('    fn steady_state_audio_quantum_never_reaches_the_heap() {',
         '    fn steady_state_audio_quantum_never_reaches_the_heap() {\n        super::capacity_tests::cold_graph_and_collection();\n        super::capacity_tests::growth_after_first_render();'),
        ('    fn a_graph_change_on_the_render_path_never_reaches_the_heap() {',
         '    fn a_graph_change_on_the_render_path_never_reaches_the_heap() {\n        super::capacity_tests::edge_growth_without_new_nodes();'),
    ]
    for old, new in replacements:
        assert before.count(old) == 1
        before = before.replace(old, new)
    return before, replacements


def run(binary, package, env, work, label, test=None, expected=0, assertion=None):
    log = work / (label + '.log')
    command = [str(binary)] + (['--exact', test, '--nocapture'] if test else [])
    code = product.whole.support.clock.common.run(command, log, package, env, timeout=900 if test is None else 120)
    (work / (label + '-exit.txt')).write_text(str(code) + '\n')
    text = log.read_text()
    assert code == expected, (label, code, log)
    if test:
        assert 'running 1 test' in text
        markers = ['panicked at', '1 failed', assertion] if expected == 101 else ['1 passed; 0 failed;']
        assert all(marker in text for marker in markers), log
    else:
        match = re.search(r'test result: ok\. (\d+) passed; 0 failed; (\d+) ignored;', text)
        assert match and int(match[1]) > 0, log
    return dict(label=label, test=test, exit=code, expected_assertion=assertion)


def main(work, target, before_source=None, preflight=False):
    package, env, frozen = prepare(work, target)
    source = package / 'src/context.rs'
    original = source.read_text()
    controls = [
        ('nodes-only-provisioned-before-first-render', 'self.reserve_graph_nodes();',
         'if self.dense_ids.is_empty() { self.reserve_graph_nodes(); }', TESTS[0]),
        ('edges-only-provisioned-before-first-render', 'self.reserve_graph_edges();',
         'if self.edge_src.is_empty() { self.reserve_graph_edges(); }', TESTS[1]),
        ('mix-buffer-lazy-first-use', 'mix_buffer: vec![0.0; DEVICE_QUANTUM_FRAMES * channels.max(1) as usize],',
         'mix_buffer: Vec::new(),', TESTS[0]),
    ]
    for name, old, new, _ in controls:
        assert old != new and original.count(old) == 1, (name, 'anchor drift')
    programs, red, negative, suites = [], [], [], {}
    def record(name, binary, pruned):
        digest = product.whole.support.codecs.sha(binary)
        programs.append(dict(name=name, path=str(binary), sha256=digest, pruned=pruned))
        if pruned: binary.unlink()
        return digest
    positive = product.src.build(package, work, env, 'positive-debug', False)
    profile.verify(work / 'positive-debug-build.jsonl', json.loads((work / 'debug-profile-policy.json').read_text()), work / 'debug-profile-artifacts.json')
    record('positive-debug', positive, False)
    for n, test in enumerate(TESTS): run(positive, package, env, work, f'positive-targeted-{n}', test)
    assert product.whole.support.clock.common.run(['cargo', 'clippy', '--lib', '--tests', '--offline', '--locked', '--message-format=json'], work / 'clippy.jsonl', package, env, timeout=300) == 0
    if not preflight:
        if before_source:
            old = before_source.read_text()
            (work / 'before-context.rs').write_text(old)
            previous, replacements = original_with_new_tests(old)
            (work / 'before-test-rewrites.json').write_text(json.dumps(replacements, indent=2) + '\n')
            try:
                source.write_text(previous)
                binary = product.src.build(package, work, env, 'before-with-same-tests', False)
                for n, test in enumerate(TESTS):
                    red.append(run(binary, package, env, work, f'before-red-{n}', test, 101, 'cold context render allocated'))
                record('before-with-same-tests', binary, True)
            finally: source.write_text(original)
        for name, old, new, test in controls:
            try:
                source.write_text(original.replace(old, new))
                binary = product.src.build(package, work, env, name, False)
                item = run(binary, package, env, work, name, test, 101, 'cold context render allocated')
                item['binary_sha256'] = record(name, binary, True)
                negative.append(item)
                (work / 'completed-controls.json').write_text(json.dumps(negative, indent=2) + '\n')
                print('Actual graph capacity fault rejected', name, flush=True)
            finally: source.write_text(original)
        suites['debug'] = run(positive, package, env, work, 'full-debug')
        release = product.src.build(package, work, env, 'positive-release', True)
        record('positive-release', release, False)
        suites['isolated-release'] = run(release, package, env, work, 'full-release')
    product.whole.verify_inputs(work, frozen)
    root = ROOT / 'engine/crates/audio/src'
    assert all((package / 'src' / p.relative_to(root)).read_bytes() == p.read_bytes() for p in root.rglob('*') if p.is_file())
    result = dict(programs=programs, controls=negative, original_red=red, suites=suites, root_inputs_unchanged=True, source_copies_restored=True, root_dev_profile_verified=True, isolated_release_is_not_root_shipping=True, preflight=preflight)
    (work / 'summary.json').write_text(json.dumps(result, indent=2) + '\n')
    print('Graph scratch capacity verified', len(negative), 'faults,', len(red), 'old-code red checks,', len(suites), 'full suites', flush=True)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--target-dir', type=Path, required=True)
    parser.add_argument('--before-source', type=Path)
    parser.add_argument('--preflight', action='store_true')
    args = parser.parse_args()
    main(args.output.resolve(), args.target_dir.resolve(), args.before_source, args.preflight)
