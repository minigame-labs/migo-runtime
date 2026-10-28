#!/usr/bin/env python3
"""Exercise package ingest publication and the shared tracked atomic writer.

The old fixed .mpkg.tmp name let concurrent ingests delete one another's
output. The new path must also retain format errors and track buffered header
fixups. Rebuild actual isolated IO for every fault; compile errors and timeouts
never count as rejected runtime controls. Recheck the existing ZIP/move and
descriptor/atomic controls because they share the changed atomic helper.
"""
from __future__ import annotations

import argparse
import importlib.util
import json
from pathlib import Path
import re
import tomllib

ROOT = Path(__file__).resolve().parents[2]


def module(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    value = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(value)
    return value


output = module('ingest_output', ROOT / 'scripts/testing/check_output_mutations.py')
mutations = module('ingest_mutations', ROOT / 'scripts/testing/check_file_mutations.py')
io = output.io


def controls(work, env):
    prefix = 'package_ingest::mutation_tests::'
    tracked_test = prefix + 'typed_atomic_writer_tracks_package_flush_and_header_fixup_then_returns_identity'
    public_test = prefix + 'public_ingest_leaves_an_existing_legacy_temporary_object_untouched'
    changes = [
        ('ingest-unregistered-stage', 'atomic_write.rs',
         '''        let _write = crate::fs_ops::begin_file_write(&f)?;
        f.set_len(0)?;
        let value = writer(&mut f)?;''',
         '''        f.set_len(0)?;
        let value = writer(&mut f)?;''', tracked_test),
        ('ingest-ends-epoch-before-buffered-write', 'atomic_write.rs',
         '        let value = writer(&mut f)?;',
         '        drop(_write);\n        let value = writer(&mut f)?;', tracked_test),
        ('ingest-reuses-one-stage-name', 'atomic_write.rs',
         'let id = TMP_ID.fetch_add(1, Ordering::Relaxed);', 'let id = 0;',
         prefix + 'concurrent_ingests_keep_independent_tracked_stages_until_complete_publication'),
        ('ingest-returns-wrong-identity', 'package_ingest.rs',
         '        writer.finish(package_name, package_version)',
         '''        writer.finish(package_name, package_version).map(|mut identity| {
            identity.checksum ^= 1;
            identity
        })''', public_test),
        ('ingest-flattens-format-errors', 'package_ingest.rs',
         '    })\n}\n\npub async fn ingest_zip_to_package_with_scheduler(',
         '''    }).map_err(|error: PackageError| PackageError::Io(
        std::io::Error::other(error.to_string())))
}

pub async fn ingest_zip_to_package_with_scheduler(''',
         prefix + 'invalid_entry_path_preserves_package_error_type_and_previous_output'),
        ('ingest-leaks-failed-stage', 'atomic_write.rs',
         '''        Err(e) => {
            let _ = fs::remove_file(&tmp);
            return Err(e);
        }''',
         '''        Err(e) => {
            // Deliberately leak a failed package stage.
            return Err(e);
        }''',
         prefix + 'corrupted_zip_after_partial_transfer_keeps_old_output_and_removes_stage'),
        ('ingest-publishes-before-validation', 'atomic_write.rs',
         '        let value = writer(&mut f)?;',
         '        fs::rename(&tmp, path)?;\n        let value = writer(&mut f)?;',
         prefix + 'corrupted_zip_after_partial_transfer_keeps_old_output_and_removes_stage'),
        ('ingest-never-publishes', 'atomic_write.rs',
         '''    if let Err(e) = fs::rename(&tmp, path) {
        let _ = fs::remove_file(&tmp);
        return Err(e.into());
    }''',
         '''    if let Err(e) = Ok::<(), io::Error>(()) {
        let _ = fs::remove_file(&tmp);
        return Err(e.into());
    }''', public_test),
    ]
    for name, relative, before, after, test in changes:
        path = work / 'probe/src' / relative
        original = path.read_text()
        assert original.count(before) == 1, (name, 'anchor drift')
        path.write_text(original.replace(before, after))
        try:
            binary = io.g.build(work / 'probe', work, env, name, features=['zip-extract'])
            log = work / f'{name}-test.log'
            status = io.g.run([str(binary), '--exact', test, '--nocapture'],
                              log, work / 'probe', env, timeout=40)
            observed = log.read_text()
            assert status == 101 and 'panicked at' in observed and '1 failed' in observed, (
                name, 'must reject an executed assertion', status)
        finally:
            path.write_text(original)
        print('actual package ingest fault', name, ': executed assertion rejected', flush=True)
    return len(changes)


def main(work, target, positive_only):
    extra = sorted((ROOT / 'scripts/testing').glob('*.py'))
    env, frozen, retained = io.prepare(work, target, extra)
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
    assert match and positive.count('test package_ingest::mutation_tests::') == 5
    ingest_count = 0 if positive_only else controls(work, env)
    output_count = 0 if positive_only else output.controls(work, env)
    descriptor_count = 0 if positive_only else mutations.io_controls(work, env)
    assert all(io.g.sha(ROOT / path) == digest for path, digest in frozen.items()), 'Root changed'
    assert all(io.g.sha(work / 'probe' / path) == digest for path, digest in retained.items()), 'copy not restored'
    summary = {
        'passed': int(match[1]), 'ignored': int(match[2]), 'ingest_tests': 5,
        'ingest_compiled_controls': ingest_count, 'zip_move_compiled_controls': output_count,
        'descriptor_atomic_compiled_controls': descriptor_count,
        'compiled_controls': ingest_count + output_count + descriptor_count,
        'positive_only': positive_only, 'binary_sha256': io.g.sha(binary),
        'root_inputs_unchanged': True, 'source_copies_restored': True,
        'existing_dependency_versions_preserved': True,
        'root_lock_sha256': io.g.sha(ROOT / 'engine/Cargo.lock'),
        'scope': 'Linux actual ZIP ingest, independent atomic stages, per-object epochs through buffered writes and header fixup, typed identity/errors, corruption cleanup; shared ZIP/move and descriptor/atomic controls rebuilt. No arbitrary host-writer snapshot, native recorder, power-loss/device/performance acceptance.',
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
