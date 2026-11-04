#!/usr/bin/env python3
"""Actor reverse-index admission and real node factory/PCM regression gate."""
from __future__ import annotations
import argparse,json,re
from pathlib import Path
import check_audio_command_ownership as base
import check_audio_waveshaper_storage as wave
import check_audio_waveshaper_capacity as cold
import check_audio_graph_capacity as graph
import check_audio_render_buffers as render
ROOT=base.ROOT
PREFIX='audio_thread::node_index_tests::'
PCM=render.PCM
pcm_receipt=render.pcm_receipt
INDEX_PCM=PREFIX+'real_factory_routes_keep_types_and_complete_pcm_and_collection'
def index_pcm_receipt(log):
    decoder=json.JSONDecoder();rows={}
    for line in log.read_text().splitlines():
        if not line.startswith('MIGO_INDEX_PCM '):continue
        _,key,tail=line.split(' ',2);items=[]
        for _ in range(3):
            data,end=decoder.raw_decode(tail);items.append(data);tail=tail[end:].lstrip()
        assert key not in rows;rate,channels,frames=map(int,key.split(':'))
        assert len(items[0])==frames*channels+channels-1
        rows[key]=dict(pcm=items[0],collected=items[1],ended=items[2],clock=int(tail))
    assert len(rows)==36,(log,len(rows))
    return rows
def declarations(originals):
    rows=[]
    def add(name,key,before,after,test,message):
        assert before!=after and originals[key].count(before)==1,(name,key,'anchor drift')
        rows.append(dict(name=name,changes=[(key,before,after)],test=PREFIX+test,expected_assertion=message))
    initial='initial_index_charges_actual_standard_table_before_allocating'
    saturated='saturated_index_constructor_refuses_before_bucket_allocation'
    refusal='index_refusal_precedes_factory_and_preserves_every_existing_entry'
    failed='failed_factory_never_installs_reserved_table_or_publishes_binding'
    growth='growth_and_drop_keep_old_and_new_bulk_tables_charged_until_original_cpu_drop'
    clearing='replacement_removal_clear_and_closed_cpu_reuse_keep_live_allocation_credit'
    routing='all_thirteen_factory_routes_refuse_without_changing_graph_or_index'
    golden='real_factory_routes_keep_types_and_complete_pcm_and_collection'
    iir='borrowed_iir_index_refusal_keeps_original_transport_ticket_until_cpu_drop'
    method=wave.method(originals['index'],'    pub(super) fn with_host(')
    changed=method.replace('        let initial = Initial::reserve(&host, 64)?;\n','').replace('initial.finish()','GraphMap::with_capacity(64)')
    add('index-initial-table-unaccounted','index',method,changed,initial,'index constructor omitted actual table allocation charge')
    add('index-buckets-before-admission','index','        let initial = Initial::reserve(&host, 64)?;','        let _early=HashMap::<u32,u32>::with_capacity(64);\n        let initial = Initial::reserve(&host, 64)?;',saturated,'index allocated buckets before admission')
    method=wave.method(originals['index'],'    pub(super) fn create(')
    changed=method.replace('        let required =','        let made=factory();\n        let required =',1).replace('if !factory()','if !made')
    add('index-factory-before-admission','index',method,changed,refusal,'index refusal entered node factory')
    block='        if let Some(plan) = plan {\n            plan.install(&mut self.node_to_ctx);\n        }\n'
    assert method.count(block)==1
    changed=method.replace(block,'').replace('        if !factory()',block+'        if !factory()',1)
    add('index-table-installed-before-factory','index',method,changed,failed,'failed factory installed index table')
    add('index-success-binding-omitted','index','        self.node_to_ctx.insert(node_id, ctx_id);','        // fault: binding omitted',golden,'node factory published wrong index owner')
    add('index-wrong-context-published','index','        self.node_to_ctx.insert(node_id, ctx_id);','        self.node_to_ctx.insert(node_id, ctx_id+1);',golden,'node factory published wrong index owner')
    add('index-failed-factory-published','index','        if !factory() {','        if false && !factory() {',failed,'index published binding after failed factory')
    add('index-context-clear-omitted','index','self.node_to_ctx.retain(|_, value| *value != ctx_id);','self.node_to_ctx.retain(|_, _value| true);',clearing,'context clear left stale index binding')
    add('index-node-unregister-omitted','index','self.node_to_ctx.remove(&node_id);','let _=node_id;',clearing,'node unregister left stale binding')
    add('index-clear-all-omitted','index','self.node_to_ctx.clear();','// fault: clear omitted',clearing,'index clear all left stale bindings')
    add('index-ordinary-factory-bypasses-boundary','creation','    index.create(node_id, ctx_id, || match command {','    let _early=ctx.create_gain(node_id);\n    index.create(node_id, ctx_id, || match command {',routing,'index admission changed graph before refusing')
    method=wave.method(originals['iir'],'pub(super) fn forward(')
    before='        index.create(*node_id, *ctx_id, || {\n            ctx.create_iir_filter(*node_id, feedforward, feedback)\n        });'
    assert method.count(before)==1
    add('index-borrowed-iir-bypasses-boundary','iir',method,method.replace(before,'        ctx.create_iir_filter(*node_id, feedforward, feedback);',1),iir,'IIR index refusal entered node factory')
    add('index-retain-releases-live-credit','map','        self.data.retain(keep);','        self.data.retain(keep);\n        if let Owner::Inline(permit)= &mut self.owner {permit.shrink_to(0);}',clearing,'index deletion returned allocated capacity credit')
    add('index-table-data-not-transferred-to-cpu','map','            **value = Some(std::mem::take(&mut self.data));','            **value = None;',growth,'index growth freed old buckets on caller')
    add('index-drop-allocates-late-handoff','map','    fn drop(&mut self) {','    fn drop(&mut self) {\n        let _late=std::hint::black_box(Box::new(1u8));',growth,'index drop allocated a late handoff')
    add('index-credit-before-real-data-drop','retirement','            let lease = self.lease.take().unwrap();','            let mut lease = self.lease.take().unwrap();\n            lease.permit.shrink_to(0);',growth,'index growth failed to charge overlapping tables')
    for name,before,after,test in graph.declarations(originals['context']):
        rows.append(dict(name=name,changes=[('context',before,after)],test=test,expected_assertion='cold context render allocated'))
    for name,key,before,after,message in cold.declarations({'nodes/wave_shaper.rs':originals['wave'],'context.rs':originals['context']}):
        rows.append(dict(name=name,changes=[('wave'if key=='nodes/wave_shaper.rs'else'context',before,after)],test=cold.TEST,expected_assertion=message))
    assert len(rows)==23
    return rows
def before_index(old_actor,current):
    start=old_actor.index('/// Reverse lookup from node_id');end=old_actor.index('/// Insert a new context',start)
    raw=old_actor[start:end].replace('struct NodeContextIndex {','pub(super) struct NodeContextIndex {').replace('    node_to_ctx:','    host: Option<crate::node_retirement::Host>,\n    node_to_ctx:',1).replace('            node_to_ctx:','            host: None,\n            node_to_ctx:',1)
    raw=re.sub(r'(?m)^    fn ', '    pub(super) fn ',raw)
    bridge="""
impl NodeContextIndex {
    pub(super) fn storage_host(&self)->crate::node_retirement::Host {self.host.as_ref().unwrap().clone()}
    pub(super) fn with_host(host:crate::node_retirement::Host)->EngineResult<Self> {let mut value=Self::new();value.host=Some(host);Ok(value)}
    pub(super) fn new_managed(cpu:migo_io::audio_cpu::AudioCpu,aggregate:Arc<AudioAggregateLedger>,channels:u32)->EngineResult<Self>{Self::with_host(crate::node_retirement::Host::new(cpu,aggregate,channels)?)}
    pub(super) fn create(&mut self,node_id:u32,ctx_id:u32,factory:impl FnOnce()->bool)->bool {if !factory(){return false}self.register(node_id,ctx_id);true}
    pub(super) fn allocation_bytes(&self)->usize {crate::graph_table_layout::Layout::for_entries::<(u32,u32)>(self.node_to_ctx.capacity()).unwrap().bytes}
    pub(super) fn observe(&mut self,_hook:Arc<dyn Fn()+Send+Sync>){}
"""
    identity=wave.method(current,'    pub(super) fn identity(')
    return 'use super::*;\n'+raw+bridge+identity+'\n}\n'
def main(work,target,before_directory,preflight=False):
    package,env,frozen=base.prepare(work,target)
    product=base.capacity.product;common=product.whole.support.clock.common
    names={'index':'audio_thread_node_index.rs','creation':'audio_thread_node_creation.rs','iir':'audio_thread_command_iir.rs','map':'graph_map.rs','actor':'audio_thread.rs','context':'context.rs','retirement':'node_retirement.rs','wave':'nodes/wave_shaper.rs'}
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
            paths['index'].write_text(before_index((before_directory/'engine/crates/audio/src/audio_thread.rs').read_text(),originals['index']))
            binary=build('before-node-index')
            for label,test,message in [('before-index-charge','initial_index_charges_actual_standard_table_before_allocating','index constructor omitted actual table allocation charge'),('before-index-factory','index_refusal_precedes_factory_and_preserves_every_existing_entry','index refusal entered node factory')]:
                run(binary,label,PREFIX+test,101,message);red.append(dict(name=label,test=PREFIX+test,exit=101,expected_assertion=message,sha256=programs[-1]['sha256']))
            run(binary,'pcm-before',PCM);before_pcm=pcm_receipt(work/'pcm-before.log');assert before_pcm==current_pcm
            run(binary,'index-pcm-before',INDEX_PCM);before_index_pcm=index_pcm_receipt(work/'index-pcm-before.log');assert before_index_pcm==current_index_pcm
            (work/'pcm-before-comparison.json').write_text(json.dumps(dict(configured_blocks=len(before_pcm),complete_pcm_samples=sum(len(row['pcm'])for row in before_pcm.values()),actor_factory_blocks=len(before_index_pcm),actor_factory_complete_pcm_samples=sum(len(row['pcm'])for row in before_index_pcm.values()),all_bits_and_collection_and_clock_equal=True,collection_ids_canonicalized=True,actual_before_program_sha256=programs[-1]['sha256'],original_hashmap_index_methods_preserved=True,hashmap_dense_order_fixed_only_in_test_fixture=True,production_cycle_algorithm_unchanged=True,production_cycle_order_unchanged=False),indent=2)+'\n')
            programs[-1]['pruned']=True;binary.unlink()
        finally:
            for key in ['index']:paths[key].write_text(originals[key])
        for row in rows:
            keys=set()
            try:
                for key,before,after in row['changes']:
                    assert key not in keys;keys.add(key);paths[key].write_text(originals[key].replace(before,after))
                binary=build(row['name']);run(binary,row['name'],row['test'],101,row['expected_assertion'])
                controls.append({key:value for key,value in row.items()if key!='changes'}|dict(exit=101,sha256=programs[-1]['sha256']));programs[-1]['pruned']=True;binary.unlink();(work/'completed-controls.json').write_text(json.dumps(controls,indent=2)+'\n');print('Actual node index fault rejected',row['name'],flush=True)
            finally:
                for key in keys:paths[key].write_text(originals[key])
        release=build('positive-release',True);run(release,'full-release');run(release,'pcm-release',PCM);assert pcm_receipt(work/'pcm-release.log')==current_pcm,'O3 complete context PCM/collection/clock changed'
        run(release,'index-pcm-release',INDEX_PCM);assert index_pcm_receipt(work/'index-pcm-release.log')==current_index_pcm
    product.whole.verify_inputs(work,frozen)
    assert all(path.read_text()==originals[key]for key,path in paths.items())
    root=ROOT/'engine/crates/audio/src';assert all((package/'src'/p.relative_to(root)).read_bytes()==p.read_bytes()for p in root.rglob('*')if p.is_file())
    (work/'summary.json').write_text(json.dumps(dict(programs=programs,controls=controls,original_red=red,suites=suites,root_inputs_unchanged=True,source_copies_restored=True,root_dev_profile_verified=True,isolated_release_is_not_root_shipping=True,preflight=preflight),indent=2)+'\n');print('Actor node index verified',len(controls),'fresh faults',flush=True)


if __name__=='__main__':
    parser=argparse.ArgumentParser(description=__doc__);parser.add_argument('--output',type=Path,required=True);parser.add_argument('--target-dir',type=Path,required=True);parser.add_argument('--before-directory',type=Path,required=True);parser.add_argument('--preflight',action='store_true');args=parser.parse_args();main(args.output.resolve(),args.target_dir.resolve(),args.before_directory.resolve(),args.preflight)
