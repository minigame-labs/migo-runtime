#!/usr/bin/env python3
"""Compile actual package range/mount contracts and semantic faults in a copy.

Pinned backend Arcs previously still reopened a pathname for each range and
allocated fresh compressed/decoded buffers. Mutation/rename and allocation
contracts must reject this drift in executed Rust, never in a text-only check.
"""
from __future__ import annotations
import argparse, importlib.util, json, os, re, shutil, subprocess, tomllib
from pathlib import Path
ROOT=Path(__file__).resolve().parents[2]
spec=importlib.util.spec_from_file_location('pack_io_gate',ROOT/'scripts/testing/check_audio_cpu_admission.py')
helper=importlib.util.module_from_spec(spec);spec.loader.exec_module(helper)
sha=helper.sha

def build(work,env,label):
    package=work/'engine/crates/shared'
    assert helper.run(['cargo','clean','--offline','--locked','-p','migo-shared'],work/f'{label}-clean.log',package,env)==0
    assert helper.run(['cargo','test','--offline','--locked','--lib','--no-run','--message-format=json'],work/f'{label}-build.jsonl',package,env)==0,(label,'compile failure is not red evidence')
    rows=[]
    for line in (work/f'{label}-build.jsonl').read_text().splitlines():
        try:row=json.loads(line)
        except ValueError:continue
        if row.get('reason')=='compiler-artifact'and row.get('executable')and row.get('target',{}).get('name')=='shared':rows.append(row)
    assert len(rows)==1 and not rows[0]['fresh']
    binary=work/f'{label}-tests';shutil.copy2(rows[0]['executable'],binary);return binary

def prepare(work,target,extra_inputs=()):
    assert not work.exists();work.mkdir(parents=True);package=work/'engine/crates/shared';package.mkdir(parents=True)
    original=ROOT/'engine/crates/shared';shutil.copytree(original/'src',package/'src')
    external=['contracts/runtime/domain-patterns.json','contracts/runtime/minigame-api.json','platforms/android/library/src/main/java/com/migo/runtime/ErrorCode.java']
    for rel in external:
        to=work/rel;to.parent.mkdir(parents=True,exist_ok=True);shutil.copy2(ROOT/rel,to)
    root_manifest=(ROOT/'engine/Cargo.toml').read_text()
    manifest=helper.rebase((original/'Cargo.toml').read_text(),original)
    manifest+='\n[workspace]\n[workspace.package]\n'+helper.section(root_manifest,'workspace.package')
    manifest+='\n[workspace.dependencies]\n'+helper.rebase(helper.section(root_manifest,'workspace.dependencies'),ROOT/'engine')
    manifest+='\n[patch.crates-io]\n'+helper.rebase(helper.section(root_manifest,'patch.crates-io'),ROOT/'engine')
    (package/'Cargo.toml').write_text(manifest);(package/'Cargo.lock').write_bytes((ROOT/'engine/Cargo.lock').read_bytes())
    env=os.environ.copy();env.update(RUSTUP_TOOLCHAIN='1.95.0',CARGO_TARGET_DIR=str(target),CARGO_NET_OFFLINE='true',CC='/usr/bin/clang',CXX='/usr/bin/clang++')
    for name in ['ANDROID_NDK','ANDROID_NDK_HOME']:env.pop(name,None)
    assert helper.run(['cargo','metadata','--offline','--format-version','1'],work/'metadata.jsonl',package,env)==0
    selected=json.loads(next(l for l in(work/'metadata.jsonl').read_text().splitlines()if l.startswith('{"packages"')))['packages']
    locked={}
    for row in tomllib.loads((ROOT/'engine/Cargo.lock').read_text())['package']:locked.setdefault(row['name'],set()).add(row['version'])
    assert all(row['name']not in locked or row['version']in locked[row['name']]for row in selected),'existing version drift'
    inputs={Path(__file__).resolve(),ROOT/'scripts/testing/check_audio_cpu_admission.py',ROOT/'engine/Cargo.toml',ROOT/'engine/Cargo.lock',original/'Cargo.toml'}
    inputs.update(ROOT/rel for rel in external)
    inputs.update(p for p in(original/'src').rglob('*')if p.is_file())
    for row in selected:
        directory=Path(row['manifest_path']).parent
        if row.get('source')is None and directory.is_relative_to(ROOT):
            inputs.add(directory/'Cargo.toml');inputs.update(p for p in(directory/'src').rglob('*')if p.is_file())
            if(directory/'build.rs').is_file():inputs.add(directory/'build.rs')
    inputs.update(extra_inputs)
    frozen={str(p.relative_to(ROOT)):sha(p)for p in sorted(inputs)}
    (work/'source-inputs.json').write_text(json.dumps(frozen,indent=2)+'\n')
    for rel in frozen:
        to=work/'source-inputs'/rel;to.parent.mkdir(parents=True,exist_ok=True);shutil.copy2(ROOT/rel,to)
    retained={str(p.relative_to(package)):sha(p)for p in(package/'src').rglob('*')if p.is_file()}
    return env,frozen,retained

def controls(work,env):
    package=work/'engine/crates/shared'
    base='vfs::package::entry_reader::tests::'
    declarations=[
        ('package-path-reopened','package/entry_reader.rs','read_exact_at(&self.inner.file, compressed, chunk.data_offset)?;','read_exact_at(&std::fs::File::open(&self.inner.path)?, compressed, chunk.data_offset)?;','path_replacement_keeps_ranges_digest_and_crc_on_the_open_file'),
        ('encoded-scratch-limit-removed','package/entry_reader.rs','if encoded > limits.max_encoded_chunk || decoded > limits.max_decoded_chunk {','if false || decoded > limits.max_decoded_chunk {','scratch_limits_reject_before_payload_or_decoder_work'),
        ('decoded-scratch-limit-removed','package/entry_reader.rs','if encoded > limits.max_encoded_chunk || decoded > limits.max_decoded_chunk {','if encoded > limits.max_encoded_chunk || false {','scratch_limits_reject_before_payload_or_decoder_work'),
        ('pre-read-stamp-removed','package/entry_reader.rs','        self.verify()?;\n        let mut at = offset;','        let mut at = offset;','mutation_before_cached_read_or_after_fill_never_publishes_success'),
        ('post-read-stamp-removed','package/entry_reader.rs','        self.verify()?;\n        self.failed = false;','        self.failed = false;','mutation_before_cached_read_or_after_fill_never_publishes_success'),
        ('failed-package-reader-reused','package/entry_reader.rs','pub fn read_exact_at(&mut self, offset: u64, output: &mut [u8]) -> Result<(), PackageError> {\n        if self.failed {','pub fn read_exact_at(&mut self, offset: u64, output: &mut [u8]) -> Result<(), PackageError> {\n        if false {','bad_extents_are_sticky_and_never_decode_a_payload'),
        ('cached-chunk-always-decoded','package/entry_reader.rs','if self.cached == Some(i) {','if false {','subranges_reuse_the_last_decompressed_chunk'),
        ('package-range-allocates','package/entry_reader.rs','        let mut at = offset;','        std::hint::black_box(vec![0_u8;64]);\n        let mut at = offset;','range_steps_reuse_scratch_without_steady_rust_heap_allocations'),
        ('decoded-length-not-exact','package/entry_reader.rs','if size != chunk.raw_size as usize {','if false {','short_decoded_chunk_cannot_expose_initialized_scratch_tail'),
        ('trailing-frame-accepted','package/entry_reader.rs','if frame_size != compressed.len() {','if false {','trailing_zstd_frames_are_not_a_single_package_chunk'),
        ('mount-generation-not-checked','mount.rs','if generation != self.generation.load(Ordering::Acquire) {','if false {','admitted_package_backend_survives_mount_replacement_and_rejects_stale_admission'),
        ('pinned-overlay-ignored','mount.rs','let (backend, relative) = match highest_matching_overlay(&inner.overlays, &normalized) {','let (backend, relative) = match None::<(&MountEntry, &str)> {','admitted_package_backend_survives_mount_replacement_and_rejects_stale_admission'),
        ('whole-file-default-fallback','mount.rs','        Err(io::Error::new(\n            io::ErrorKind::Unsupported,\n            "backend has no bounded package reader",','        let _ = self.read(_relative_path)?;\n        Err(io::Error::new(\n            io::ErrorKind::Unsupported,\n            "backend has no bounded package reader",','admitted_package_backend_survives_mount_replacement_and_rejects_stale_admission'),
        ('package-offset-truncated-u32','file_io.rs','file.read_exact_at(buffer, offset)','file.read_exact_at(buffer, u64::from(offset as u32))','package_chunk_offsets_above_u32_are_read_without_truncation'),
    ]
    for name,file,before,after,test in declarations:
        path=package/'src/vfs'/file;source=path.read_text();assert source.count(before)==1,(name,'anchor drift')
        path.write_text(source.replace(before,after))
        try:
            mutant=build(work,env,name);log=work/f'{name}-test.log'
            status=helper.run([str(mutant),'--exact',base+test,'--nocapture'],log,package,env,timeout=30)
            observed=log.read_text();assert status==101 and 'panicked at'in observed and '1 failed'in observed,(name,'must reject executed assertion',status)
        finally:path.write_text(source)
        print('actual package fault',name,': executed assertion rejected',flush=True)
    return len(declarations)

def main(work,target,positive_only):
    env,frozen,retained=prepare(work,target)
    package=work/'engine/crates/shared'
    binary=build(work,env,'positive')
    assert helper.run([str(binary),'--nocapture','--test-threads=1'],work/'positive.log',package,env)==0
    positive=(work/'positive.log').read_text();match=re.search(r'test result: ok\. (\d+) passed; 0 failed; (\d+) ignored;',positive);assert match
    assert positive.count('test vfs::package::entry_reader::tests::')==11
    negative=0 if positive_only else controls(work,env)
    assert all(sha(ROOT/p)==h for p,h in frozen.items()),'Root changed'
    assert all(sha(package/p)==h for p,h in retained.items()),'copy not restored'
    summary={'passed':int(match[1]),'ignored':int(match[2]),'new_package_tests':11,'compiled_controls':negative,'positive_only':positive_only,'binary_sha256':sha(binary),'root_inputs_unchanged':True,'source_copies_restored':True,'existing_dependency_versions_preserved':True,'root_lock_sha256':sha(ROOT/'engine/Cargo.lock'),'scope':'Host package positioned ranges, pinned mounts and persistent Rust scratch; not audio Pack-class integration, spool, native-device or whole-pipeline performance acceptance'}
    (work/'summary.json').write_text(json.dumps(summary,indent=2)+'\n');print(json.dumps(summary,indent=2),flush=True)
if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--output',type=Path,required=True);p.add_argument('--target-dir',type=Path,required=True);p.add_argument('--positive-only',action='store_true');a=p.parse_args();main(a.output.resolve(),a.target_dir.resolve(),a.positive_only)
