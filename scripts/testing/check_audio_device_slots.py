#!/usr/bin/env python3
"""Pending device-slot capacity, synchronous operation revocation and full SRC PCM gate."""
from __future__ import annotations
import argparse,json,re
from pathlib import Path
import check_audio_context_table as previous
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
PREFIX='device_prepare::pull::session::resources::owner::storage_tests::'
SLOT_PCM=PREFIX+'complete_managed_pending_slot_pcm_clock_and_rehash_trace'
def slot_pcm_receipt(log):
    decoder=json.JSONDecoder();rows={}
    for line in log.read_text().splitlines():
        if not line.startswith('MIGO_SLOT_PCM '):continue
        _,key,tail=line.split(' ',2);values=[]
        for _ in range(4):
            value,end=decoder.raw_decode(tail);values.append(value);tail=tail[end:].lstrip()
        assert not tail and key not in rows
        rate,target,channels,frames=map(int,key.split(':'));assert len(values[0])==frames*channels+channels-1
        assert len(values[3])==4
        rows[key]=dict(pcm=values[0],collected=values[1],ended=values[2],clocks=values[3])
    assert len(rows)==36,(log,len(rows))
    return rows
def declarations(originals):
    rows=[]
    def add(name,key,before,after,test,message):
        assert before!=after and originals[key].count(before)==1,(name,key,'anchor drift')
        rows.append(dict(name=name,changes=[(key,before,after)],test=PREFIX+test,expected_assertion=message))
    first='empty_is_lazy_and_first_insert_charges_real_native_layout'
    refusal='first_refusal_precedes_buckets_operations_and_wakeup'
    growth='full_table_refusal_preserves_all_slots_before_request_side_effects'
    duplicate='duplicate_ensure_is_allocation_free_under_full_budget'
    factory='failed_factory_keeps_full_table_and_refunds_reserved_growth'
    replace='full_key_replacement_preserves_capacity_and_revokes_only_old_operation'
    rehash='bulk_rehash_keeps_operations_live_and_both_bucket_allocations_charged'
    remove='remove_and_restart_cancel_immediately_without_returning_capacity_credit'
    shutdown='closed_cpu_drop_revokes_operations_before_empty_buckets_really_die'
    method=wave.method(originals['slots'],'    pub(super) fn managed(')
    add('slots-first-table-unaccounted','slots',method,method.replace('host: Some(host)','host: None'),first,'slot first table omitted physical charge')
    reservation='        let plan = Plan::reserve(&self.data, self.host.as_ref(), required)?;'
    add('slots-first-buckets-before-admission','slots',reservation,'        let _early=HashMap::<u32,Slot>::with_capacity(required);\n'+reservation,refusal,'slot first buckets allocated before admission')
    method=wave.method(originals['map'],'    pub(crate) fn reserve(\n        data: &GraphMap<V>,')
    add('slots-growth-unaccounted','map',method,method.replace('Admission::reserve(host, layout.bytes)','Admission::reserve(host, if required==29 {0}else{layout.bytes})'),growth,'slot growth bypassed admission')
    add('slots-request-before-admission','slots',reservation+'\n        let slot = factory()?;','        let slot = factory()?;\n'+reservation,growth,'slot factory request ran before table admission')
    add('slots-target-before-admission','slots',reservation,'        if id==10000 {let _early=HashMap::<u32,Slot>::with_capacity(required);}\n'+reservation,growth,'slot refusal allocated target buckets')
    method=wave.method(originals['owner'],'    fn ensure(');match=re.search(r'        if self\n            \.slots.*?return Ok\(\(\)\);\n        }\n',method,re.S);assert match
    add('slots-duplicate-restarts-request','owner',method,method.replace(match[0],''),duplicate,'duplicate slot ensure allocated')
    pair='        let slot = factory()?;\n        if let Some(plan) = plan {\n            plan.install(&mut self.data);\n        }'
    add('slots-installed-before-factory','slots',pair,'        if let Some(plan)=plan {plan.install(&mut self.data);}\n        let slot = factory()?;',factory,'failed slot factory installed or published table growth')
    add('slots-new-entry-not-published','slots','        self.data.insert(id, slot);','        if id!=10000 {self.data.insert(id,slot);}',rehash,'slot creation did not publish new entry')
    add('slots-replacement-omitted','slots','        self.data.insert(id, slot);','        if !self.data.contains_key(&id) {self.data.insert(id,slot);}',replace,'slot replacement did not cancel old operation')
    add('slots-remove-does-nothing','slots','        self.data.remove(id)','        let _=id; None',remove,'slot remove did not revoke current operation')
    add('slots-restart-does-nothing','slots','        self.data.clear();','        // fault: keep all slots',remove,'slot restart did not revoke remaining operations')
    add('slots-revocation-delayed-to-cpu','slots','        for (_, slot) in self.data.drain() {\n            drop(slot);\n        }','        // fault: leave live slots in CPU-bound buckets',shutdown,'slot shutdown postponed operation revocation')
    add('slots-real-buckets-not-transferred','map','            **value = Some(std::mem::take(&mut self.data));','            **value = None;',rehash,'slot rehash freed old buckets on caller')
    add('slots-credit-returned-early','retirement','            let lease = self.lease.take().unwrap();','            let mut lease = self.lease.take().unwrap();\n            lease.permit.shrink_to(0);',rehash,'slot rehash returned overlapping bucket credit')
    add('slots-late-handoff-allocation','slots','    fn drop(&mut self) {','    fn drop(&mut self) {\n        let _late=std::hint::black_box(Box::new(1u8));',shutdown,'slot shutdown allocated late handoff')
    add('slots-rehash-loses-operations','slots','        self.data.insert(id, slot);','        if id==10000 {self.data.clear();}\n        self.data.insert(id,slot);',rehash,'slot rehash lost existing operations')
    for name,before,after,test in graph.declarations(originals['context']):
        rows.append(dict(name=name,changes=[('context',before,after)],test=test,expected_assertion='cold context render allocated'))
    for name,key,before,after,message in cold.declarations({'nodes/wave_shaper.rs':originals['wave'],'context.rs':originals['context']}):
        rows.append(dict(name=name,changes=[('wave'if key=='nodes/wave_shaper.rs'else'context',before,after)],test=cold.TEST,expected_assertion=message))
    assert len(rows)==23
    return rows
def before_slots(current):
    head="""use super::*;
use crate::node_retirement::Host as StorageHost;
use std::collections::HashMap;
use std::ops::Deref;
pub(super) struct Slots {data:HashMap<u32,Slot>,_host:Option<StorageHost>}
impl Slots {
pub(super) fn new()->Self{Self{data:HashMap::new(),_host:None}}
pub(super) fn managed(host:StorageHost)->Self{Self{data:HashMap::new(),_host:Some(host)}}
pub(super) fn create_with(&mut self,id:u32,factory:impl FnOnce()->EngineResult<Slot>)->EngineResult<()>{let slot=factory()?;self.data.insert(id,slot);Ok(())}
pub(super) fn table_bytes(&self)->usize{crate::graph_table_layout::Layout::for_entries::<(u32,Slot)>(self.data.capacity()).unwrap().bytes}
pub(super) fn observe(&mut self,_hook:Arc<dyn Fn()+Send+Sync>){}
"""
    for name in ['get_mut','remove','retain','clear','identity']:head+=wave.method(current,'    pub(super) fn '+name+'(')+'\n'
    return head+'}\n'+current[current.index('impl Deref for Slots'):]
def main(work,target,before_directory,preflight=False):
    package,env,frozen=base.prepare(work,target)
    product=base.capacity.product;common=product.whole.support.clock.common
    names={'slots':'device_slot_storage.rs','owner':'device_owner_slot.rs','map':'graph_map.rs','actor':'audio_thread.rs','retirement':'node_retirement.rs','context':'context.rs','wave':'nodes/wave_shaper.rs'}
    paths={key:package/'src'/name for key,name in names.items()};originals={key:path.read_text()for key,path in paths.items()}
    rows=declarations(originals);programs=[];controls=[];red=[];suites={}
    def build(label,release=False):
        binary=product.src.build(package,work,env,label,release)
        programs.append(dict(name=label,path=str(binary),sha256=product.whole.support.codecs.sha(binary),pruned=False));return binary
    def run(binary,label,test='',expected=0,assertion=None):
        status=common.run([str(binary)]+(['--exact',test]if test else[])+['--nocapture'],work/(label+'.log'),package,env,timeout=300)
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
            paths['slots'].write_text(before_slots(originals['slots']))
            binary=build('before-device-slots')
            for label,test,message in [('before-slots-first-charge','empty_is_lazy_and_first_insert_charges_real_native_layout','slot first table omitted physical charge'),('before-slots-growth-charge','full_table_refusal_preserves_all_slots_before_request_side_effects','slot growth bypassed admission')]:
                run(binary,label,PREFIX+test,101,message);red.append(dict(name=label,test=PREFIX+test,exit=101,expected_assertion=message,sha256=programs[-1]['sha256']))
            run(binary,'pcm-before',PCM);before_pcm=pcm_receipt(work/'pcm-before.log');assert before_pcm==current_pcm
            run(binary,'index-pcm-before',INDEX_PCM);before_index_pcm=index_pcm_receipt(work/'index-pcm-before.log');assert before_index_pcm==current_index_pcm
            run(binary,'media-pcm-before',MEDIA_PCM);before_media_pcm=media_pcm_receipt(work/'media-pcm-before.log');assert before_media_pcm==current_media_pcm
            run(binary,'inner-pcm-before',INNER_PCM);before_inner_pcm=inner_pcm_receipt(work/'inner-pcm-before.log');assert before_inner_pcm==current_inner_pcm
            run(binary,'context-pcm-before',CONTEXT_PCM);before_context_pcm=context_pcm_receipt(work/'context-pcm-before.log');assert before_context_pcm==current_context_pcm
            run(binary,'slot-pcm-before',SLOT_PCM);before_slot_pcm=slot_pcm_receipt(work/'slot-pcm-before.log');assert before_slot_pcm==current_slot_pcm
            (work/'pcm-before-comparison.json').write_text(json.dumps(dict(configured_blocks=len(before_pcm),complete_pcm_samples=sum(len(row['pcm'])for row in before_pcm.values()),actor_factory_blocks=len(before_index_pcm),actor_factory_complete_pcm_samples=sum(len(row['pcm'])for row in before_index_pcm.values()),media_control_blocks=len(before_media_pcm),media_control_complete_pcm_samples=sum(len(row['pcm'])for row in before_media_pcm.values()),all_bits_and_collection_and_clock_equal=True,all_media_state_position_and_ordered_membership_equal=True,collection_ids_canonicalized=True,actual_before_program_sha256=programs[-1]['sha256'],original_unaccounted_standard_pending_slot_table=True,slot_blocks=len(before_slot_pcm),slot_complete_pcm_samples=sum(len(row['pcm'])for row in before_slot_pcm.values()),all_slot_pcm_clock_and_collection_equal=True,context_table_blocks=len(before_context_pcm),context_table_complete_pcm_samples=sum(len(row['pcm'])for row in before_context_pcm.values()),all_context_table_clock_and_collection_equal=True,inner_player_blocks=len(before_inner_pcm),inner_player_complete_pcm_samples=sum(len(row['pcm'])for row in before_inner_pcm.values()),all_inner_state_position_equal=True,production_cycle_algorithm_unchanged=True,production_cycle_order_unchanged=False,test_only_canonical_mix_order=True),indent=2)+'\n')
            programs[-1]['pruned']=True;binary.unlink()
        finally:
            for key in ['slots']:paths[key].write_text(originals[key])
        for row in rows:
            keys=set()
            try:
                for key,before,after in row['changes']:
                    assert key not in keys;keys.add(key);paths[key].write_text(originals[key].replace(before,after))
                binary=build(row['name']);run(binary,row['name'],row['test'],101,row['expected_assertion'])
                controls.append({key:value for key,value in row.items()if key!='changes'}|dict(exit=101,sha256=programs[-1]['sha256']));programs[-1]['pruned']=True;binary.unlink();(work/'completed-controls.json').write_text(json.dumps(controls,indent=2)+'\n');print('Actual pending slot fault rejected',row['name'],flush=True)
            finally:
                for key in keys:paths[key].write_text(originals[key])
        release=build('positive-release',True);run(release,'full-release');run(release,'pcm-release',PCM);assert pcm_receipt(work/'pcm-release.log')==current_pcm,'O3 complete context PCM/collection/clock changed'
        run(release,'index-pcm-release',INDEX_PCM);assert index_pcm_receipt(work/'index-pcm-release.log')==current_index_pcm
        run(release,'media-pcm-release',MEDIA_PCM);assert media_pcm_receipt(work/'media-pcm-release.log')==current_media_pcm
        run(release,'inner-pcm-release',INNER_PCM);assert inner_pcm_receipt(work/'inner-pcm-release.log')==current_inner_pcm
        run(release,'context-pcm-release',CONTEXT_PCM);assert context_pcm_receipt(work/'context-pcm-release.log')==current_context_pcm
        run(release,'slot-pcm-release',SLOT_PCM);assert slot_pcm_receipt(work/'slot-pcm-release.log')==current_slot_pcm
    product.whole.verify_inputs(work,frozen)
    assert all(path.read_text()==originals[key]for key,path in paths.items())
    root=ROOT/'engine/crates/audio/src';assert all((package/'src'/p.relative_to(root)).read_bytes()==p.read_bytes()for p in root.rglob('*')if p.is_file())
    (work/'summary.json').write_text(json.dumps(dict(programs=programs,controls=controls,original_red=red,suites=suites,root_inputs_unchanged=True,source_copies_restored=True,root_dev_profile_verified=True,isolated_release_is_not_root_shipping=True,preflight=preflight),indent=2)+'\n');print('Pending device slots verified',len(controls),'fresh faults',flush=True)


if __name__=='__main__':
    parser=argparse.ArgumentParser(description=__doc__);parser.add_argument('--output',type=Path,required=True);parser.add_argument('--target-dir',type=Path,required=True);parser.add_argument('--before-directory',type=Path,required=True);parser.add_argument('--preflight',action='store_true');args=parser.parse_args();main(args.output.resolve(),args.target_dir.resolve(),args.before_directory.resolve(),args.preflight)
