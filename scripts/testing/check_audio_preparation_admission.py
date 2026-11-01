#!/usr/bin/env python3
"""A32-route cold burst exhausted ordinary Audio admission and rebuilt owners.

Prove per-executor preparation admission, live refill headroom and real owner
lifetime through close/cancellation. Mutation evidence must execute one exact
Rust test and reach its business assertion; build failures/timeouts do not count.
"""
from __future__ import annotations
import argparse
import json
from pathlib import Path
import check_audio_cpu_admission as io
import check_audio_device_retirement as retirement
import check_audio_whole_decode as audio
import audio_debug_profile as profile

ROOT = io.ROOT
IO_TEST = 'audio_cpu::tests::background_demotion_retains_the_same_host_and_close_notification'
AUDIO_TESTS = [
    'device_prepare::pull::session::resources::tests::a_closed_four_host_burst_destroys_all_32_owners_on_cpu_workers',
    'device_prepare::pull::session::resources::tests::aborted_running_preparation_holds_credit_until_its_actual_owner_dies',
]


def declarations(scope, originals):
    if scope == 'io':
        pools = originals['pools.rs']
        begin = pools.index('        let audio_preparation = ')
        end = pools.index('        Arc::new(Self {', begin)
        initialization = pools[begin:end]
        global_gate = '''        let audio_preparation = {
            static GATE: std::sync::OnceLock<Arc<tokio::sync::Semaphore>> = std::sync::OnceLock::new();
            Arc::clone(GATE.get_or_init(|| Arc::new(tokio::sync::Semaphore::new(config.class_caps[pool_index(PoolKind::Audio)]))))
        };
'''
        cpu = originals['audio_cpu.rs']
        begin = cpu.index('    pub async fn preparation_permit(')
        end = cpu.index('    /// Reserve drop-only', begin)
        method = cpu[begin:end]
        no_close = '''    pub async fn preparation_permit(&self) -> EngineResult<tokio::sync::OwnedSemaphorePermit> {
        Ok(self.pools.audio_preparation_permit().await)
    }

'''
        rows = [
            ('preparation-cap-exceeds-cpu', 'pools.rs', initialization, initialization.replace('config.class_caps[pool_index(PoolKind::Audio)]', 'config.worker_count'), IO_TEST, 'preparation admission exceeded existing CPU parallelism'),
            ('preparation-global-executor-gate', 'pools.rs', initialization, global_gate, IO_TEST, 'preparation on one executor blocked another executor'),
            ('preparation-wait-ignores-close', 'audio_cpu.rs', method, no_close, IO_TEST, 'preparation wait ignored host close'),
        ]
    else:
        rows = [
            ('preparation-burst-bypasses-admission', 'device_resources.rs', 'let preparation = Some(self.cpu.preparation_permit().await?);', 'let preparation = None;', AUDIO_TESTS[0], 'preparation burst rejected an admitted managed route'),
            ('preparation-permit-stays-with-async-future', 'device_resources.rs', '                preparation,', '                preparation: None,', AUDIO_TESTS[1], 'cancelled native preparation released admission early'),
            ('preparation-active-session-retains-permit', 'device_resources.rs', '        drop(lease.retired.as_mut().unwrap().preparation.take());', '', AUDIO_TESTS[1], 'completed preparation retained CPU admission'),
        ]
    for name, file, before, after, _, _ in rows:
        assert before != after and originals[file].count(before) == 1, (name, 'anchor drift')
    return rows


def prepare(scope, work, target):
    if scope == 'io':
        return io.prepare(work, target, [Path(__file__), Path(retirement.__file__), ROOT / 'scripts/testing/check_gitignore_contract.py'])
    extra = [Path(__file__), *profile.inputs(ROOT), ROOT / 'engine/.cargo/config.toml']
    extra += [p for p in (ROOT / 'engine/crates/frame-wire').rglob('*') if p.is_file()]
    package, env, frozen = audio.whole.support.clock.prepare(work, target, audio.whole.extra_inputs() + extra)
    env['CARGO_INCREMENTAL'] = '0'
    profile.apply(ROOT, work, package, env, io)
    return package, env, frozen


def main(scope, work, target, before_source=None, preflight=False):
    package, env, frozen = prepare(scope, work, target)
    env['CARGO_INCREMENTAL'] = '0'
    names = ['audio_cpu.rs', 'pools.rs'] if scope == 'io' else ['device_resources.rs']
    files = {n: package / 'src' / n for n in names}
    originals = {n: p.read_text() for n, p in files.items()}
    rows = declarations(scope, originals)
    programs, negatives, red, suites = [], [], [], {}
    def build(name, release=False):
        return retirement.io_build(package, work, env, name, release) if scope == 'io' else audio.src.build(package, work, env, name, release)
    def record(name, binary, pruned=False):
        digest = io.sha(binary)
        programs.append(dict(name=name, path=str(binary), sha256=digest, pruned=pruned))
        if pruned: binary.unlink()
        return digest
    def execute(binary, label, test=None, expected=0, assertion=None):
        log = work / (label + '.log')
        status = io.run([str(binary)] + (['--exact', test, '--nocapture'] if test else []), log, package, env, timeout=900 if not test else 120)
        (work / (label + '-exit.txt')).write_text(str(status) + '\n')
        text = log.read_text()
        assert status == expected, (label, status, log)
        if test:
            assert 'running 1 test' in text
            markers = ['panicked at', '1 failed', assertion] if expected == 101 else ['1 passed; 0 failed;']
            assert all(marker in text for marker in markers), log
        else:
            count, ignored = (417, 9) if scope == 'io' else (567, 2)
            assert f'test result: ok. {count} passed; 0 failed; {ignored} ignored;' in text, log
        return dict(label=label, test=test, exit=status, expected_assertion=assertion)
    positive = build('positive-debug')
    record('positive-debug', positive)
    if scope == 'audio':
        profile.verify(work / 'positive-debug-build.jsonl', json.loads((work / 'debug-profile-policy.json').read_text()), work / 'debug-profile-artifacts.json')
    for n, test in enumerate([IO_TEST] if scope == 'io' else AUDIO_TESTS): execute(positive, f'positive-targeted-{n}', test)
    clippy = ['cargo', 'clippy', '--lib', '--tests', '--offline', '--locked', '--message-format=json']
    if scope == 'io': clippy.append('--no-default-features')
    assert io.run(clippy, work / 'clippy.jsonl', package, env, timeout=300) == 0
    if not preflight:
        if before_source and scope == 'audio':
            old = before_source.read_text()
            (work / 'before-device-resources.rs').write_text(old)
            addition = '\n#[cfg(test)]\n#[path = "device_preparation_tests.rs"]\nmod preparation_tests;\n'
            try:
                files['device_resources.rs'].write_text(old + addition)
                binary = build('before-with-same-tests')
                for n, test in enumerate(AUDIO_TESTS):
                    assertion = 'preparation burst rejected an admitted managed route' if n == 0 else 'cancelled native preparation released admission early'
                    red.append(execute(binary, f'before-red-{n}', test, 101, assertion))
                record('before-with-same-tests', binary, True)
            finally: files['device_resources.rs'].write_text(originals['device_resources.rs'])
        for name, file, before, after, test, assertion in rows:
            try:
                files[file].write_text(originals[file].replace(before, after))
                binary = build(name)
                item = execute(binary, name, test, 101, assertion)
                item['binary_sha256'] = record(name, binary, True)
                negatives.append(item)
                (work / 'completed-controls.json').write_text(json.dumps(negatives, indent=2) + '\n')
                print('Actual preparation admission fault rejected', scope, name, flush=True)
            finally: files[file].write_text(originals[file])
        suites['debug'] = execute(positive, 'full-debug')
        release = build('positive-release', True)
        record('positive-release', release)
        suites['isolated-release'] = execute(release, 'full-release')
    inputs = {str(p.relative_to(ROOT)): io.sha(p) for p in [ROOT / n for n in frozen]}
    assert inputs == frozen and all(io.sha(work / 'source-inputs' / n) == h for n, h in frozen.items())
    root = ROOT / ('engine/crates/io/src' if scope == 'io' else 'engine/crates/audio/src')
    assert all((package / 'src' / p.relative_to(root)).read_bytes() == p.read_bytes() for p in root.rglob('*') if p.is_file())
    result = dict(scope=scope, programs=programs, controls=negatives, original_red=red, suites=suites, root_inputs_unchanged=True, source_copies_restored=True, preflight=preflight)
    (work / 'summary.json').write_text(json.dumps(result, indent=2) + '\n')
    print('Preparation admission verified', scope, len(negatives), 'faults', len(red), 'old-code reds', len(suites), 'full suites', flush=True)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--scope', choices=['io', 'audio'], required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--target-dir', type=Path, required=True)
    parser.add_argument('--before-source', type=Path)
    parser.add_argument('--preflight', action='store_true')
    args = parser.parse_args()
    main(args.scope, args.output.resolve(), args.target_dir.resolve(), args.before_source, args.preflight)
