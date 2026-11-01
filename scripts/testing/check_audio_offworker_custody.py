#!/usr/bin/env python3
"""OffWorker cancellation dropped state,captures and computed output on async.

Keep original admission through actual input disposal; preserve original CPU admission and exclusive state
semantics and immediate responses; faults must execute fresh business assertions.
"""
from __future__ import annotations
import argparse,json
from pathlib import Path
import check_audio_command_ownership as base
capacity=base.capacity
ROOT=base.ROOT
TEST='off_worker::tests::successive_steps_keep_the_same_exclusive_value_address'


def main(work,target,preflight=False):
    package,env,frozen=base.prepare(work,target)
    paths={'owner':package/'src/off_worker.rs'}
    originals={k:p.read_text() for k,p in paths.items()}
    def method(text,marker):
        a=text.index(marker);left=text.index('{',a);depth=1;i=left+1
        while depth:depth+=(text[i]=='{')-(text[i]=='}');i+=1
        return text[a:i]
    raw=originals['owner'];owner=method(raw,'impl<T> Drop for OffWorker<T>');step=method(raw,'    pub(crate) fn with<R:');take=method(raw,'    fn take(mut self)')
    call='        let call = Handoff {\n            value: Some((self, step)),\n        };\n        async move {'
    out='                    Handoff {\n                        value: Some((owner, outcome)),\n                    }'
    rows=[
      ('offworker-owner-inline',owner,'impl<T> Drop for OffWorker<T> {fn drop(&mut self){drop(self.value.take());}}','offworker state escaped CPU retirement'),
      ('offworker-retirement-on-async','cpu.retirement().retire(value).await;','drop(value);','offworker state escaped CPU retirement'),
      ('offworker-adjunct-inline','_adjunct: Some(Box::new(adjunct)),','_adjunct: {drop(adjunct);None},','offworker capture or result escaped CPU retirement'),
      ('offworker-unpolled-guard-late',call,'        async move {\n        let call=Handoff{value:Some((self,step))};','offworker capture or result escaped CPU retirement'),
      ('offworker-queued-capture-unguarded',step,step.replace('            let result = cpu','            let (mut owner,step)=call.take();\n            let result = cpu',1).replace('                    let (mut owner, step) = call.take();\n','',1),'offworker capture or result escaped CPU retirement'),
      ('offworker-computed-result-unguarded',step,step.replace(out,'                    (owner,outcome)').replace('Ok(result.take())','Ok(result)'),'offworker capture or result escaped CPU retirement'),
      ('offworker-state-reboxed',take,take.replace('self.value.take().unwrap()','{let(mut owner,p)=self.value.take().unwrap();let value=owner.value.take().unwrap();owner.value=Some(Box::new(*value));(owner,p)}'),'offworker handoff changed exclusive address or mutated state'),
      ('offworker-closed-cleanup-skipped','cpu.retirement().retire(value).await;','if !cpu.is_closed(){cpu.retirement().retire(value).await;}','offworker capture or result escaped CPU retirement'),
      ('offworker-original-error-lost','                .await?;','                .await.map_err(|_|shared::error::EngineError::new(shared::error::ErrorCode::Cancelled))?;','offworker refused step lost original typed error'),
    ]
    for n,a,z,_ in rows:assert a!=z and raw.count(a)==1,(n,'anchor drift',raw.count(a))
    programs=[];controls=[];suites={}
    product=capacity.product
    def record(name,binary,pruned=False,kind='audio'):
        h=product.whole.support.codecs.sha(binary);programs.append(dict(name=name,path=str(binary),sha256=h,pruned=pruned,target_name=kind))
        if pruned:binary.unlink()
        return h
    positive=product.src.build(package,work,env,'positive-debug',False);record('positive-debug',positive)
    capacity.profile.verify(work/'positive-debug-build.jsonl',json.loads((work/'debug-profile-policy.json').read_text()),work/'debug-profile-artifacts.json')
    capacity.run(positive,package,env,work,'positive-offworker',TEST)
    assert product.whole.support.clock.common.run(['cargo','clippy','--lib','--tests','--offline','--locked','--message-format=json'],work/'clippy.jsonl',package,env,timeout=300)==0
    if not preflight:
        suites['audio-debug']=capacity.run(positive,package,env,work,'full-debug')
        for name,before,after,assertion in rows:
            key='owner'
            try:
                paths[key].write_text(originals[key].replace(before,after))
                binary=product.src.build(package,work,env,name,False)
                item=capacity.run(binary,package,env,work,name,TEST,101,assertion);item['binary_sha256']=record(name,binary,True);controls.append(item);(work/'completed-controls.json').write_text(json.dumps(controls,indent=2)+'\n');print('Actual OffWorker custody fault rejected',name,flush=True)
            finally:
                paths[key].write_text(originals[key])
        for mode,release in [('debug',False),('release',True)]:
            if release:
                binary=product.src.build(package,work,env,'positive-release',True);record('positive-release',binary);suites['audio-release']=capacity.run(binary,package,env,work,'full-release')
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
    (work/'summary.json').write_text(json.dumps(result,indent=2)+'\n');print('OffWorker custody verified',len(controls),'faults',len(suites),'full suites',flush=True)

if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--output',type=Path,required=True);p.add_argument('--target-dir',type=Path,required=True);p.add_argument('--preflight',action='store_true');a=p.parse_args();main(a.output.resolve(),a.target_dir.resolve(),a.preflight)
