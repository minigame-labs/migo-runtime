#!/usr/bin/env python3
"""Actual media table/vector admission, ordered control and PCM regression gate."""
from __future__ import annotations
import argparse,json,re
from pathlib import Path
import check_audio_command_ownership as base
import check_audio_waveshaper_storage as wave
import check_audio_waveshaper_capacity as cold
import check_audio_graph_capacity as graph
import check_audio_render_buffers as render
import check_audio_node_index as index_gate
ROOT=base.ROOT
PREFIX='audio_thread::media_membership_tests::'
PCM=render.PCM
pcm_receipt=render.pcm_receipt
INDEX_PCM=index_gate.INDEX_PCM
index_pcm_receipt=index_gate.index_pcm_receipt
MEDIA_PCM=PREFIX+'complete_media_controls_pcm_and_ordered_trace'
def media_pcm_receipt(log):
    decoder=json.JSONDecoder();rows={};members={}
    for line in log.read_text().splitlines():
        if line.startswith('MIGO_MEDIA_MEMBERS '):
            _,key,tail=line.split(' ',2);assert key not in members;members[key]=json.loads(tail)
        elif line.startswith('MIGO_MEDIA_PCM '):
            _,key,tail=line.split(' ',2);values=[]
            for _ in range(3):
                value,end=decoder.raw_decode(tail);values.append(value);tail=tail[end:].lstrip()
            assert not tail and key not in rows
            rate,channels,frames=map(int,key.split(':'));assert len(values[0])==frames*channels+channels-1
            rows[key]=dict(pcm=values[0],states=values[1],positions=values[2])
    assert len(rows)==len(members)==36 and set(rows)==set(members),(log,len(rows),len(members))
    return {key:row|dict(members=members[key])for key,row in rows.items()}
def declarations(originals):
    rows=[]
    def add(name,key,before,after,test,message):
        assert before!=after and originals[key].count(before)==1,(name,key,'anchor drift')
        rows.append(dict(name=name,changes=[(key,before,after)],test=PREFIX+test,expected_assertion=message))
    initial='initial_table_charges_real_owned_layout_and_drops_after_data'
    saturated='initial_refusal_allocates_no_table_buckets'
    table='table_refusal_preserves_all_membership_and_vector_addresses'
    vector='vector_refusal_precedes_copy_and_keeps_duplicate_and_unknown_noops'
    remove='removal_keeps_capacity_credit_and_reuses_slots_after_cpu_close'
    reset='duplicate_create_and_destroy_retire_real_large_vectors_without_late_allocation'
    bulk='bulk_table_growth_moves_nested_vectors_and_keeps_both_allocations_until_cpu_drop'
    rehash='successful_table_growth_preserves_existing_order_and_vector_identity'
    controls='playback_lookup_and_controls_follow_only_current_membership'
    method=wave.method(originals['media'],'    pub(super) fn new(')
    changed=method.replace('        let initial = Initial::reserve(&host, 4)?;\n','').replace('initial.finish()','GraphMap::with_capacity(4)')
    add('media-initial-table-unaccounted','media',method,changed,initial,'media initial table omitted physical charge')
    add('media-buckets-before-initial-admission','media','        let initial = Initial::reserve(&host, 4)?;','        let _early=HashMap::<u32,GraphVec<InnerAudioId>>::with_capacity(4);\n        let initial = Initial::reserve(&host, 4)?;',saturated,'media table buckets allocated before admission')
    method=wave.method(originals['map'],'    pub(crate) fn reserve(\n        data: &GraphMap<V>,')
    add('media-table-growth-unaccounted','map',method,method.replace('Admission::reserve(host, layout.bytes)','Admission::reserve(host, 0)'),table,'media table growth bypassed admission')
    add('media-target-table-before-admission','media','        let plan = Plan::reserve(&self.players, Some(&self.host), required)?;','        let _early=HashMap::<u32,GraphVec<InnerAudioId>>::with_capacity(required);\n        let plan = Plan::reserve(&self.players, Some(&self.host), required)?;',table,'media table refusal allocated target buckets')
    add('media-player-not-published','media','        self.players.insert(id, GraphVec::new());','        // fault: not published',controls,'empty existing media player lost start intent')
    add('media-duplicate-create-no-reset','media','        self.players.insert(id, GraphVec::new());','        if !self.players.contains_key(&id) {self.players.insert(id,GraphVec::new());}',reset,'repeated media create did not reset members')
    method=wave.method(originals['vector'],'    pub(crate) fn reserve(\n        data: &GraphVec<T>,')
    add('media-vector-growth-unaccounted','vector',method,method.replace('Admission::reserve(host, capacity)?','Admission::reserve(host, 0)?'),vector,'media vector growth bypassed admission')
    add('media-vector-copy-before-admission','media','        let plan = VectorPlan::reserve(sources, Some(&self.host), sources.len() + 1)?;','        let _early=sources.to_vec();\n        let plan = VectorPlan::reserve(sources, Some(&self.host), sources.len() + 1)?;',vector,'media vector copied before admission')
    guard='        if sources.contains(&source) {\n            return Ok(());\n        }\n'
    add('media-source-dedup-omitted','media',guard,'',controls,'media add changed order or duplicated source')
    add('media-remove-does-nothing','media','            sources.retain(|value| *value != source);','            sources.retain(|_value| true);',controls,'media stop changed an unrelated source')
    add('media-destroy-does-nothing','media','        self.players.remove(&id);','        let _=id;',reset,'media destroy left membership')
    add('media-vector-data-not-transferred','vector','            **value = Some(std::mem::take(&mut self.data));','            **value = None;',reset,'media reset freed source data on caller')
    add('media-table-data-not-transferred','map','            **value = Some(std::mem::take(&mut self.data));','            **value = None;',bulk,'media growth freed old table on caller')
    add('media-credit-before-actual-drop','retirement','            let lease = self.lease.take().unwrap();','            let mut lease = self.lease.take().unwrap();\n            lease.permit.shrink_to(0);',bulk,'media rehash released overlapping table charge')
    add('media-vector-late-handoff-allocation','vector','    fn drop(&mut self) {','    fn drop(&mut self) {\n        let _late=std::hint::black_box(Box::new(1u8));',reset,'media reset or destroy allocated late handoff')
    add('media-rehash-loses-existing-players','map','            data.insert(key, value);','            drop((key,value));',rehash,'media successful rehash lost existing player or moved source allocation')
    for name,before,after,test in graph.declarations(originals['context']):
        rows.append(dict(name=name,changes=[('context',before,after)],test=test,expected_assertion='cold context render allocated'))
    for name,key,before,after,message in cold.declarations({'nodes/wave_shaper.rs':originals['wave'],'context.rs':originals['context']}):
        rows.append(dict(name=name,changes=[('wave'if key=='nodes/wave_shaper.rs'else'context',before,after)],test=cold.TEST,expected_assertion=message))
    assert len(rows)==23
    return rows
def before_media(current):
    header="""use super::*;
use crate::node_retirement::Host;
type Identity=(usize,usize,Vec<(u32,Vec<u32>,usize,usize)>);
pub(super) struct MediaPlayers {players:HashMap<u32,Vec<InnerAudioId>>,_host:Host}
impl MediaPlayers {
pub(super) fn new(host:Host)->EngineResult<Self>{Ok(Self{players:HashMap::with_capacity(4),_host:host})}
pub(super) fn create(&mut self,id:u32)->EngineResult<()> {self.players.insert(id,Vec::new());Ok(())}
pub(super) fn add_source(&mut self,id:u32,source:InnerAudioId)->EngineResult<()> {if let Some(sources)=self.players.get_mut(&id){if !sources.contains(&source){sources.push(source)}}Ok(())}
pub(super) fn remove_source(&mut self,id:u32,source:InnerAudioId){if let Some(sources)=self.players.get_mut(&id){sources.retain(|&id|id!=source)}}
pub(super) fn get(&self,id:&u32)->Option<&[InnerAudioId]>{self.players.get(id).map(Vec::as_slice)}
pub(super) fn destroy(&mut self,id:u32){self.players.remove(&id);}
pub(super) fn table_bytes(&self)->usize{crate::graph_table_layout::Layout::for_entries::<(u32,Vec<InnerAudioId>)>(self.players.capacity()).unwrap().bytes}
pub(super) fn observe_table(&mut self,_hook:Arc<dyn Fn()+Send+Sync>){}
pub(super) fn observe_sources(&mut self,_id:u32,_hook:Arc<dyn Fn()+Send+Sync>){}
"""
    for method in ['start','stop','identity']:
        header+=wave.method(current,'    pub(super) fn '+method+'(')+'\n'
    return header+'}\n'+current[current.index('#[cfg(any(target_os = "ios", test))]'):]
def main(work,target,before_directory,preflight=False):
    package,env,frozen=base.prepare(work,target)
    product=base.capacity.product;common=product.whole.support.clock.common
    names={'media':'audio_thread_media_players.rs','index':'audio_thread_node_index.rs','map':'graph_map.rs','vector':'graph_vec.rs','actor':'audio_thread.rs','retirement':'node_retirement.rs','context':'context.rs','wave':'nodes/wave_shaper.rs'}
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
            paths['media'].write_text(before_media(originals['media']))
            binary=build('before-media-players')
            for label,test,message in [('before-media-table-charge','initial_table_charges_real_owned_layout_and_drops_after_data','media initial table omitted physical charge'),('before-media-vector-charge','vector_refusal_precedes_copy_and_keeps_duplicate_and_unknown_noops','media vector growth bypassed admission')]:
                run(binary,label,PREFIX+test,101,message);red.append(dict(name=label,test=PREFIX+test,exit=101,expected_assertion=message,sha256=programs[-1]['sha256']))
            run(binary,'pcm-before',PCM);before_pcm=pcm_receipt(work/'pcm-before.log');assert before_pcm==current_pcm
            run(binary,'index-pcm-before',INDEX_PCM);before_index_pcm=index_pcm_receipt(work/'index-pcm-before.log');assert before_index_pcm==current_index_pcm
            run(binary,'media-pcm-before',MEDIA_PCM);before_media_pcm=media_pcm_receipt(work/'media-pcm-before.log');assert before_media_pcm==current_media_pcm
            (work/'pcm-before-comparison.json').write_text(json.dumps(dict(configured_blocks=len(before_pcm),complete_pcm_samples=sum(len(row['pcm'])for row in before_pcm.values()),actor_factory_blocks=len(before_index_pcm),actor_factory_complete_pcm_samples=sum(len(row['pcm'])for row in before_index_pcm.values()),media_control_blocks=len(before_media_pcm),media_control_complete_pcm_samples=sum(len(row['pcm'])for row in before_media_pcm.values()),all_bits_and_collection_and_clock_equal=True,all_media_state_position_and_ordered_membership_equal=True,collection_ids_canonicalized=True,actual_before_program_sha256=programs[-1]['sha256'],original_unaccounted_standard_media_table_and_vectors=True,production_cycle_algorithm_unchanged=True,production_cycle_order_unchanged=False,test_only_canonical_mix_order=True),indent=2)+'\n')
            programs[-1]['pruned']=True;binary.unlink()
        finally:
            for key in ['media']:paths[key].write_text(originals[key])
        for row in rows:
            keys=set()
            try:
                for key,before,after in row['changes']:
                    assert key not in keys;keys.add(key);paths[key].write_text(originals[key].replace(before,after))
                binary=build(row['name']);run(binary,row['name'],row['test'],101,row['expected_assertion'])
                controls.append({key:value for key,value in row.items()if key!='changes'}|dict(exit=101,sha256=programs[-1]['sha256']));programs[-1]['pruned']=True;binary.unlink();(work/'completed-controls.json').write_text(json.dumps(controls,indent=2)+'\n');print('Actual media membership fault rejected',row['name'],flush=True)
            finally:
                for key in keys:paths[key].write_text(originals[key])
        release=build('positive-release',True);run(release,'full-release');run(release,'pcm-release',PCM);assert pcm_receipt(work/'pcm-release.log')==current_pcm,'O3 complete context PCM/collection/clock changed'
        run(release,'index-pcm-release',INDEX_PCM);assert index_pcm_receipt(work/'index-pcm-release.log')==current_index_pcm
        run(release,'media-pcm-release',MEDIA_PCM);assert media_pcm_receipt(work/'media-pcm-release.log')==current_media_pcm
    product.whole.verify_inputs(work,frozen)
    assert all(path.read_text()==originals[key]for key,path in paths.items())
    root=ROOT/'engine/crates/audio/src';assert all((package/'src'/p.relative_to(root)).read_bytes()==p.read_bytes()for p in root.rglob('*')if p.is_file())
    (work/'summary.json').write_text(json.dumps(dict(programs=programs,controls=controls,original_red=red,suites=suites,root_inputs_unchanged=True,source_copies_restored=True,root_dev_profile_verified=True,isolated_release_is_not_root_shipping=True,preflight=preflight),indent=2)+'\n');print('Actor media membership verified',len(controls),'fresh faults',flush=True)


if __name__=='__main__':
    parser=argparse.ArgumentParser(description=__doc__);parser.add_argument('--output',type=Path,required=True);parser.add_argument('--target-dir',type=Path,required=True);parser.add_argument('--before-directory',type=Path,required=True);parser.add_argument('--preflight',action='store_true');args=parser.parse_args();main(args.output.resolve(),args.target_dir.resolve(),args.before_directory.resolve(),args.preflight)
