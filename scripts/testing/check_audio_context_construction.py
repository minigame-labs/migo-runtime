#!/usr/bin/env python3
"""Pre-admit all owned managed-context data, then verify full old/new PCM.

Every negative control must compile fresh and reach one designated Rust business
assertion. Allocator counters cover early and last-ticket constructor rejection.
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
PREFIX='audio_thread::context_construction_tests::'
PCM=render.PCM
pcm_receipt=render.pcm_receipt

def declarations(originals):
    rows=[]
    def add(name,key,before,after,test,assertion):
        assert before!=after and originals[key].count(before)==1,(name,key,'anchor drift')
        rows.append(dict(name=name,changes=[(key,before,after)],test=PREFIX+test,expected_assertion=assertion))
    early='saturated_constructor_refuses_before_any_large_data_allocation'
    late='final_close_ticket_refusal_precedes_all_vector_table_and_render_data'
    table='default_constructor_failure_allocates_no_table_bucket_data'
    state='initial_state_and_capacities_match_native_adoption_with_no_transient_data_copy'
    actor='actor_refusal_keeps_full_context_map_and_existing_addresses'
    duplicate='duplicate_context_rejects_before_factory_or_ledger_lookup'
    node='preallocated_destination_ticket_retires_real_node_payload_on_original_cpu'
    initial='initial_tickets_price_real_capacity_before_allocating_and_finish_without_readmission'
    close='newly_constructed_bulk_storage_survives_close_and_cpu_shutdown_until_real_drop'
    add('constructor-native-data-before-admission','constructor','        let host = Host::new(cpu, aggregate, channels)?;','        let _early=Self::new(id,sample_rate,channels);\n        let host = Host::new(cpu, aggregate, channels)?;',early,'constructor allocated data before admission')
    add('constructor-last-close-ticket-omitted','constructor','close: host.reserve_value::<ContextStorage>()?,','close: Reservation::unmanaged(),',late,'constructor omitted final admission')
    method=wave.method(originals['vector'],'    pub(crate) fn reserve(host: &Host, capacity: usize)')
    changed=method.replace('        Ok(Self {','        let _early=Vec::<T>::with_capacity(capacity);\n        Ok(Self {',1)
    add('constructor-vector-data-before-batch','vector',method,changed,late,'constructor data allocated before complete admission')
    method=wave.method(originals['map'],'    pub(crate) fn reserve(host: &Host, entries: usize)')
    changed=method.replace('        Ok(Self {','        let _early=HashMap::<u32,V>::with_capacity(layout.capacity);\n        Ok(Self {',1)
    add('constructor-map-data-before-batch','map',method,changed,table,'constructor allocated table buckets before complete admission')
    method=wave.method(originals['render'],'    pub(crate) fn reserve(host: &Host, samples: usize)')
    changed=method.replace('        Ok(Self {','        let _early=vec![0.0f32;samples];\n        Ok(Self {',1)
    add('constructor-render-before-batch','render',method,changed,late,'constructor data allocated before complete admission')
    method=wave.method(originals['actor'],'fn create_context_scoped_with_ledger(')
    changed=method.replace('    let context = match cpu {','    contexts.reserve(1);\n    let context = match cpu {',1)
    add('constructor-actor-entry-before-admission','actor',method,changed,actor,'rejected context moved an existing map entry')
    guard='    if contexts.contains_key(&ctx_id) {\n        return false;\n    }\n'
    assert method.count(guard)==1
    add('constructor-duplicate-overwrites-context','actor',method,method.replace(guard,''),duplicate,'duplicate context replaced live entry')
    add('constructor-node-ticket-not-installed','retirement','lease: Some(self.lease),','lease: None,',node,'pre-admitted node escaped original CPU')
    add('constructor-node-payload-bytes-omitted','retirement','self.admit_bytes(std::mem::size_of::<T>())?','self.admit_bytes(0)?',node,'destination ticket omitted node payload bytes')
    method=wave.method(originals['vector'],'    pub(crate) fn reserve(host: &Host, capacity: usize)')
    add('constructor-vector-bytes-omitted','vector',method,method.replace('Admission::reserve(host, capacity)?','Admission::reserve(host, 0)?'),initial,'initial vector ticket omitted bytes')
    method=wave.method(originals['map'],'    pub(crate) fn reserve(host: &Host, entries: usize)')
    add('constructor-map-bytes-omitted','map',method,method.replace('Admission::reserve(host, layout.bytes)?','Admission::reserve(host, 0)?'),initial,'initial table ticket omitted bytes')
    method=wave.method(originals['render'],'    pub(crate) fn reserve(host: &Host, samples: usize)')
    add('constructor-render-bytes-omitted','render',method,method.replace('capacity_bytes(samples)?','0'),initial,'initial render ticket omitted bytes')
    add('constructor-mix-not-initialized','constructor','        context.mix_buffer.resize(samples, 0.0);','        // fault: mix not initialized',state,'managed constructor changed initial vector layout')
    add('constructor-destination-id-changed','constructor','            DESTINATION_NODE_ID,\n            initial','            DESTINATION_NODE_ID+1,\n            initial',state,'managed constructor changed initial graph values')
    method=wave.method(originals['actor'],'fn create_context_scoped_with_ledger(')
    changed=method.replace('                return false;','                contexts.clear();\n                return false;',1)
    add('constructor-refusal-drops-old-contexts','actor',method,changed,actor,'rejected context moved an existing map entry')
    add('constructor-credit-returned-before-data','retirement','            let lease = self.lease.take().unwrap();','            let mut lease = self.lease.take().unwrap();\n            lease.permit.shrink_to(0);',close,'constructor credit returned before actual CPU destruction')
    for name,before,after,test in graph.declarations(originals['context']):
        rows.append(dict(name=name,changes=[('context',before,after)],test=test,expected_assertion='cold context render allocated'))
    for name,key,before,after,assertion in cold.declarations({'nodes/wave_shaper.rs':originals['wave'],'context.rs':originals['context']}):
        rows.append(dict(name=name,changes=[('wave'if key=='nodes/wave_shaper.rs'else'context',before,after)],test=cold.TEST,expected_assertion=assertion))
    assert len(rows)==23
    return rows

def before_context(raw):
    assert 'mod construction;' not in raw
    return raw+"""
impl AudioContext {
    pub(crate) fn new_managed(id:AudioContextId,rate:u32,channels:u32,cpu:migo_io::audio_cpu::AudioCpu,aggregate:Arc<crate::limits::AudioAggregateLedger>)->EngineResult<Self> {
        let mut context=Self::new(id,rate,channels);
        context.prepare_node_retirement(cpu,aggregate)?;
        Ok(context)
    }
}
"""

def before_actor(current,old):
    before=wave.method(old,'fn create_context_scoped(')
    before=before.replace('fn create_context_scoped(','fn create_context_scoped_with_ledger(',1)
    signature='    cpu: Option<&migo_io::audio_cpu::AudioCpu>,\n'
    assert before.count(signature)==1
    before=before.replace(signature,signature+'    aggregate: impl FnOnce()->Arc<crate::limits::AudioAggregateLedger>,\n',1)
    before=before.replace('crate::limits::AudioAggregateLedger::process_global()','aggregate()')
    return current.replace(wave.method(current,'fn create_context_scoped_with_ledger('),before,1)

def main(work,target,before_directory,preflight=False):
    package,env,frozen=base.prepare(work,target)
    product=base.capacity.product;common=product.whole.support.clock.common
    names={'constructor':'context_construction.rs','vector':'graph_vec.rs','map':'graph_map.rs','render':'render_buffer.rs','actor':'audio_thread.rs','context':'context.rs','retirement':'node_retirement.rs','wave':'nodes/wave_shaper.rs'}
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
            paths['actor'].write_text(before_actor(originals['actor'],(before_directory/'engine/crates/audio/src/audio_thread.rs').read_text()))
            binary=build('before-context-construction')
            for label,test,assertion in [('before-constructor-admission','saturated_constructor_refuses_before_any_large_data_allocation','constructor allocated data before admission'),('before-actor-admission','actor_refusal_keeps_full_context_map_and_existing_addresses','rejected context moved an existing map entry')]:
                run(binary,label,PREFIX+test,101,assertion);red.append(dict(name=label,test=PREFIX+test,exit=101,expected_assertion=assertion,sha256=programs[-1]['sha256']))
            run(binary,'pcm-before',PCM);before_pcm=pcm_receipt(work/'pcm-before.log');assert before_pcm==current_pcm,'complete old context PCM/collection/clock changed'
            (work/'pcm-before-comparison.json').write_text(json.dumps(dict(configured_blocks=len(before_pcm),complete_pcm_samples=sum(len(row['pcm'])for row in before_pcm.values()),all_bits_and_collection_and_clock_equal=True,collection_ids_canonicalized=True,actual_before_program_sha256=programs[-1]['sha256'],old_numeric_context_body_unchanged=True,hashmap_dense_order_fixed_only_in_test_fixture=True,production_cycle_order_unchanged=False,production_cycle_algorithm_unchanged=True,allocation_growth_can_change_unordered_iteration=True),indent=2)+'\n')
            programs[-1]['pruned']=True;binary.unlink()
        finally:
            for key in ['context','actor']:paths[key].write_text(originals[key])
        for row in rows:
            keys=set()
            try:
                for key,before,after in row['changes']:
                    assert key not in keys;keys.add(key);paths[key].write_text(originals[key].replace(before,after))
                binary=build(row['name']);run(binary,row['name'],row['test'],101,row['expected_assertion'])
                controls.append({key:value for key,value in row.items()if key!='changes'}|dict(exit=101,sha256=programs[-1]['sha256']));programs[-1]['pruned']=True;binary.unlink();(work/'completed-controls.json').write_text(json.dumps(controls,indent=2)+'\n');print('Actual constructor fault rejected',row['name'],flush=True)
            finally:
                for key in keys:paths[key].write_text(originals[key])
        release=build('positive-release',True);run(release,'full-release');run(release,'pcm-release',PCM);assert pcm_receipt(work/'pcm-release.log')==current_pcm,'O3 complete context PCM/collection/clock changed'
    product.whole.verify_inputs(work,frozen)
    assert all(path.read_text()==originals[key]for key,path in paths.items())
    root=ROOT/'engine/crates/audio/src';assert all((package/'src'/p.relative_to(root)).read_bytes()==p.read_bytes()for p in root.rglob('*')if p.is_file())
    (work/'summary.json').write_text(json.dumps(dict(programs=programs,controls=controls,original_red=red,suites=suites,root_inputs_unchanged=True,source_copies_restored=True,root_dev_profile_verified=True,isolated_release_is_not_root_shipping=True,preflight=preflight),indent=2)+'\n');print('Context construction verified',len(controls),'fresh faults',flush=True)


if __name__=='__main__':
    parser=argparse.ArgumentParser(description=__doc__);parser.add_argument('--output',type=Path,required=True);parser.add_argument('--target-dir',type=Path,required=True);parser.add_argument('--before-directory',type=Path,required=True);parser.add_argument('--preflight',action='store_true');args=parser.parse_args();main(args.output.resolve(),args.target_dir.resolve(),args.before_directory.resolve(),args.preflight)
