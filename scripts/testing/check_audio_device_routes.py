#!/usr/bin/env python3
"""Active device-route capacity, synchronous owner handoff and complete SRC PCM gate."""
from __future__ import annotations
import argparse,json,re
from pathlib import Path
import check_audio_device_slots as previous
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
PREFIX='device_prepare::pull::session::resources::owner::mixer::storage_tests::'
ROUTE_PCM=PREFIX+'complete_active_route_pcm_clock_and_rehash_trace'
def route_pcm_receipt(log):
    decoder=json.JSONDecoder();rows={}
    for line in log.read_text().splitlines():
        if not line.startswith('MIGO_ROUTE_PCM '):continue
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
    first='empty_is_lazy_and_first_table_charges_real_native_layout'
    refusal='first_refusal_precedes_request_and_keeps_idle_retry_without_rendering'
    growth='full_table_refusal_preserves_routes_and_does_not_call_factory'
    factory='failed_factory_preserves_table_and_refunds_growth_without_target_buckets'
    rehash='bulk_rehash_preserves_epochs_and_keeps_overlapping_credit_to_cpu_drop'
    remove='remove_retain_and_restart_keep_retained_bucket_credit'
    shutdown='closed_cpu_drop_releases_route_owners_before_empty_bulk_buckets_die'
    method=wave.method(originals['routes'],'    pub(super) fn managed(')
    add('routes-first-table-unaccounted','routes',method,method.replace('host: Some(host)','host: None'),first,'route first table omitted physical charge')
    reservation='        let plan = Plan::reserve(&self.data, self.host.as_ref(), required)?;'
    add('routes-first-buckets-before-admission','routes',reservation,'        let _early=HashMap::<u32,Route>::with_capacity(required);\n'+reservation,refusal,'route first buckets allocated before admission')
    method=wave.method(originals['map'],'    pub(crate) fn reserve(\n        data: &GraphMap<V>,')
    add('routes-growth-unaccounted','map',method,method.replace('Admission::reserve(host, layout.bytes)','Admission::reserve(host, if required==29 {0}else{layout.bytes})'),growth,'route factory ran before capacity admission')
    add('routes-factory-before-admission','routes',reservation+'\n        let route = factory()?;','        let route = factory()?;\n'+reservation,growth,'route factory ran before capacity admission')
    pair='        let route = factory()?;\n        if let Some(plan) = plan {\n            plan.install(&mut self.data);\n        }'
    add('routes-installed-before-factory','routes',pair,'        if let Some(plan)=plan {plan.install(&mut self.data); }\n        let route=factory()?;',factory,'failed route factory installed or published table growth')
    add('routes-new-entry-not-published','routes','        self.data.insert(id, route);','        if id!=10000 {self.data.insert(id,route);}',rehash,'route creation did not publish new entry')
    add('routes-rehash-loses-epochs','routes','        self.data.insert(id, route);','        if id==10000 {self.data.clear();}\n        self.data.insert(id,route);',rehash,'route rehash lost existing epochs')
    add('routes-restart-does-nothing','routes','        self.data.clear();','        // fault: retain routes',remove,'route restart retained entries')
    add('routes-retain-does-nothing','routes','        self.data.retain(keep);','        let _=keep;',remove,'route retain left discarded entries')
    add('routes-live-drop-delayed-to-cpu','routes','        for (_, route) in self.data.drain() {\n            drop(route);\n        }','        // fault: retain Device bindings in CPU-bound buckets',shutdown,'route shutdown postponed live route destruction')
    add('routes-real-buckets-not-transferred','map','            **value = Some(std::mem::take(&mut self.data));','            **value = None;',rehash,'route rehash freed old buckets on caller')
    add('routes-credit-returned-early','retirement','            let lease = self.lease.take().unwrap();','            let mut lease = self.lease.take().unwrap();\n            lease.permit.shrink_to(0);',rehash,'route rehash returned overlapping bucket credit')
    add('routes-late-handoff-allocation','routes','    fn drop(&mut self) {','    fn drop(&mut self) {\n        let _late=std::hint::black_box(Box::new(1u8));',shutdown,'route shutdown allocated late handoff')
    add('routes-retry-not-scheduled','mixer','                        self.admission_retry = Some(Instant::now() + ADMISSION_RETRY);','                        self.admission_retry = None;',refusal,'route admission refusal lost idle retry')
    retry='            .chain(\n                self.admission_retry\n                    .map(|at| at.saturating_duration_since(now)),\n            )'
    add('routes-idle-wait-ignores-retry','mixer',retry,'',refusal,'route admission refusal lost idle retry')
    reset='        self.admission_retry = None;\n        self.routes.retain'
    add('routes-success-keeps-old-retry','mixer',reset,'        self.routes.retain',refusal,'route success retained obsolete retry deadline')
    for name,before,after,test in graph.declarations(originals['context']):
        rows.append(dict(name=name,changes=[('context',before,after)],test=test,expected_assertion='cold context render allocated'))
    for name,key,before,after,message in cold.declarations({'nodes/wave_shaper.rs':originals['wave'],'context.rs':originals['context']}):
        rows.append(dict(name=name,changes=[('wave'if key=='nodes/wave_shaper.rs'else'context',before,after)],test=cold.TEST,expected_assertion=message))
    assert len(rows)==23
    return rows
def before_routes(current):
    head="""use super::*;
use crate::node_retirement::Host as StorageHost;
use std::collections::HashMap;
use std::ops::Deref;
pub(super) struct Routes {data:HashMap<u32,Route>,_host:Option<StorageHost>}
impl Routes {
pub(super) fn new()->Self{Self{data:HashMap::new(),_host:None}}
pub(super) fn managed(host:StorageHost)->Self{Self{data:HashMap::new(),_host:Some(host)}}
pub(super) fn create_with(&mut self,id:u32,factory:impl FnOnce()->EngineResult<Route>)->EngineResult<()>{let route=factory()?;self.data.insert(id,route);Ok(())}
pub(super) fn table_bytes(&self)->usize{crate::graph_table_layout::Layout::for_entries::<(u32,Route)>(self.data.capacity()).unwrap().bytes}
pub(super) fn observe(&mut self,_hook:Arc<dyn Fn()+Send+Sync>){}
"""
    for name in ['get_mut','remove','retain','clear','identity','insert']:head+=wave.method(current,'    pub(super) fn '+name+'(')+'\n'
    return head+'}\n'+current[current.index('impl Deref for Routes'):]
def main(work,target,before_directory,preflight=False):
    package,env,frozen=base.prepare(work,target)
    product=base.capacity.product;common=product.whole.support.clock.common
    names={'routes':'device_route_storage.rs','mixer':'device_mixer.rs','map':'graph_map.rs','actor':'audio_thread.rs','retirement':'node_retirement.rs','context':'context.rs','wave':'nodes/wave_shaper.rs'}
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
    run(positive,'route-pcm-debug',ROUTE_PCM);current_route_pcm=route_pcm_receipt(work/'route-pcm-debug.log')
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
            paths['routes'].write_text(before_routes(originals['routes']))
            binary=build('before-device-routes')
            for label,test,message in [('before-routes-first-charge','empty_is_lazy_and_first_table_charges_real_native_layout','route first table omitted physical charge'),('before-routes-growth-charge','full_table_refusal_preserves_routes_and_does_not_call_factory','route factory ran before capacity admission')]:
                run(binary,label,PREFIX+test,101,message);red.append(dict(name=label,test=PREFIX+test,exit=101,expected_assertion=message,sha256=programs[-1]['sha256']))
            run(binary,'pcm-before',PCM);before_pcm=pcm_receipt(work/'pcm-before.log');assert before_pcm==current_pcm
            run(binary,'index-pcm-before',INDEX_PCM);before_index_pcm=index_pcm_receipt(work/'index-pcm-before.log');assert before_index_pcm==current_index_pcm
            run(binary,'media-pcm-before',MEDIA_PCM);before_media_pcm=media_pcm_receipt(work/'media-pcm-before.log');assert before_media_pcm==current_media_pcm
            run(binary,'inner-pcm-before',INNER_PCM);before_inner_pcm=inner_pcm_receipt(work/'inner-pcm-before.log');assert before_inner_pcm==current_inner_pcm
            run(binary,'context-pcm-before',CONTEXT_PCM);before_context_pcm=context_pcm_receipt(work/'context-pcm-before.log');assert before_context_pcm==current_context_pcm
            run(binary,'slot-pcm-before',SLOT_PCM);before_slot_pcm=slot_pcm_receipt(work/'slot-pcm-before.log');assert before_slot_pcm==current_slot_pcm
            run(binary,'route-pcm-before',ROUTE_PCM);before_route_pcm=route_pcm_receipt(work/'route-pcm-before.log');assert before_route_pcm==current_route_pcm
            (work/'pcm-before-comparison.json').write_text(json.dumps(dict(configured_blocks=len(before_pcm),complete_pcm_samples=sum(len(row['pcm'])for row in before_pcm.values()),actor_factory_blocks=len(before_index_pcm),actor_factory_complete_pcm_samples=sum(len(row['pcm'])for row in before_index_pcm.values()),media_control_blocks=len(before_media_pcm),media_control_complete_pcm_samples=sum(len(row['pcm'])for row in before_media_pcm.values()),all_bits_and_collection_and_clock_equal=True,all_media_state_position_and_ordered_membership_equal=True,collection_ids_canonicalized=True,actual_before_program_sha256=programs[-1]['sha256'],original_unaccounted_standard_active_route_table=True,route_blocks=len(before_route_pcm),route_complete_pcm_samples=sum(len(row['pcm'])for row in before_route_pcm.values()),all_route_pcm_clock_and_collection_equal=True,slot_blocks=len(before_slot_pcm),slot_complete_pcm_samples=sum(len(row['pcm'])for row in before_slot_pcm.values()),all_slot_pcm_clock_and_collection_equal=True,context_table_blocks=len(before_context_pcm),context_table_complete_pcm_samples=sum(len(row['pcm'])for row in before_context_pcm.values()),all_context_table_clock_and_collection_equal=True,inner_player_blocks=len(before_inner_pcm),inner_player_complete_pcm_samples=sum(len(row['pcm'])for row in before_inner_pcm.values()),all_inner_state_position_equal=True,production_cycle_algorithm_unchanged=True,production_cycle_order_unchanged=False,test_only_canonical_mix_order=True),indent=2)+'\n')
            programs[-1]['pruned']=True;binary.unlink()
        finally:
            for key in ['routes']:paths[key].write_text(originals[key])
        for row in rows:
            keys=set()
            try:
                for key,before,after in row['changes']:
                    assert key not in keys;keys.add(key);paths[key].write_text(originals[key].replace(before,after))
                binary=build(row['name']);run(binary,row['name'],row['test'],101,row['expected_assertion'])
                controls.append({key:value for key,value in row.items()if key!='changes'}|dict(exit=101,sha256=programs[-1]['sha256']));programs[-1]['pruned']=True;binary.unlink();(work/'completed-controls.json').write_text(json.dumps(controls,indent=2)+'\n');print('Actual active route fault rejected',row['name'],flush=True)
            finally:
                for key in keys:paths[key].write_text(originals[key])
        release=build('positive-release',True);run(release,'full-release');run(release,'pcm-release',PCM);assert pcm_receipt(work/'pcm-release.log')==current_pcm,'O3 complete context PCM/collection/clock changed'
        run(release,'index-pcm-release',INDEX_PCM);assert index_pcm_receipt(work/'index-pcm-release.log')==current_index_pcm
        run(release,'media-pcm-release',MEDIA_PCM);assert media_pcm_receipt(work/'media-pcm-release.log')==current_media_pcm
        run(release,'inner-pcm-release',INNER_PCM);assert inner_pcm_receipt(work/'inner-pcm-release.log')==current_inner_pcm
        run(release,'context-pcm-release',CONTEXT_PCM);assert context_pcm_receipt(work/'context-pcm-release.log')==current_context_pcm
        run(release,'slot-pcm-release',SLOT_PCM);assert slot_pcm_receipt(work/'slot-pcm-release.log')==current_slot_pcm
        run(release,'route-pcm-release',ROUTE_PCM);assert route_pcm_receipt(work/'route-pcm-release.log')==current_route_pcm
    product.whole.verify_inputs(work,frozen)
    assert all(path.read_text()==originals[key]for key,path in paths.items())
    root=ROOT/'engine/crates/audio/src';assert all((package/'src'/p.relative_to(root)).read_bytes()==p.read_bytes()for p in root.rglob('*')if p.is_file())
    (work/'summary.json').write_text(json.dumps(dict(programs=programs,controls=controls,original_red=red,suites=suites,root_inputs_unchanged=True,source_copies_restored=True,root_dev_profile_verified=True,isolated_release_is_not_root_shipping=True,preflight=preflight),indent=2)+'\n');print('Active device routes verified',len(controls),'fresh faults',flush=True)


if __name__=='__main__':
    parser=argparse.ArgumentParser(description=__doc__);parser.add_argument('--output',type=Path,required=True);parser.add_argument('--target-dir',type=Path,required=True);parser.add_argument('--before-directory',type=Path,required=True);parser.add_argument('--preflight',action='store_true');args=parser.parse_args();main(args.output.resolve(),args.target_dir.resolve(),args.before_directory.resolve(),args.preflight)
