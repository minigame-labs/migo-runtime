#!/usr/bin/env python3
"""Compile cooperative native-object mutation epochs and managed writer faults.

Length/mtime can be restored after a write. Readers need an epoch shared by
native identity, and destructive opens must enrol before truncation. This gate
does not claim complete writer coverage or a writable audio snapshot product.
"""
from __future__ import annotations
import argparse,importlib.util,json,re,tomllib
from pathlib import Path
ROOT=Path(__file__).resolve().parents[2]
def module(name,path):
    spec=importlib.util.spec_from_file_location(name,path);value=importlib.util.module_from_spec(spec);spec.loader.exec_module(value);return value
directory=module('mutation_directory',ROOT/'scripts/testing/check_directory_reads.py');shared=directory.package
io=module('mutation_io',ROOT/'scripts/testing/check_audio_package_io.py');sha=shared.sha

def shared_controls(work,env):
    declarations=[
        ('mutation-revision-discarded','file_mutation.rs','state.revision = revision;','state.revision = 0;','active_and_completed_writes_are_visible_even_with_restored_metadata'),
        ('mutation-active-reader-accepted','file_mutation.rs','if state.writers != 0 {','if false {','active_and_completed_writes_are_visible_even_with_restored_metadata'),
        ('mutation-verified-revision-ignored','file_mutation.rs','|| state.revision != self.revision','|| false','active_and_completed_writes_are_visible_even_with_restored_metadata'),
        ('mutation-registry-sharing-bypassed','file_mutation.rs','match registry.get(&identity).and_then(Weak::upgrade) {','match None::<Arc<Tracked>> {','independent_handles_share_one_tracker_but_other_objects_do_not'),
        ('mutation-dead-weak-entries-retained','file_mutation.rs','registry.remove(&self.identity);','// Deliberately retain the dead weak entry.','reader_epochs_keep_the_registration_alive_after_original_tracker_drop'),
        ('mutation-old-drop-removes-new-registration','file_mutation.rs','std::ptr::eq(entry.as_ptr(), self)','true','old_destructor_cannot_remove_a_new_registration_for_the_same_object'),
        ('mutation-borrowed-writer-never-finishes','file_mutation.rs',"impl Drop for WriteGuard<'_> {\n    fn drop(&mut self) {\n        self.0.state.lock().writers -= 1;","impl Drop for WriteGuard<'_> {\n    fn drop(&mut self) {\n        // Deliberately keep the writer active.",'overlapping_writers_and_failed_attempts_revoke_existing_epochs'),
        ('mutation-owned-writer-never-finishes','file_mutation.rs','impl Drop for OwnedWriteGuard {\n    fn drop(&mut self) {\n        self.0.state.lock().writers -= 1;','impl Drop for OwnedWriteGuard {\n    fn drop(&mut self) {\n        // Deliberately keep the writer active.','owned_write_guard_moves_registration_until_the_last_operation_owner_drops'),
        ('mutation-revision-overflows','file_mutation.rs','state.revision.checked_add(1)','Some(state.revision.wrapping_add(1))','revision_and_writer_overflow_poison_without_wrapping_or_native_io'),
        ('mutation-writer-count-overflows','file_mutation.rs','state.writers.checked_add(1)','Some(state.writers.wrapping_add(1))','revision_and_writer_overflow_poison_without_wrapping_or_native_io'),
        ('mutation-steady-writer-allocates','file_mutation.rs',"pub fn begin_write(&self) -> io::Result<WriteGuard<'_>> {","pub fn begin_write(&self) -> io::Result<WriteGuard<'_>> {\n        std::hint::black_box(vec![0_u8;64]);",'retained_write_and_read_epochs_have_no_steady_rust_heap_allocation'),
        ('mutation-native-object-id-collapsed','file_io.rs','unix: (meta.dev(), meta.ino()),','unix: (meta.dev(), 0),','native_identity_is_object_based_across_handles_rename_and_replacement'),
    ]
    for name,file,before,after,test in declarations:
        path=work/'engine/crates/shared/src/vfs'/file;original=path.read_text();assert original.count(before)==1,(name,'anchor drift');path.write_text(original.replace(before,after))
        try:
            binary=shared.build(work,env,name);log=work/f'{name}-test.log';status=shared.helper.run([str(binary),'--exact','vfs::file_mutation::tests::'+test,'--nocapture'],log,work/'engine/crates/shared',env,timeout=30)
            observed=log.read_text();assert status==101 and 'panicked at'in observed and '1 failed'in observed,(name,'must reject executed assertion',status)
        finally:path.write_text(original)
        print('actual mutation ledger fault',name,': executed assertion rejected',flush=True)
    return len(declarations)

def io_controls(work,env):
    path=work/'probe/src/fs_ops.rs';body=path.read_text()
    retained='''            let _write = self
                .mutations
                .as_ref()
                .map(FileMutations::begin_write)
                .transpose()
                .map_err(io_err)?;'''
    declarations=[
        ('mutation-fd-write-untracked','fs_ops.rs',retained+'\n            self.file.write_all(data).map_err(io_err)?;','            self.file.write_all(data).map_err(io_err)?;','descriptor_mutations_coordinate_across_independent_domains'),
        ('mutation-fd-truncate-untracked','fs_ops.rs',retained+'\n            self.file.set_len(len).map_err(io_err)?;','            self.file.set_len(len).map_err(io_err)?;','descriptor_mutations_coordinate_across_independent_domains'),
        ('mutation-open-truncates-before-registration','fs_ops.rs','opts.write(true).create(true).truncate(false);','opts.write(true).create(true).truncate(true);','truncating_open_preserves_bytes_until_registration_then_revokes_the_epoch'),
        ('mutation-readwrite-open-truncates-before-registration','fs_ops.rs','opts.read(true).write(true).create(true).truncate(false);','opts.read(true).write(true).create(true).truncate(true);','truncating_open_preserves_bytes_until_registration_then_revokes_the_epoch'),
        ('mutation-writable-fd-not-registered','fs_ops.rs','FileMutations::for_file(&file).map_err(io_err)?','None','descriptor_mutations_coordinate_across_independent_domains'),
        ('mutation-fast-overwrite-untracked','fs_ops.rs','let write = begin_file_write(&file)?;','let write = None::<OwnedWriteGuard>;','path_fast_overwrite_and_durable_or_fast_append_revoke_the_right_object'),
        ('mutation-path-append-untracked','fs_ops.rs','let _write = begin_file_write(&file).map_err(io_err)?;','// Deliberately omit append registration.','path_fast_overwrite_and_durable_or_fast_append_revoke_the_right_object'),
        ('mutation-failed-fd-writes-not-counted','fs_ops.rs',retained+'\n            self.file.write_all(data).map_err(io_err)?;','''            self.file.write_all(data).map_err(io_err)?;
'''+retained,'native_write_and_truncate_failures_revoke_but_readonly_opens_do_not'),
        ('mutation-atomic-byte-writer-untracked','atomic_write.rs','''        let _write = crate::fs_ops::begin_file_write(&f)?;
        f.set_len(0)?;
        f.write_all(bytes)?;''','''        f.set_len(0)?;
        f.write_all(bytes)?;''','ATOMIC:byte_writer_tracks_an_already_open_temporary_object'),
        ('mutation-atomic-closure-writer-untracked','atomic_write.rs','''        let _write = crate::fs_ops::begin_file_write(&f)?;
        f.set_len(0)?;
        let value = writer(&mut f)?;''','''        f.set_len(0)?;
        let value = writer(&mut f)?;''','atomic_writer_enrols_its_temporary_object_before_calling_the_writer'),
    ]
    for name,file,before,after,test in declarations:
        path=work/'probe/src'/file;original=path.read_text();assert original.count(before)==1,(name,'anchor drift');path.write_text(original.replace(before,after))
        try:
            binary=io.g.build(work/'probe',work,env,name);log=work/f'{name}-test.log';prefix='atomic_write::tests::'if test.startswith('ATOMIC:')else'fs_ops::mutation_tests::';status=io.g.run([str(binary),'--exact',prefix+test.removeprefix('ATOMIC:'),'--nocapture'],log,work/'probe',env,timeout=30)
            observed=log.read_text();assert status==101 and 'panicked at'in observed and '1 failed'in observed,(name,'must reject executed assertion',status)
        finally:path.write_text(original)
        print('actual managed writer fault',name,': executed assertion rejected',flush=True)
    return len(declarations)

def main(work,shared_target,io_target,positive_only):
    work.mkdir(parents=True,exist_ok=False)
    extra=[Path(__file__).resolve(),ROOT/'scripts/testing/check_directory_reads.py']
    sw=work/'shared';env,frozen,retained=shared.prepare(sw,shared_target,extra)
    version=next(r['version']for r in tomllib.loads((ROOT/'engine/Cargo.lock').read_text())['package']if r['name']=='parking_lot')
    targets=directory.windows_api_check(sw,env,['file_io','file_mutation'],{'parking_lot':version})
    binary=shared.build(sw,env,'positive');assert shared.helper.run([str(binary),'--nocapture','--test-threads=1'],sw/'positive.log',sw/'engine/crates/shared',env)==0
    match=re.search(r'test result: ok\. (\d+) passed; 0 failed; (\d+) ignored;',(sw/'positive.log').read_text());assert match and (sw/'positive.log').read_text().count('test vfs::file_mutation::tests::')==10
    shared_count=0 if positive_only else shared_controls(sw,env)
    assert all(sha(ROOT/p)==h for p,h in frozen.items());assert all(sha(sw/'engine/crates/shared'/p)==h for p,h in retained.items())
    s={'passed':int(match[1]),'ignored':int(match[2]),'mutation_tests':10,'compiled_controls':shared_count,'binary_sha256':sha(binary),'root_inputs_unchanged':True,'source_copies_restored':True,'windows_api_check_targets':targets,'existing_dependency_versions_preserved':True}
    (sw/'summary.json').write_text(json.dumps(s,indent=2)+'\n')
    iw=work/'io';ienv,inputs,copies=io.prepare(iw,io_target,extra)
    selected=json.loads(next(l for l in(iw/'metadata.jsonl').read_text().splitlines()if l.startswith('{"packages"')))['packages'];locked={}
    for row in tomllib.loads((ROOT/'engine/Cargo.lock').read_text())['package']:locked.setdefault(row['name'],set()).add(row['version'])
    assert all(row['name']not in locked or row['version']in locked[row['name']]for row in selected),'IO probe existing version drift'
    binary=io.g.build(iw/'probe',iw,ienv,'positive');assert io.g.run([str(binary),'--nocapture','--test-threads=1'],iw/'positive.log',iw/'probe',ienv)==0
    match=re.search(r'test result: ok\. (\d+) passed; 0 failed; (\d+) ignored;',(iw/'positive.log').read_text());assert match and (iw/'positive.log').read_text().count('test fs_ops::mutation_tests::')==9
    assert 'test atomic_write::tests::byte_writer_tracks_an_already_open_temporary_object ... ok'in(iw/'positive.log').read_text()
    io_count=0 if positive_only else io_controls(iw,ienv)
    assert all(sha(ROOT/p)==h for p,h in inputs.items());assert all(sha(iw/'probe'/p)==h for p,h in copies.items())
    i={'passed':int(match[1]),'ignored':int(match[2]),'writer_tests':10,'compiled_controls':io_count,'binary_sha256':sha(binary),'root_inputs_unchanged':True,'source_copies_restored':True,'existing_dependency_versions_preserved':True}
    (iw/'summary.json').write_text(json.dumps(i,indent=2)+'\n')
    summary={'shared':s,'io':i,'compiled_controls':shared_count+io_count,'positive_only':positive_only,'root_lock_sha256':sha(ROOT/'engine/Cargo.lock'),'scope':'Cooperative per-native-object mutation ledger and FD/pathfast/append/atomictemporary writer hooks only. Ordinary copy/packcopy/otherwriters and actualwritablesnapshot/source dispatch remain pending. Cross-FS stage reuses the atomic writer; ZIP has its own output gate. Windows sourceAPI check only; no native/device/performance acceptance.'}
    (work/'summary.json').write_text(json.dumps(summary,indent=2)+'\n');print(json.dumps(summary,indent=2),flush=True)
if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--output',type=Path,required=True);p.add_argument('--shared-target-dir',type=Path,required=True);p.add_argument('--io-target-dir',type=Path,required=True);p.add_argument('--positive-only',action='store_true');a=p.parse_args();main(a.output.resolve(),a.shared_target_dir.resolve(),a.io_target_dir.resolve(),a.positive_only)
