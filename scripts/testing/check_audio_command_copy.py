#!/usr/bin/env python3
"""Borrowed CopyToChannel input still died on audio after success or quota refusal.

Keep original admission through input CPU disposal, preserve copy/COW/error
semantics and immediate responses; faults must execute fresh business assertions.
"""
from __future__ import annotations
import argparse,json
from pathlib import Path
import check_audio_command_ownership as base
capacity=base.capacity
ROOT=base.ROOT
TEST='audio_thread::tests::decode_job_and_result_transports_are_bounded_and_nonblocking'


def main(work,target,preflight=False):
    package,env,frozen=base.prepare(work,target)
    paths={'copy':package/'src/audio_thread_command_copy.rs','inbox':package/'src/command_inbox.rs','channel':package/'local-deps/shared/src/audio_channel.rs'}
    originals={k:p.read_text() for k,p in paths.items()}
    cpu='completed copy input escaped CPU retirement'
    response='copy response changed or waited for CPU retirement'
    cap='data.capacity().saturating_mul(std::mem::size_of::<f32>()) < 4096'
    copy_call='    let result = copy(contexts, *ctx_id, *buffer_id, data, *channel, *start);'
    rows=[
      ('copy-retirement-bypassed','copy','if !delivery.has_queue_permit()','if true || !delivery.has_queue_permit()','bulk copy fell through caller cleanup'),
      ('copy-input-length-charged','copy',cap,cap.replace('capacity()','len()'),'bulk copy fell through caller cleanup'),
      ('copy-input-inline','channel','if permit.is_none() || '+cap+' {','if true {',cpu),
      ('copy-credit-returned-early','channel','            _payload: Box::new(data),\n            _permit: permit,','            _payload: Box::new(data),\n            _permit: None,','completed copy returned original credit before CPU drop'),
      ('copy-byte-credit-returned-early','channel','        let _ = resp.send(result);','        let _ = resp.send(result);\n        let mut permit=permit;\n        if let Some(credit)=permit.as_mut(){credit.shrink_to(0);}','completed copy returned original credit before CPU drop'),
      ('copy-response-closed','channel','        let _ = resp.send(result);','        drop((resp,result));',response),
      ('copy-input-on-async','inbox','cpu.retirement().retire(batch).await;','drop(batch);',cpu),
      ('copy-not-executed','copy',copy_call,'    let result = Ok(());','copy completion changed complete PCM or frozen COW owner'),
      ('copy-channel-and-offset-lost','copy','ctx.copy_to_channel(buffer_id, data, channel, start)','ctx.copy_to_channel(buffer_id, data, 0, 0)','copy completion changed complete PCM or frozen COW owner'),
      ('copy-errors-hidden','copy','Err(error) => Err(error),','Err(_) => Ok(()),',response),
      ('copy-closed-response-skips-effect','copy',copy_call,'    let result = if matches!(delivery.command(), AudioCmd::CopyToChannel{resp,..} if resp.is_closed()) { Ok(()) } else { copy(contexts, *ctx_id, *buffer_id, data, *channel, *start) };','copy completion changed complete PCM or frozen COW owner'),
      ('copy-small-input-offloaded','copy',cap,cap.replace('< 4096','< 0'),'small copy unnecessarily handed to CPU'),
    ]
    for n,k,a,z,_ in rows:assert a!=z and originals[k].count(a)==1,(n,'anchor drift')
    programs=[];controls=[];suites={}
    product=capacity.product
    def record(name,binary,pruned=False,kind='audio'):
        h=product.whole.support.codecs.sha(binary);programs.append(dict(name=name,path=str(binary),sha256=h,pruned=pruned,target_name=kind))
        if pruned:binary.unlink()
        return h
    positive=product.src.build(package,work,env,'positive-debug',False);record('positive-debug',positive)
    capacity.profile.verify(work/'positive-debug-build.jsonl',json.loads((work/'debug-profile-policy.json').read_text()),work/'debug-profile-artifacts.json')
    capacity.run(positive,package,env,work,'positive-copy',TEST)
    assert product.whole.support.clock.common.run(['cargo','clippy','--lib','--tests','--offline','--locked','--message-format=json'],work/'clippy.jsonl',package,env,timeout=300)==0
    if not preflight:
        suites['audio-debug']=capacity.run(positive,package,env,work,'full-debug')
        for name,key,before,after,assertion in rows:
            try:
                paths[key].write_text(originals[key].replace(before,after))
                if name=='copy-small-input-offloaded':
                    assert originals['channel'].count(cap)==1
                    paths['channel'].write_text(originals['channel'].replace(cap,cap.replace('< 4096','< 0')))
                binary=product.src.build(package,work,env,name,False)
                item=capacity.run(binary,package,env,work,name,TEST,101,assertion);item['binary_sha256']=record(name,binary,True);controls.append(item);(work/'completed-controls.json').write_text(json.dumps(controls,indent=2)+'\n');print('Actual copy-input fault rejected',name,flush=True)
            finally:
                paths[key].write_text(originals[key])
                if name=='copy-small-input-offloaded':paths['channel'].write_text(originals['channel'])
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
    (work/'summary.json').write_text(json.dumps(result,indent=2)+'\n');print('Command copy verified',len(controls),'faults',len(suites),'full suites',flush=True)

if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--output',type=Path,required=True);p.add_argument('--target-dir',type=Path,required=True);p.add_argument('--preflight',action='store_true');a=p.parse_args();main(a.output.resolve(),a.target_dir.resolve(),a.preflight)
