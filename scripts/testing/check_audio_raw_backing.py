#!/usr/bin/env python3
"""Raw planar backing must keep physical credit through the actual allocation owner.

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
    paths={'frozen':package/'local-deps/shared/src/audio_resources.rs','initial':package/'local-deps/shared/src/audio_initial_backing.rs','service':package/'local-deps/services/src/audio.rs'}
    originals={k:p.read_text() for k,p in paths.items()}
    shared_test='audio_resources_tests::an_adopted_decode_is_frozen_from_the_start_and_plays_without_a_copy'
    response_test=TEST
    body='    pub fn commit(self) -> AudioPlanarBacking {\n        self.commit_owned()\n    }'
    lost='    pub fn commit(self) -> AudioPlanarBacking {let mut backing=self.commit_owned();backing._physical.take();backing}'
    copied='    pub fn commit(self) -> AudioPlanarBacking {let mut backing=self.commit_owned();backing.samples=backing.samples.clone();backing}'
    frozen_registry='    pub fn commit(self) -> AudioPlanarBacking {let registry=self.registry.clone();let key=self.key;let mut backing=self.commit_owned();{let mut state=registry.state.lock();if let BackingState::Writable{_process_permit,..}=&mut state.entries.get_mut(&key).unwrap().state{_process_permit.physical=backing._physical.take();}}backing}'
    initial_registry='    pub fn commit(self) -> AudioPlanarBacking {let(registry,key)=match &self{Self::Initial(p)=>(p.registry.clone(),p.key),Self::Materialized(p)=>(p.registry.clone(),p.key)};let mut backing=self.commit_owned();{let mut state=registry.state.lock();if let BackingState::Writable{_process_permit,..}=&mut state.entries.get_mut(&key).unwrap().state{_process_permit.physical=backing._physical.take();}}backing}'
    frozen_twice='    pub fn commit(self) -> AudioPlanarBacking {let _duplicate=self.registry.process.try_reserve(self.planar.len()*4).unwrap();self.commit_owned()}'
    initial_twice='    pub fn commit(self) -> AudioPlanarBacking {let(registry,bytes)=match &self{Self::Initial(p)=>(p.registry.clone(),p.planar.len()*4),Self::Materialized(p)=>(p.registry.clone(),p.samples().len()*4)};let _duplicate=registry.process.try_reserve(bytes).unwrap();self.commit_owned()}'
    start=originals['service'].index('pub fn materialize_buffer(');end=originals['service'].index('/// Prepare without committing',start);raw=originals['service'][start:end]
    rows=[
      ('shared-raw-frozen-discarded-credit','frozen',body,lost,'raw commit lost allocation credit'),
      ('shared-raw-initial-discarded-credit','initial',body,lost,['raw initial credit ended at logical release','raw commit lost allocation credit']),
      ('shared-raw-frozen-registry-credit','frozen',body,frozen_registry,'raw backing credit ended at logical release'),
      ('shared-raw-initial-registry-credit','initial',body,initial_registry,'raw initial credit ended at logical release'),
      ('shared-raw-frozen-copied','frozen',body,copied,'raw materialization copied PCM allocation'),
      ('shared-raw-initial-copied','initial',body,copied,'raw materialization copied PCM allocation'),
      ('shared-raw-mutable-slice-offset','frozen','fn deref_mut(&mut self) -> &mut Self::Target {\n        &mut self.samples\n    }','fn deref_mut(&mut self) -> &mut Self::Target {\n        &mut self.samples[1..]\n    }','raw mutable slice changed the wrong sample'),
      ('audio-raw-service-initial-route','service',raw,raw.replace('.prepare_writable_backing(','.prepare_materialize('),['external initial service did not initialize pending backing','embedded initial service did not initialize pending backing']),
      ('shared-raw-frozen-duplicate-logical','frozen',body,frozen_twice,'raw commit required additional logical allowance'),
      ('shared-raw-initial-duplicate-logical','initial',body,initial_twice,'raw commit required additional logical allowance'),
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
    (work/'summary.json').write_text(json.dumps(result,indent=2)+'\n');print('Raw backing custody verified',len(controls),'faults',len(suites),'full suites',flush=True)

if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--output',type=Path,required=True);p.add_argument('--target-dir',type=Path,required=True);p.add_argument('--preflight',action='store_true');a=p.parse_args();main(a.output.resolve(),a.target_dir.resolve(),a.preflight)
