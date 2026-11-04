#!/usr/bin/env python3
"""Verify actual standard hash allocations, atomic growth, and original CPU custody.

Pinned Rust/std layout is checked against observed allocator/free bytes. Retain
whole old-context PCM receipts and legacy graph/WaveShaper cold source faults.
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
ROOT=base.ROOT
PREFIX='audio_thread::graph_table_tests::'
PCM=render.PCM
pcm_receipt=render.pcm_receipt

def declarations(originals):
    rows=[]
    def add(name,key,before,after,test,assertion):
        assert before!=after and originals[key].count(before)==1,(name,key,'anchor drift')
        rows.append(dict(name=name,changes=[(key,before,after)],test=PREFIX+test,expected_assertion=assertion))
    model='allocation_model_matches_real_standard_map_and_set_allocations_and_frees'
    removed='removals_and_empty_tables_keep_original_allocation_credit'
    adopt='all_six_tables_adopt_without_copy_and_stay_charged_until_original_cpu_drop'
    batch='growth_batch_reserves_before_any_table_allocation'
    node='last_node_table_refusal_keeps_graph_nodes_and_recycled_buffer'
    growth='table_growth_transfers_nonclone_values_and_retains_old_table_credit'
    payload='actual_table_payload_drops_on_original_cpu_without_late_allocation'
    boundary='inline_bulk_foreign_ledger_and_repeated_adoption_are_exact'
    closed='closed_host_reuses_owned_slots_and_guard_blocks_unadmitted_insert'
    watch='watch_table_refusal_keeps_notifications_and_original_tables'
    add('table-control-bytes-omitted','layout','.checked_add(buckets)','.checked_add(0)',model,'hash layout omitted real allocation bytes')
    add('table-group-width-wrong','layout','"16" => 16,','"16" => 8,',model,'standard table capacity model changed')
    add('table-adoption-live-length','map','Admission::reserve(host, self.layout.bytes)?','Admission::reserve(host, Layout::for_entries::<(u32,V)>(self.len())?.bytes)?',removed,'empty table adoption used live entries instead of allocation')
    add('table-bulk-data-credit-omitted','map','host.reserve_value_with_bytes::<Option<Data<V>>>(bytes)?','host.reserve_value_with_bytes::<Option<Data<V>>>(0)?',boundary,'bulk table omitted CPU credit')
    add('table-inline-credit-omitted','map','host.reserve_inline(bytes)','host.reserve_inline(0)',boundary,'inline table credit differs from allocation')
    add('table-final-context-adoption-omitted','context','        self.ended_watch.map.prepare(&host)?;','        // fault: last table not adopted',adopt,'context table adoption omitted allocated capacity')
    last='released: Plan::reserve(&ctx.released.map, host, nodes)?,'
    add('table-last-node-ticket-omitted','tables',last,'released: None,',node,'node table batch bypassed admission')
    method=wave.method(originals['context'],'    pub fn add_node(')
    a=method.index('        let tables = match');b=method.index('        let buffer = match',a);segment=method[a:b];changed=method[:a]+method[b:];changed=changed.replace('        growth.install(self);',segment+'        growth.install(self);')
    add('table-pool-popped-before-admission','context',method,changed,node,'refused tables changed vector allocation')
    anchor='        let admission = host\n'
    add('table-data-allocated-before-batch','map',anchor,'        let _premature=HashMap::<u32,V>::with_capacity(layout.capacity);\n'+anchor,batch,'table allocated before complete batch admission')
    anchor='            let table = match crate::graph_map::Plan::reserve('
    add('table-watch-controls-mutated-first','context',anchor,'            self.device_controls_pending = true;\n'+anchor,watch,'refused table watch changed controls')
    add('table-growth-loses-entries','map','            data.insert(key, value);','            let _ = key; data.insert(0, value);',growth,'table growth lost entries')
    add('table-actual-payload-not-transferred','map','            **value = Some(std::mem::take(&mut self.data));','            let _ = value;',payload,'actual table payload escaped original CPU')
    add('table-drop-allocates-late','map','            **value = Some(std::mem::take(&mut self.data));','            **value = Some(self.data.drain().collect());',payload,'table drop allocated a late handoff')
    add('table-credit-returned-before-data','retirement','            let lease = self.lease.take().unwrap();','            let mut lease = self.lease.take().unwrap();\n            lease.permit.shrink_to(0);',payload,'table credit returned before payload destruction')
    add('table-foreign-inline-accepted','map','host.accepts_inline(permit)','{ let _ = permit; true }',boundary,'table accepted foreign ledger')
    add('table-foreign-bulk-accepted','map','value.accepts_physical_ledger(host)','{ let _ = value; true }',boundary,'table accepted foreign ledger')
    anchor='        if required <= data.capacity() {\n            return Ok(None);'
    add('table-reuse-requires-new-ticket','map',anchor,'        if required <= data.capacity() {\n            if let Some(host)=host {let _=host.reserve_inline(1)?;}\n            return Ok(None);',closed,'owned table slot required fresh admission')
    method=wave.method(originals['map'],'    pub(crate) fn insert(');a=method.index('        assert!(');b=method.index('        let old = self.data.insert',a);changed=method[:a]+method[b:]
    add('table-capacity-guard-removed','map',method,changed,closed,'managed table bypassed capacity guard')
    add('table-full-replacement-uses-insert','map','            return Some(std::mem::replace(old, value));','            let _=old; return self.data.insert(key,value);',closed,'full table replacement allocated')
    add('table-small-values-offloaded','map','if bytes < 4096 {','if bytes == 0 {',boundary,'inline table credit differs from allocation')
    add('table-native-growth-not-tracked','map','            self.track_native_growth();','            // fault: allocation history omitted',removed,'native table tracking lost actual allocation')
    for name,before,after,test in graph.declarations(originals['context']):
        rows.append(dict(name=name,changes=[('context',before,after)],test=test,expected_assertion='cold context render allocated'))
    for name,key,before,after,assertion in cold.declarations({'nodes/wave_shaper.rs':originals['wave'],'context.rs':originals['context']}):
        rows.append(dict(name=name,changes=[('wave'if key=='nodes/wave_shaper.rs'else'context',before,after)],test=cold.TEST,expected_assertion=assertion))
    assert len(rows)==28
    return rows

def before_context(raw):
    assert "GraphMap<" not in raw and "nodes: HashMap<" in raw
    return raw+'\n#[cfg(test)]\ntype TableIdentity=(&\'static str,usize,usize,usize,Vec<(u32,usize)>);\n#[cfg(test)]\nimpl AudioContext {\n    pub(crate) fn graph_table_identities(&self)->Vec<TableIdentity> {\n        let mut rows=Vec::new();\n        let mut pointers:Vec<_>=self.nodes.iter().map(|(key,value)|(*key,value as *const crate::node_retirement::Node as usize)).collect();pointers.sort_unstable();\n        rows.push(("nodes",self.nodes.len(),self.nodes.capacity(),crate::graph_table_layout::Layout::for_entries::<(u32,crate::node_retirement::Node)>(self.nodes.capacity()).unwrap().bytes,pointers));\n        let mut pointers:Vec<_>=self.buffers.iter().map(|(key,value)|(*key,value as *const crate::node_retirement::Value<Arc<RetainedAudio>> as usize)).collect();pointers.sort_unstable();\n        rows.push(("buffers",self.buffers.len(),self.buffers.capacity(),crate::graph_table_layout::Layout::for_entries::<(u32,crate::node_retirement::Value<Arc<RetainedAudio>>)>(self.buffers.capacity()).unwrap().bytes,pointers));\n        let mut pointers:Vec<_>=self.node_buffers.iter().map(|(key,value)|(*key,value as *const RenderBuffer as usize)).collect();pointers.sort_unstable();\n        rows.push(("node_buffers",self.node_buffers.len(),self.node_buffers.capacity(),crate::graph_table_layout::Layout::for_entries::<(u32,RenderBuffer)>(self.node_buffers.capacity()).unwrap().bytes,pointers));\n        let mut pointers:Vec<_>=self.dense_index.iter().map(|(key,value)|(*key,value as *const u32 as usize)).collect();pointers.sort_unstable();\n        rows.push(("dense_index",self.dense_index.len(),self.dense_index.capacity(),crate::graph_table_layout::Layout::for_entries::<(u32,u32)>(self.dense_index.capacity()).unwrap().bytes,pointers));\n        let mut pointers:Vec<_>=self.released.iter().map(|key|(*key,key as *const u32 as usize)).collect();pointers.sort_unstable();\n        rows.push(("released",self.released.len(),self.released.capacity(),crate::graph_table_layout::Layout::for_entries::<(u32,())>(self.released.capacity()).unwrap().bytes,pointers));\n        let mut pointers:Vec<_>=self.ended_watch.iter().map(|key|(*key,key as *const u32 as usize)).collect();pointers.sort_unstable();\n        rows.push(("ended_watch",self.ended_watch.len(),self.ended_watch.capacity(),crate::graph_table_layout::Layout::for_entries::<(u32,())>(self.ended_watch.capacity()).unwrap().bytes,pointers));\n        rows\n    }\n    pub(crate) fn set_native_table_capacity(&mut self,name:&str,capacity:usize) {\n        match name {\n            "nodes" => {let mut data=std::collections::HashMap::with_capacity_and_hasher(capacity.max(self.nodes.len()),self.nodes.hasher().clone());data.extend(self.nodes.drain());self.nodes=data;},\n            "buffers" => {let mut data=std::collections::HashMap::with_capacity_and_hasher(capacity.max(self.buffers.len()),self.buffers.hasher().clone());data.extend(self.buffers.drain());self.buffers=data;},\n            "node_buffers" => {let mut data=std::collections::HashMap::with_capacity_and_hasher(capacity.max(self.node_buffers.len()),self.node_buffers.hasher().clone());data.extend(self.node_buffers.drain());self.node_buffers=data;},\n            "dense_index" => {let mut data=std::collections::HashMap::with_capacity_and_hasher(capacity.max(self.dense_index.len()),self.dense_index.hasher().clone());data.extend(self.dense_index.drain());self.dense_index=data;},\n            "released" => {let mut data=std::collections::HashSet::with_capacity_and_hasher(capacity.max(self.released.len()),self.released.hasher().clone());data.extend(self.released.drain());self.released=data;},\n            "ended_watch" => {let mut data=std::collections::HashSet::with_capacity_and_hasher(capacity.max(self.ended_watch.len()),self.ended_watch.hasher().clone());data.extend(self.ended_watch.drain());self.ended_watch=data;},\n            _=>panic!("unknown table")\n        }\n    }\n    pub(crate) fn observe_graph_table(&mut self,_name:&str,_hook:Arc<dyn Fn()+Send+Sync>) {}\n    pub(crate) fn grow_graph_tables_for_test(&mut self,_nodes:usize)->EngineResult<()> {Ok(())}\n}\n'

def main(work,target,before_directory,preflight=False):
    package,env,frozen=base.prepare(work,target)
    product=base.capacity.product;common=product.whole.support.clock.common
    names={'map':'graph_map.rs','layout':'graph_table_layout.rs','tables':'context_graph_tables.rs','context':'context.rs','retirement':'node_retirement.rs','wave':'nodes/wave_shaper.rs'}
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
            paths['context'].write_text(before_context((before_directory/'engine/crates/audio/src/context.rs').read_text()))
            binary=build('before-table-storage')
            for label,test,assertion in [('before-table-adoption','all_six_tables_adopt_without_copy_and_stay_charged_until_original_cpu_drop','context table adoption omitted allocated capacity'),('before-node-table-batch','last_node_table_refusal_keeps_graph_nodes_and_recycled_buffer','node table batch bypassed admission')]:
                run(binary,label,PREFIX+test,101,assertion);red.append(dict(name=label,test=PREFIX+test,exit=101,expected_assertion=assertion,sha256=programs[-1]['sha256']))
            run(binary,'pcm-before',PCM);before_pcm=pcm_receipt(work/'pcm-before.log');assert before_pcm==current_pcm,'complete old context PCM/collection/clock changed'
            (work/'pcm-before-comparison.json').write_text(json.dumps(dict(configured_blocks=len(before_pcm),complete_pcm_samples=sum(len(row['pcm'])for row in before_pcm.values()),all_bits_and_collection_and_clock_equal=True,collection_ids_canonicalized=True,actual_before_program_sha256=programs[-1]['sha256'],old_numeric_context_body_unchanged=True,hashmap_dense_order_fixed_only_in_test_fixture=True,production_cycle_order_unchanged=False,production_cycle_algorithm_unchanged=True,allocation_growth_can_change_unordered_iteration=True),indent=2)+'\n')
            programs[-1]['pruned']=True;binary.unlink()
        finally:
            for key in ['context']:paths[key].write_text(originals[key])
        for row in rows:
            keys=set()
            try:
                for key,before,after in row['changes']:
                    assert key not in keys;keys.add(key);paths[key].write_text(originals[key].replace(before,after))
                binary=build(row['name']);run(binary,row['name'],row['test'],101,row['expected_assertion'])
                controls.append({key:value for key,value in row.items()if key!='changes'}|dict(exit=101,sha256=programs[-1]['sha256']));programs[-1]['pruned']=True;binary.unlink();(work/'completed-controls.json').write_text(json.dumps(controls,indent=2)+'\n');print('Actual table storage fault rejected',row['name'],flush=True)
            finally:
                for key in keys:paths[key].write_text(originals[key])
        release=build('positive-release',True);run(release,'full-release');run(release,'pcm-release',PCM);assert pcm_receipt(work/'pcm-release.log')==current_pcm,'O3 complete context PCM/collection/clock changed'
    product.whole.verify_inputs(work,frozen)
    assert all(path.read_text()==originals[key]for key,path in paths.items())
    root=ROOT/'engine/crates/audio/src';assert all((package/'src'/p.relative_to(root)).read_bytes()==p.read_bytes()for p in root.rglob('*')if p.is_file())
    (work/'summary.json').write_text(json.dumps(dict(programs=programs,controls=controls,original_red=red,suites=suites,root_inputs_unchanged=True,source_copies_restored=True,root_dev_profile_verified=True,isolated_release_is_not_root_shipping=True,preflight=preflight),indent=2)+'\n');print('Graph table storage verified',len(controls),'fresh faults',flush=True)


if __name__=='__main__':
    parser=argparse.ArgumentParser(description=__doc__);parser.add_argument('--output',type=Path,required=True);parser.add_argument('--target-dir',type=Path,required=True);parser.add_argument('--before-directory',type=Path,required=True);parser.add_argument('--preflight',action='store_true');args=parser.parse_args();main(args.output.resolve(),args.target_dir.resolve(),args.before_directory.resolve(),args.preflight)
