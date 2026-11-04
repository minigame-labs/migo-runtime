#!/usr/bin/env python3
"""Actor inner-player table admission, synchronous cancellation and complete PCM gate."""
from __future__ import annotations
import argparse,json,re
from pathlib import Path
import check_audio_media_players as media_gate
base=media_gate.base
wave=media_gate.wave
cold=media_gate.cold
graph=media_gate.graph
ROOT=base.ROOT
PCM=media_gate.PCM
pcm_receipt=media_gate.pcm_receipt
INDEX_PCM=media_gate.INDEX_PCM
index_pcm_receipt=media_gate.index_pcm_receipt
MEDIA_PCM=media_gate.MEDIA_PCM
media_pcm_receipt=media_gate.media_pcm_receipt
PREFIX='audio_thread::inner_collection_tests::'
INNER_PCM=PREFIX+'complete_inner_player_table_pcm_and_lifecycle_trace'
def inner_pcm_receipt(log):
    decoder=json.JSONDecoder();rows={}
    for line in log.read_text().splitlines():
        if not line.startswith('MIGO_INNER_PCM '):continue
        _,key,tail=line.split(' ',2);values=[]
        for _ in range(3):
            value,end=decoder.raw_decode(tail);values.append(value);tail=tail[end:].lstrip()
        assert not tail and key not in rows
        rate,channels,frames=map(int,key.split(':'));assert len(values[0])==frames*channels+channels-1
        rows[key]=dict(pcm=values[0],states=values[1],positions=values[2])
    assert len(rows)==36,(log,len(rows))
    return rows
def declarations(originals):
    rows=[]
    def add(name,key,before,after,test,message):
        assert before!=after and originals[key].count(before)==1,(name,key,'anchor drift')
        rows.append(dict(name=name,changes=[(key,before,after)],test=PREFIX+test,expected_assertion=message))
    initial='initial_table_charges_native_layout_and_retires_real_buckets'
    saturated='initial_refusal_precedes_bucket_allocation'
    refusal='growth_refusal_precedes_factory_and_keeps_live_players'
    duplicate='duplicate_create_is_allocation_free_after_cpu_close'
    factory='failed_factory_does_not_install_reserved_growth'
    growth='successful_growth_preserves_shared_identity_and_overlapping_table_credit'
    remove='remove_cancels_immediately_and_numeric_id_reuse_rejects_old_completion'
    shutdown='shutdown_cancels_live_players_before_closed_cpu_releases_empty_table'
    method=wave.method(originals['inner'],'    pub(super) fn new(')
    add('inner-initial-unaccounted','inner',method,method.replace('        let initial = Initial::reserve(&host, 16)?;\n','').replace('initial.finish()','GraphMap::with_capacity(16)'),initial,'inner initial table omitted physical charge')
    add('inner-buckets-before-initial-admission','inner','        let initial = Initial::reserve(&host, 16)?;','        let _early=HashMap::<u32,InnerAudioPlayer>::with_capacity(16);\n        let initial = Initial::reserve(&host, 16)?;',saturated,'inner buckets allocated before initial admission')
    method=wave.method(originals['map'],'    pub(crate) fn reserve(\n        data: &GraphMap<V>,')
    add('inner-growth-unaccounted','map',method,method.replace('Admission::reserve(host, layout.bytes)','Admission::reserve(host, 0)'),refusal,'inner growth bypassed admission')
    reservation='        let plan = Plan::reserve(&self.players, Some(&self.host), self.players.len() + 1)?;'
    pair=reservation+'\n        let player = factory()?;'
    add('inner-factory-before-admission','inner',pair,'        let player = factory()?;\n'+reservation,refusal,'inner factory ran before table admission')
    add('inner-target-before-admission','inner',reservation,'        let _early=HashMap::<u32,InnerAudioPlayer>::with_capacity(self.players.len()+1);\n'+reservation,refusal,'inner refusal allocated target buckets')
    add('inner-duplicate-admits-growth','inner','        if self.players.contains_key(&id) {\n            return Ok(false);\n        }\n','',duplicate,'duplicate inner create attempted new admission')
    pair='        let player = factory()?;\n        if let Some(plan) = plan {\n            plan.install(&mut self.players);\n        }'
    add('inner-install-before-factory','inner',pair,'        if let Some(plan) = plan {plan.install(&mut self.players);}\n        let player = factory()?;',factory,'failed inner factory installed or published growth')
    add('inner-new-player-not-published','inner','        self.players.insert(id, player);','        if id != 10000 {self.players.insert(id, player);}',growth,'inner create did not publish new player')
    add('inner-rehash-loses-live-players','map','            data.insert(key, value);','            drop((key,value));',growth,'inner rehash lost or cloned live player')
    add('inner-remove-does-nothing','inner','        self.players.remove(id)','        let _=id; None',remove,'inner remove left player in table')
    add('inner-cancellation-delayed-to-cpu','inner','        for (_, player) in self.players.drain() {\n            drop(player);\n        }','        // fault: live players stay in CPU-bound table',shutdown,'inner shutdown postponed source cancellation')
    add('inner-original-table-not-transferred','map','            **value = Some(std::mem::take(&mut self.data));','            **value = None;',initial,'inner table freed buckets on caller')
    add('inner-credit-returned-early','retirement','            let lease = self.lease.take().unwrap();','            let mut lease = self.lease.take().unwrap();\n            lease.permit.shrink_to(0);',initial,'inner table returned credit before CPU destruction')
    add('inner-late-handoff-allocation','inner','    fn drop(&mut self) {','    fn drop(&mut self) {\n        let _late=std::hint::black_box(Box::new(1u8));',shutdown,'inner shutdown allocated late handoff')
    for name,before,after,test in graph.declarations(originals['context']):
        rows.append(dict(name=name,changes=[('context',before,after)],test=test,expected_assertion='cold context render allocated'))
    for name,key,before,after,message in cold.declarations({'nodes/wave_shaper.rs':originals['wave'],'context.rs':originals['context']}):
        rows.append(dict(name=name,changes=[('wave'if key=='nodes/wave_shaper.rs'else'context',before,after)],test=cold.TEST,expected_assertion=message))
    assert len(rows)==21
    return rows
def before_inner(current):
    header="""use super::*;
use crate::node_retirement::Host;
pub(super) struct InnerPlayers {players:HashMap<InnerAudioId,InnerAudioPlayer>,_host:Host}
impl InnerPlayers {
pub(super) fn new(host:Host)->EngineResult<Self>{Ok(Self{players:HashMap::with_capacity(16),_host:host})}
pub(super) fn create_with(&mut self,id:InnerAudioId,factory:impl FnOnce()->EngineResult<InnerAudioPlayer>)->EngineResult<bool>{if self.players.contains_key(&id){return Ok(false)}self.players.insert(id,factory()?);Ok(true)}
pub(super) fn remove(&mut self,id:&InnerAudioId)->Option<InnerAudioPlayer>{self.players.remove(id)}
pub(super) fn table_bytes(&self)->usize{crate::graph_table_layout::Layout::for_entries::<(u32,InnerAudioPlayer)>(self.players.capacity()).unwrap().bytes}
pub(super) fn observe_table(&mut self,_hook:Arc<dyn Fn()+Send+Sync>){}
"""
    return header+wave.method(current,'    pub(super) fn identity(')+'\n}\n'+current[current.index('pub(crate) trait PlayerView'):]
def main(work,target,before_directory,preflight=False):
    package,env,frozen=base.prepare(work,target)
    product=base.capacity.product;common=product.whole.support.clock.common
    names={'inner':'audio_thread_inner_players.rs','map':'graph_map.rs','actor':'audio_thread.rs','retirement':'node_retirement.rs','context':'context.rs','wave':'nodes/wave_shaper.rs'}
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
            paths['inner'].write_text(before_inner(originals['inner']))
            binary=build('before-inner-players')
            for label,test,message in [('before-inner-table-charge','initial_table_charges_native_layout_and_retires_real_buckets','inner initial table omitted physical charge'),('before-inner-growth-charge','growth_refusal_precedes_factory_and_keeps_live_players','inner growth bypassed admission')]:
                run(binary,label,PREFIX+test,101,message);red.append(dict(name=label,test=PREFIX+test,exit=101,expected_assertion=message,sha256=programs[-1]['sha256']))
            run(binary,'pcm-before',PCM);before_pcm=pcm_receipt(work/'pcm-before.log');assert before_pcm==current_pcm
            run(binary,'index-pcm-before',INDEX_PCM);before_index_pcm=index_pcm_receipt(work/'index-pcm-before.log');assert before_index_pcm==current_index_pcm
            run(binary,'media-pcm-before',MEDIA_PCM);before_media_pcm=media_pcm_receipt(work/'media-pcm-before.log');assert before_media_pcm==current_media_pcm
            run(binary,'inner-pcm-before',INNER_PCM);before_inner_pcm=inner_pcm_receipt(work/'inner-pcm-before.log');assert before_inner_pcm==current_inner_pcm
            (work/'pcm-before-comparison.json').write_text(json.dumps(dict(configured_blocks=len(before_pcm),complete_pcm_samples=sum(len(row['pcm'])for row in before_pcm.values()),actor_factory_blocks=len(before_index_pcm),actor_factory_complete_pcm_samples=sum(len(row['pcm'])for row in before_index_pcm.values()),media_control_blocks=len(before_media_pcm),media_control_complete_pcm_samples=sum(len(row['pcm'])for row in before_media_pcm.values()),all_bits_and_collection_and_clock_equal=True,all_media_state_position_and_ordered_membership_equal=True,collection_ids_canonicalized=True,actual_before_program_sha256=programs[-1]['sha256'],original_unaccounted_standard_inner_table=True,inner_player_blocks=len(before_inner_pcm),inner_player_complete_pcm_samples=sum(len(row['pcm'])for row in before_inner_pcm.values()),all_inner_state_position_equal=True,production_cycle_algorithm_unchanged=True,production_cycle_order_unchanged=False,test_only_canonical_mix_order=True),indent=2)+'\n')
            programs[-1]['pruned']=True;binary.unlink()
        finally:
            for key in ['inner']:paths[key].write_text(originals[key])
        for row in rows:
            keys=set()
            try:
                for key,before,after in row['changes']:
                    assert key not in keys;keys.add(key);paths[key].write_text(originals[key].replace(before,after))
                binary=build(row['name']);run(binary,row['name'],row['test'],101,row['expected_assertion'])
                controls.append({key:value for key,value in row.items()if key!='changes'}|dict(exit=101,sha256=programs[-1]['sha256']));programs[-1]['pruned']=True;binary.unlink();(work/'completed-controls.json').write_text(json.dumps(controls,indent=2)+'\n');print('Actual inner table fault rejected',row['name'],flush=True)
            finally:
                for key in keys:paths[key].write_text(originals[key])
        release=build('positive-release',True);run(release,'full-release');run(release,'pcm-release',PCM);assert pcm_receipt(work/'pcm-release.log')==current_pcm,'O3 complete context PCM/collection/clock changed'
        run(release,'index-pcm-release',INDEX_PCM);assert index_pcm_receipt(work/'index-pcm-release.log')==current_index_pcm
        run(release,'media-pcm-release',MEDIA_PCM);assert media_pcm_receipt(work/'media-pcm-release.log')==current_media_pcm
        run(release,'inner-pcm-release',INNER_PCM);assert inner_pcm_receipt(work/'inner-pcm-release.log')==current_inner_pcm
    product.whole.verify_inputs(work,frozen)
    assert all(path.read_text()==originals[key]for key,path in paths.items())
    root=ROOT/'engine/crates/audio/src';assert all((package/'src'/p.relative_to(root)).read_bytes()==p.read_bytes()for p in root.rglob('*')if p.is_file())
    (work/'summary.json').write_text(json.dumps(dict(programs=programs,controls=controls,original_red=red,suites=suites,root_inputs_unchanged=True,source_copies_restored=True,root_dev_profile_verified=True,isolated_release_is_not_root_shipping=True,preflight=preflight),indent=2)+'\n');print('Actor inner table verified',len(controls),'fresh faults',flush=True)


if __name__=='__main__':
    parser=argparse.ArgumentParser(description=__doc__);parser.add_argument('--output',type=Path,required=True);parser.add_argument('--target-dir',type=Path,required=True);parser.add_argument('--before-directory',type=Path,required=True);parser.add_argument('--preflight',action='store_true');args=parser.parse_args();main(args.output.resolve(),args.target_dir.resolve(),args.before_directory.resolve(),args.preflight)
