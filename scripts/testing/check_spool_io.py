#!/usr/bin/env python3
"""Compile actual private-spool IO and disk-ledger faults in an isolated crate.

A byte cap alone does not bound empty files, and credit cannot end before the
anonymous file's last read owner. Failed/incomplete writers never expose reads.
"""
from __future__ import annotations
import argparse,importlib.util,json,re
from pathlib import Path
ROOT=Path(__file__).resolve().parents[2]
spec=importlib.util.spec_from_file_location('package_io',ROOT/'scripts/testing/check_audio_package_io.py');base=importlib.util.module_from_spec(spec);spec.loader.exec_module(base);g=base.g

def main(work,target,positive_only):
    env,frozen,retained=base.prepare(work,target,[Path(__file__).resolve()]);package=work/'probe'
    binary=g.build(package,work,env,'positive');assert g.run([str(binary),'--nocapture','--test-threads=1'],work/'positive.log',package,env)==0
    positive=(work/'positive.log').read_text();match=re.search(r'test result: ok\. (\d+) passed; 0 failed; (\d+) ignored;',positive);assert match;assert positive.count('test spool::tests::')==7
    reservation='''        let ticket = budget.reserve(expected.unwrap_or(max_bytes))?;
        std::fs::create_dir_all(dir)?;
        let file = tempfile::tempfile_in(dir)?;'''
    declarations=[
        ('spool-creates-before-disk-credit',reservation,'''        std::fs::create_dir_all(dir)?;
        let file = tempfile::tempfile_in(dir)?;
        let ticket = budget.reserve(expected.unwrap_or(max_bytes))?;''','quota_and_header_limit_reject_before_filesystem_work'),
        ('spool-header-object-bound-removed','max_bytes == 0 || expected.is_some_and(|n| n > max_bytes)','max_bytes == 0','quota_and_header_limit_reject_before_filesystem_work'),
        ('spool-disk-byte-cap-removed','total > self.0.max_bytes || used.files >= self.0.max_files','false || used.files >= self.0.max_files','quota_and_header_limit_reject_before_filesystem_work'),
        ('spool-empty-file-slots-unbounded','total > self.0.max_bytes || used.files >= self.0.max_files','total > self.0.max_bytes || false','empty_spools_still_hold_a_file_slot_until_the_last_reader_owner'),
        ('spool-append-object-bound-removed','|| next > self.max_bytes','|| false','size_chunk_and_partial_write_failures_are_sticky_and_never_seal'),
        ('spool-append-chunk-bound-removed','bytes.len() > FILE_CHUNK_BYTES','false','size_chunk_and_partial_write_failures_are_sticky_and_never_seal'),
        ('spool-failed-writer-reused','        if self.failed {','        if false {','size_chunk_and_partial_write_failures_are_sticky_and_never_seal'),
        ('spool-incomplete-writer-sealed','self.failed || self.expected.is_some_and(|n| n != self.written)','self.failed','size_chunk_and_partial_write_failures_are_sticky_and_never_seal'),
        ('spool-sealed-reader-loses-disk-credit','_ticket: self.ticket,','_ticket: {drop(self.ticket);SpoolBudget::new(1,1)?.reserve(0)?},','empty_spools_still_hold_a_file_slot_until_the_last_reader_owner'),
        ('spool-written-offset-overflows','''        let next = self
            .written
            .checked_add(bytes.len() as u64)
            .ok_or_else(|| invalid("spool length overflow"))?;''','''        let next = self.written.wrapping_add(bytes.len() as u64);''','checked_lengths_reject_overflow_before_native_write_or_quota_change'),
    ]
    if not positive_only:
        for name,before,after,test in declarations:
            path=package/'src/spool.rs';source=path.read_text();assert source.count(before)==1,(name,'anchor drift');path.write_text(source.replace(before,after))
            try:
                mutant=g.build(package,work,env,name);log=work/f'{name}-test.log';status=g.run([str(mutant),'--exact','spool::tests::'+test,'--nocapture'],log,package,env,timeout=30);observed=log.read_text();assert status==101 and 'panicked at'in observed and '1 failed'in observed,(name,'must reject executed assertion',status)
            finally:path.write_text(source)
            print('actual spool IO fault',name,': executed assertion rejected',flush=True)
    assert all(g.sha(ROOT/p)==h for p,h in frozen.items()),'Root changed';assert all(g.sha(package/p)==h for p,h in retained.items()),'source not restored'
    summary={'passed':int(match[1]),'ignored':int(match[2]),'spool_tests':7,'compiled_controls':0 if positive_only else len(declarations),'positive_only':positive_only,'binary_sha256':g.sha(binary),'root_inputs_unchanged':True,'source_copies_restored':True,'root_lock_sha256':g.sha(ROOT/'engine/Cargo.lock'),'scope':'Host anonymous spool/disk byte+file quotas/sealed reader lifecycle; no HTTP, crash recovery, writable-file atomic snapshot, native/device or peak-performance acceptance'}
    (work/'summary.json').write_text(json.dumps(summary,indent=2)+'\n');print(json.dumps(summary,indent=2),flush=True)
if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--output',type=Path,required=True);p.add_argument('--target-dir',type=Path,required=True);p.add_argument('--positive-only',action='store_true');a=p.parse_args();main(a.output.resolve(),a.target_dir.resolve(),a.positive_only)
