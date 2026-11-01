#!/usr/bin/env python3
"""Synchronous decoder refusal must return encoded storage to the original ticket.

Keep original admission through actual input disposal; preserve decode/source
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
    paths={'dispatch':package/'src/audio_thread_decode_rejection.rs','pool':package/'src/decode_pool.rs','actor':package/'src/audio_thread.rs','inbox':package/'src/command_inbox.rs','channel':package/'local-deps/shared/src/audio_channel.rs'}
    originals={k:p.read_text() for k,p in paths.items()}
    cpu='refused decode input escaped CPU retirement'
    credit='decode refusal returned transport credit before CPU drop'
    response='decode refusal changed typed response or waited for CPU drop'
    target='decode admission changed target rate or complete PCM'
    selector='        || !matches!(\n            delivery.command(),\n            AudioCmd::DecodeAudioData { .. }\n                | AudioCmd::InnerAudioLoad { .. }\n                | AudioCmd::InnerAudioCompleteLoad { .. }\n        )'
    cap='delivery.command().queued_payload_bytes() < 4096'
    bridge='            Self { command, permit }.into_rejected_payload(error)'
    cancel='        if job.is_cancelled() {\n            return Some((job, ErrorCode::Cancelled, "audio decode request cancelled"));'
    start='        if job.is_cancelled() {\n            self.pool.get_or_insert_with(|| DecodePool::new(DECODE_POOL_SIZE,self.result_tx.clone(),self.wakeup.clone(),self.cpu.clone()));\n            return Some((job, ErrorCode::Cancelled, "audio decode request cancelled"));'
    rows=[
      ('decode-owned-dispatch-bypassed','dispatch','if !delivery.has_queue_permit()','if true || !delivery.has_queue_permit()','bulk decode refusal bypassed owned dispatch'),
      ('decode-length-used','dispatch',cap,'matches!(delivery.command(),AudioCmd::DecodeAudioData{data,..} if data.len()<4096) || '+cap,'bulk decode refusal bypassed owned dispatch'),
      ('decode-input-inline','channel',bridge,'            { drop((command,permit,error)); None }',cpu),
      ('decode-input-on-async','inbox','cpu.retirement().retire(batch).await;','drop(batch);',cpu),
      ('decode-item-credit-early','channel',bridge,'            { let mut p=Self {command,permit}.into_rejected_payload(error); if let Some(v)=p.as_mut(){drop(v._permit.take());} p }',credit),
      ('decode-byte-credit-early','channel',bridge,'            { let mut permit=permit; if let Some(p)=permit.as_mut(){p.shrink_to(0);} Self {command,permit}.into_rejected_payload(error) }',credit),
      ('decode-full-response-lost','pool','(ErrorCode::InputSaturated, DECODE_JOB_QUEUE_FULL_DETAIL)','(ErrorCode::InputSaturated, DECODE_JOB_BYTE_LIMIT_DETAIL)',response),
      ('decode-byte-response-lost','pool','(ErrorCode::InputSaturated, DECODE_JOB_BYTE_LIMIT_DETAIL)','(ErrorCode::InputSaturated, DECODE_JOB_QUEUE_FULL_DETAIL)',response),
      ('decode-closed-response-lost','pool','(ErrorCode::Disconnected, DECODE_QUEUE_DISCONNECTED_DETAIL)','(ErrorCode::InputSaturated, DECODE_QUEUE_DISCONNECTED_DETAIL)',response),
      ('decode-cancel-starts-pool','actor',cancel,start,'refused decode mutated decoder admission or published a result'),
      ('decode-context-rate-lost','dispatch','target_rate: context.sample_rate(),','target_rate: pool.sample_rate,',target),
      ('decode-device-rate-lost','dispatch','target_rate: pool.sample_rate,','target_rate: 32000,',target),
      ('decode-owned-input-cloned','dispatch','target_rate: pool.sample_rate,\n                    data,','target_rate: pool.sample_rate,\n                    data: data.clone(),',cpu),
      ('decode-generation-error-lost','dispatch','Err(error) => return Some((AudioCmd::InnerAudioLoad { id, data, resp }, error)),','Err(_error) => return Some((AudioCmd::InnerAudioLoad { id, data, resp }, EngineError::from_detail(ErrorCode::Disconnected,"lost"))),','generation failure changed response or started decoder'),
      ('decode-small-offloaded','dispatch',cap,cap.replace('< 4096','< 0'),'decode refusal small or legacy dispatch changed'),
      ('decode-unrelated-selected','dispatch',selector,'        || false','decode completion requires a whole-file command'),
    ]
    for n,k,a,z,_ in rows:
        assert a!=z and originals[k].count(a)==(2 if n in ['decode-device-rate-lost','decode-owned-input-cloned'] else 1),(n,'anchor drift',originals[k].count(a))
    programs=[];controls=[];suites={}
    product=capacity.product
    def record(name,binary,pruned=False,kind='audio'):
        h=product.whole.support.codecs.sha(binary);programs.append(dict(name=name,path=str(binary),sha256=h,pruned=pruned,target_name=kind))
        if pruned:binary.unlink()
        return h
    positive=product.src.build(package,work,env,'positive-debug',False);record('positive-debug',positive)
    capacity.profile.verify(work/'positive-debug-build.jsonl',json.loads((work/'debug-profile-policy.json').read_text()),work/'debug-profile-artifacts.json')
    capacity.run(positive,package,env,work,'positive-decode',TEST)
    assert product.whole.support.clock.common.run(['cargo','clippy','--lib','--tests','--offline','--locked','--message-format=json'],work/'clippy.jsonl',package,env,timeout=300)==0
    if not preflight:
        suites['audio-debug']=capacity.run(positive,package,env,work,'full-debug')
        for name,key,before,after,assertion in rows:
            try:
                paths[key].write_text(originals[key].replace(before,after))
                if name=='decode-small-offloaded':
                    assert originals['channel'].count('if permit.is_none() || bytes < 4096 {')==1
                    paths['channel'].write_text(originals['channel'].replace('if permit.is_none() || bytes < 4096 {','if permit.is_none() || bytes == 0 {'))
                binary=product.src.build(package,work,env,name,False)
                item=capacity.run(binary,package,env,work,name,TEST,101,assertion);item['binary_sha256']=record(name,binary,True);controls.append(item);(work/'completed-controls.json').write_text(json.dumps(controls,indent=2)+'\n');print('Actual synchronous decode refusal fault rejected',name,flush=True)
            finally:
                paths[key].write_text(originals[key])
                if name=='decode-small-offloaded':paths['channel'].write_text(originals['channel'])
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
    (work/'summary.json').write_text(json.dumps(result,indent=2)+'\n');print('Decode refusal ownership verified',len(controls),'faults',len(suites),'full suites',flush=True)

if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--output',type=Path,required=True);p.add_argument('--target-dir',type=Path,required=True);p.add_argument('--preflight',action='store_true');a=p.parse_args();main(a.output.resolve(),a.target_dir.resolve(),a.preflight)
