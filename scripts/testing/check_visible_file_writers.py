#!/usr/bin/env python3
"""Compile actual buffer-URL and host-import writer ownership regressions.

Timestamp names plus fs::write could truncate an existing object. Host imports
closed their exclusive reservation and copied by pathname without an epoch.
Use isolated real Shared/IO/services sources; require executed assertion failures
after fresh builds, and retain the no-VFS engine-free dependency boundary.
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
spec = importlib.util.spec_from_file_location(
    'visible_writer_helpers', ROOT / 'scripts/testing/check_pack_copy_budget.py')
helper = importlib.util.module_from_spec(spec)
spec.loader.exec_module(helper)
g = helper.g


def build(work, env, name, crate, minimal=False):
    package = work / 'engine/crates' / crate
    assert g.run(['cargo', 'clean', '--offline', '--locked', '-p', 'migo-shared',
                  '-p', 'migo-io', '-p', 'migo-services'],
                 work / f'{name}-clean.log', work / 'engine/crates/services', env) == 0
    command = ['cargo', 'test', '--offline', '--locked', '--lib', '--no-run', '--message-format=json']
    if minimal:
        command.append('--no-default-features')
    assert g.run(command, work / f'{name}-build.jsonl', package, env) == 0, (
        name, 'compile failure is not a rejected runtime control')
    target = {'shared': 'shared', 'io': 'migo_io', 'services': 'migo_services'}[crate]
    artifacts = []
    for line in (work / f'{name}-build.jsonl').read_text().splitlines():
        try:
            row = json.loads(line)
        except ValueError:
            continue
        if row.get('reason') == 'compiler-artifact' and row.get('executable') and row['target']['name'] == target:
            artifacts.append(row)
    assert len(artifacts) == 1 and artifacts[0]['fresh'] is False
    binary = work / f'{name}-tests'
    shutil.copy2(artifacts[0]['executable'], binary)
    return binary


def declarations():
    sp = 'storage::buffer_url_tests::'
    hp = 'services::host_files::mutation_tests::'
    cases = [
        ('buffer-truncates-a-collision', 'shared/vfs/unix.rs',
         'libc::O_EXCL', '0',
         sp + 'exclusive_creation_skips_existing_objects_without_truncating_or_following_them'),
        ('buffer-unregistered-write', 'services/storage.rs',
         '        let _write = mutations.into_write()?;', '        let _write = mutations;',
         sp + 'buffer_write_tracks_the_actual_open_object_and_releases_its_epoch'),
        ('buffer-epoch-ends-before-write', 'services/storage.rs',
         '        write(&mut file)', '        drop(_write);\n        write(&mut file)',
         sp + 'buffer_write_tracks_the_actual_open_object_and_releases_its_epoch'),
        ('buffer-leaks-a-partial-file', 'services/storage.rs',
         '        let _ = dir.remove_file(&name);',
         '        // Deliberately leave the failed URL visible.',
         sp + 'a_partial_write_error_removes_the_failed_url_and_releases_the_epoch'),
        ('buffer-hides-a-write-error', 'services/storage.rs',
         '        return Err(error);\n    }\n    Ok(name)',
         '        return Ok(name);\n    }\n    Ok(name)',
         sp + 'a_partial_write_error_removes_the_failed_url_and_releases_the_epoch'),
        ('buffer-drops-the-last-byte', 'services/storage.rs',
         '|file| file.write_all(buffer),',
         '|file| file.write_all(&buffer[..buffer.len().saturating_sub(1)]),',
         sp + 'concurrent_public_buffer_urls_keep_complete_independent_payloads'),
        ('import-unregistered-copy', 'shared/services/host_files.rs',
         '''            let _write = crate::vfs::file_mutation::FileMutations::for_file(&output)?
                .ok_or_else(|| std::io::Error::other("import output is not a regular file"))?
                .into_write()?;''',
         '            let _write = ();',
         hp + 'import_copy_tracks_the_reserved_object_and_reads_the_pinned_source'),
        ('import-epoch-ends-before-copy', 'shared/services/host_files.rs',
         '            transfer(&mut input, &mut output)?;',
         '            #[cfg(feature = "vfs")]\n            drop(_write);\n            transfer(&mut input, &mut output)?;',
         hp + 'import_copy_tracks_the_reserved_object_and_reads_the_pinned_source'),
        ('import-leaks-a-partial-file', 'shared/services/host_files.rs',
         '            let _ = std::fs::remove_file(&dest);',
         '            // Deliberately retain the partial import.',
         hp + 'import_copy_failure_preserves_the_source_and_removes_partial_output'),
        ('import-removes-source-before-copy', 'shared/services/host_files.rs',
         '    if !moved {\n        let result',
         '    if !moved {\n        std::fs::remove_file(source).map_err(|e| e.to_string())?;\n        let result',
         hp + 'import_copy_failure_preserves_the_source_and_removes_partial_output'),
        ('import-hides-transfer-error', 'shared/services/host_files.rs',
         '            transfer(&mut input, &mut output)?;',
         '            let _ = transfer(&mut input, &mut output);',
         hp + 'import_copy_failure_preserves_the_source_and_removes_partial_output'),
        ('import-skips-the-native-transfer', 'shared/services/host_files.rs',
         '    crate::vfs::file_copy::copy_contents(source, dest)?;',
         '    Ok::<(), std::io::Error>(())?;',
         hp + 'import_copy_tracks_the_reserved_object_and_reads_the_pinned_source'),
        ('import-forgets-copied-permissions', 'shared/services/host_files.rs',
         '                output.set_permissions(writable)?;\n            }\n            #[cfg',
         '                // Deliberately keep default permissions.\n            }\n            #[cfg',
         hp + 'import_copy_tracks_the_reserved_object_and_reads_the_pinned_source'),
        ('import-chmods-the-replaced-name', 'shared/services/host_files.rs',
         '    } else {\n        make_owner_writable(&dest);\n    }',
         '    }\n    make_owner_writable(&dest);',
         hp + 'import_copy_never_reopens_the_reserved_destination_name'),
    ]
    return cases


def validate_anchors(crates):
    for name, relative, before, _, _ in declarations():
        crate, relative = relative.split('/', 1)
        assert (crates / crate / 'src' / relative).read_text().count(before) == 1, (name, 'anchor drift')


def controls(work, env):
    cases = declarations()
    for name, relative, before, after, test in cases:
        crate, relative = relative.split('/', 1)
        path = work / 'engine/crates' / crate / 'src' / relative
        original = path.read_text()
        assert original.count(before) == 1, (name, 'anchor drift')
        path.write_text(original.replace(before, after))
        try:
            target_crate = 'services' if test.startswith('storage::') else 'shared'
            binary = build(work, env, name, target_crate)
            log = work / f'{name}-test.log'
            status = g.run([str(binary), '--exact', test, '--nocapture'], log,
                           work / 'engine/crates' / crate, env, timeout=40)
            observed = log.read_text()
            assert status == 101 and 'panicked at' in observed and '1 failed' in observed, (
                name, 'require an executed assertion failure', status)
        finally:
            path.write_text(original)
        print('actual visible writer fault', name, ': executed assertion rejected', flush=True)
    return len(cases)


def main(work, target, positive_only):
    validate_anchors(ROOT / 'engine/crates')
    env, frozen, copies = helper.prepare(work, target)
    validate_anchors(work / 'engine/crates')
    env.pop('MIGO_AUDIO_CHECKS_BLESS', None)
    fixture = Path('platforms/apple/WebContent/PerformancePlus/test/fixtures/audio-check-answers.json')
    destination = work / fixture
    destination.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(ROOT / fixture, destination)
    for path in [Path(__file__).resolve(), ROOT / 'scripts/testing/check_gitignore_contract.py', ROOT / fixture]:
        relative = str(path.relative_to(ROOT))
        frozen[relative] = g.sha(path)
        destination = work / 'source-inputs' / relative
        destination.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(path, destination)
    (work / 'source-inputs.json').write_text(json.dumps(frozen, indent=2) + '\n')
    shared = work / 'engine/crates/shared'
    assert g.run(['cargo', 'metadata', '--offline', '--format-version', '1'],
                 work / 'shared-metadata.jsonl', shared, env) == 0
    selected = json.loads(next(line for line in (work / 'shared-metadata.jsonl').read_text().splitlines()
                               if line.startswith('{"packages"')))['packages']
    locked = {}
    for row in tomllib.loads((ROOT / 'engine/Cargo.lock').read_text())['package']:
        locked.setdefault(row['name'], set()).add(row['version'])
    assert all(row['name'] not in locked or row['version'] in locked[row['name']]
               for row in selected), 'Shared existing dependency version drift'
    assert g.run(['cargo', 'check', '--offline', '--locked', '--lib', '--no-default-features',
                  '--message-format=json'], work / 'shared-minimal-check.jsonl', shared, env) == 0
    names, minimal_features = set(), []
    for line in (work / 'shared-minimal-check.jsonl').read_text().splitlines():
        try:
            row = json.loads(line)
        except ValueError:
            continue
        if row.get('reason') == 'compiler-artifact':
            names.add(row['target']['name'])
            if row['target']['name'] == 'shared':
                minimal_features.append(row['features'])
    assert minimal_features == [[]] and not names.intersection({'zstd', 'zstd_safe', 'zstd_sys'})
    suites = {}
    for name, crate, minimal in [('shared-positive', 'shared', False),
                                  ('shared-minimal-positive', 'shared', True),
                                  ('services-positive', 'services', False)]:
        binary = build(work, env, name, crate, minimal)
        log = work / f'{name}.log'
        assert g.run([str(binary), '--nocapture', '--test-threads=1'], log,
                     work / 'engine/crates' / crate, env) == 0
        observed = log.read_text()
        match = re.search(r'test result: ok\. (\d+) passed; 0 failed; (\d+) ignored;', observed)
        assert match
        expected = 0 if minimal else (3 if crate == 'shared' else 4)
        prefix = 'services::host_files::mutation_tests::' if crate == 'shared' else 'storage::buffer_url_tests::'
        assert observed.count('test ' + prefix) == expected
        suites[name] = {'passed': int(match[1]), 'ignored': int(match[2]), 'binary_sha256': g.sha(binary)}
    count = 0 if positive_only else controls(work, env)
    assert all(g.sha(ROOT / p) == h for p, h in frozen.items()), 'Root changed'
    assert all(g.sha(work / p) == h for p, h in copies.items()), 'source copy not restored'
    summary = {
        'suites': suites, 'new_tests': 7, 'compiled_controls': count,
        'positive_only': positive_only, 'root_inputs_unchanged': True,
        'source_copies_restored': True, 'existing_dependency_versions_preserved': True,
        'minimal_shared_features': [], 'minimal_shared_compiles_no_zstd': True,
        'root_lock_sha256': g.sha(ROOT / 'engine/Cargo.lock'),
        'scope': 'Actual buffer URL exclusive creation/epochs/failure, pinned tracked host import copy and permissions, default and no-VFS Shared. Host hands over quiescent owned paths; no arbitrary host-writer or hostile rename snapshot, full mutable-source dispatch, native/device/performance acceptance.',
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
