#!/usr/bin/env python3
"""Execute private SQLite handover and service hot-path regressions.

SQLite wrote through the guest-visible user mount without file mutation epochs.
Move its complete quiescent directory before content publication, retain WAL,
and prove real failures using freshly rebuilt copies of the production crates.
Compile failures and timeouts never count as rejected controls.
"""
from __future__ import annotations

import argparse
import importlib.util
import json
from pathlib import Path
import re
import shutil
import tomllib

ROOT = Path(__file__).resolve().parents[2]


def module(name, file):
    spec = importlib.util.spec_from_file_location(name, ROOT / 'scripts/testing' / file)
    result = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(result)
    return result


writers = module('private_storage_writers', 'check_visible_file_writers.py')
directories = module('private_storage_directories', 'check_directory_reads.py')
g = writers.g


def declarations():
    io = 'storage_migration::tests::'
    services = 'storage::private_tests::'
    source = 'io/storage_migration.rs'
    return [
        ('no-initialization-lock', source, '    lock.lock()?;', '    // Lock deliberately omitted.',
         io + 'initialization_holds_a_kernel_lock_before_inspecting_the_namespace'),
        ('handover-only-main-database', source, '        fs::rename(paths.legacy(), target)?;',
         '        fs::create_dir(target)?;\n        fs::rename(paths.legacy().join("storage.db"), target.join("storage.db"))?;',
         io + 'migration_preserves_committed_wal_and_keeps_sqlite_outside_the_game_vfs'),
        ('preparation-error-hidden', 'shared/vfs/game_paths.rs', '            prepare()?;',
         '            let _ = prepare();',
         io + 'an_unsafe_legacy_entry_does_not_move_or_publish_and_can_be_retried'),
        ('legacy-not-validated-before-move', source,
         '        let source = validate_directory(paths.legacy(), true)?;',
         '        let source = pin(paths.legacy())?;',
         io + 'an_unsafe_legacy_entry_does_not_move_or_publish_and_can_be_retried'),
        ('hardlinks-not-counted', 'shared/vfs/file_io.rs', '        Ok(file.metadata()?.nlink())',
         '        Ok(1)', io + 'hardlinked_legacy_files_cannot_expose_private_writes_through_a_game_alias'),
        ('ambiguous-stores-overlooked', source, '        if has_legacy {', '        if false {',
         io + 'ambiguous_histories_are_reported_without_overwriting_either_store'),
        ('ready-store-not-authoritative', source,
         '            true\n        }\n        Err(shared::vfs::VfsOpenError::NotFound) => false,',
         '            false\n        }\n        Err(shared::vfs::VfsOpenError::NotFound) => false,',
         io + 'a_ready_private_store_ignores_a_recreated_game_visible_legacy_directory'),
        ('readiness-published-before-validation', source, '    let user_root = paths',
         '    crate::atomic_write::atomic_write(parent.join(READY), MAGIC)?;\n    let user_root = paths',
         io + 'an_unsafe_legacy_entry_does_not_move_or_publish_and_can_be_retried'),
        ('content-publishes-before-preparation', 'services/content.rs',
         '    migo_io::storage_migration::prepare(game_paths.storage())?;', '',
         services + 'content_mount_prepares_storage_before_publishing_the_game_filesystem'),
        ('warm-read-submits-another-fs-job', 'services/storage.rs',
         'fn prepare_sync_dir(\n    scheduler: &IoScheduler,\n    game_paths: Option<&GamePaths>,\n) -> Result<PathBuf, EngineError> {\n    let paths = storage_paths(game_paths)?;\n    if !paths.is_prepared() {',
         'fn prepare_sync_dir(\n    scheduler: &IoScheduler,\n    game_paths: Option<&GamePaths>,\n) -> Result<PathBuf, EngineError> {\n    let paths = storage_paths(game_paths)?;\n    if true {',
         services + 'prepared_synchronous_reads_do_not_wait_for_another_fs_worker'),
        ('rollback-recovery-opened-during-upgrade', source,
         '        if fs::symlink_metadata(path.join("storage.db-journal")).is_ok() {',
         '        if false {',
         io + 'unsupported_rollback_recovery_is_retained_without_opening_sqlite'),
        ('sqlite-follows-database-symlink', 'io/kv_store.rs',
         'rusqlite::OpenFlags::default() | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW',
         'rusqlite::OpenFlags::default()',
         io + 'sqlite_open_refuses_a_database_symlink_before_any_schema_write'),
        ('corrupt-readiness-accepted', source, '            if bytes != MAGIC {',
         '            if false {',
         io + 'a_corrupt_readiness_record_never_selects_or_overwrites_a_store'),
    ]


def validate_anchors(crates):
    for name, relative, before, _, _ in declarations():
        crate, relative = relative.split('/', 1)
        assert (crates / crate / 'src' / relative).read_text().count(before) == 1, (name, 'anchor drift')


def controls(work, env):
    for name, relative, before, after, test in declarations():
        crate, relative = relative.split('/', 1)
        path = work / 'engine/crates' / crate / 'src' / relative
        original = path.read_text()
        assert original.count(before) == 1, (name, 'anchor drift')
        path.write_text(original.replace(before, after))
        try:
            target = 'io' if test.startswith('storage_migration::') else 'services'
            binary = writers.build(work, env, name, target)
            log = work / f'{name}-test.log'
            status = g.run([str(binary), '--exact', test, '--nocapture'], log,
                           work / 'engine/crates' / target, env, timeout=40)
            observed = log.read_text()
            assert status == 101 and 'panicked at' in observed and '1 failed' in observed, (
                name, 'require an executed assertion failure', status)
        finally:
            path.write_text(original)
        print('actual private storage fault', name, ': executed assertion rejected', flush=True)
    return len(declarations())


def main(work, target, positive_only):
    validate_anchors(ROOT / 'engine/crates')
    env, frozen, copies = writers.helper.prepare(work, target)
    validate_anchors(work / 'engine/crates')
    env.pop('MIGO_AUDIO_CHECKS_BLESS', None)
    fixture = Path('platforms/apple/WebContent/PerformancePlus/test/fixtures/audio-check-answers.json')
    (work / fixture).parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(ROOT / fixture, work / fixture)
    extras = [Path(__file__).resolve(), ROOT / fixture,
              ROOT / 'scripts/testing/check_visible_file_writers.py',
              ROOT / 'scripts/testing/check_directory_reads.py',
              ROOT / 'scripts/testing/check_package_positioned_reads.py',
              ROOT / 'scripts/testing/check_gitignore_contract.py',
              ROOT / 'engine/crates/runtime-v8/src/tests/storage_isolation.rs']
    for path in extras:
        relative = str(path.relative_to(ROOT))
        frozen[relative] = g.sha(path)
        dest = work / 'source-inputs' / relative
        dest.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(path, dest)
    (work / 'source-inputs.json').write_text(json.dumps(frozen, indent=2) + '\n')
    locked = {}
    for row in tomllib.loads((ROOT / 'engine/Cargo.lock').read_text())['package']:
        locked.setdefault(row['name'], set()).add(row['version'])
    for crate in ['shared', 'io']:
        log = work / f'{crate}-metadata.jsonl'
        assert g.run(['cargo', 'metadata', '--offline', '--format-version', '1'], log,
                     work / 'engine/crates' / crate, env) == 0
        selected = json.loads(next(line for line in log.read_text().splitlines()
                                   if line.startswith('{"packages"')))['packages']
        assert all(row['name'] not in locked or row['version'] in locked[row['name']]
                   for row in selected), 'existing dependency version drift'
    log = work / 'shared-minimal-check.jsonl'
    assert g.run(['cargo', 'check', '--offline', '--locked', '--lib', '--no-default-features',
                  '--message-format=json'], log, work / 'engine/crates/shared', env) == 0
    names, features = set(), []
    for line in log.read_text().splitlines():
        try:
            row = json.loads(line)
        except ValueError:
            continue
        if row.get('reason') == 'compiler-artifact':
            names.add(row['target']['name'])
            if row['target']['name'] == 'shared':
                features.append(row['features'])
    assert features == [[]] and not names.intersection({'zstd', 'zstd_safe', 'zstd_sys'})
    windows = directories.windows_api_check(work, env)
    suites = {}
    for name, crate, minimal in [('shared-positive', 'shared', False),
                                  ('shared-minimal-positive', 'shared', True),
                                  ('io-positive', 'io', False),
                                  ('services-positive', 'services', False)]:
        binary = writers.build(work, env, name, crate, minimal)
        log = work / f'{name}.log'
        assert g.run([str(binary), '--nocapture', '--test-threads=1'], log,
                     work / 'engine/crates' / crate, env) == 0
        match = re.search(r'test result: ok\. (\d+) passed; 0 failed; (\d+) ignored;', log.read_text())
        assert match
        suites[name] = {'passed': int(match[1]), 'ignored': int(match[2]),
                        'binary_sha256': g.sha(binary)}
    count = 0 if positive_only else controls(work, env)
    assert all(g.sha(ROOT / p) == h for p, h in frozen.items()), 'Root changed'
    assert all(g.sha(work / p) == h for p, h in copies.items()), 'source copy not restored'
    summary = {
        'suites': suites, 'new_tests': 16, 'compiled_controls': count,
        'positive_only': positive_only, 'root_inputs_unchanged': True,
        'source_copies_restored': True, 'existing_dependency_versions_preserved': True,
        'minimal_shared_features': [], 'minimal_shared_compiles_no_zstd': True,
        'windows_api_check_targets': windows,
        'root_lock_sha256': g.sha(ROOT / 'engine/Cargo.lock'),
        'scope': 'Quiescent legacy SQLite directory handover before guest publication, WAL preservation, retry/ambiguity/alias rejection, kernel initialization lock and cached service hot path. Windows actual-source API compilation only. No concurrent older SDK, multiprocess KV writers, hostile host namespace, native/device/power-loss/performance acceptance.',
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
