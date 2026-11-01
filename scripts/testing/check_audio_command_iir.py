#!/usr/bin/env python3
"""IIR normalization returned before large command input destruction on audio.

Keep original admission through actual input disposal; preserve original IIR construction and DSP
semantics and immediate responses; faults must execute fresh business assertions.
"""
from __future__ import annotations
import argparse,json
from pathlib import Path
import check_audio_command_ownership as base
capacity=base.capacity
ROOT=base.ROOT
TEST='audio_thread::tests::set_buffer_rejects_claimed_context_that_does_not_own_node'


def run_full(binary,package,env,work,label):
    import re
    common=capacity.product.whole.support.clock.common
    code=common.run([str(binary)],work/(label+'.log'),package,env,timeout=300)
    text=(work/(label+'.log')).read_text();assert code==0 and re.search(r'test result: ok\. [1-9][0-9]* passed; 0 failed;',text)
    (work/(label+'-exit.txt')).write_text(str(code)+'\n');return dict(exit=code)

def declarations(originals):
    def method(text,marker):
        a=text.index(marker);left=text.index('{',a);depth=1;i=left+1
        while depth:depth+=(text[i]=='{')-(text[i]=='}');i+=1
        return text[a:i]
    discard=method(originals['channel'],'    pub fn into_discarded_payload(')
    cpu='IIR input escaped CPU retirement'
    credit='IIR input credit returned before CPU drop'
    semantic='IIR construction changed node admission, index or frequency response'
    golden='IIR borrowed constructor changed normalization or channel history'
    cap='delivery.command().queued_payload_bytes() < 4096'
    rows=[
      ('iir-owned-dispatch-bypassed','dispatch','if !delivery.has_queue_permit()','if true || !delivery.has_queue_permit()','bulk IIR construction bypassed borrowed dispatch'),
      ('iir-length-used','dispatch',cap,'(feedforward.len()+feedback.len())*8 < 4096','bulk IIR construction bypassed borrowed dispatch'),
      ('iir-feedback-capacity-ignored','dispatch',cap,'feedforward.capacity()*8 < 4096','bulk IIR construction bypassed borrowed dispatch'),
      ('iir-wrong-element-size','dispatch',cap,'delivery.command().queued_payload_bytes()/2 < 4096','IIR small boundary or legacy dispatch changed'),
      ('iir-input-inline','dispatch','inbox.finish_parameters(delivery);','drop(delivery);',cpu),
      ('iir-input-on-async','inbox','cpu.retirement().retire(batch).await;','drop(batch);',cpu),
      ('iir-item-credit-early','channel',discard,discard.replace('_permit: permit,','_permit: None,'),credit),
      ('iir-byte-credit-early','channel',discard,discard.replace('let (command, permit) = self.into_parts();','let (command, mut permit) = self.into_parts(); if let Some(p)=permit.as_mut(){p.shrink_to(0);}'),credit),
      ('iir-coefficients-swapped','dispatch','ctx.create_iir_filter(*node_id, feedforward, feedback)','ctx.create_iir_filter(*node_id, feedback, feedforward)',semantic),
      ('iir-index-owner-lost','dispatch','index.register(*node_id, *ctx_id);','index.register(*node_id, 1);',semantic),
      ('iir-refusal-still-registered','dispatch','&& ctx.create_iir_filter(*node_id, feedforward, feedback)','&& (ctx.create_iir_filter(*node_id, feedforward, feedback) || true)',semantic),
      ('iir-ff-normalization-lost','node','feedforward.iter().map(|&v| v * inv_a0).collect()','feedforward.to_vec()',golden),
      ('iir-fb-normalization-lost','node','feedback.iter().map(|&v| v * inv_a0).collect()','feedback.to_vec()',golden),
      ('iir-channel-layout-lost','node','            channels,','            channels: 1,','IIR borrowed constructor changed frame or channel layout'),
      ('iir-unrelated-selected','dispatch','    let AudioCmd::CreateIIRFilter {','    if matches!(delivery.command(),AudioCmd::SetNodeParam{..}) {drop(delivery);return None;}\n    let AudioCmd::CreateIIRFilter {','IIR dispatcher selected unrelated command'),
    ]
    for n,k,a,z,_ in rows:assert a!=z and originals[k].count(a)==1,(n,'anchor drift',originals[k].count(a))
    return rows

def main(work,target,preflight=False):
    package,env,frozen=base.prepare(work,target)
    paths={'dispatch':package/'src/audio_thread_command_iir.rs','node':package/'src/nodes/iir_filter.rs','inbox':package/'src/command_inbox.rs','channel':package/'local-deps/shared/src/audio_channel.rs'}
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
    capacity.run(positive,package,env,work,'positive-iir',TEST)
    assert product.whole.support.clock.common.run(['cargo','clippy','--lib','--tests','--offline','--locked','--message-format=json'],work/'clippy.jsonl',package,env,timeout=300)==0
    if not preflight:
        suites['audio-debug']=run_full(positive,package,env,work,'full-debug')
        for name,key,before,after,assertion in rows:
            try:
                paths[key].write_text(originals[key].replace(before,after))
                binary=product.src.build(package,work,env,name,False)
                item=capacity.run(binary,package,env,work,name,TEST,101,assertion);item['binary_sha256']=record(name,binary,True);controls.append(item);(work/'completed-controls.json').write_text(json.dumps(controls,indent=2)+'\n');print('Actual IIR construction input fault rejected',name,flush=True)
            finally:
                paths[key].write_text(originals[key])
        for mode,release in [('debug',False),('release',True)]:
            if release:
                binary=product.src.build(package,work,env,'positive-release',True);record('positive-release',binary);suites['audio-release']=run_full(binary,package,env,work,'full-release')
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
    (work/'summary.json').write_text(json.dumps(result,indent=2)+'\n');print('IIR construction input ownership verified',len(controls),'faults',len(suites),'full suites',flush=True)

if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--output',type=Path,required=True);p.add_argument('--target-dir',type=Path,required=True);p.add_argument('--preflight',action='store_true');a=p.parse_args();main(a.output.resolve(),a.target_dir.resolve(),a.preflight)
