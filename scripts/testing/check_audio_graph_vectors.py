#!/usr/bin/env python3
"""Own graph vector capacities and reserve complete growth batches before mutation.

Keep actual whole old-context PCM comparisons and cold graph contracts. Every
source fault must compile fresh and reach the designated Rust business assertion.
"""
from __future__ import annotations
import argparse
import json
from pathlib import Path
import re
import check_audio_command_ownership as base
import check_audio_waveshaper_storage as wave
import check_audio_waveshaper_capacity as cold
import check_audio_graph_capacity as graph
import check_audio_render_buffers as render

ROOT = base.ROOT
PREFIX = 'audio_thread::graph_storage_tests::'
PCM = render.PCM
pcm_receipt = render.pcm_receipt


def declarations(originals):
    rows=[]
    def add(name,key,before,after,test,assertion):
        assert before!=after and originals[key].count(before)==1,(name,key,'anchor drift')
        rows.append(dict(name=name,changes=[(key,before,after)],test=PREFIX+test,expected_assertion=assertion))
    migration='all_eighteen_capacities_adopt_without_copy_and_keep_credit_until_cpu_drop'
    batch='node_batch_refusal_precedes_every_vector_allocation_and_mutation'
    node='failed_node_insertion_keeps_graph_and_recycled_buffer'
    edge='edge_refusal_preserves_connections_and_dc_liveness'
    ended='ended_watch_refusal_keeps_original_watchers_and_flags'
    growth='growth_preserves_all_prefixes_and_keeps_old_cpu_credit'
    payload='live_cpu_vector_transfers_real_payload_without_allocating_on_drop'
    boundary='inline_bulk_boundary_reuse_and_foreign_ledger_remain_exact'
    closed='closed_host_reuses_capacity_but_refuses_new_growth'
    add('graph-adoption-length-for-capacity','vector','Admission::reserve(host, self.capacity())?','Admission::reserve(host, self.len())?',migration,'graph adoption omitted actual vector capacities')
    add('graph-cpu-capacity-uncharged','vector','host.reserve_value_with_bytes::<Option<Vec<T>>>(bytes)?','host.reserve_value_with_bytes::<Option<Vec<T>>>(0)?',batch,'node graph growth bypassed physical admission')
    add('graph-inline-capacity-uncharged','vector','host.reserve_inline(bytes)','host.reserve_inline(0)',boundary,'small graph vector acquired artificial handoff')
    add('graph-bulk-boundary-inline','vector','if bytes < 4096 {','if bytes <= 4096 {',boundary,'bulk graph vector omitted CPU handoff')
    add('graph-allocation-before-batch-admission','vector','        let admission = Admission::reserve(host, capacity)?;','        let _premature = Vec::<T>::with_capacity(capacity);\n        let admission = Admission::reserve(host, capacity)?;',batch,'graph vectors allocated before complete batch admission')
    add('graph-last-node-reservation-omitted','plans','native_collected: Plan::reserve(&ctx.native_collected, host, nodes)?,','native_collected: None,',batch,'node graph growth bypassed physical admission')
    add('graph-last-edge-reservation-omitted','plans','edge_dst_port: Plan::reserve(&ctx.edge_dst_port, host, edges)?,','edge_dst_port: None,',edge,'edge batch bypassed vector admission')
    add('graph-node-growth-not-reserved','context','NodeGrowth::reserve(self, nodes)','{ let _ = nodes; NodeGrowth::reserve(self, 0) }',node,'node insertion ignored vector admission')
    method=wave.method(originals['context'],'    pub fn add_node(')
    start=method.index('        let next_nodes =');end=method.index('        let buffer = match',start)
    segment=method[start:end];changed=method[:start]+method[end:];anchor='        growth.install(self);';assert changed.count(anchor)==1;changed=changed.replace(anchor,segment+anchor,1)
    add('graph-node-pops-pool-before-reservation','context',method,changed,node,'failed insertion changed graph capacity')
    add('graph-edge-enables-dc-before-admission','context','            let growth = self.reserve_graph_edges(edges)?;','            if let Some(node) = self.nodes.get_mut(&src) { node.set_output_connected(true); }\n            let growth = self.reserve_graph_edges(edges)?;',edge,'refused connection enabled DC source')
    add('graph-ended-watch-mutated-before-reservation','context','        if enabled {\n            let watchers =','        if enabled {\n            self.ended_watch.insert(node_id);\n            let watchers =',ended,'refused ended watch changed watchers or controls')
    add('graph-growth-loses-prefix','vector','                data.extend_from_slice(&target.data);','                if let Some(first) = target.data.first() { data.resize(target.len(), first.clone()); }',growth,'graph vector growth lost original prefix')
    add('graph-real-payload-not-transferred','vector','            **value = Some(std::mem::take(&mut self.data));','            let _ = value;',payload,'graph vector payload dropped away from original CPU')
    add('graph-drop-allocates-late-handoff','vector','            **value = Some(std::mem::take(&mut self.data));','            **value = Some(self.data.drain(..).collect());',payload,'graph drop allocated a late handoff')
    add('graph-credit-returned-before-payload','retirement','            let lease = self.lease.take().unwrap();','            let mut lease = self.lease.take().unwrap();\n            lease.permit.shrink_to(0);',payload,'graph vector credit returned before payload destruction')
    add('graph-cpu-owner-dropped-inline','retirement','if self.lease.is_some() {','if false {',payload,'graph vector payload dropped away from original CPU')
    add('graph-foreign-inline-accepted','vector','host.accepts_inline(permit)','{ let _ = permit; true }',boundary,'graph vector accepted foreign ledger')
    add('graph-foreign-cpu-accepted','vector','value.accepts_physical_ledger(host)','{ let _ = value; true }',boundary,'graph vector accepted foreign ledger')
    add('graph-reuse-requires-new-ticket','vector','        if required <= data.capacity() {\n            return Ok(None);','        if required <= data.capacity() {\n            if let Some(host) = host { let _ = host.reserve_inline(1)?; }\n            return Ok(None);',closed,'existing graph capacity required fresh admission')
    method=wave.method(originals['vector'],'    fn check_capacity(')
    add('graph-capacity-guard-omitted','vector',method,'    fn check_capacity(&self, _required: usize) {}',closed,'managed graph vector escaped admitted capacity')
    add('graph-context-adoption-omitted','context','        self.prepare_graph_vectors(&host)?;','        // fault: graph vectors omitted',migration,'graph adoption omitted actual vector capacities')
    for name,before,after,test in graph.declarations(originals['context']):
        rows.append(dict(name=name,changes=[('context',before,after)],test=test,expected_assertion='cold context render allocated'))
    for name,key,before,after,assertion in cold.declarations({'nodes/wave_shaper.rs':originals['wave'],'context.rs':originals['context']}):
        rows.append(dict(name=name,changes=[('wave'if key=='nodes/wave_shaper.rs'else'context',before,after)],test=cold.TEST,expected_assertion=assertion))
    assert len(rows)==28
    return rows


def before_context(raw):
    assert "connections: Vec<NodeConnection>" in raw and "GraphVec" not in raw
    return raw + '\nimpl AudioContext {\n    #[cfg(test)]\n    pub(crate) fn graph_vector_identities(&self) -> Vec<(&\'static str,usize,usize,usize,usize)> {\n        fn row<T>(name: &\'static str,data: &Vec<T>)->(&\'static str,usize,usize,usize,usize) {(name,data.len(),data.capacity(),data.as_ptr()as usize,data.capacity()*std::mem::size_of::<T>())}\n        vec![\n            row("connections",&self.connections),\n            row("processing_order",&self.processing_order),\n            row("dense_ids",&self.dense_ids),\n            row("in_degree",&self.in_degree),\n            row("input_start",&self.input_start),\n            row("input_edges",&self.input_edges),\n            row("output_start",&self.output_start),\n            row("output_edges",&self.output_edges),\n            row("ready_queue",&self.ready_queue),\n            row("edge_src",&self.edge_src),\n            row("edge_src_port",&self.edge_src_port),\n            row("edge_dst_port",&self.edge_dst_port),\n            row("collected",&self.collected),\n            row("ended",&self.ended),\n            row("mix_buffer",&self.mix_buffer),\n            row("native_input",&self.native_input),\n            row("native_collected",&self.native_collected),\n            row("buffer_pool",&self.buffer_pool),\n        ]\n    }\n    #[cfg(test)]\n    pub(crate) fn graph_test_state(&self) -> String {\n        let mut watchers:Vec<_>=self.ended_watch.iter().copied().collect();watchers.sort_unstable();\n        let mut nodes:Vec<_>=self.nodes.keys().copied().collect();nodes.sort_unstable();\n        format!("{:?}",vec![\n            format!("{:?}", &self.connections),\n            format!("{:?}", &self.processing_order),\n            format!("{:?}", &self.dense_ids),\n            format!("{:?}", &self.in_degree),\n            format!("{:?}", &self.input_start),\n            format!("{:?}", &self.input_edges),\n            format!("{:?}", &self.output_start),\n            format!("{:?}", &self.output_edges),\n            format!("{:?}", &self.ready_queue),\n            format!("{:?}", &self.edge_src),\n            format!("{:?}", &self.edge_src_port),\n            format!("{:?}", &self.edge_dst_port),\n            format!("{:?}", &self.collected),\n            format!("{:?}", &self.ended),\n            format!("{:?}", &self.mix_buffer),\n            format!("{:?}", &self.native_input),\n            format!("{:?}", &self.native_collected),\n            format!("{:?}",(watchers,nodes,self.graph_dirty,self.device_controls_pending))\n        ])\n    }\n    #[cfg(test)]\n    pub(crate) fn set_native_graph_capacity(&mut self,name:&str,capacity:usize) {\n        match name {\n            "connections" => {let mut data=Vec::with_capacity(capacity.max(self.connections.len()));data.append(&mut self.connections);self.connections=data;},\n            "processing_order" => {let mut data=Vec::with_capacity(capacity.max(self.processing_order.len()));data.append(&mut self.processing_order);self.processing_order=data;},\n            "dense_ids" => {let mut data=Vec::with_capacity(capacity.max(self.dense_ids.len()));data.append(&mut self.dense_ids);self.dense_ids=data;},\n            "in_degree" => {let mut data=Vec::with_capacity(capacity.max(self.in_degree.len()));data.append(&mut self.in_degree);self.in_degree=data;},\n            "input_start" => {let mut data=Vec::with_capacity(capacity.max(self.input_start.len()));data.append(&mut self.input_start);self.input_start=data;},\n            "input_edges" => {let mut data=Vec::with_capacity(capacity.max(self.input_edges.len()));data.append(&mut self.input_edges);self.input_edges=data;},\n            "output_start" => {let mut data=Vec::with_capacity(capacity.max(self.output_start.len()));data.append(&mut self.output_start);self.output_start=data;},\n            "output_edges" => {let mut data=Vec::with_capacity(capacity.max(self.output_edges.len()));data.append(&mut self.output_edges);self.output_edges=data;},\n            "ready_queue" => {let mut data=Vec::with_capacity(capacity.max(self.ready_queue.len()));data.append(&mut self.ready_queue);self.ready_queue=data;},\n            "edge_src" => {let mut data=Vec::with_capacity(capacity.max(self.edge_src.len()));data.append(&mut self.edge_src);self.edge_src=data;},\n            "edge_src_port" => {let mut data=Vec::with_capacity(capacity.max(self.edge_src_port.len()));data.append(&mut self.edge_src_port);self.edge_src_port=data;},\n            "edge_dst_port" => {let mut data=Vec::with_capacity(capacity.max(self.edge_dst_port.len()));data.append(&mut self.edge_dst_port);self.edge_dst_port=data;},\n            "collected" => {let mut data=Vec::with_capacity(capacity.max(self.collected.len()));data.append(&mut self.collected);self.collected=data;},\n            "ended" => {let mut data=Vec::with_capacity(capacity.max(self.ended.len()));data.append(&mut self.ended);self.ended=data;},\n            "mix_buffer" => {let mut data=Vec::with_capacity(capacity.max(self.mix_buffer.len()));data.append(&mut self.mix_buffer);self.mix_buffer=data;},\n            "native_input" => {let mut data=Vec::with_capacity(capacity.max(self.native_input.len()));data.append(&mut self.native_input);self.native_input=data;},\n            "native_collected" => {let mut data=Vec::with_capacity(capacity.max(self.native_collected.len()));data.append(&mut self.native_collected);self.native_collected=data;},\n            "buffer_pool" => {let mut data=Vec::with_capacity(capacity.max(self.buffer_pool.len()));data.append(&mut self.buffer_pool);self.buffer_pool=data;},\n            _=>panic!("unknown graph vector")\n        }\n    }\n    #[cfg(test)]\n    pub(crate) fn observe_graph_vector(&mut self,_name:&str,_hook:Arc<dyn Fn()+Send+Sync>) {}\n    #[cfg(test)]\n    pub(crate) fn prepare_graph_vectors_for_test(&mut self,_host:&crate::node_retirement::Host)->EngineResult<()> {Ok(())}\n    #[cfg(test)]\n    pub(crate) fn grow_graph_nodes_for_test(&mut self,_nodes:usize)->EngineResult<()> {Ok(())}\n    pub(crate) fn try_connect_ports(&mut self,src:AudioNodeId,src_output:u32,dst:AudioNodeId,dst_input:u32)->EngineResult<bool> {\n        let duplicate=self.connections.iter().any(|c|c.src==src&&c.dst==dst&&c.src_output==src_output&&c.dst_input==dst_input);\n        self.connect_ports(src,src_output,dst,dst_input);Ok(!duplicate)\n    }\n}\n'


def main(work,target,before_directory,preflight=False):
    package,env,frozen=base.prepare(work,target)
    product=base.capacity.product;common=product.whole.support.clock.common
    names={'vector':'graph_vec.rs','plans':'context_graph_storage.rs','context':'context.rs','retirement':'node_retirement.rs','wave':'nodes/wave_shaper.rs'}
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
    assert common.run(['cargo','clippy','--lib','--tests','--offline','--locked','--message-format=json'],work/'clippy.jsonl',package,env,timeout=300)==0
    old_manifest=json.loads((before_directory/'root-sha256.json').read_text());changed=[n for n,h in old_manifest.items()if n.startswith('engine/crates/audio/src/')and product.whole.support.codecs.sha(ROOT/n)!=h]
    saved={n:(package/'src'/n.removeprefix('engine/crates/audio/src/')).read_bytes()for n in changed}
    try:
        for n in changed:(package/'src'/n.removeprefix('engine/crates/audio/src/')).write_bytes((before_directory/n).read_bytes())
        assert common.run(['cargo','clippy','--lib','--tests','--offline','--locked','--message-format=json'],work/'clippy-before.jsonl',package,env,timeout=300)==0
    finally:
        for n,data in saved.items():(package/'src'/n.removeprefix('engine/crates/audio/src/')).write_bytes(data)
    old,current=wave.diagnostics(work/'clippy-before.jsonl',package),wave.diagnostics(work/'clippy.jsonl',package);added=current-old
    delta=dict(before=sum(old.values()),after=sum(current.values()),new_diagnostics=sum(added.values()),exact_multiset=old==current,actor_included=True,new=[dict(key=key,count=count)for key,count in added.items()]);(work/'clippy-delta.json').write_text(json.dumps(delta,indent=2)+'\n');assert not added,delta
    if not preflight:
        try:
            paths['context'].write_text(before_context((before_directory/'engine/crates/audio/src/context.rs').read_text()))
            binary=build('before-graph-storage')
            for label,test,assertion in [('before-graph-adoption','all_eighteen_capacities_adopt_without_copy_and_keep_credit_until_cpu_drop','graph adoption omitted actual vector capacities'),('before-edge-admission','edge_refusal_preserves_connections_and_dc_liveness','edge batch bypassed vector admission')]:
                run(binary,label,PREFIX+test,101,assertion);red.append(dict(name=label,test=PREFIX+test,exit=101,expected_assertion=assertion,sha256=programs[-1]['sha256']))
            run(binary,'pcm-before',PCM);before_pcm=pcm_receipt(work/'pcm-before.log');assert before_pcm==current_pcm,'complete old context PCM/collection/clock changed'
            (work/'pcm-before-comparison.json').write_text(json.dumps(dict(configured_blocks=len(before_pcm),complete_pcm_samples=sum(len(row['pcm'])for row in before_pcm.values()),all_bits_and_collection_and_clock_equal=True,collection_ids_canonicalized=True,actual_before_program_sha256=programs[-1]['sha256'],old_numeric_context_body_unchanged=True,hashmap_dense_order_fixed_only_in_test_fixture=True,production_cycle_order_unchanged=True),indent=2)+'\n')
            programs[-1]['pruned']=True;binary.unlink()
        finally:
            for key in ['context']:paths[key].write_text(originals[key])
        for row in rows:
            keys=set()
            try:
                for key,before,after in row['changes']:
                    assert key not in keys;keys.add(key);paths[key].write_text(originals[key].replace(before,after))
                binary=build(row['name']);run(binary,row['name'],row['test'],101,row['expected_assertion'])
                controls.append({key:value for key,value in row.items()if key!='changes'}|dict(exit=101,sha256=programs[-1]['sha256']));programs[-1]['pruned']=True;binary.unlink();(work/'completed-controls.json').write_text(json.dumps(controls,indent=2)+'\n');print('Actual render storage fault rejected',row['name'],flush=True)
            finally:
                for key in keys:paths[key].write_text(originals[key])
        release=build('positive-release',True);run(release,'full-release');run(release,'pcm-release',PCM);assert pcm_receipt(work/'pcm-release.log')==current_pcm,'O3 complete context PCM/collection/clock changed'
    product.whole.verify_inputs(work,frozen)
    assert all(path.read_text()==originals[key]for key,path in paths.items())
    root=ROOT/'engine/crates/audio/src';assert all((package/'src'/p.relative_to(root)).read_bytes()==p.read_bytes()for p in root.rglob('*')if p.is_file())
    (work/'summary.json').write_text(json.dumps(dict(programs=programs,controls=controls,original_red=red,suites=suites,root_inputs_unchanged=True,source_copies_restored=True,root_dev_profile_verified=True,isolated_release_is_not_root_shipping=True,preflight=preflight),indent=2)+'\n');print('Graph vector storage verified',len(controls),'fresh faults',flush=True)


if __name__=='__main__':
    parser=argparse.ArgumentParser(description=__doc__);parser.add_argument('--output',type=Path,required=True);parser.add_argument('--target-dir',type=Path,required=True);parser.add_argument('--before-directory',type=Path,required=True);parser.add_argument('--preflight',action='store_true');args=parser.parse_args();main(args.output.resolve(),args.target_dir.resolve(),args.before_directory.resolve(),args.preflight)
