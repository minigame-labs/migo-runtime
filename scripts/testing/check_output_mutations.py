#!/usr/bin/env python3
"""Exercise tracked ZIP output and cross-filesystem move staging.

O_TRUNC mutates before a pathname can be enrolled. An epoch dropped before the
copy finishes also admits a mixed read. Compile actual IO with zip-extract,
then require executed assertion failures for each isolated regression below.
Ordinary native copy and mounted copy remain outside this gate's scope.
"""
from __future__ import annotations

import argparse
import importlib.util
import json
from pathlib import Path
import re
import tomllib

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location(
    'output_mutation_io', ROOT / 'scripts/testing/check_audio_package_io.py')
io = importlib.util.module_from_spec(spec)
spec.loader.exec_module(io)


def controls(work, env):
    changes = [
        ('zip-native-open-truncates-before-registration', 'zip_extract.rs',
         'libc::O_WRONLY\n                    | libc::O_CREAT',
         'libc::O_WRONLY | libc::O_TRUNC\n                    | libc::O_CREAT',
         'zip_extract::mutation_tests::output_open_preserves_existing_payload_until_registration'),
        ('zip-registers-another-object', 'zip_extract.rs',
         'crate::fs_ops::begin_file_write(&file)?',
         'crate::fs_ops::begin_file_write(&tempfile::tempfile()?)?',
         'zip_extract::mutation_tests::output_open_preserves_existing_payload_until_registration'),
        ('zip-retains-old-tail', 'zip_extract.rs',
         '        file.set_len(0)?;', '        // Deliberately omit truncation.',
         'zip_extract::mutation_tests::output_open_preserves_existing_payload_until_registration'),
        ('zip-ends-epoch-before-transfer', 'zip_extract.rs',
         '    let count = io::copy(reader, &mut outfile)?;',
         '    drop(_write);\n    let count = io::copy(reader, &mut outfile)?;',
         'zip_extract::mutation_tests::entry_transfer_holds_epoch_during_each_read_and_releases_it_after_read_failure'),
        ('move-bypasses-atomic-staging', 'fs_ops.rs',
         'crate::atomic_write::atomic_write_with(&new_path_buf, |dst| transfer(&mut src, dst))?;',
         'transfer(&mut src, &mut File::create(&new_path_buf)?)?;',
         'fs_ops::mutation_tests::cross_fs_transfer_uses_active_epoch_and_atomic_publication'),
        ('move-ignores-transfer-failure', 'fs_ops.rs',
         '|dst| transfer(&mut src, dst)',
         '|dst| { let _ = transfer(&mut src, dst); Ok(()) }',
         'fs_ops::mutation_tests::cross_fs_transfer_failure_keeps_source_and_target_and_cleans_partial_stage'),
        ('move-removes-source-before-publication', 'fs_ops.rs',
         '    // Reuse the tracked temporary writer and its error cleanup, sync and',
         '    std::fs::remove_file(old_path)?;\n    // Reuse the tracked temporary writer and its error cleanup, sync and',
         'fs_ops::mutation_tests::cross_fs_transfer_failure_keeps_source_and_target_and_cleans_partial_stage'),
        ('move-retains-published-source', 'fs_ops.rs',
         'if let Err(e) = std::fs::remove_file(old_path) {',
         'if let Err(e) = Ok::<(), std::io::Error>(()) {',
         'fs_ops::mutation_tests::cross_fs_production_transfer_preserves_a_large_payload'),
        ('move-leaks-partial-stage', 'atomic_write.rs',
         '''        Err(e) => {
            let _ = fs::remove_file(&tmp);
            return Err(e);
        }''',
         '''        Err(e) => {
            // Deliberately leave the failed stage behind.
            return Err(e);
        }''',
         'fs_ops::mutation_tests::cross_fs_transfer_failure_keeps_source_and_target_and_cleans_partial_stage'),
        ('move-copies-only-a-prefix', 'fs_ops.rs',
         '            std::io::copy(src, dst)?;',
         '            std::io::copy(&mut src.take(1), dst)?;',
         'fs_ops::mutation_tests::cross_fs_production_transfer_preserves_a_large_payload'),
    ]
    for name, file, before, after, test in changes:
        # The native copy specialization above is Linux/Android only. This
        # gate runs on Linux; non-Linux requires its own native CI cohort.
        path = work / 'probe/src' / file
        original = path.read_text()
        assert original.count(before) == 1, (name, 'anchor drift')
        path.write_text(original.replace(before, after))
        try:
            binary = io.g.build(work / 'probe', work, env, name, features=['zip-extract'])
            log = work / f'{name}-test.log'
            status = io.g.run([str(binary), '--exact', test, '--nocapture'],
                              log, work / 'probe', env, timeout=30)
            observed = log.read_text()
            assert status == 101 and 'panicked at' in observed and '1 failed' in observed, (
                name, 'must reject an executed assertion', status)
        finally:
            path.write_text(original)
        print('actual output mutation fault', name, ': executed assertion rejected', flush=True)
    return len(changes)


def main(work: Path, target: Path, positive_only: bool):
    env, frozen, retained = io.prepare(work, target, [Path(__file__).resolve()])
    selected = json.loads(next(line for line in (work / 'metadata.jsonl').read_text().splitlines()
                               if line.startswith('{"packages"')))['packages']
    locked = {}
    for row in tomllib.loads((ROOT / 'engine/Cargo.lock').read_text())['package']:
        locked.setdefault(row['name'], set()).add(row['version'])
    assert all(row['name'] not in locked or row['version'] in locked[row['name']]
               for row in selected), 'existing dependency version drift'
    binary = io.g.build(work / 'probe', work, env, 'positive', features=['zip-extract'])
    assert io.g.run([str(binary), '--nocapture', '--test-threads=1'],
                    work / 'positive.log', work / 'probe', env) == 0
    positive = (work / 'positive.log').read_text()
    match = re.search(r'test result: ok\. (\d+) passed; 0 failed; (\d+) ignored;', positive)
    assert match and positive.count('test zip_extract::mutation_tests::') == 6
    assert positive.count('test fs_ops::mutation_tests::cross_fs_') == 3
    count = 0 if positive_only else controls(work, env)
    assert all(io.g.sha(ROOT / path) == digest for path, digest in frozen.items()), 'Root changed'
    assert all(io.g.sha(work / 'probe' / path) == digest for path, digest in retained.items()), 'copy not restored'
    summary = {
        'passed': int(match[1]), 'ignored': int(match[2]), 'zip_mutation_tests': 6,
        'move_tests': 3, 'compiled_controls': count, 'positive_only': positive_only,
        'binary_sha256': io.g.sha(binary), 'root_inputs_unchanged': True,
        'source_copies_restored': True, 'existing_dependency_versions_preserved': True,
        'root_lock_sha256': io.g.sha(ROOT / 'engine/Cargo.lock'),
        'scope': 'Linux actual IO plus zip-extract. ZIP native-open/regular-object/epoch/copy-error and cross-FS fallback stage/publication/cleanup. No ordinary or mounted copy, arbitrary host-writer snapshot, actual EXDEV-mount, native Windows/Apple/device/performance acceptance.',
    }
    (work / 'summary.json').write_text(json.dumps(summary, indent=2) + '\n')
    print(json.dumps(summary, indent=2), flush=True)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--target-dir', type=Path, required=True)
    parser.add_argument('--positive-only', action='store_true')
    args = parser.parse_args()
    main(args.output.resolve(), args.target_dir.resolve(), args.positive_only)
