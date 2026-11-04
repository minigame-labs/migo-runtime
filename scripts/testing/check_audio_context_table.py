#!/usr/bin/env python3
"""Actor context table admission, original lifecycle barriers and complete PCM gate."""
from __future__ import annotations
import argparse,json,re
from pathlib import Path
import check_audio_inner_players as previous
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
PREFIX='audio_thread::context_table_tests::'
CONTEXT_PCM=PREFIX+'complete_managed_context_table_factory_pcm_clock_and_route_trace'
def context_pcm_receipt(log):
    decoder=json.JSONDecoder();rows={}
    for line in log.read_text().splitlines():
        if not line.startswith('MIGO_CONTEXT_PCM '):continue
        _,key,tail=line.split(' ',2);values=[]
        for _ in range(3):
            value,end=decoder.raw_decode(tail);values.append(value);tail=tail[end:].lstrip()
        clock=int(tail);assert key not in rows
        rate,channels,frames=map(int,key.split(':'));assert len(values[0])==frames*channels+channels-1
        rows[key]=dict(pcm=values[0],collected=values[1],ended=values[2],clock=clock)
    assert len(rows)==36,(log,len(rows))
    return rows
def declarations(originals):
    rows=[]
    def add(name,key,before,after,test,message):
        assert before!=after and originals[key].count(before)==1,(name,key,'anchor drift')
        rows.append(dict(name=name,changes=[(key,before,after)],test=PREFIX+test,expected_assertion=message))
    initial='initial_table_charges_native_layout_and_retires_original_buckets'
    saturated='initial_refusal_precedes_bucket_allocation'
    refusal='growth_refusal_precedes_context_factory_and_ledger_lookup'
    duplicate='duplicate_create_is_allocation_free_after_cpu_close'
    factory='actual_constructor_refusal_does_not_install_reserved_table_growth'
    growth='successful_growth_moves_graphs_and_keeps_overlapping_table_credit'
    remove='scoped_close_revokes_only_claimed_route_and_preserves_table_capacity'
    shutdown='shutdown_revokes_routes_before_closed_cpu_destroys_original_empty_table'
    method=wave.method(originals['table'],'    pub(super) fn new(')
    add('context-table-initial-unaccounted','table',method,method.replace('        let initial = Initial::reserve(&host, 4)?;\n','').replace('initial.finish()','GraphMap::with_capacity(4)'),initial,'context initial table omitted physical charge')
    add('context-buckets-before-initial-admission','table','        let initial = Initial::reserve(&host, 4)?;','        let _early=HashMap::<u32,AudioContext>::with_capacity(4);\n        let initial = Initial::reserve(&host, 4)?;',saturated,'context buckets allocated before initial admission')
    method=wave.method(originals['map'],'    pub(crate) fn reserve(\n        data: &GraphMap<V>,')
    add('context-table-growth-unaccounted','map',method,method.replace('Admission::reserve(host, layout.bytes)','Admission::reserve(host, 0)'),refusal,'context growth bypassed admission')
    reservation='        let plan = Plan::reserve(&self.contexts, Some(&self.host), self.contexts.len() + 1)?;'
    add('context-factory-before-table-admission','table',reservation+'\n        let context = factory()?;','        let context = factory()?;\n'+reservation,refusal,'context factory entered before table admission')
    add('context-target-buckets-before-admission','table',reservation,'        let _early=HashMap::<u32,AudioContext>::with_capacity(self.contexts.len()+1);\n'+reservation,refusal,'context refusal allocated target buckets')
    add('context-duplicate-attempts-admission','table','        if self.contexts.contains_key(&id) {\n            return Ok(false);\n        }\n','',duplicate,'duplicate context attempted new table admission')
    pair='        let context = factory()?;\n        if let Some(plan) = plan {\n            plan.install(&mut self.contexts);\n        }'
    add('context-table-installed-before-factory','table',pair,'        if let Some(plan) = plan {plan.install(&mut self.contexts);}\n        let context = factory()?;',factory,'failed context factory installed or published table growth')
    add('context-new-entry-not-published','table','        self.contexts.insert(id, context);','        if id != 10000 {self.contexts.insert(id, context);}',growth,'context creation did not publish new entry')
    add('context-table-rehash-loses-entries','map','            data.insert(key, value);','            drop((key,value));',growth,'context rehash lost live entries')
    add('context-table-remove-does-nothing','table','        self.contexts.remove(id)','        let _=id; None',remove,'scoped context release left table entry')
    add('context-route-revocation-delayed-to-cpu','table','        for (_, context) in self.contexts.drain() {\n            drop(context);\n        }','        // fault: live contexts remain in CPU-bound buckets',shutdown,'context shutdown postponed route revocation')
    add('context-original-buckets-not-transferred','map','            **value = Some(std::mem::take(&mut self.data));','            **value = None;',initial,'context table freed original buckets on caller')
    add('context-table-credit-returned-early','retirement','            let lease = self.lease.take().unwrap();','            let mut lease = self.lease.take().unwrap();\n            lease.permit.shrink_to(0);',initial,'context table returned credit before CPU destruction')
    add('context-table-late-handoff-allocation','table','    fn drop(&mut self) {','    fn drop(&mut self) {\n        let _late=std::hint::black_box(Box::new(1u8));',shutdown,'context shutdown allocated late handoff')
    for name,before,after,test in graph.declarations(originals['context']):
        rows.append(dict(name=name,changes=[('context',before,after)],test=test,expected_assertion='cold context render allocated'))
    for name,key,before,after,message in cold.declarations({'nodes/wave_shaper.rs':originals['wave'],'context.rs':originals['context']}):
        rows.append(dict(name=name,changes=[('wave'if key=='nodes/wave_shaper.rs'else'context',before,after)],test=cold.TEST,expected_assertion=message))
    assert len(rows)==21
    return rows
def before_table(current):
    header="""use super::*;
use crate::node_retirement::Host;
pub(super) struct ContextTable {contexts:HashMap<AudioContextId,AudioContext>,_host:Host}
impl ContextTable {
pub(super) fn new(host:Host)->EngineResult<Self>{Ok(Self{contexts:HashMap::with_capacity(4),_host:host})}
pub(super) fn create_with(&mut self,id:AudioContextId,factory:impl FnOnce()->EngineResult<AudioContext>)->EngineResult<bool>{if self.contexts.contains_key(&id){return Ok(false)}let context=factory()?;self.contexts.insert(id,context);Ok(true)}
pub(super) fn table_bytes(&self)->usize{crate::graph_table_layout::Layout::for_entries::<(u32,AudioContext)>(self.contexts.capacity()).unwrap().bytes}
pub(super) fn observe_table(&mut self,_hook:Arc<dyn Fn()+Send+Sync>){}
"""
    return header+wave.method(current,'    pub(super) fn identity(')+'\n}\n'+current[current.index('pub(crate) trait ContextView'):]
def main(work,target,before_directory,preflight=False):
    package,env,frozen=base.prepare(work,target)
    product=base.capacity.product;common=product.whole.support.clock.common
    names={'table':'audio_thread_context_table.rs','map':'graph_map.rs','actor':'audio_thread.rs','retirement':'node_retirement.rs','context':'context.rs','wave':'nodes/wave_shaper.rs'}
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
            paths['table'].write_text(before_table(originals['table']))
            binary=build('before-context-table')
            for label,test,message in [('before-context-table-charge','initial_table_charges_native_layout_and_retires_original_buckets','context initial table omitted physical charge'),('before-context-table-growth','growth_refusal_precedes_context_factory_and_ledger_lookup','context growth bypassed admission')]:
                run(binary,label,PREFIX+test,101,message);red.append(dict(name=label,test=PREFIX+test,exit=101,expected_assertion=message,sha256=programs[-1]['sha256']))
            run(binary,'pcm-before',PCM);before_pcm=pcm_receipt(work/'pcm-before.log');assert before_pcm==current_pcm
            run(binary,'index-pcm-before',INDEX_PCM);before_index_pcm=index_pcm_receipt(work/'index-pcm-before.log');assert before_index_pcm==current_index_pcm
            run(binary,'media-pcm-before',MEDIA_PCM);before_media_pcm=media_pcm_receipt(work/'media-pcm-before.log');assert before_media_pcm==current_media_pcm
            run(binary,'inner-pcm-before',INNER_PCM);before_inner_pcm=inner_pcm_receipt(work/'inner-pcm-before.log');assert before_inner_pcm==current_inner_pcm
            run(binary,'context-pcm-before',CONTEXT_PCM);before_context_pcm=context_pcm_receipt(work/'context-pcm-before.log');assert before_context_pcm==current_context_pcm
            (work/'pcm-before-comparison.json').write_text(json.dumps(dict(configured_blocks=len(before_pcm),complete_pcm_samples=sum(len(row['pcm'])for row in before_pcm.values()),actor_factory_blocks=len(before_index_pcm),actor_factory_complete_pcm_samples=sum(len(row['pcm'])for row in before_index_pcm.values()),media_control_blocks=len(before_media_pcm),media_control_complete_pcm_samples=sum(len(row['pcm'])for row in before_media_pcm.values()),all_bits_and_collection_and_clock_equal=True,all_media_state_position_and_ordered_membership_equal=True,collection_ids_canonicalized=True,actual_before_program_sha256=programs[-1]['sha256'],original_unaccounted_standard_context_table=True,context_table_blocks=len(before_context_pcm),context_table_complete_pcm_samples=sum(len(row['pcm'])for row in before_context_pcm.values()),all_context_table_clock_and_collection_equal=True,inner_player_blocks=len(before_inner_pcm),inner_player_complete_pcm_samples=sum(len(row['pcm'])for row in before_inner_pcm.values()),all_inner_state_position_equal=True,production_cycle_algorithm_unchanged=True,production_cycle_order_unchanged=False,test_only_canonical_mix_order=True),indent=2)+'\n')
            programs[-1]['pruned']=True;binary.unlink()
        finally:
            for key in ['table']:paths[key].write_text(originals[key])
        for row in rows:
            keys=set()
            try:
                for key,before,after in row['changes']:
                    assert key not in keys;keys.add(key);paths[key].write_text(originals[key].replace(before,after))
                binary=build(row['name']);run(binary,row['name'],row['test'],101,row['expected_assertion'])
                controls.append({key:value for key,value in row.items()if key!='changes'}|dict(exit=101,sha256=programs[-1]['sha256']));programs[-1]['pruned']=True;binary.unlink();(work/'completed-controls.json').write_text(json.dumps(controls,indent=2)+'\n');print('Actual context table fault rejected',row['name'],flush=True)
            finally:
                for key in keys:paths[key].write_text(originals[key])
        release=build('positive-release',True);run(release,'full-release');run(release,'pcm-release',PCM);assert pcm_receipt(work/'pcm-release.log')==current_pcm,'O3 complete context PCM/collection/clock changed'
        run(release,'index-pcm-release',INDEX_PCM);assert index_pcm_receipt(work/'index-pcm-release.log')==current_index_pcm
        run(release,'media-pcm-release',MEDIA_PCM);assert media_pcm_receipt(work/'media-pcm-release.log')==current_media_pcm
        run(release,'inner-pcm-release',INNER_PCM);assert inner_pcm_receipt(work/'inner-pcm-release.log')==current_inner_pcm
        run(release,'context-pcm-release',CONTEXT_PCM);assert context_pcm_receipt(work/'context-pcm-release.log')==current_context_pcm
    product.whole.verify_inputs(work,frozen)
    assert all(path.read_text()==originals[key]for key,path in paths.items())
    root=ROOT/'engine/crates/audio/src';assert all((package/'src'/p.relative_to(root)).read_bytes()==p.read_bytes()for p in root.rglob('*')if p.is_file())
    (work/'summary.json').write_text(json.dumps(dict(programs=programs,controls=controls,original_red=red,suites=suites,root_inputs_unchanged=True,source_copies_restored=True,root_dev_profile_verified=True,isolated_release_is_not_root_shipping=True,preflight=preflight),indent=2)+'\n');print('Actor context table verified',len(controls),'fresh faults',flush=True)


if __name__=='__main__':
    parser=argparse.ArgumentParser(description=__doc__);parser.add_argument('--output',type=Path,required=True);parser.add_argument('--target-dir',type=Path,required=True);parser.add_argument('--before-directory',type=Path,required=True);parser.add_argument('--preflight',action='store_true');args=parser.parse_args();main(args.output.resolve(),args.target_dir.resolve(),args.before_directory.resolve(),args.preflight)
