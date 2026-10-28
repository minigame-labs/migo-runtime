#!/usr/bin/env python3
"""Execute directory capability, mount generation and traversal regressions.

A resolved directory path can be replaced before an asynchronous asset open.
Pin the root at publication, select the capability without IO at admission,
and keep native traversal and package generation contracts executable.
"""
from __future__ import annotations
import argparse, importlib.util, json, re, shutil, tomllib
from pathlib import Path
ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location('package_reads', ROOT/'scripts/testing/check_package_positioned_reads.py')
package = importlib.util.module_from_spec(spec); spec.loader.exec_module(package)
sha = package.sha

def windows_api_check(work, env, extra_modules=(), extra_dependencies=None):
    # Actual native module + exact Root error/helper excerpts, not fake Win32
    # declarations. This checks Rust API availability, never native execution.
    probe = work/'windows-api'; (probe/'src').mkdir(parents=True)
    source = (ROOT/'engine/crates/shared/src/vfs/mod.rs').read_text()
    start = source.index('#[derive(Debug, Clone, PartialEq, Eq)]\npub enum VfsError')
    end = source.index('// ---------------------------------------------------------------------------\n// VirtualFS\n', start)
    def function(signature):
        at = source.index(signature); stop = source.index('\n}\n', at)+3
        return source[at:stop]
    text = 'pub mod vfs {\nuse std::path::{Path, PathBuf};\n'+source[start:end]
    text += function('fn is_unsafe_windows_component(')+function('pub(crate) fn normalize_path(')
    for name in dict.fromkeys(['windows', 'file_io', 'read_root', *extra_modules]):
        origin = work/'engine/crates/shared/src/vfs'/f'{name}.rs'
        text += f'#[path={json.dumps(str(origin))}] pub mod {name};\n'
    (probe/'src/lib.rs').write_text(text+'}\n')
    dependency = tomllib.loads((ROOT/'engine/crates/shared/Cargo.toml').read_text())['target']['cfg(windows)']['dependencies']['windows-sys']
    manifest='[package]\nname="migo-directory-windows-api"\nversion="0.0.0"\nedition="2024"\n[workspace]\n[dependencies]\nwindows-sys='+json.dumps({'version':'='+dependency['version'], 'features':dependency['features']}).replace(':', '=')+'\n'
    for name, version in (extra_dependencies or {}).items():manifest+=name+'='+json.dumps('='+version)+'\n'
    (probe/'Cargo.toml').write_text(manifest)
    (probe/'Cargo.lock').write_bytes((ROOT/'engine/Cargo.lock').read_bytes())
    assert package.helper.run(['cargo','metadata','--offline','--format-version','1'], work/'windows-metadata.jsonl', probe, env)==0
    selected=json.loads(next(l for l in(work/'windows-metadata.jsonl').read_text().splitlines()if l.startswith('{"packages"')))['packages'];locked={}
    for row in tomllib.loads((ROOT/'engine/Cargo.lock').read_text())['package']:locked.setdefault(row['name'],set()).add(row['version'])
    assert all(row['name']not in locked or row['version']in locked[row['name']]for row in selected),'Windows probe existing version drift'
    targets = ['x86_64-pc-windows-gnu', 'aarch64-pc-windows-msvc']
    for target in targets:
        assert package.helper.run(['cargo','check','--offline','--locked','--lib','--target',target],work/f'windows-{target}-check.log',probe,env)==0
    return targets

def controls(work, env):
    tests = 'vfs::read_root::tests::'
    declarations = [
        ('directory-root-reopened-at-admission','mount.rs','        self.read_root.clone()','        super::read_root::ReadRoot::open(&self.root).ok().map(Arc::new)','directory_mount_uses_captured_root_not_a_later_path_lookup'),
        ('directory-generation-ignored','mount.rs','if generation != self.generation.load(Ordering::Acquire) {','if false {','overlay_identity_generation_and_retirement_are_fixed_at_admission'),
        ('directory-overlay-ignored','mount.rs','let selected = highest_matching_overlay(&inner.overlays, &normalized);','let selected = None::<(&MountEntry, &str)>;','overlay_identity_generation_and_retirement_are_fixed_at_admission'),
        ('portable-component-guard-removed','read_root.rs','pub fn open_regular(&self, relative: &Path) -> Result<File, VfsOpenError> {\n        validate_relative(relative)?;','pub fn open_regular(&self, relative: &Path) -> Result<File, VfsOpenError> {','portable_names_and_nonrelative_paths_fail_before_native_open'),
        ('overlay-hides-unsafe-logical-prefix','mount.rs','        super::read_root::validate_relative(Path::new(&normalized)).map_err(io::Error::other)?;','','overlay_identity_generation_and_retirement_are_fixed_at_admission'),
        ('unicode-controls-accepted','read_root.rs',"c.is_control() || c == '\\\\' || c == ':'","(c as u32) < 0x20 || c == '\\\\' || c == ':'",'portable_names_and_nonrelative_paths_fail_before_native_open'),
        ('backend-path-implies-read-authority','mount.rs','fn directory_read_root(&self) -> Option<Arc<super::read_root::ReadRoot>> {\n        None','fn directory_read_root(&self) -> Option<Arc<super::read_root::ReadRoot>> {\n        self.root_dir().and_then(|path| super::read_root::ReadRoot::open(path).ok()).map(Arc::new)','pinning_needs_explicit_authority_and_does_not_stat_or_reopen_paths'),
        ('directory-admission-stats-path','mount.rs','        let root = entry.backend.directory_read_root().ok_or_else(|| {','        let _ = entry.backend.is_file(sub);\n        let root = entry.backend.directory_read_root().ok_or_else(|| {','pinning_needs_explicit_authority_and_does_not_stat_or_reopen_paths'),
        ('directory-absolute-path-accepted','mount.rs',"if relative_path.starts_with('/') {",'if false {','overlay_identity_generation_and_retirement_are_fixed_at_admission'),
        ('parent-traversal-not-held-through-final-open','unix.rs','    for component in parents {\n        parent = openat(parent.as_raw_fd(), component, DIRECTORY_OPEN_FLAGS)?;\n    }\n\n    before_final();','    before_final();\n    for component in parents {\n        parent = openat(parent.as_raw_fd(), component, DIRECTORY_OPEN_FLAGS)?;\n    }','replacing_a_parent_after_traversal_keeps_the_opened_directory'),
    ]
    for name, file, before, after, test in declarations:
        path = work/'engine/crates/shared/src/vfs'/file; original = path.read_text()
        assert original.count(before)==1,(name,'anchor drift')
        path.write_text(original.replace(before,after))
        try:
            binary = package.build(work,env,name); log = work/f'{name}-test.log'
            status = package.helper.run([str(binary),'--exact',tests+test,'--nocapture'],log,work/'engine/crates/shared',env,timeout=30)
            observed = log.read_text()
            assert status==101 and 'panicked at' in observed and '1 failed' in observed,(name,'must reject an executed assertion',status)
        finally: path.write_text(original)
        print('actual directory fault',name,': executed assertion rejected',flush=True)
    return len(declarations)

def main(work,target,positive_only):
    env,frozen,retained = package.prepare(work,target,[Path(__file__).resolve()])
    targets = windows_api_check(work,env)
    binary = package.build(work,env,'positive'); log = work/'positive.log'
    assert package.helper.run([str(binary),'--nocapture','--test-threads=1'],log,work/'engine/crates/shared',env)==0
    observed = log.read_text(); match = re.search(r'test result: ok\. (\d+) passed; 0 failed; (\d+) ignored;',observed); assert match
    assert observed.count('test vfs::read_root::tests::')==9
    assert observed.count('test vfs::package::entry_reader::tests::')==11
    negative = 0 if positive_only else controls(work,env)
    package_negative = 0 if positive_only else package.controls(work,env)
    assert all(sha(ROOT/rel)==h for rel,h in frozen.items()),'Root changed'
    assert all(sha(work/'engine/crates/shared'/rel)==h for rel,h in retained.items()),'source copy not restored'
    summary = {'passed':int(match[1]),'ignored':int(match[2]),'directory_tests':9,'package_tests':11,'directory_compiled_controls':negative,'package_compiled_controls':package_negative,'windows_api_check_targets':targets,'positive_only':positive_only,'binary_sha256':sha(binary),'root_inputs_unchanged':True,'source_copies_restored':True,'existing_dependency_versions_preserved':True,'root_lock_sha256':sha(ROOT/'engine/Cargo.lock'),'scope':'Linux directory capability/overlay/atomic install and package regressions; Windows actual-source Rust API check only, native execution remains in Windows CI. Not a file snapshot, writable spool, product codec/DSP cutover or device/performance acceptance.'}
    (work/'summary.json').write_text(json.dumps(summary,indent=2)+'\n'); print(json.dumps(summary,indent=2),flush=True)
if __name__=='__main__':
    parser=argparse.ArgumentParser(description=__doc__);parser.add_argument('--output',type=Path,required=True);parser.add_argument('--target-dir',type=Path,required=True);parser.add_argument('--positive-only',action='store_true');a=parser.parse_args();main(a.output.resolve(),a.target_dir.resolve(),a.positive_only)
