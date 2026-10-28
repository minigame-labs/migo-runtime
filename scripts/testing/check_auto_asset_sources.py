#!/usr/bin/env python3
"""Prove one-generation asset selection and bounded deferred snapshot opening.

Trying directory then package could select different mounts or infer authority
from a missing path. A receiver's timeout alone cannot stop expired queued IO.
Compile actual cloned sources; accept only executed assertion failures.
"""
from __future__ import annotations
import argparse, importlib.util, json, re, shutil, tomllib
from pathlib import Path
ROOT=Path(__file__).resolve().parents[2]
spec=importlib.util.spec_from_file_location('asset_snapshot',ROOT/'scripts/testing/check_source_snapshots.py')
snapshot=importlib.util.module_from_spec(spec);spec.loader.exec_module(snapshot)
writers=snapshot.writers;g=snapshot.g

def declarations():
    pin='    pub fn pin_asset_read('
    step='    fn step_until('
    return [
        ('asset-implicit-package-authority','shared/vfs/mount.rs',pin,'if entry.backend.has_package_reader() {','if true {','shared','vfs::mount::asset_tests::unsupported_backends_are_not_assumed_to_be_packages'),
        ('asset-directory-authority-discarded','shared/vfs/mount.rs',pin,'if let Some(root) = entry.backend.directory_read_root() {','if let Some(root) = None::<Arc<super::read_root::ReadRoot>> {','shared','vfs::mount::asset_tests::automatic_selection_retains_the_original_directory_and_package_across_replacements'),
        ('asset-stale-generation-accepted','shared/vfs/mount.rs',pin,'        self.validate_read_generation(generation)?;','','shared','vfs::mount::asset_tests::overlay_choice_and_generation_are_captured_before_queued_open'),
        ('asset-overlay-ignored','shared/vfs/mount.rs',pin,'highest_matching_overlay(&inner.overlays, &normalized)','None::<(&MountEntry,&str)>','shared','vfs::mount::asset_tests::overlay_choice_and_generation_are_captured_before_queued_open'),
        ('asset-logical-prefix-hidden','shared/vfs/mount.rs',pin,'        super::read_root::validate_relative(Path::new(&normalized)).map_err(io::Error::other)?;','','shared','vfs::mount::asset_tests::unsafe_prefixes_cannot_be_hidden_by_overlay_selection'),
        ('asset-package-opens-base-backend','shared/vfs/mount.rs',pin,'backend: entry.backend.clone(),','backend: inner.base.backend.clone(),','shared','vfs::mount::asset_tests::overlay_choice_and_generation_are_captured_before_queued_open'),
        ('snapshot-queued-open-renews-deadline','io/source_snapshot.rs','async fn snapshot_open_inner(','            check_deadline(deadline)?;\n            check(&worker, &flag)?;','            check(&worker, &flag)?;','io','source_snapshot::tests::expired_queued_open_is_rejected_without_repolling_the_waiter'),
        ('snapshot-step-ignores-expiry','io/source_snapshot.rs',step,'            check_deadline(deadline)?;\n            check(scheduler, cancelled)?;','            check(scheduler, cancelled)?;','io','source_snapshot::tests::copy_deadline_is_checked_between_steps_and_before_final_sealing'),
        ('snapshot-seals-after-expiry','io/source_snapshot.rs',step,'        check_deadline(deadline)?;\n        check(scheduler, cancelled)?;','        check(scheduler, cancelled)?;','io','source_snapshot::tests::copy_deadline_is_checked_between_steps_and_before_final_sealing'),
        ('snapshot-opens-before-invalid-limits','io/source_snapshot.rs','async fn snapshot_open_inner(','''    control_limit(max_control_bytes)?;
    if max_bytes == 0 || !spool_dir.is_absolute() {''','''    if false {''','io','source_snapshot::tests::deferred_opener_runs_on_fs_and_is_not_called_for_invalid_or_unpolled_work'),
        ('reserved-file-discards-credit','io/read_at_file.rs','    pub fn with_budget(','Ok(ChunkPool::new(Some(lease)))','{drop(lease);Ok(ChunkPool::new(None))}','io','read_at_file::tests::reserved_reader_keeps_both_slots_charged_until_the_final_chunk'),
        ('reserved-media-charged-twice','io/read_at_file.rs','    pub fn transient_media_bytes(','if self.pool.0._lease.is_some() {','if false {','io','source_snapshot::tests::byte_credit_follows_the_last_chunk_after_the_reader_is_dropped'),
        ('unreserved-media-never-charged','io/read_at_file.rs','    pub fn transient_media_bytes(','FILE_CHUNK_BYTES as u64','0','io','read_at_file::tests::bounded_controls_rewinds_and_two_media_slots_keep_exact_bytes'),
    ]

def section(text,scope):
    start=text.index(scope);end=text.index('\n    }\n',start)+7 if scope.startswith('    ') else text.index('\n}\n',start)+3
    return start,end,text[start:end]

def validate(crates):
    snapshot.validate_anchors(crates/'io/src')
    for name,relative,scope,before,_,_,_ in declarations():
        crate,file=relative.split('/',1);text=(crates/crate/'src'/file).read_text();_,_,part=section(text,scope)
        assert part.count(before)==1,(name,'anchor drift')

def controls(work,env):
    for name,relative,scope,before,after,target,test in declarations():
        crate,file=relative.split('/',1);path=work/'engine/crates'/crate/'src'/file;original=path.read_text();start,end,part=section(original,scope)
        assert part.count(before)==1,(name,'anchor drift');path.write_text(original[:start]+part.replace(before,after)+original[end:])
        try:
            binary=writers.build(work,env,name,target);log=work/f'{name}-test.log';status=g.run([str(binary),'--exact',test,'--nocapture'],log,work/'engine/crates'/target,env,timeout=40)
            observed=log.read_text();assert status==101 and 'panicked at'in observed and '1 failed'in observed,(name,'require actual executed failure',status)
        finally:path.write_text(original)
        print('actual asset source fault',name,': executed assertion rejected',flush=True)
    return len(declarations())

def main(work,target,positive_only):
    validate(ROOT/'engine/crates');env,frozen,copies=writers.helper.prepare(work,target);validate(work/'engine/crates');env.pop('MIGO_AUDIO_CHECKS_BLESS',None)
    fixture=Path('platforms/apple/WebContent/PerformancePlus/test/fixtures/audio-check-answers.json');(work/fixture).parent.mkdir(parents=True,exist_ok=True);shutil.copy2(ROOT/fixture,work/fixture)
    for path in [Path(__file__).resolve(),ROOT/'scripts/testing/check_source_snapshots.py',ROOT/'scripts/testing/check_visible_file_writers.py',ROOT/'scripts/testing/check_gitignore_contract.py',ROOT/fixture]:
        relative=str(path.relative_to(ROOT));frozen[relative]=g.sha(path);dest=work/'source-inputs'/relative;dest.parent.mkdir(parents=True,exist_ok=True);shutil.copy2(path,dest)
    (work/'source-inputs.json').write_text(json.dumps(frozen,indent=2)+'\n')
    locked={}
    for row in tomllib.loads((ROOT/'engine/Cargo.lock').read_text())['package']:locked.setdefault(row['name'],set()).add(row['version'])
    for crate in ['io','shared']:
        log=work/f'{crate}-metadata.jsonl';assert g.run(['cargo','metadata','--offline','--format-version','1'],log,work/'engine/crates'/crate,env)==0
        selected=json.loads(next(l for l in log.read_text().splitlines()if l.startswith('{"packages"')))['packages'];assert all(r['name']not in locked or r['version']in locked[r['name']]for r in selected)
    suites={}
    for name,crate,minimal in [('shared-positive','shared',False),('shared-minimal-positive','shared',True),('io-positive','io',False),('io-minimal-positive','io',True),('services-positive','services',False)]:
        binary=writers.build(work,env,name,crate,minimal);log=work/f'{name}.log';assert g.run([str(binary),'--nocapture','--test-threads=1'],log,work/'engine/crates'/crate,env)==0
        match=re.search(r'test result: ok\. (\d+) passed; 0 failed; (\d+) ignored;',log.read_text());assert match
        suites[name]={'passed':int(match[1]),'ignored':int(match[2]),'binary_sha256':g.sha(binary)}
    existing=0 if positive_only else snapshot.controls(work,env);count=0 if positive_only else controls(work,env)
    assert all(g.sha(ROOT/p)==h for p,h in frozen.items())and all(g.sha(work/p)==h for p,h in copies.items())
    summary={'suites':suites,'compiled_controls':existing+count,'existing_snapshot_controls':existing,'new_asset_controls':count,'new_root_tests':10,'positive_only':positive_only,'root_inputs_unchanged':True,'source_copies_restored':True,'existing_dependency_versions_preserved':True,'root_lock_sha256':g.sha(ROOT/'engine/Cargo.lock'),'scope':'One-generation explicit bounded asset authority, deferred Fs opening and absolute deadlines, persistent file media credit. Existing21 snapshot controls rerun. Not automatic format/HTTP fallback, product codec cutover/native/device/performance acceptance.'}
    (work/'summary.json').write_text(json.dumps(summary,indent=2)+'\n');print(json.dumps(summary,indent=2),flush=True)

if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--output',type=Path,required=True);p.add_argument('--target-dir',type=Path,required=True);p.add_argument('--positive-only',action='store_true');a=p.parse_args();main(a.output.resolve(),a.target_dir.resolve(),a.positive_only)
