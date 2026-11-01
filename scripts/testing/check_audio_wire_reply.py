#!/usr/bin/env python3
"""External sync audio bytes require physical credit through final response release.

Keep shared native freeze/materialize copies in the original process ledger,
including managed materialized backing ownership beyond logical registry release.
"""
from __future__ import annotations
import argparse,json
from pathlib import Path
import check_audio_command_ownership as base
capacity=base.capacity
ROOT=base.ROOT
TEST='audio_thread::decode_pool::tests::actual_decode_pools_share_admission_and_release_before_result_visibility'


def main(work,target,preflight=False):
    package,env,frozen=base.prepare(work,target)
    paths={'wire':package/'local-deps/shared/src/audio_initial_backing.rs','reply':package/'local-deps/shared/src/sync_reply.rs'}
    originals={k:p.read_text() for k,p in paths.items()}
    shared_test='audio_resources_tests::an_adopted_decode_is_frozen_from_the_start_and_plays_without_a_copy'
    response_test=TEST
    physical='        let physical = ledger.try_reserve(len, "AudioBuffer wire reply")?;'
    a=originals['wire'].index('        let mut bytes = bytemuck::allocation::try_zeroed_vec::<u8>(len)')
    z=originals['wire'].index('        debug_assert_eq!',a)
    allocation=originals['wire'][a:z].rstrip()
    rows=[
      ('shared-wire-charge-zero','wire','.try_reserve(len, "AudioBuffer wire reply")','.try_reserve(0, "AudioBuffer wire reply")','wire response omitted final capacity or retained temporary PCM'),
      ('shared-wire-header-unaccounted','wire','.try_reserve(len, "AudioBuffer wire reply")','.try_reserve(len-prefix.len(), "AudioBuffer wire reply")','wire response omitted final capacity or retained temporary PCM'),
      ('shared-wire-owner-discarded','reply','_physical: Some(physical)','_physical: {drop(physical);None}','wire response omitted final capacity or retained temporary PCM'),
      ('shared-wire-commit-omitted','wire','drop(self.commit_owned());','drop(self);',['wire response did not commit writable transition','wire response omitted final capacity or retained temporary PCM']),
      ('shared-wire-endian-reversed','wire','sample.to_le_bytes()','sample.to_be_bytes()','wire response changed complete little endian PCM'),
      ('shared-wire-prefix-omitted','wire','bytes[..prefix.len()].copy_from_slice(prefix);','bytes[..prefix.len()].fill(0);','wire response prefix changed'),
      ('shared-wire-response-shape','wire','.checked_add(self.samples().len()','.checked_add(1+self.samples().len()','wire response shape changed'),
      ('shared-wire-output-before-admission','wire',physical+'\n'+allocation,allocation+'\n'+physical,'wire reply refusal allocated output before physical admission'),
    ]
    for n,k,a,z,_ in rows:assert a!=z and originals[k].count(a)==1,(n,'anchor drift',originals[k].count(a))
    def run_fault(binary,package,env,work,name,test,assertion):
        allowed=assertion if isinstance(assertion,list) else [assertion]
        log=work/(name+'.log')
        code=capacity.product.whole.support.clock.common.run([str(binary),'--exact',test,'--nocapture'],log,package,env,timeout=120)
        (work/(name+'-exit.txt')).write_text(str(code)+'\n')
        output=log.read_text()
        assert code==101 and all(marker in output for marker in ['running 1 test','panicked at','1 failed']),(name,code,log)
        matched=[marker for marker in allowed if marker in output]
        assert matched,(name,allowed,log)
        return dict(label=name,test=test,exit=code,expected_assertion=matched[0],allowed_assertions=allowed)
    programs=[];controls=[];suites={}
    product=capacity.product
    def record(name,binary,pruned=False,kind='audio'):
        h=product.whole.support.codecs.sha(binary);programs.append(dict(name=name,path=str(binary),sha256=h,pruned=pruned,target_name=kind))
        if pruned:binary.unlink()
        return h
    positive=product.src.build(package,work,env,'positive-debug',False);record('positive-debug',positive)
    capacity.profile.verify(work/'positive-debug-build.jsonl',json.loads((work/'debug-profile-policy.json').read_text()),work/'debug-profile-artifacts.json')
    capacity.run(positive,package,env,work,'positive-result',TEST)
    shared=base.build_shared(package,work,env,'shared-debug',False)
    capacity.run(shared,package,env,work,'positive-shared-response',shared_test)
    if not preflight:suites['shared-debug']=base.run_shared(shared,package,env,work,'full-shared-debug')
    record('shared-debug',shared,True,'shared')
    assert product.whole.support.clock.common.run(['cargo','clippy','--lib','--tests','--offline','--locked','--message-format=json'],work/'clippy.jsonl',package,env,timeout=300)==0
    assert product.whole.support.clock.common.run(['cargo','clippy','-p','migo-shared','--lib','--tests','--no-default-features','--features','vfs,codec','--offline','--locked','--message-format=json'],work/'shared-clippy.jsonl',package,env,timeout=300)==0
    if not preflight:
        suites['audio-debug']=capacity.run(positive,package,env,work,'full-debug')
        for name,key,before,after,assertion in rows:
            try:
                paths[key].write_text(originals[key].replace(before,after))
                kind='shared' if name.startswith('shared-') else 'audio'
                binary=base.build_shared(package,work,env,name,False) if kind=='shared' else product.src.build(package,work,env,name,False)
                test=shared_test if kind=='shared' else response_test
                item=run_fault(binary,package,env,work,name,test,assertion);item['binary_sha256']=record(name,binary,True,kind);controls.append(item);(work/'completed-controls.json').write_text(json.dumps(controls,indent=2)+'\n');print('Actual response ownership fault rejected',name,flush=True)
            finally:
                paths[key].write_text(originals[key])
        for mode,release in [('debug',False),('release',True)]:
            if release:
                binary=product.src.build(package,work,env,'positive-release',True);record('positive-release',binary);suites['audio-release']=capacity.run(binary,package,env,work,'full-release')
            if release:
                shared=base.build_shared(package,work,env,'shared-'+mode,release);suites['shared-'+mode]=base.run_shared(shared,package,env,work,'full-shared-'+mode);record('shared-'+mode,shared,True,'shared')
            source,raw=base.core_facet(package,work)
            try:
                binary=product.src.build(package,work,env,'core-facet-'+mode,release);log=work/('core-facet-'+mode+'.log');code=product.whole.support.clock.common.run([str(binary),'core_service_audio_probe::tests::','--nocapture'],log,package,env,timeout=120);assert code==0 and '6 passed; 0 failed;' in log.read_text(),log;(work/('core-facet-'+mode+'-exit.txt')).write_text(str(code)+'\n');record('core-facet-'+mode,binary,True)
            finally:source.write_text(raw)
    product.whole.verify_inputs(work,frozen)
    for k,p in paths.items():assert p.read_text()==originals[k]
    for dep in ['shared','io','services','frame-wire']:
        assert all((package/'local-deps'/dep/p.relative_to(ROOT/'engine/crates'/dep)).read_bytes()==p.read_bytes() for p in (ROOT/'engine/crates'/dep/'src').rglob('*') if p.is_file())
    assert all((package/'src'/p.relative_to(ROOT/'engine/crates/audio/src')).read_bytes()==p.read_bytes() for p in (ROOT/'engine/crates/audio/src').rglob('*') if p.is_file())
    result=dict(programs=programs,controls=controls,suites=suites,preflight=preflight,root_inputs_unchanged=True,source_copies_restored=True,root_dev_profile_verified=True,isolated_release_is_not_root_shipping=True,core_facet='Exact host-audio service/test bodies; cfg selectors and crate alias only; embedded-v8 branches excluded')
    (work/'summary.json').write_text(json.dumps(result,indent=2)+'\n');print('Wire reply custody verified',len(controls),'faults',len(suites),'full suites',flush=True)

if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--output',type=Path,required=True);p.add_argument('--target-dir',type=Path,required=True);p.add_argument('--preflight',action='store_true');a=p.parse_args();main(a.output.resolve(),a.target_dir.resolve(),a.preflight)
