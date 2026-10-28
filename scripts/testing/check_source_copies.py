#!/usr/bin/env python3
"""Compile pinned copy, pre-truncation identity and bounded package transfer faults.

The old path reopened/truncated destinations before source admission and
collected an entire package entry. Retain actual objects, keep epochs active
through failed IO, and share the existing chunk decoder and CRC checks.
Compile failures and timeouts never count as rejected runtime regressions.
"""
from __future__ import annotations
import argparse, importlib.util, json, re, tomllib
from pathlib import Path
ROOT=Path(__file__).resolve().parents[2]
def module(name,path):
    spec=importlib.util.spec_from_file_location(name,path);value=importlib.util.module_from_spec(spec);spec.loader.exec_module(value);return value
mutation=module('copy_mutations',ROOT/'scripts/testing/check_file_mutations.py')
shared=mutation.shared;io=mutation.io;sha=shared.sha

def shared_controls(work,env):
    base='vfs::package::entry_reader::copy_tests::'
    cases=[
        ('copy-crc-ignored','package/entry_reader.rs','actual != self.entry.crc32','false',base+'crc_mismatch_poison_is_sticky_even_after_all_bytes_were_written'),
        ('copy-pre-verification-removed','package/entry_reader.rs','self.failed = true;\n        self.verify()?;\n        let mut hasher','self.failed = true;\n        let mut hasher',base+'mutation_before_copy_is_rejected_without_writing_any_output'),
        ('copy-post-verification-removed','package/entry_reader.rs','        self.verify()?;\n        let actual = hasher.finalize();','        let actual = hasher.finalize();',base+'mutation_after_first_decoded_chunk_fails_final_source_verification'),
        ('copy-reader-not-poisoned','package/entry_reader.rs','self.failed = true;\n        self.verify()?;\n        let mut hasher','self.failed = false;\n        self.verify()?;\n        let mut hasher',base+'failed_partial_writer_poison_prevents_read_or_copy_reuse'),
        ('copy-allocates-entry-output','package/entry_reader.rs','        let mut hasher = crc32fast::Hasher::new();','        std::hint::black_box(vec![0_u8;self.entry.raw_size as usize]);\n        let mut hasher = crc32fast::Hasher::new();',base+'streaming_scratch_has_no_entry_sized_or_steady_rust_heap_allocation'),
        ('copy-clones-each-decoded-chunk','package/entry_reader.rs','            let bytes = &self.decoded[..size];','            let copy = self.decoded[..size].to_vec();\n            let bytes = &copy;',base+'streaming_scratch_has_no_entry_sized_or_steady_rust_heap_allocation'),
        ('copy-native-source-cursor-not-reset','file_copy.rs','    source.rewind()?;','    // Deliberately keep the old cursor.','vfs::file_copy::tests::file_copy_preserves_concrete_file_payload_and_resets_source_without_steady_allocation'),
        ('copy-default-collects-whole-backend','mount.rs','''        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "backend has no copy source",''','''        let _ = self.read(_relative_path)?;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "backend has no copy source",''','vfs::mount::copy_tests::unsupported_copy_backend_never_falls_back_to_whole_entry_read'),
        ('copy-backend-opens-under-mount-lock','mount.rs','''    pub fn open_copy_source(&self, relative_path: &str) -> io::Result<CopySource> {
        self.pin_copy_source(relative_path)?.open()''','''    pub fn open_copy_source(&self, relative_path: &str) -> io::Result<CopySource> {
        let _guard = self.inner.read();
        self.pin_copy_source(relative_path)?.open()''','vfs::mount::copy_tests::copy_source_selection_releases_mount_lock_before_open_and_pins_old_backend'),
        ('copy-overlay-selection-ignored','mount.rs','Some((entry, sub)) => (Arc::clone(&entry.backend), sub.to_owned()),','Some((_entry, _sub)) => (Arc::clone(&inner.base.backend), normalized.clone()),','vfs::mount::copy_tests::admitted_copy_keeps_backend_selected_before_a_queued_open'),
    ]
    for name,file,before,after,test in cases:
        path=work/'engine/crates/shared/src/vfs'/file;original=path.read_text();assert original.count(before)==1,(name,'anchor drift');path.write_text(original.replace(before,after))
        try:
            binary=shared.build(work,env,name);log=work/f'{name}-test.log';status=shared.helper.run([str(binary),'--exact',test,'--nocapture'],log,work/'engine/crates/shared',env,timeout=30)
            observed=log.read_text();assert status==101 and 'panicked at'in observed and '1 failed'in observed,(name,'must reject executed assertion',status)
        finally:path.write_text(original)
        print('actual shared copy fault',name,': executed assertion rejected',flush=True)
    return len(cases)

def io_controls(work,env):
    cases=[
        ('copy-open-truncates-before-registration','source_copy.rs','options.write(true).create(true).truncate(false);','options.write(true).create(true).truncate(true);','native_open_precedes_registration_without_truncation_and_source_is_pinned'),
        ('copy-same-file-guard-removed','source_copy.rs','if self.identity == Some(mutations.identity()) {','if false {','aliases_fail_before_any_payload_or_epoch_mutation'),
        ('copy-forgets-package-backing-identity','source_copy.rs','CopySource::Package(reader) => (Some(reader.backing_identity()?), None),','CopySource::Package(_reader) => (None, None),','package_container_and_hardlink_alias_are_rejected_without_damaging_archive'),
        ('copy-registers-another-object','source_copy.rs','let _write = mutations.into_write()?;','let _write = {drop(mutations);FileMutations::for_file(&tempfile::tempfile()?)?.unwrap().into_write()?};','native_copy_revokes_restored_metadata_and_preserves_complete_payload'),
        ('copy-epoch-ends-before-transfer','source_copy.rs','        match &mut self.source {','        drop(_write);\n        match &mut self.source {','stream_transfer_holds_epoch_through_partial_io_failure'),
        ('copy-retains-old-destination-tail','source_copy.rs','        destination.set_len(0)?;','        // Deliberately retain the old tail.','native_open_precedes_registration_without_truncation_and_source_is_pinned'),
        ('copy-ignores-stream-read-failure','source_copy.rs','                io::copy(reader, destination)?;','                let _ = io::copy(reader, destination);','stream_transfer_holds_epoch_through_partial_io_failure'),
        ('copy-drops-source-permissions','source_copy.rs','''        if let Some(permissions) = self.permissions {
            destination.set_permissions(permissions)?;
        }''','        drop(self.permissions);','native_copy_resets_source_and_destination_cursors_and_preserves_permissions'),
        ('copy-keeps-destination-cursor','source_copy.rs','        destination.rewind()?;','        // Deliberately keep destination cursor.','native_copy_resets_source_and_destination_cursors_and_preserves_permissions'),
    ]
    for name,file,before,after,test in cases:
        path=work/'probe/src'/file;original=path.read_text();assert original.count(before)==1,(name,'anchor drift');path.write_text(original.replace(before,after))
        try:
            binary=io.g.build(work/'probe',work,env,name);log=work/f'{name}-test.log';status=io.g.run([str(binary),'--exact','source_copy::tests::'+test,'--nocapture'],log,work/'probe',env,timeout=30)
            observed=log.read_text();assert status==101 and 'panicked at'in observed and '1 failed'in observed,(name,'must reject executed assertion',status)
        finally:path.write_text(original)
        print('actual IO copy fault',name,': executed assertion rejected',flush=True)
    return len(cases)

def api_checks(work,env):
    rows=tomllib.loads((ROOT/'engine/Cargo.lock').read_text())['package'];version=lambda name:next(r['version']for r in rows if r['name']==name)
    windows=mutation.directory.windows_api_check(work,env,['file_io','file_mutation','file_copy'],{'parking_lot':version('parking_lot')})
    probe=work/'apple-api';(probe/'src').mkdir(parents=True)
    origin=work/'engine/crates/shared/src/vfs/file_copy.rs'
    (probe/'src/lib.rs').write_text(f'#[path={json.dumps(str(origin))}] pub mod file_copy;\n')
    (probe/'Cargo.toml').write_text('[package]\nname="migo-file-copy-apple-api"\nversion="0.0.0"\nedition="2024"\n[workspace]\n[dependencies]\nlibc="='+version('libc')+'"\n')
    (probe/'Cargo.lock').write_bytes((ROOT/'engine/Cargo.lock').read_bytes())
    assert shared.helper.run(['cargo','metadata','--offline','--format-version','1'],work/'apple-metadata.jsonl',probe,env)==0
    targets=['aarch64-apple-darwin','aarch64-apple-ios']
    for target in targets:assert shared.helper.run(['cargo','check','--offline','--locked','--lib','--target',target],work/f'apple-{target}-check.log',probe,env)==0
    return windows,targets

def main(work,shared_target,io_target,positive_only):
    work.mkdir(parents=True,exist_ok=False)
    extra=[Path(__file__).resolve(),ROOT/'scripts/testing/check_file_mutations.py',ROOT/'scripts/testing/check_directory_reads.py']
    extra += [ROOT/'engine/crates/services/src'/name for name in ['fs.rs','fs_tests.rs','cursor.rs']]
    sw=work/'shared';env,frozen,copies=shared.prepare(sw,shared_target,extra)
    windows,apple=api_checks(sw,env)
    binary=shared.build(sw,env,'positive');assert shared.helper.run([str(binary),'--nocapture','--test-threads=1'],sw/'positive.log',sw/'engine/crates/shared',env)==0
    positive=(sw/'positive.log').read_text();match=re.search(r'test result: ok\. (\d+) passed; 0 failed; (\d+) ignored;',positive);assert match
    assert positive.count('test vfs::package::entry_reader::copy_tests::')==6 and positive.count('test vfs::mount::copy_tests::')==4 and positive.count('test vfs::file_copy::tests::')==1
    new=0 if positive_only else shared_controls(sw,env);old=0 if positive_only else shared.controls(sw,env)
    assert all(sha(ROOT/p)==h for p,h in frozen.items())and all(sha(sw/'engine/crates/shared'/p)==h for p,h in copies.items())
    s={'passed':int(match[1]),'ignored':int(match[2]),'new_copy_tests':11,'compiled_copy_controls':new,'compiled_existing_range_controls':old,'compiled_controls':new+old,'binary_sha256':sha(binary),'windows_api_targets':windows,'apple_api_targets':apple,'root_inputs_unchanged':True,'source_copies_restored':True}
    (sw/'summary.json').write_text(json.dumps(s,indent=2)+'\n')
    iw=work/'io';ienv,inputs,retained=io.prepare(iw,io_target,extra)
    binary=io.g.build(iw/'probe',iw,ienv,'positive');assert io.g.run([str(binary),'--nocapture','--test-threads=1'],iw/'positive.log',iw/'probe',ienv)==0
    observed=(iw/'positive.log').read_text();match=re.search(r'test result: ok\. (\d+) passed; 0 failed; (\d+) ignored;',observed);assert match and observed.count('test source_copy::tests::')==9
    count=0 if positive_only else io_controls(iw,ienv)
    assert all(sha(ROOT/p)==h for p,h in inputs.items())and all(sha(iw/'probe'/p)==h for p,h in retained.items())
    selected=json.loads(next(line for line in(iw/'metadata.jsonl').read_text().splitlines()if line.startswith('{"packages"')))['packages'];locked={}
    for row in tomllib.loads((ROOT/'engine/Cargo.lock').read_text())['package']:locked.setdefault(row['name'],set()).add(row['version'])
    assert all(row['name']not in locked or row['version']in locked[row['name']]for row in selected)
    i={'passed':int(match[1]),'ignored':int(match[2]),'new_copy_tests':9,'compiled_controls':count,'binary_sha256':sha(binary),'root_inputs_unchanged':True,'source_copies_restored':True,'existing_dependency_versions_preserved':True}
    (iw/'summary.json').write_text(json.dumps(i,indent=2)+'\n')
    summary={'shared':s,'io':i,'positive_only':positive_only,'compiled_controls':new+old+count,'root_lock_sha256':sha(ROOT/'engine/Cargo.lock'),'scope':'Pinned source/destination identity, epoch through failed IO, bounded package CRC copy and shared range regression. Native File helper Windows/Apple API check only. No arbitrary-host writable snapshot, Windows CopyFile offload/APFS clone equivalence, native/device/peak-performance or product codec cutover acceptance.'}
    (work/'summary.json').write_text(json.dumps(summary,indent=2)+'\n');print(json.dumps(summary,indent=2),flush=True)
if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--output',type=Path,required=True);p.add_argument('--shared-target-dir',type=Path,required=True);p.add_argument('--io-target-dir',type=Path,required=True);p.add_argument('--positive-only',action='store_true');a=p.parse_args();main(a.output.resolve(),a.shared_target_dir.resolve(),a.io_target_dir.resolve(),a.positive_only)
