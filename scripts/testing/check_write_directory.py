#!/usr/bin/env python3
"""Execute writable-directory authority and virtual buffer URL regressions.

Raw buffer URL prefixes admitted traversal and host path reopens followed a
replaced directory. Reuse the real writer gate, then fault native/name/permission
boundaries. Only freshly compiled executed assertions count, never build errors.
"""
from __future__ import annotations
import argparse
import importlib.util
import json
import os
from pathlib import Path
import shutil

ROOT = Path(__file__).resolve().parents[2]


def module(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    value = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(value)
    return value


writers = module('directory_writers', ROOT / 'scripts/testing/check_visible_file_writers.py')
directory = module('directory_reads', ROOT / 'scripts/testing/check_directory_reads.py')
g = writers.g


def cases():
    shared = 'vfs::write_directory::tests::'
    service = 'storage::buffer_url_namespace_tests::'
    native = (ROOT / 'engine/crates/shared/src/vfs/unix.rs').read_text()
    at = native.index('const DIRECTORY_OPEN_FLAGS:')
    flags = native[at:native.index(';', at) + 1]
    assert flags.count('| libc::O_NOFOLLOW') == 1
    return [
        ('directory-read-authority-grants-writes', 'shared/vfs/mod.rs',
         '.resolve_textual(virtual_path, FileOp::Write)', '.resolve_textual(virtual_path, FileOp::Read)',
         shared + 'mutation_capability_requires_write_create_and_delete_permissions'),
        ('directory-partial-permissions-grant-all', 'shared/vfs/mod.rs',
         'if !mapping.permissions.create || !mapping.permissions.delete {', 'if false {',
         shared + 'mutation_capability_requires_write_create_and_delete_permissions'),
        ('directory-follows-replaced-parent-link', 'shared/vfs/unix.rs',
         flags, flags.replace('| libc::O_NOFOLLOW', ''),
         shared + 'directory_traversal_never_follows_symlinked_ancestors'),
        ('directory-open-only-creates', 'shared/vfs/unix.rs',
         'if !create || error.kind() != std::io::ErrorKind::NotFound {',
         'if error.kind() != std::io::ErrorKind::NotFound {',
         shared + 'opening_a_missing_directory_is_noncreating_and_creation_is_explicit'),
        ('directory-creates-invalid-child-name', 'shared/vfs/write_directory.rs',
         '    pub fn create_new(&self, name: &str) -> io::Result<File> {\n        validate_name(name)?;',
         '    pub fn create_new(&self, name: &str) -> io::Result<File> {',
         shared + 'child_names_reject_traversal_aliases_and_absolute_paths_before_native_io'),
        ('directory-revocation-traverses-outside-child', 'shared/vfs/write_directory.rs',
         '    pub fn remove_file(&self, name: &str) -> io::Result<()> {\n        validate_name(name)?;',
         '    pub fn remove_file(&self, name: &str) -> io::Result<()> {',
         service + 'revocation_accepts_only_one_child_of_the_virtual_buffer_directory'),
        ('buffer-revocation-creates-a-directory', 'services/storage.rs',
         'vfs.write_directory(BUFFER_URL_ROOT, false)', 'vfs.write_directory(BUFFER_URL_ROOT, true)',
         service + 'revocation_is_lazy_and_unloaded_services_fail_before_filesystem_access'),
        ('buffer-url-loses-its-virtual-namespace', 'services/storage.rs',
         'Ok(format!("{BUFFER_URL_ROOT}/{name}"))', 'Ok(name)',
         service + 'revocation_accepts_only_one_child_of_the_virtual_buffer_directory'),
    ]


def main(work, target):
    changes = cases()
    for name, relative, before, _, _ in changes:
        crate, relative = relative.split('/', 1)
        assert (ROOT / 'engine/crates' / crate / 'src' / relative).read_text().count(before) == 1, (name, 'anchor drift')
    writers.validate_anchors(ROOT / 'engine/crates')
    extra = [Path(__file__).resolve(), ROOT / 'scripts/testing/check_directory_reads.py',
             ROOT / 'engine/crates/runtime-v8/src/storage/mod.rs',
             ROOT / 'engine/crates/runtime-v8/src/tests/storage_isolation.rs',
             ROOT / 'engine/crates/core/src/runtime/external_services.rs']
    extra_bytes = {str(p.relative_to(ROOT)): p.read_bytes() for p in extra}
    writers.main(work, target, False)
    old = json.loads((work / 'summary.json').read_text())
    assert old['compiled_controls'] == 14
    shutil.copy2(work / 'summary.json', work / 'writer-summary.json')
    env = os.environ.copy()
    env.update(RUSTUP_TOOLCHAIN='1.95.0', CARGO_TARGET_DIR=str(target), CARGO_NET_OFFLINE='true',
               CC='/usr/bin/clang', CXX='/usr/bin/clang++', TMPDIR=str(work / 'runtime-tmp'),
               TMP=str(work / 'runtime-tmp'), TEMP=str(work / 'runtime-tmp'))
    for key in ['ANDROID_NDK', 'ANDROID_NDK_HOME', 'MIGO_AUDIO_CHECKS_BLESS']:
        env.pop(key, None)
    # Compile actual Windows helper/wrapper source for both installed targets;
    # these are API checks, not native Windows execution.
    win_env = env.copy()
    win_env['CARGO_TARGET_DIR'] = '/tmp/migo-w09-mutation-shared-target'
    windows = directory.windows_api_check(work, win_env, extra_modules=['write_directory'])
    frozen = json.loads((work / 'source-inputs.json').read_text())
    for relative, data in extra_bytes.items():
        import hashlib
        assert (ROOT / relative).read_bytes() == data, 'extra Root input changed'
        frozen[relative] = hashlib.sha256(data).hexdigest()
        destination = work / 'source-inputs' / relative
        destination.parent.mkdir(parents=True, exist_ok=True)
        destination.write_bytes(data)
    (work / 'source-inputs.json').write_text(json.dumps(frozen, indent=2) + '\n')
    for name, relative, before, after, test in changes:
        crate, relative = relative.split('/', 1)
        path = work / 'engine/crates' / crate / 'src' / relative
        original = path.read_text()
        assert original.count(before) == 1, (name, 'anchor drift')
        path.write_text(original.replace(before, after))
        try:
            target_crate = 'services' if test.startswith('storage::') else 'shared'
            binary = writers.build(work, env, name, target_crate)
            log = work / f'{name}-test.log'
            status = g.run([str(binary), '--exact', test, '--nocapture'], log,
                           work / 'engine/crates' / target_crate, env, timeout=40)
            observed = log.read_text()
            assert status == 101 and 'panicked at' in observed and '1 failed' in observed, (
                name, 'must reject an executed assertion', status)
        finally:
            path.write_text(original)
        print('actual writable directory fault', name, ': executed assertion rejected', flush=True)
    assert all(g.sha(ROOT / p) == h for p, h in frozen.items()), 'Root changed'
    for crate in ['shared', 'io', 'services']:
        for path in (work / 'engine/crates' / crate / 'src').rglob('*'):
            if path.is_file():
                assert path.read_bytes() == (ROOT / path.relative_to(work)).read_bytes(), 'copy not restored'
    summary = dict(old, directory_new_tests=8, service_namespace_new_tests=4,
                   directory_compiled_controls=8, previous_writer_compiled_controls=14,
                   compiled_controls=22, windows_api_check_targets=windows,
                   root_inputs_unchanged=True, source_copies_restored=True,
                   scope='Captured writable VFS directory, literal children, permission/namespace isolation, virtual buffer URLs and existing writer hooks. Linux execution and Windows actual-source API compilation only; no arbitrary host snapshot/native/device/performance acceptance.')
    (work / 'summary.json').write_text(json.dumps(summary, indent=2) + '\n')
    print(json.dumps(summary, indent=2), flush=True)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--target-dir', type=Path, required=True)
    args = parser.parse_args()
    main(args.output.resolve(), args.target_dir.resolve())
