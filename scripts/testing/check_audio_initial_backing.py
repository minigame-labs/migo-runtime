#!/usr/bin/env python3
"""Initial JS AudioBuffer construction bypassed the physical backing owner.

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
    paths={'initial':package/'local-deps/shared/src/audio_initial_backing.rs','services':package/'local-deps/services/src/audio.rs'}
    originals={k:p.read_text() for k,p in paths.items()}
    response_test=TEST
    shared_test='audio_resources_tests::an_adopted_decode_is_frozen_from_the_start_and_plays_without_a_copy'
    def method(text,marker):
        a=text.index(marker);left=text.index('{',a);depth=1;i=left+1
        while depth:depth+=(text[i]=='{')-(text[i]=='}');i+=1
        return text[a:i]
    prepare=method(originals['initial'],'    pub fn prepare_writable_backing(')
    reservation=method(originals['services'],'pub fn reserve_buffer(')
    owned=method(originals['services'],'pub fn materialize_owned_buffer(')
    raw=method(originals['services'],'pub fn materialize_buffer(')
    physical='        let physical = self\n            .inner\n            .physical\n            .try_reserve(bytes, "initial AudioBuffer")?;'
    planar='        let planar = try_exact_pcm_allocation(checked_sample_count(format)?)?;'
    rows=[
      ('shared-initial-pending-not-armed','initial','self.reserve_backing_for(generation, format, true)','self.reserve_backing_for(generation, format, false)','initial reservation was not routed to native initialization'),
      ('shared-initial-charge-zero','initial','.try_reserve(bytes, "initial AudioBuffer")','.try_reserve(0, "initial AudioBuffer")','initial native allocation was not physically charged'),
      ('shared-initial-owned-credit-left-in-registry','initial','if !owned {','if true {','logical release returned live initial backing credit'),
      ('shared-initial-pending-not-cleared','initial','*initial_pending = false;','*initial_pending = true;','initial backing was initialized more than once'),
      ('shared-initial-repeat-route-bypassed','initial','if !initial {','if false {','initial backing was initialized more than once'),
      ('shared-initial-pcm-shape-zero','initial','try_exact_pcm_allocation(checked_sample_count(format)?)?','try_exact_pcm_allocation(0)?','initial backing shape changed'),
      ('shared-initial-allocation-before-admission','initial',physical+'\n'+planar,planar+'\n'+physical,'initial physical refusal allocated PCM before admission'),
      ('shared-initial-domain-charged-twice','initial','        if !initial {','        let _duplicate=self.inner.process.try_reserve(bytes)?;\n        if !initial {','initial initialization required second process allowance'),
      ('audio-initial-service-pending-unwired','services',reservation,reservation.replace('.reserve_initial_backing(','.reserve_backing('),'embedded initial service did not initialize pending backing'),
      ('audio-initial-owned-service-unwired','services',owned,owned.replace('.prepare_writable_backing(','.prepare_materialize('),'embedded initial service did not initialize pending backing'),
      ('audio-initial-external-service-unwired','services',raw,raw.replace('.prepare_writable_backing(','.prepare_materialize('),'external initial service did not initialize pending backing'),
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
    (work/'summary.json').write_text(json.dumps(result,indent=2)+'\n');print('Initial backing custody verified',len(controls),'faults',len(suites),'full suites',flush=True)

if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--output',type=Path,required=True);p.add_argument('--target-dir',type=Path,required=True);p.add_argument('--preflight',action='store_true');a=p.parse_args();main(a.output.resolve(),a.target_dir.resolve(),a.preflight)
