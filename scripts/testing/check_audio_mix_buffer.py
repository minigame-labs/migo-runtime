#!/usr/bin/env python3
"""Actor output scratch capacity, replacement admission and complete mixed PCM gate."""
from __future__ import annotations
import argparse,json,re
from pathlib import Path
import check_audio_device_routes as previous
base=previous.base
wave=previous.wave
cold=previous.cold
graph=previous.graph
ROOT=base.ROOT
PCM=previous.PCM
pcm_receipt=previous.pcm_receipt
INDEX_PCM=previous.INDEX_PCM
index_pcm_receipt=previous.index_pcm_receipt
MEDIA_PCM=previous.MEDIA_PCM
media_pcm_receipt=previous.media_pcm_receipt
INNER_PCM=previous.INNER_PCM
inner_pcm_receipt=previous.inner_pcm_receipt
CONTEXT_PCM=previous.CONTEXT_PCM
context_pcm_receipt=previous.context_pcm_receipt
SLOT_PCM=previous.SLOT_PCM
slot_pcm_receipt=previous.slot_pcm_receipt
ROUTE_PCM=previous.ROUTE_PCM
route_pcm_receipt=previous.route_pcm_receipt
PREFIX='audio_thread::mix_buffer::tests::'
MIX_PCM=PREFIX+'complete_actor_mix_limiter_and_output_layout_pcm_trace'
def mix_pcm_receipt(log):
    decoder=json.JSONDecoder();rows={}
    for line in log.read_text().splitlines():
        if not line.startswith('MIGO_MIX_BUFFER_PCM '):continue
        _,key,tail=line.split(' ',2);values=[]
        for _ in range(3):
            value,end=decoder.raw_decode(tail);values.append(value);tail=tail[end:].lstrip()
        assert not tail and key not in rows
        rate,source_channels,channels,turn=map(int,key.split(':'));assert len(values[0])==128*channels and len(values[2])==1
        rows[key]=dict(pcm=values[0],ended=values[1],clocks=values[2])
    assert len(rows)==90,(log,len(rows))
    return rows
def declarations(originals):
    rows=[]
    def add(name,key,before,after,test,message):
        assert before!=after and originals[key].count(before)==1,(name,key,'anchor drift')
        rows.append(dict(name=name,changes=[(key,before,after)],test=PREFIX+test,expected_assertion=message))
    initial='first_capacity_is_charged_from_real_allocation_and_zero_initialized'
    first='first_refusal_does_not_allocate_pcm_or_leak_tickets'
    growth='growth_refusal_keeps_original_capacity_pointer_length_and_pcm'
    prefix='admitted_growth_keeps_prefix_and_zero_tail_until_render_clears_it'
    reuse='shrink_and_reuse_under_full_budget_do_not_allocate_or_return_capacity_credit'
    overlap='bulk_growth_keeps_both_credits_and_destroys_original_vec_on_original_cpu'
    shutdown='closed_cpu_drop_keeps_credit_and_pcm_until_real_drop_without_late_allocation'
    candidate='refused_output_candidate_is_disposed_before_old_buffer_or_route_can_be_replaced'
    add('mix-initial-capacity-unaccounted','buffer','        let mut data = initial.finish();','        let mut data = GraphVec::from_vec(Vec::with_capacity(samples)); drop::<Initial<f32>>(initial);',initial,'mix buffer initial capacity omitted physical credit')
    reservation='        let initial = Initial::reserve(&host, samples)?;'
    add('mix-initial-pcm-before-admission','buffer',reservation,'        let _early=std::hint::black_box(Vec::<f32>::with_capacity(samples));\n'+reservation,first,'mix buffer initial PCM allocated before admission')
    plan='        if let Some(plan) = Plan::reserve(&self.data, Some(&self.host), samples)? {'
    add('mix-growth-unaccounted','buffer',plan,plan.replace('Some(&self.host)','None'),growth,'mix buffer growth bypassed admission')
    add('mix-growth-pcm-before-admission','buffer',plan,'        let _early=std::hint::black_box(Vec::<f32>::with_capacity(samples));\n'+plan,growth,'mix buffer growth allocated PCM before admission')
    add('mix-new-length-not-published','buffer','        self.data.resize(samples, 0.0);','        // fault: retain old logical length',prefix,'mix buffer growth did not publish complete new length')
    add('mix-growth-prefix-cleared','buffer','            plan.install(&mut self.data);','            plan.install(&mut self.data); self.data.fill(0.0);',prefix,'mix buffer growth changed original PCM prefix')
    add('mix-shrink-replaces-backing','buffer',plan,'        if samples<self.data.len(){self.data=GraphVec::new();}\n'+plan,reuse,'mix buffer within capacity attempted fresh admission')
    add('mix-reuse-demands-new-credit','buffer',plan,'        let _unnecessary=self.host.reserve_inline(1)?;\n'+plan,reuse,'mix buffer within capacity attempted fresh admission')
    add('mix-real-vec-not-transferred','vec','            **value = Some(std::mem::take(&mut self.data));','            **value = None;',overlap,'mix buffer growth freed old PCM on caller')
    add('mix-overlap-credit-returned-early','retirement','            let lease = self.lease.take().unwrap();','            let mut lease = self.lease.take().unwrap();\n            lease.permit.shrink_to(0);',overlap,'mix buffer returned overlapping PCM credit early')
    add('mix-late-shutdown-allocation','buffer','impl Deref for MixBuffer {','impl Drop for MixBuffer {fn drop(&mut self){let _late=std::hint::black_box(Box::new(1u8));}}\nimpl Deref for MixBuffer {',shutdown,'mix buffer shutdown allocated late handoff')
    add('mix-output-candidate-bypasses-admission','buffer','        self.resize(samples)?;','        let _=samples;',candidate,'mix output candidate bypassed buffer admission')
    for name,before,after,test in graph.declarations(originals['context']):
        rows.append(dict(name=name,changes=[('context',before,after)],test=test,expected_assertion='cold context render allocated'))
    for name,key,before,after,message in cold.declarations({'nodes/wave_shaper.rs':originals['wave'],'context.rs':originals['context']}):
        rows.append(dict(name=name,changes=[('wave'if key=='nodes/wave_shaper.rs'else'context',before,after)],test=cold.TEST,expected_assertion=message))
    assert len(rows)==19
    return rows
def before_buffer(current):
    head="""use crate::node_retirement::Host;
use shared::error::EngineResult;
use std::ops::{Deref,DerefMut};
pub(super) struct MixBuffer{data:Vec<f32>,_host:Host}
impl MixBuffer{
pub(super) fn new(host:Host,samples:usize)->EngineResult<Self>{Ok(Self{data:vec![0.0;samples],_host:host})}
fn resize(&mut self,samples:usize)->EngineResult<()>{self.data.resize(samples,0.0);Ok(())}
fn capacity(&self)->usize{self.data.capacity()}
fn observe(&mut self,_hook:std::sync::Arc<dyn Fn()+Send+Sync>){}
"""
    head+=wave.method(current,'    pub(super) fn admit<T>')+'\n'
    return head+'}\n'+current[current.index('impl Deref for MixBuffer'):]
def main(work,target,before_directory,preflight=False):
    package,env,frozen=base.prepare(work,target)
    product=base.capacity.product;common=product.whole.support.clock.common
    names={'buffer':'audio_thread_mix_buffer.rs','vec':'graph_vec.rs','actor':'audio_thread.rs','retirement':'node_retirement.rs','context':'context.rs','wave':'nodes/wave_shaper.rs'}
    paths={key:package/'src'/name for key,name in names.items()};originals={key:path.read_text()for key,path in paths.items()}
    rows=declarations(originals);programs=[];controls=[];red=[];suites={}
    def build(label,release=False):
        binary=product.src.build(package,work,env,label,release)
        programs.append(dict(name=label,path=str(binary),sha256=product.whole.support.codecs.sha(binary),pruned=False));return binary
    def run(binary,label,test='',expected=0,assertion=None):
        status=common.run([str(binary)]+(['--exact',test]if test else[])+(['--nocapture']if test else[]),work/(label+'.log'),package,env,timeout=300)
        (work/(label+'-exit.txt')).write_text(str(status)+'\n');text=(work/(label+'.log')).read_text();assert status==expected,(label,status)
        if expected:assert all(marker in text for marker in ['running 1 test','panicked at','1 failed',assertion]),label
        else:
            match=re.search(r'test result: ok\. ([1-9][0-9]*) passed; 0 failed; (\d+) ignored;',text);assert match,label
            suites[label]=dict(passed=int(match[1]),ignored=int(match[2]))
    positive=build('positive-debug')
    base.capacity.profile.verify(work/'positive-debug-build.jsonl',json.loads((work/'debug-profile-policy.json').read_text()),work/'debug-profile-artifacts.json')
    run(positive,'full-debug');run(positive,'pcm-debug',PCM);current_pcm=pcm_receipt(work/'pcm-debug.log')
    run(positive,'index-pcm-debug',INDEX_PCM);current_index_pcm=index_pcm_receipt(work/'index-pcm-debug.log')
    run(positive,'media-pcm-debug',MEDIA_PCM);current_media_pcm=media_pcm_receipt(work/'media-pcm-debug.log')
    run(positive,'inner-pcm-debug',INNER_PCM);current_inner_pcm=inner_pcm_receipt(work/'inner-pcm-debug.log')
    run(positive,'context-pcm-debug',CONTEXT_PCM);current_context_pcm=context_pcm_receipt(work/'context-pcm-debug.log')
    run(positive,'slot-pcm-debug',SLOT_PCM);current_slot_pcm=slot_pcm_receipt(work/'slot-pcm-debug.log')
    run(positive,'route-pcm-debug',ROUTE_PCM);current_route_pcm=route_pcm_receipt(work/'route-pcm-debug.log')
    run(positive,'mix-pcm-debug',MIX_PCM);current_mix_pcm=mix_pcm_receipt(work/'mix-pcm-debug.log')
    assert common.run(['cargo','clippy','--lib','--tests','--offline','--locked','--message-format=json'],work/'clippy.jsonl',package,env,timeout=300)==0
    old_manifest=json.loads((before_directory/'root-sha256.json').read_text());changed=[n for n,h in old_manifest.items()if n.startswith('engine/crates/audio/')and product.whole.support.codecs.sha(ROOT/n)!=h]
    saved={n:(package/n.removeprefix('engine/crates/audio/')).read_bytes()for n in changed}
    try:
        for n in changed:(package/n.removeprefix('engine/crates/audio/')).write_bytes((before_directory/n).read_bytes())
        assert common.run(['cargo','clippy','--lib','--tests','--offline','--locked','--message-format=json'],work/'clippy-before.jsonl',package,env,timeout=300)==0
    finally:
        for n,data in saved.items():(package/n.removeprefix('engine/crates/audio/')).write_bytes(data)
    old,current=wave.diagnostics(work/'clippy-before.jsonl',package),wave.diagnostics(work/'clippy.jsonl',package);added=current-old
    delta=dict(before=sum(old.values()),after=sum(current.values()),new_diagnostics=sum(added.values()),exact_multiset=old==current,actor_included=True,new=[dict(key=key,count=count)for key,count in added.items()]);(work/'clippy-delta.json').write_text(json.dumps(delta,indent=2)+'\n');assert not added,delta
    if not preflight:
        try:
            paths['buffer'].write_text(before_buffer(originals['buffer']))
            binary=build('before-actor-mix-buffer')
            for label,test,message in [('before-mix-first-charge','first_capacity_is_charged_from_real_allocation_and_zero_initialized','mix buffer initial capacity omitted physical credit'),('before-mix-growth-charge','growth_refusal_keeps_original_capacity_pointer_length_and_pcm','mix buffer growth bypassed admission')]:
                run(binary,label,PREFIX+test,101,message);red.append(dict(name=label,test=PREFIX+test,exit=101,expected_assertion=message,sha256=programs[-1]['sha256']))
            run(binary,'pcm-before',PCM);before_pcm=pcm_receipt(work/'pcm-before.log');assert before_pcm==current_pcm
            run(binary,'index-pcm-before',INDEX_PCM);before_index_pcm=index_pcm_receipt(work/'index-pcm-before.log');assert before_index_pcm==current_index_pcm
            run(binary,'media-pcm-before',MEDIA_PCM);before_media_pcm=media_pcm_receipt(work/'media-pcm-before.log');assert before_media_pcm==current_media_pcm
            run(binary,'inner-pcm-before',INNER_PCM);before_inner_pcm=inner_pcm_receipt(work/'inner-pcm-before.log');assert before_inner_pcm==current_inner_pcm
            run(binary,'context-pcm-before',CONTEXT_PCM);before_context_pcm=context_pcm_receipt(work/'context-pcm-before.log');assert before_context_pcm==current_context_pcm
            run(binary,'slot-pcm-before',SLOT_PCM);before_slot_pcm=slot_pcm_receipt(work/'slot-pcm-before.log');assert before_slot_pcm==current_slot_pcm
            run(binary,'route-pcm-before',ROUTE_PCM);before_route_pcm=route_pcm_receipt(work/'route-pcm-before.log');assert before_route_pcm==current_route_pcm
            run(binary,'mix-pcm-before',MIX_PCM);before_mix_pcm=mix_pcm_receipt(work/'mix-pcm-before.log');assert before_mix_pcm==current_mix_pcm
            (work/'pcm-before-comparison.json').write_text(json.dumps(dict(configured_blocks=len(before_pcm),complete_pcm_samples=sum(len(row['pcm'])for row in before_pcm.values()),actor_factory_blocks=len(before_index_pcm),actor_factory_complete_pcm_samples=sum(len(row['pcm'])for row in before_index_pcm.values()),media_control_blocks=len(before_media_pcm),media_control_complete_pcm_samples=sum(len(row['pcm'])for row in before_media_pcm.values()),all_bits_and_collection_and_clock_equal=True,all_media_state_position_and_ordered_membership_equal=True,collection_ids_canonicalized=True,actual_before_program_sha256=programs[-1]['sha256'],original_unaccounted_actor_process_vec=True,mix_buffer_blocks=len(before_mix_pcm),mix_buffer_complete_pcm_samples=sum(len(row['pcm'])for row in before_mix_pcm.values()),all_mix_buffer_pcm_clock_and_ended_equal=True,route_blocks=len(before_route_pcm),route_complete_pcm_samples=sum(len(row['pcm'])for row in before_route_pcm.values()),all_route_pcm_clock_and_collection_equal=True,slot_blocks=len(before_slot_pcm),slot_complete_pcm_samples=sum(len(row['pcm'])for row in before_slot_pcm.values()),all_slot_pcm_clock_and_collection_equal=True,context_table_blocks=len(before_context_pcm),context_table_complete_pcm_samples=sum(len(row['pcm'])for row in before_context_pcm.values()),all_context_table_clock_and_collection_equal=True,inner_player_blocks=len(before_inner_pcm),inner_player_complete_pcm_samples=sum(len(row['pcm'])for row in before_inner_pcm.values()),all_inner_state_position_equal=True,production_cycle_algorithm_unchanged=True,production_cycle_order_unchanged=False,test_only_canonical_mix_order=True),indent=2)+'\n')
            programs[-1]['pruned']=True;binary.unlink()
        finally:
            for key in ['buffer']:paths[key].write_text(originals[key])
        for row in rows:
            keys=set()
            try:
                for key,before,after in row['changes']:
                    assert key not in keys;keys.add(key);paths[key].write_text(originals[key].replace(before,after))
                binary=build(row['name']);run(binary,row['name'],row['test'],101,row['expected_assertion'])
                controls.append({key:value for key,value in row.items()if key!='changes'}|dict(exit=101,sha256=programs[-1]['sha256']));programs[-1]['pruned']=True;binary.unlink();(work/'completed-controls.json').write_text(json.dumps(controls,indent=2)+'\n');print('Actual actor mix buffer fault rejected',row['name'],flush=True)
            finally:
                for key in keys:paths[key].write_text(originals[key])
        release=build('positive-release',True);run(release,'full-release');run(release,'pcm-release',PCM);assert pcm_receipt(work/'pcm-release.log')==current_pcm,'O3 complete context PCM/collection/clock changed'
        run(release,'index-pcm-release',INDEX_PCM);assert index_pcm_receipt(work/'index-pcm-release.log')==current_index_pcm
        run(release,'media-pcm-release',MEDIA_PCM);assert media_pcm_receipt(work/'media-pcm-release.log')==current_media_pcm
        run(release,'inner-pcm-release',INNER_PCM);assert inner_pcm_receipt(work/'inner-pcm-release.log')==current_inner_pcm
        run(release,'context-pcm-release',CONTEXT_PCM);assert context_pcm_receipt(work/'context-pcm-release.log')==current_context_pcm
        run(release,'slot-pcm-release',SLOT_PCM);assert slot_pcm_receipt(work/'slot-pcm-release.log')==current_slot_pcm
        run(release,'route-pcm-release',ROUTE_PCM);assert route_pcm_receipt(work/'route-pcm-release.log')==current_route_pcm
        run(release,'mix-pcm-release',MIX_PCM);assert mix_pcm_receipt(work/'mix-pcm-release.log')==current_mix_pcm
    product.whole.verify_inputs(work,frozen)
    assert all(path.read_text()==originals[key]for key,path in paths.items())
    root=ROOT/'engine/crates/audio/src';assert all((package/'src'/p.relative_to(root)).read_bytes()==p.read_bytes()for p in root.rglob('*')if p.is_file())
    (work/'summary.json').write_text(json.dumps(dict(programs=programs,controls=controls,original_red=red,suites=suites,root_inputs_unchanged=True,source_copies_restored=True,root_dev_profile_verified=True,isolated_release_is_not_root_shipping=True,preflight=preflight),indent=2)+'\n');print('Actor mix buffer verified',len(controls),'fresh faults',flush=True)


if __name__=='__main__':
    parser=argparse.ArgumentParser(description=__doc__);parser.add_argument('--output',type=Path,required=True);parser.add_argument('--target-dir',type=Path,required=True);parser.add_argument('--before-directory',type=Path,required=True);parser.add_argument('--preflight',action='store_true');args=parser.parse_args();main(args.output.resolve(),args.target_dir.resolve(),args.before_directory.resolve(),args.preflight)
