#!/usr/bin/env python3
"""Replacement quota refusal dropped incoming curve/snapshot storage on audio.

Keep original admission through actual input disposal; preserve replacement/start
semantics and immediate responses; faults must execute fresh business assertions.
"""
from __future__ import annotations
import argparse,json
from pathlib import Path
import check_audio_command_ownership as base
capacity=base.capacity
ROOT=base.ROOT
TEST='audio_thread::tests::snapshot_buffer_commands_validate_context_and_node_ownership'


def declarations(originals):
    def method(text,marker):
        a=text.index(marker);left=text.index('{',a);depth=1;i=left+1
        while depth:depth+=(text[i]=='{')-(text[i]=='}');i+=1
        return text[a:i]
    curve=method(originals['context'],'    pub(crate) fn try_set_wave_shaper_curve(')
    snapshot=method(originals['context'],'    pub(crate) fn try_set_started_buffer(')
    cpu='replacement input escaped CPU retirement'
    credit='replacement credit returned before CPU drop'
    pcm='replacement changed complete PCM, offset, duration or old binding'
    consumed='consumed replacement retained or cloned its input ticket'
    start='replacement changed start or resume semantics'
    selector='        || !matches!(\n            delivery.command(),\n            AudioCmd::SetWaveShaperCurve { .. }\n                | AudioCmd::SetStartedBuffer { .. }\n                | AudioCmd::StartBuffer { .. }\n        )'
    cap='bytes < 4096'
    rows=[
      ('replacement-owned-dispatch-bypassed','replacement','if !delivery.has_queue_permit()','if true || !delivery.has_queue_permit()','bulk replacement bypassed owned dispatch'),
      ('replacement-length-used','replacement',cap,'matches!(delivery.command(), AudioCmd::SetWaveShaperCurve{curve:Some(v),..} if v.len()*4<4096) || '+cap,'bulk replacement bypassed owned dispatch'),
      ('replacement-input-inline','channel','(result, self.into_discarded_payload())','{ drop(self); (result,None) }',cpu),
      ('replacement-input-on-async','inbox','cpu.retirement().retire(batch).await;','drop(batch);',cpu),
      ('replacement-item-credit-early','channel','        (result, self.into_discarded_payload())','        let mut payload=self.into_discarded_payload();\n        if let Some(value)=payload.as_mut(){drop(value._permit.take());}\n        (result,payload)',credit),
      ('replacement-byte-credit-early','channel','        (result, self.into_discarded_payload())','        if let Some(permit)=self.permit.as_mut(){permit.shrink_to(0); }\n        (result,self.into_discarded_payload())',credit),
      ('replacement-curve-taken-before-admission','context',curve,curve.replace('        self.with_node_typed', '        let mut incoming=curve.take();\n        self.with_node_typed',1).replace('node.try_set_curve(curve)','node.try_set_curve(&mut incoming)'),cpu),
      ('replacement-snapshot-taken-before-admission','context',snapshot,snapshot.replace('        if let Some(node) =','        let incoming=buffer.take();\n        if let Some(node) =',1).replace('source.set_snapshot(buffer.take());','source.set_snapshot(incoming);'),cpu),
      ('replacement-curve-old-admission-ignored','node','self.retirement.as_ref(),\n                    data.capacity() * std::mem::size_of::<f32>(),','None,\n                    data.capacity() * std::mem::size_of::<f32>(),',pcm),
      ('replacement-snapshot-old-admission-ignored','context',snapshot,snapshot.replace('source.retire_buffer(self.node_retirement.as_ref())','Ok::<(), EngineError>(())'),start),
      ('replacement-curve-cloned','context','node.try_set_curve(curve)','node.try_set_curve(&mut curve.clone())',consumed),
      ('replacement-snapshot-cloned','context','source.set_snapshot(buffer.take());','source.set_snapshot(buffer.clone());',consumed),
      ('replacement-start-time-lost','replacement','ctx.start_source(*node_id, *when, *offset, *duration)','ctx.start_source(*node_id, 0.0, *offset, *duration)',pcm),
      ('replacement-start-offset-lost','replacement','ctx.start_source(*node_id, *when, *offset, *duration)','ctx.start_source(*node_id, *when, 0.0, *duration)',pcm),
      ('replacement-start-duration-lost','replacement','ctx.start_source(*node_id, *when, *offset, *duration)','ctx.start_source(*node_id, *when, *offset, None)',pcm),
      ('replacement-start-after-refusal','replacement','&& ctx.start_source(*node_id, *when, *offset, *duration)','| ctx.start_source(*node_id, *when, *offset, *duration)',start),
      ('replacement-resume-lost','replacement','return started.then_some((*ctx_id, *node_id));','return None;',start),
      ('replacement-small-offloaded','replacement',cap,cap.replace('< 4096','< 0'),'replacement threshold or legacy dispatch changed'),
      ('replacement-unrelated-selected','replacement',selector,'        || false','replacement completion requires a curve or snapshot command'),
    ]
    for n,k,a,z,_ in rows:assert a!=z and originals[k].count(a)==1,(n,'anchor drift',originals[k].count(a))
    return rows


def main(work,target,preflight=False):
    package,env,frozen=base.prepare(work,target)
    paths={'replacement':package/'src/audio_thread_replacement_input.rs','context':package/'src/context.rs','inbox':package/'src/command_inbox.rs','channel':package/'local-deps/shared/src/audio_channel.rs','node':package/'src/nodes/wave_shaper.rs'}
    originals={k:p.read_text() for k,p in paths.items()}
    rows=declarations(originals)
    programs=[];controls=[];suites={}
    product=capacity.product
    def record(name,binary,pruned=False,kind='audio'):
        h=product.whole.support.codecs.sha(binary);programs.append(dict(name=name,path=str(binary),sha256=h,pruned=pruned,target_name=kind))
        if pruned:binary.unlink()
        return h
    positive=product.src.build(package,work,env,'positive-debug',False);record('positive-debug',positive)
    capacity.profile.verify(work/'positive-debug-build.jsonl',json.loads((work/'debug-profile-policy.json').read_text()),work/'debug-profile-artifacts.json')
    capacity.run(positive,package,env,work,'positive-replacement',TEST)
    assert product.whole.support.clock.common.run(['cargo','clippy','--lib','--tests','--offline','--locked','--message-format=json'],work/'clippy.jsonl',package,env,timeout=300)==0
    if not preflight:
        suites['audio-debug']=capacity.run(positive,package,env,work,'full-debug')
        for name,key,before,after,assertion in rows:
            try:
                paths[key].write_text(originals[key].replace(before,after))
                if name=='replacement-small-offloaded':
                    assert originals['channel'].count('if self.permit.is_none() || bytes < 4096 {')==1
                    paths['channel'].write_text(originals['channel'].replace('if self.permit.is_none() || bytes < 4096 {','if self.permit.is_none() || bytes == 0 {'))
                binary=product.src.build(package,work,env,name,False)
                item=capacity.run(binary,package,env,work,name,TEST,101,assertion);item['binary_sha256']=record(name,binary,True);controls.append(item);(work/'completed-controls.json').write_text(json.dumps(controls,indent=2)+'\n');print('Actual replacement-input fault rejected',name,flush=True)
            finally:
                paths[key].write_text(originals[key])
                if name=='replacement-small-offloaded':paths['channel'].write_text(originals['channel'])
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
    (work/'summary.json').write_text(json.dumps(result,indent=2)+'\n');print('Replacement input ownership verified',len(controls),'faults',len(suites),'full suites',flush=True)

if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--output',type=Path,required=True);p.add_argument('--target-dir',type=Path,required=True);p.add_argument('--preflight',action='store_true');a=p.parse_args();main(a.output.resolve(),a.target_dir.resolve(),a.preflight)
