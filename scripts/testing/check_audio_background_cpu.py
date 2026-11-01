#!/usr/bin/env python3
"""Whole-file steps must yield to refill without bypassing Audio admission.

Use the existing typed executor, background aging and host rotation. Verify
actual worker order, both priorities' lifecycle/allocator contracts and the
complete MP3 handoff. Only rebuilt Rust runtime assertions reject faults.
"""
from __future__ import annotations

import argparse
import json
from pathlib import Path
import re
import tomllib

import check_audio_cpu_admission as admission
import check_audio_io_renewal as audio

ROOT = admission.ROOT


def declarations(originals):
    rows = []
    order = 'audio_cpu::background_tests::refill_overtakes_an_older_background_decode_on_the_same_audio_lane'
    aging = 'audio_cpu::background_tests::background_audio_ages_amid_a_sustained_foreground_io_backlog'
    quota = 'audio_cpu::background_tests::foreground_and_background_share_admission_and_release_rejected_captures'

    def region(name, path, start, end, before, after, test):
        text = originals[path]
        begin = text.index(start)
        finish = text.index(end, begin)
        original = text[begin:finish]
        assert original.count(before) == 1, (name, 'anchor drift')
        rows.append((name, path, original, original.replace(before, after), test))

    region('background-factory-promotes-preparation', 'scheduler.rs',
           '    pub fn background_audio_cpu(', '    pub fn domain(',
           'PriorityClass::Background', 'PriorityClass::ForegroundAsync', order)
    region('refill-factory-demotes-active-audio', 'scheduler.rs',
           '    pub fn audio_cpu(', '    /// Whole-file preparation',
           'PriorityClass::ForegroundAsync', 'PriorityClass::Background', order)
    region('background-handle-detaches-host-domain', 'scheduler.rs',
           '    pub fn background_audio_cpu(', '    pub fn domain(',
           'Arc::clone(&self.domain)', 'Arc::new(IoDomain::new())',
           'audio_cpu::tests::audio_handle_does_not_keep_scheduler_scope_open')
    rows.append(('audio-handle-priority-not-submitted', 'audio_cpu.rs',
                 '.submit_audio(self.priority, pending.0.clone())',
                 '.submit_audio(PriorityClass::ForegroundAsync, pending.0.clone())', order))
    region('typed-audio-pool-discards-background-priority', 'pools.rs',
           '    pub(crate) fn submit_audio(', '    pub fn run<T, F>',
           '            priority,', '            PriorityClass::ForegroundAsync,', order)
    region('typed-background-evades-audio-admission', 'pools.rs',
           '    pub(crate) fn submit_audio(', '    pub fn run<T, F>',
           'PoolKind::Audio', 'PoolKind::Fs', quota)
    region('background-aging-disabled', 'pools.rs', '    fn pop_with_fairness(',
           '    fn has_priority_work(', 'if aging_due {', 'if false {', aging)
    region('background-host-round-robin-disabled', 'pools.rs',
           '            if queue.jobs.is_empty() {', '    fn has_work(',
           'self.rotation.push_back(host);', 'self.rotation.push_front(host);',
           'audio_cpu::background_tests::background_audio_keeps_the_existing_host_round_robin')
    assert len(rows) == 8
    for name, path, before, _after, _test in rows:
        assert originals[path].count(before) == 1, name
    return rows


def program(work, name, prune=False):
    binary = work / (name + '-tests')
    artifacts = []
    for line in (work / (name + '-build.jsonl')).read_text().splitlines():
        try:
            row = json.loads(line)
        except ValueError:
            continue
        if row.get('reason') == 'compiler-artifact' and row.get('executable') and row.get('target', {}).get('name') == 'migo_io':
            artifacts.append(row)
    assert len(artifacts) == 1 and artifacts[0]['fresh'] is False
    result = {'name': name, 'path': str(binary), 'sha256': admission.sha(binary),
              'bytes': binary.stat().st_size, 'fresh': False, 'pruned_after_execution': prune}
    ledger = work / 'executed-programs.json'
    rows = json.loads(ledger.read_text()) if ledger.exists() else []
    assert all(row['name'] != name for row in rows)
    rows.append(result)
    ledger.write_text(json.dumps(rows, indent=2) + '\n')
    if prune:
        binary.unlink()
    return result


def extra_inputs():
    return list((ROOT / 'scripts/testing').glob('check_audio_*.py')) + [
        ROOT / 'scripts/testing/check_gitignore_contract.py',
        ROOT / 'engine/crates/shared/Cargo.toml',
        ROOT / 'engine/testing/alloc-probe/Cargo.toml',
        *list((ROOT / 'engine/testing/alloc-probe/src').rglob('*.rs')),
    ]


def io_main(work, target, positive_only):
    package, env, frozen = admission.prepare(work, target, extra_inputs())
    originals = {name: (package / 'src' / name).read_text()
                 for name in ['audio_cpu.rs', 'scheduler.rs', 'pools.rs']}
    rows = declarations(originals) + admission.declarations(originals)
    assert len(rows) == 22 and len({row[0] for row in rows}) == 22
    locked = tomllib.loads((ROOT / 'engine/Cargo.lock').read_text())['package']
    selected = tomllib.loads((package / 'Cargo.lock').read_text())['package']
    identity = lambda row: (row['name'], row['version'], row.get('source'), row.get('checksum'))
    assert set(map(identity, selected)) <= set(map(identity, locked))
    binary = admission.build(package, work, env, 'positive')
    log = work / 'positive-tests.log'
    assert admission.run([str(binary)], log, package, env) == 0
    match = re.search(r'test result: ok\. (\d+) passed; 0 failed; (\d+) ignored;', log.read_text())
    assert match
    program(work, 'positive')
    receipts = []
    for row in [] if positive_only else rows:
        receipt, = audio.public.faults(
            work, package, env, originals, [row],
            lambda name: admission.build(package, work, env, name), package / 'src')
        assert program(work, row[0], prune=True)['sha256'] == receipt['binary_sha256']
        receipts.append(receipt)
        (work / 'completed-controls.json').write_text(json.dumps(receipts, indent=2) + '\n')
    assert all(admission.sha(ROOT / p) == h and admission.sha(work / 'source-inputs' / p) == h
               for p, h in frozen.items())
    source = ROOT / 'engine/crates/io/src'
    assert all(p.read_bytes() == (package / 'src' / p.relative_to(source)).read_bytes()
               for p in source.rglob('*.rs'))
    summary = {'positive_tests': int(match[1]), 'ignored_tests': int(match[2]),
               'compiled_controls': len(receipts), 'new_controls': 0 if positive_only else 8,
               'retained_controls': 0 if positive_only else 14, 'controls': receipts,
               'root_inputs_unchanged': True, 'source_copies_restored': True,
               'root_lock_sha256': admission.sha(ROOT / 'engine/Cargo.lock'),
               'scope': 'Typed Audio background priority, shared admission/rotation/aging/lifecycle; '
                        'not production DecodePool migration or a codec/device/performance acceptance.'}
    (work / 'summary.json').write_text(json.dumps(summary, indent=2) + '\n')
    print('IO complete', summary['positive_tests'], len(receipts), flush=True)


def native_main(work, target):
    fixtures = ROOT / 'engine/crates/audio/src/decoder/mp3/fixtures'
    extra = extra_inputs() + list(fixtures.iterdir())
    package, env, frozen = audio.clock.prepare(work, target, extra)
    binary = audio.public.build_native(package, work, env, 'positive')
    log = work / 'positive-tests.log'
    assert audio.clock.common.run([str(binary)], log, package, env) == 0
    assert re.search(r'test result: ok\. 397 passed; 0 failed; 2 ignored;', log.read_text())
    audio.program(work, 'positive', 'native')
    assert all(admission.sha(ROOT / p) == h and admission.sha(work / 'source-inputs' / p) == h
               for p, h in frozen.items())
    source = ROOT / 'engine/crates/audio/src'
    assert all(p.read_bytes() == (package / 'src' / p.relative_to(source)).read_bytes()
               for p in source.rglob('*') if p.is_file())
    summary = {'positive_tests': 397, 'ignored_tests': 2, 'root_inputs_unchanged': True,
               'source_copies_restored': True, 'root_lock_sha256': admission.sha(ROOT / 'engine/Cargo.lock'),
               'scope': 'Complete native audio regression, actual MP3 codec/collect/DSP background handoff; '
                        'production DecodePool still synchronously drains on its original threads.'}
    (work / 'summary.json').write_text(json.dumps(summary, indent=2) + '\n')
    print('native complete 397', flush=True)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--scope', choices=['io', 'native'], required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--target-dir', type=Path, required=True)
    parser.add_argument('--positive-only', action='store_true')
    args = parser.parse_args()
    if args.scope == 'io':
        io_main(args.output.resolve(), args.target_dir.resolve(), args.positive_only)
    else:
        native_main(args.output.resolve(), args.target_dir.resolve())
