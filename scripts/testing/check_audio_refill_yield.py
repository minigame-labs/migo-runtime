#!/usr/bin/env python3
"""Larger audio turns saved CPU but increased foreground queue latency.

Verify a shared, cancellation-safe refill hint on the existing typed executor.
Only fresh executables' exact Rust assertion failures reject runtime faults.
"""
from __future__ import annotations
import argparse
import json
from pathlib import Path

import check_audio_background_cpu as background

ROOT = background.ROOT
common = background.admission


def declarations(originals):
    cancel = 'audio_cpu::tests::queued_refill_hint_is_shared_counted_and_cancelled_before_execution'
    retire = 'audio_cpu::tests::refill_hint_retires_before_execution_rejection_and_domain_close'
    pulse = 'audio_cpu::tests::completed_refills_leave_a_shared_generation_without_pending_work'
    rows = [
        ('refill-generation-not-incremented', 'pools.rs', 'activity.generation.fetch_add(1, Ordering::Release);',
         'let _ = &activity;', pulse),
        ('refill-generation-hidden', 'audio_cpu.rs', 'self.pools.audio_refill_generation()', '0', pulse),
        ('background-is-a-refill', 'pools.rs', 'if priority == PriorityClass::Background {', 'if false {', cancel),
        ('refill-hint-hidden', 'audio_cpu.rs', 'self.pools.audio_refill_pending()', 'false', cancel),
        ('cancelled-refill-hint-leaked', 'pools.rs', 'self.0.pending.fetch_sub(1, Ordering::AcqRel);', 'let _ = &self.0;', cancel),
        ('refill-cleared-after-execution', 'audio_cpu.rs', 'drop(refill);\n                    job()',
         'let result = job(); drop(refill); result', retire),
    ]
    for name, path, before, _after, _test in rows:
        assert originals[path].count(before) == 1, (name, 'anchor drift')
    return rows


def build(package, work, env, name, release=False):
    assert common.run(['cargo', 'clean', '--offline', '--locked', '-p', 'migo-io'], work / (name + '-clean.log'), package, env) == 0
    args = ['cargo', 'test', '--lib', '--no-default-features', '--offline', '--locked', '--no-run', '--message-format=json']
    if release:
        args += ['--release']
    log = work / (name + '-build.jsonl')
    assert common.run(args, log, package, env, timeout=300) == 0, log
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
    binary.write_bytes(artifacts[0].read_bytes())
    binary.chmod(0o755)
    return binary


def main(work, target):
    package, env, frozen = common.prepare(work, target, background.extra_inputs())
    originals = {name: (package / 'src' / name).read_text() for name in ['audio_cpu.rs', 'pools.rs', 'scheduler.rs']}
    profiles = {}
    for profile in ['release', 'debug']:
        name = 'positive-' + profile
        binary = build(package, work, env, name, profile == 'release')
        log = work / (name + '-tests.log')
        assert common.run([str(binary)], log, package, env, timeout=300) == 0, log
        assert 'test result: ok. 410 passed; 0 failed; 9 ignored;' in log.read_text(), log
        profiles[profile] = dict(passed=410, ignored=9)
        background.program(work, name)
    rows = declarations(originals) + background.declarations(originals) + common.declarations(originals)
    controls = []
    for name, path, before, after, test in rows:
        try:
            source = package / 'src' / path
            source.write_text(originals[path].replace(before, after))
            binary = build(package, work, env, name)
            log = work / (name + '-test.log')
            status = common.run([str(binary), '--exact', test, '--nocapture'], log, package, env, timeout=30)
            assert status == 101 and all(x in log.read_text() for x in ['running 1 test', 'panicked at', '1 failed']), (name, status, log)
            proof = background.program(work, name, prune=True)
            controls.append(dict(name=name, test=test, exit=status, binary_sha256=proof['sha256']))
            (work / 'completed-controls.json').write_text(json.dumps(controls, indent=2) + '\n')
            print('executed IO assertion rejected:', name, flush=True)
        finally:
            source.write_text(originals[path])
    assert len(controls) == 28
    assert all(common.sha(ROOT / name) == value == common.sha(work / 'source-inputs' / name) for name, value in frozen.items())
    root = ROOT / 'engine/crates/io/src'
    assert all(p.read_bytes() == (package / 'src' / p.relative_to(root)).read_bytes() for p in root.rglob('*.rs'))
    (work / 'summary.json').write_text(json.dumps(dict(profiles=profiles, controls=controls, compiled_controls=len(controls),
        root_inputs_unchanged=True, source_copies_restored=True, root_lock_sha256=common.sha(ROOT / 'engine/Cargo.lock'),
        scope='typed foreground hint lifetime and existing Audio admission/priority/aging/publication, host traces only'), indent=2) + '\n')
    print('refill hint IO verification passed', profiles, len(controls), flush=True)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--target-dir', type=Path, required=True)
    args = parser.parse_args()
    main(args.output, args.target_dir)
