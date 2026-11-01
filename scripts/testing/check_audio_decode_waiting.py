#!/usr/bin/env python3
"""Waiting decode cancellation used to destroy encoded input on the async worker.

Keep original admission through actual input disposal; preserve pending custody and decode/source
semantics and immediate responses; faults must execute fresh business assertions.
"""
from __future__ import annotations
import argparse,json
from pathlib import Path
import check_audio_command_ownership as base
capacity=base.capacity
ROOT=base.ROOT
TEST='audio_thread::decode_pool::tests::waiting_reply_cancel_returns_encoded_credit_without_an_active_slot'


def main(work,target,preflight=False):
    package,env,frozen=base.prepare(work,target)
    paths={'waiting':package/'src/decode_waiting.rs','pool':package/'src/decode_pool.rs','offworker':package/'src/off_worker.rs'}
    originals={k:p.read_text() for k,p in paths.items()}
    cpu='waiting encoded input escaped CPU retirement'
    credit='waiting pending credit returned before CPU destruction'
    response='waiting cancellation or abort response waited for CPU cleanup'
    rows=[
      ('waiting-input-inline','waiting','if self.sender.try_send(value).is_err()','if {drop(value);false}',cpu),
      ('waiting-input-on-async','waiting','cpu.retirement().retire(batch).await;','drop(batch);',cpu),
      ('waiting-pending-credit-early','waiting','if self.sender.try_send(value).is_err()','if { let mut value=value;drop(value.state.resources.as_mut().unwrap().waiting.take());self.sender.try_send(value).is_err() }',credit),
      ('waiting-byte-credit-early','waiting','if self.sender.try_send(value).is_err()','if { let mut value=value;let p=value.state.resources.as_mut().unwrap().waiting.as_mut().unwrap();p.usage.bytes.fetch_sub(p.bytes,Ordering::AcqRel);p.bytes=0;self.sender.try_send(value).is_err() }'.replace('Ordering::','std::sync::atomic::Ordering::'),credit),
      ('waiting-local-slot-early','waiting','if self.sender.try_send(value).is_err()','if { let mut value=value;drop(value.state.resources.as_mut().unwrap().slot.take());self.sender.try_send(value).is_err() }','in-flight refusal returned local slot before input retirement'),
      ('waiting-input-detached','waiting','if self.sender.try_send(value).is_err()','if { let mut value=value;drop(value.state.input.take());self.sender.try_send(value).is_err() }',cpu),
      ('waiting-cancel-response-delayed','pool','if error.code == ErrorCode::Cancelled && !retired.active.load(Ordering::Acquire)','if false && error.code == ErrorCode::Cancelled && !retired.active.load(Ordering::Acquire)',response),
      ('waiting-inflight-credit-gap','pool','    resources.in_flight = Some(budget.try_reserve()','    drop(resources.waiting.take());\n    resources.in_flight = Some(budget.try_reserve()',credit),
      ('waiting-result-visible-early','pool','            retired.wait().await;\n            if error.code == ErrorCode::Cancelled {','            if error.code == ErrorCode::Cancelled {','waiting failure published before actual input retirement'),
      ('waiting-slot-error-lost','pool','.map_err(|_| EngineError::new(ErrorCode::Disconnected))?;','.map_err(|_| EngineError::new(ErrorCode::InputSaturated))?;','waiting failure changed original typed result'),
      ('waiting-complete-pcm-lost','pool','target,\n            cancel: cancel.clone(),','target:32000,\n            cancel: cancel.clone(),','waiting handoff changed target or complete PCM'),
    ]
    for n,k,a,z,_ in rows:assert a!=z and originals[k].count(a)==1,(n,'anchor drift',originals[k].count(a))
    programs=[];controls=[];suites={}
    product=capacity.product
    def record(name,binary,pruned=False,kind='audio'):
        h=product.whole.support.codecs.sha(binary);programs.append(dict(name=name,path=str(binary),sha256=h,pruned=pruned,target_name=kind))
        if pruned:binary.unlink()
        return h
    positive=product.src.build(package,work,env,'positive-debug',False);record('positive-debug',positive)
    capacity.profile.verify(work/'positive-debug-build.jsonl',json.loads((work/'debug-profile-policy.json').read_text()),work/'debug-profile-artifacts.json')
    capacity.run(positive,package,env,work,'positive-waiting',TEST)
    assert product.whole.support.clock.common.run(['cargo','clippy','--lib','--tests','--offline','--locked','--message-format=json'],work/'clippy.jsonl',package,env,timeout=300)==0
    if not preflight:
        suites['audio-debug']=capacity.run(positive,package,env,work,'full-debug')
        for name,key,before,after,assertion in rows:
            try:
                paths[key].write_text(originals[key].replace(before,after))
                binary=product.src.build(package,work,env,name,False)
                item=capacity.run(binary,package,env,work,name,TEST,101,assertion);item['binary_sha256']=record(name,binary,True);controls.append(item);(work/'completed-controls.json').write_text(json.dumps(controls,indent=2)+'\n');print('Actual waiting decode custody fault rejected',name,flush=True)
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
    (work/'summary.json').write_text(json.dumps(result,indent=2)+'\n');print('Waiting decode custody verified',len(controls),'faults',len(suites),'full suites',flush=True)

if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--output',type=Path,required=True);p.add_argument('--target-dir',type=Path,required=True);p.add_argument('--preflight',action='store_true');a=p.parse_args();main(a.output.resolve(),a.target_dir.resolve(),a.preflight)
