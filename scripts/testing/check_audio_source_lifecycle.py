#!/usr/bin/env python3
"""Prove preparation cancellation and source snapshots reject real regressions.

The native ID guard cannot order asynchronous reads or filter already-dispatched
JS callbacks. Exercise the shared/service implementation in retained copies,
with the native and V8 adapters verified separately. Compilation
errors, timeouts and signals are never accepted runtime fault evidence.
"""
from __future__ import annotations

import argparse
import json
from pathlib import Path
import re
import shutil
import tomllib

import check_audio_cpu_admission as common
import check_pack_copy_budget as isolated

ROOT = common.ROOT


def build(package, output, env, label, crate, target):
    assert common.run(['cargo', 'clean', '--offline', '--locked', '-p', crate],
                      output / f'{label}-clean.log', package, env) == 0
    log = output / f'{label}-build.jsonl'
    assert common.run(['cargo', 'test', '-p', crate, '--lib', '--offline', '--locked',
                       '--no-run', '--message-format=json'], log, package, env, timeout=600) == 0, (
                           label, 'compilation failure', log)
    artifacts = []
    for line in log.read_text().splitlines():
        try:
            row = json.loads(line)
        except ValueError:
            continue
        if (row.get('reason') == 'compiler-artifact' and row.get('executable')
                and row.get('target', {}).get('name') == target):
            assert row.get('fresh') is False, 'test executable was not rebuilt'
            artifacts.append(Path(row['executable']))
    assert len(artifacts) == 1, artifacts
    binary = output / f'{label}-tests'
    shutil.copy2(artifacts[0], binary)
    return binary


def execute(binary, output, package, env, label, test=None):
    command = [str(binary)]
    if test:
        command += ['--exact', test, '--nocapture']
    log = output / f'{label}-test.log'
    status = common.run(command, log, package, env, timeout=120)
    observed = log.read_text()
    if test:
        assert status == 101 and 'panicked at' in observed and '1 failed' in observed, (
            label, status, 'must fail an executed assertion')
        assert 'Elapsed(())' not in observed, 'a timeout is not runtime red evidence'
        return {'name': label, 'test': test, 'exit': status, 'binary_sha256': common.sha(binary)}
    assert status == 0, log
    match = re.search(r'test result: ok\. (\d+) passed; 0 failed; (\d+) ignored;', observed)
    assert match, log
    return {'passed': int(match[1]), 'ignored': int(match[2]), 'binary_sha256': common.sha(binary)}


def primitive_controls(sources):
    rows = []

    def add(name, crate, before, after, test):
        assert sources[crate].count(before) == 1, (name, 'anchor drift')
        rows.append((name, crate, before, after, test))

    prefix = 'audio_load::tests::'
    add('source-tag-discarded', 'shared', '            source_id,', '            source_id: 0,',
        prefix + 'tokens_preserve_identity_and_source_tag_without_retaining_other_owners')
    add('owner-identity-ignored', 'shared',
        'std::ptr::addr_eq(Arc::as_ptr(&self.watch.state), Arc::as_ptr(owner)) && self.is_current()',
        'self.is_current()', prefix + 'tokens_preserve_identity_and_source_tag_without_retaining_other_owners')
    add('zero-generation-accepted', 'shared', 'self.generation != 0 && self.state.generation() == self.generation',
        'self.state.generation() == self.generation',
        prefix + 'tokens_preserve_identity_and_source_tag_without_retaining_other_owners')
    add('retired-preparation-polled', 'shared',
        'if !self.is_current() || changed.as_mut().poll(cx).is_ready() {',
        'if changed.as_mut().poll(cx).is_ready() {',
        prefix + 'a_retired_operation_is_never_polled_even_when_ready')
    add('cancellation-waker-not-registered', 'shared',
        'changed.as_mut().poll(cx).is_ready()', 'false',
        prefix + 'a_changed_owner_wakes_and_drops_pending_preparation_without_repolling_it')
    add('ready-result-published-after-cancellation', 'shared',
        '                if self.is_current() {', '                if true {',
        prefix + 'cancellation_during_a_ready_step_drops_its_unpublished_result')
    add('watch-observation-allocates', 'shared',
        '    pub fn watch(&self) -> AudioLoadWatch {',
        '    pub fn watch(&self) -> AudioLoadWatch {\n        std::hint::black_box(vec![0_u8; 64]);',
        prefix + 'source_observation_and_ready_waits_have_no_steady_heap_activity')
    add('completion-token-allocates', 'shared',
        '        Self {\n            watch: AudioLoadWatch {',
        '        std::hint::black_box(vec![0_u8; 64]);\n        Self {\n            watch: AudioLoadWatch {',
        prefix + 'source_observation_and_ready_waits_have_no_steady_heap_activity')
    source = sources['services']
    start = source.index('    let begin = request(&tx, |resp| AudioCmd::InnerAudioBeginLoad {')
    end = source.index('    Ok(async move {', start) + len('    Ok(async move {')
    before = source[start:end]
    after = '    Ok(async move {\n' + before.split('    Ok(async move {')[0]
    add('begin-boundary-deferred-until-poll', 'services', before, after,
        'audio::tests::source_begin_is_synchronous_and_old_preparation_is_not_started')
    start = source.index('                    AudioCmd::InnerAudioCompleteLoad {')
    end = source.index('                    },', start) + len('                    },')
    before = source[start:end]
    after = before.replace('InnerAudioCompleteLoad', 'InnerAudioLoad').replace('                        load,\n', '')
    add('local-completion-restarts-source', 'services', before, after,
        'audio::tests::a_packaged_sound_is_read_through_the_sandbox_and_loaded')
    start = source.index('        watch\n            .while_current(async move {')
    end = source.index('\n    })\n}', start)
    before = source[start:end]
    after = before.replace('        watch\n            .while_current(async move {', '        (async move {').replace(
        '            .await\n            .map_err(engine_error)?', '            .await')
    add('preparation-does-not-observe-cancellation', 'services', before, after,
        'audio::tests::source_begin_is_synchronous_and_old_preparation_is_not_started')
    return rows


def primitives(output, target):
    env, frozen, copies = isolated.prepare(output, target)
    fixture = ROOT / 'platforms/apple/WebContent/PerformancePlus/test/fixtures/audio-check-answers.json'
    for path in [Path(__file__).resolve(), ROOT / 'scripts/testing/check_gitignore_contract.py', fixture]:
        relative = str(path.relative_to(ROOT)); frozen[relative] = common.sha(path)
        dest = output / 'source-inputs' / relative; dest.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(path, dest)
    destination = output / fixture.relative_to(ROOT)
    destination.parent.mkdir(parents=True, exist_ok=True); shutil.copy2(fixture, destination)
    (output / 'source-inputs.json').write_text(json.dumps(frozen, indent=2) + '\n')
    paths = {'shared': output / 'engine/crates/shared/src/audio_load.rs',
             'services': output / 'engine/crates/services/src/audio.rs'}
    originals = {name: path.read_text() for name, path in paths.items()}
    rows = primitive_controls(originals)
    positives = {}
    for crate in ['shared', 'services']:
        package = output / 'engine/crates' / crate
        assert common.run(['cargo', 'metadata', '--offline', '--format-version', '1'],
                          output / f'{crate}-metadata.jsonl', package, env) == 0
        keys = lambda rows: {(row['name'], row['version'], row.get('source'), row.get('checksum')) for row in rows}
        assert keys(tomllib.loads((package / 'Cargo.lock').read_text())['package']) <= keys(
            tomllib.loads((ROOT / 'engine/Cargo.lock').read_text())['package']), 'dependency source/version drift'
        assert common.run(['cargo', 'check', '--lib', '--offline', '--locked'],
                          output / f'{crate}-normal-check.log', package, env) == 0
        binary = build(package, output, env, 'positive-' + crate, 'migo-' + crate, 'shared' if crate == 'shared' else 'migo_services')
        positives[crate] = execute(binary, output, package, env, 'positive-' + crate)
    available = {crate: set(re.findall(r'^test (\S+) \.\.\.',
                 (output / f'positive-{crate}-test.log').read_text(), re.MULTILINE))
                 for crate in ['shared', 'services']}
    assert all(test in available[crate] for _, crate, _, _, test in rows), 'fault target absent from positive suite'
    receipts = []
    for name, crate, before, after, test in rows:
        package = output / 'engine/crates' / crate
        try:
            paths[crate].write_text(originals[crate].replace(before, after))
            binary = build(package, output, env, name, 'migo-' + crate, 'shared' if crate == 'shared' else 'migo_services')
            receipts.append(execute(binary, output, package, env, name, test))
        finally:
            paths[crate].write_text(originals[crate])
        print(f'compiled source lifecycle control {name}: runtime assertion rejected', flush=True)
    assert all(common.sha(ROOT / path) == value for path, value in frozen.items()), 'Root changed'
    assert all(common.sha(output / path) == value for path, value in copies.items()), 'copy not restored'
    summary = {'positives': positives, 'compiled_controls': len(receipts), 'controls': receipts,
               'root_inputs_unchanged': True, 'source_copies_restored': True,
               'existing_dependency_versions_preserved': True,
               'root_lock_sha256': common.sha(ROOT / 'engine/Cargo.lock')}
    (output / 'summary.json').write_text(json.dumps(summary, indent=2) + '\n')
    print(json.dumps(summary, indent=2), flush=True)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--target-dir', type=Path, required=True)
    args = parser.parse_args()
    primitives(args.output.resolve(), args.target_dir.resolve())
