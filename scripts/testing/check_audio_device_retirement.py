#!/usr/bin/env python3
"""Keep device destruction on the existing CPU executor after host close.

Queue rejection retains the real owner and physical credit while the single
streaming worker remains available. A cleanup burst must leave ordinary Audio
queue admission and a heavy worker available to live requests. Every fault must build and execute one
business assertion; compilation failures and timeouts never count as proof.
"""
from __future__ import annotations
import argparse
import json
from pathlib import Path
import shutil
import check_audio_cpu_admission as io
import check_audio_whole_decode as audio
import check_audio_device_resources as resources

ROOT = io.ROOT
IO_PREFIX = 'audio_cpu::retirement_tests::'
AUDIO_PREFIX = 'device_prepare::pull::session::resources::tests::'


def io_declarations(original, pools):
    rows = []
    def change(name, before, after, test, assertion):
        assert original.count(before) == 1, (name, 'mutation anchor drift')
        rows.append((name, 'audio_cpu.rs', before, after, IO_PREFIX + test, assertion))
    identity = 'retirement_after_close_uses_the_original_worker_without_reviving_host_work'
    saturation = 'saturated_retirement_keeps_the_value_and_retries_without_inline_destruction'
    start = original.index('    pub fn retirement(&self) -> AudioRetirement {')
    end = original.index('    /// Check the same close', start)
    capability = original[start:end]
    for name, before, after, assertion in [
        ('retirement-cancelled-domain', 'Arc::new(IoDomain::new())', 'self.domain.clone()', 'retirement inherited host closure'),
        ('retirement-foreground', 'PriorityClass::Background', 'PriorityClass::ForegroundAsync', 'retirement advertised foreground refill'),
        ('retirement-new-executor', 'self.pools.clone()', 'IoPools::new(0)', 'retirement left the original CPU worker'),
    ]:
        assert capability.count(before) == 1
        change(name, capability, capability.replace(before, after), identity, assertion)
    start = original.index("    pub async fn retire<T: Send + 'static>(self, value: T) {")
    end = original.index('\n}\n\n// Execution', start)
    method = original[start:end]
    signature = method.split('\n', 1)[0]
    change('retirement-inline', method, signature + '\n        drop(value);\n    }', identity,
           'retirement left the original CPU worker')
    change('retirement-captures-heavy-owner-in-rejected-job', method,
           signature + '\n        let _ = self.cpu.run(move || drop(value)).await;\n    }', saturation,
           'rejected retirement destroyed the queued owner inline')
    change('retirement-burst-admission-bypassed',
           '        let _permit = self.cpu.pools.audio_retirement_permit().await;', '',
           'a_retirement_burst_preserves_live_foreground_admission',
           'cleanup burst denied live foreground admission')
    before = 'audio_retirement: Arc::new(tokio::sync::Semaphore::new(1)),'
    assert pools.count(before) == 1
    rows.append(('retirement-concurrent-heavy-drops', 'pools.rs', before,
                 'audio_retirement: Arc::new(tokio::sync::Semaphore::new(2)),',
                 IO_PREFIX + 'running_retirement_keeps_a_cpu_slot_for_live_work',
                 'running cleanup occupied every audio CPU slot'))
    global_gate = '''audio_retirement: {
                    static GATE: std::sync::OnceLock<Arc<tokio::sync::Semaphore>> = std::sync::OnceLock::new();
                    Arc::clone(GATE.get_or_init(|| Arc::new(tokio::sync::Semaphore::new(1))))
                },'''
    rows.append(('retirement-gate-shared-between-executors', 'pools.rs', before, global_gate,
                 IO_PREFIX + 'held_retirement_does_not_block_an_independent_executor',
                 'cleanup on one executor blocked an independent executor'))
    assert len(rows) == 8
    return rows


def audio_declarations(originals):
    before = originals['device_resources.rs']
    old = before.replace('let retirement = self.cpu.retirement();', 'let retirement = self.cpu.clone();')
    old = old.replace('retirement.retire(retired).await;', 'let _ = retirement.run(move || drop(retired)).await;')
    assert old != before
    rows = [('retirement-shipping-host-cpu-rejected', 'device_resources.rs', before, old,
             AUDIO_PREFIX + 'closed_host_retirement_keeps_the_streaming_worker_available',
             'closed-host destruction occupied the shared streaming worker')]
    retained = resources.declarations(originals)
    rows += [row for row in retained if row[0] in [
        'resources-retirement-credit-returned-at-handoff', 'resources-handoff-allocates']]
    fixture = originals['device_resources_tests.rs']
    test = AUDIO_PREFIX + 'a_closed_four_host_burst_destroys_all_32_owners_on_cpu_workers'
    before = '.run(|| crate::decoder::decode(MP3))'
    assert fixture.count(before) == 1
    rows.append(('retirement-live-codec-pcm-muted', 'device_resources_tests.rs', before,
                 '.run(|| { let mut audio = crate::decoder::decode(MP3)?; audio.samples.fill(0.0); Ok::<_, EngineError>(audio) })',
                 test, 'live codec PCM changed during retirement'))
    start = fixture.index('let _ = io_sent.send(')
    end = fixture.index('\n            },', start)
    before = fixture[start:end]
    assert fixture.count(before) == 1
    rows.append(('retirement-live-io-bytes-empty', 'device_resources_tests.rs', before,
                 'let _ = io_sent.send(Ok::<Vec<u8>, std::io::Error>(Vec::new()));',
                 test, 'live filesystem bytes changed during retirement'))
    assert len(rows) == 5
    return rows


def io_build(package, work, env, name, release):
    # A copied lib must be dirty even when restored source matches an earlier
    # mutation's mtime. Require Cargo's actual compiler artifact fresh=false.
    lib = package / 'src/lib.rs'
    lib.write_bytes(lib.read_bytes())
    assert io.run(['cargo', 'clean', '--offline', '--locked', '-p', 'migo-io'],
                  work / (name + '-clean.log'), package, env) == 0
    command = ['cargo', 'test', '--lib', '--offline', '--locked', '--no-default-features',
               '--no-run', '--message-format=json']
    if release:
        command.append('--release')
    log = work / (name + '-build.jsonl')
    assert io.run(command, log, package, env, timeout=300) == 0, ('compile failed', log)
    artifacts = []
    for line in log.read_text().splitlines():
        try:
            row = json.loads(line)
        except ValueError:
            continue
        if row.get('reason') == 'compiler-artifact' and row.get('executable') and row['target']['name'] == 'migo_io':
            assert row['fresh'] is False
            artifacts.append(Path(row['executable']))
    assert len(artifacts) == 1
    binary = work / (name + '-tests')
    shutil.copy2(artifacts[0], binary)
    return binary


def main(work, target, scope, preflight, controls_only):
    if scope == 'io':
        package, env, frozen = io.prepare(work, target, [Path(__file__), ROOT / 'scripts/testing/check_gitignore_contract.py'])
        build = io_build
        prefix, narrow, expected = IO_PREFIX, 7, 417
    else:
        package, env, frozen = audio.whole.support.clock.prepare(work, target, audio.whole.extra_inputs())
        build = audio.src.build
        prefix, narrow, expected = AUDIO_PREFIX, 14, 567
    files = {p.name: p for p in (package / 'src').glob('*.rs')}
    originals = {name: p.read_text() for name, p in files.items()}
    rows = io_declarations(originals['audio_cpu.rs'], originals['pools.rs']) if scope == 'io' else audio_declarations(originals)
    programs, controls, profiles = [], [], {}
    def record(name, binary, prune=False):
        programs.append(dict(name=name, path=str(binary), sha256=io.sha(binary), pruned=prune))
        if prune:
            binary.unlink()
    for profile in ([] if controls_only else ['release'] if preflight else ['release', 'debug']):
        name = 'positive-' + profile
        binary = build(package, work, env, name, profile == 'release')
        log = work / (name + '-tests.log')
        assert io.run([str(binary), *([prefix] if preflight else []), '--nocapture'], log, package, env, timeout=900) == 0, log
        count = narrow if preflight else expected
        ignored = 0 if preflight else 9 if scope == 'io' else 2
        assert f'test result: ok. {count} passed; 0 failed; {ignored} ignored;' in log.read_text(), log
        assert io.run([str(binary), '--list'], work / (name + '-inventory.log'), package, env) == 0
        profiles[profile] = dict(passed=count, ignored=ignored)
        record(name, binary)
    if not preflight:
        for name, file, before, after, test, assertion in rows:
            try:
                files[file].write_text(originals[file].replace(before, after))
                binary = build(package, work, env, name, True)
                log = work / (name + '-test.log')
                status = io.run([str(binary), '--exact', test, '--nocapture'], log, package, env, timeout=120)
                text = log.read_text()
                assert status == 101 and all(s in text for s in ['running 1 test', test, 'panicked at', '1 failed', assertion]), (name, status, log)
                controls.append(dict(name=name, test=test, expected_assertion=assertion, exit=status, binary_sha256=io.sha(binary)))
                record(name, binary, True)
                (work / 'completed-controls.json').write_text(json.dumps(controls, indent=2) + '\n')
                print('Actual retirement fault rejected:', name, flush=True)
            finally:
                files[file].write_text(originals[file])
    for name, digest in frozen.items():
        assert io.sha(ROOT / name) == digest and io.sha(work / 'source-inputs' / name) == digest, name
    source = ROOT / 'engine/crates' / ('io' if scope == 'io' else 'audio') / 'src'
    assert all(p.read_bytes() == (package / 'src' / p.relative_to(source)).read_bytes() for p in source.rglob('*') if p.is_file())
    (work / 'summary.json').write_text(json.dumps(dict(scope=scope, profiles=profiles, programs=programs,
        controls=controls, compiled_controls=len(controls), preflight=preflight, controls_only=controls_only,
        root_inputs_unchanged=True, source_copies_restored=True, device_or_shipping_acceptance=False), indent=2) + '\n')
    print('Device retirement verified:', scope, profiles, len(controls), flush=True)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--target-dir', type=Path, required=True)
    parser.add_argument('--scope', choices=['audio', 'io'], required=True)
    parser.add_argument('--preflight', action='store_true')
    parser.add_argument('--controls-only', action='store_true')
    args = parser.parse_args()
    assert not (args.preflight and args.controls_only)
    main(args.output.resolve(), args.target_dir.resolve(), args.scope, args.preflight, args.controls_only)
