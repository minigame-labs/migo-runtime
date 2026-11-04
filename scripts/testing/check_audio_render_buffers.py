#!/usr/bin/env python3
"""Keep render allocation credit through active, pooled and actual CPU lifetimes.

Run the complete old context/retirement implementation against the same PCM
receipt. Each source fault must compile fresh and hit a named Rust assertion.
"""
from __future__ import annotations
import argparse
import json
from pathlib import Path
import re
import check_audio_command_ownership as base
import check_audio_waveshaper_storage as wave
import check_audio_waveshaper_capacity as cold

ROOT = base.ROOT
PREFIX = 'audio_thread::render_buffer_tests::'
PCM = PREFIX + 'complete_graph_pcm_and_collection_receipt'
# Test input control only; production HashMap traversal remains untouched.
DENSE_ORDER_TEST_CONTROL = '        #[cfg(test)]\n        if self.test_dense_order {\n            self.dense_ids.sort_unstable();\n            for (index, id) in self.dense_ids.iter().enumerate() {\n                self.dense_index.insert(*id, index as u32);\n            }\n        }\n\n'


def declarations(originals):
    rows = []
    def add(name, key, before, after, test, assertion):
        assert before != after and originals[key].count(before) == 1, (name, key, 'anchor drift')
        rows.append(dict(name=name, changes=[(key, before, after)], test=PREFIX+test, expected_assertion=assertion))
    refusal='buffer_refusal_precedes_allocation_and_preserves_existing_node'
    migration='migration_adopts_active_and_pooled_actual_capacity_without_copying'
    pool='pooled_capacity_outlives_node_and_reuse_keeps_original_pointer'
    boundary='small_bulk_boundary_and_closed_reuse_keep_physical_credit'
    foreign='foreign_adoption_refusal_preserves_original_cpu_and_capacity'
    additive='additive_bus_and_partial_writes_preserve_original_zeroing'
    add('render-new-capacity-uncharged','storage','Admission::reserve(host, bytes)?','Admission::reserve(host, 0)?',refusal,'render capacity admission bypassed')
    add('render-allocation-before-admission','storage','        let reservation = Admission::reserve(host, bytes)?;\n        let data = vec![0.0; samples];','        let data = vec![0.0; samples];\n        let reservation = Admission::reserve(host, bytes)?;',refusal,'render storage allocated before admission')
    add('render-adoption-length-for-capacity','storage','capacity_bytes(data.capacity())?','capacity_bytes(data.len())?',migration,'pooled render capacity omitted during migration')
    add('render-adoption-copies-data','storage','reservation.finish(std::mem::take(data))','reservation.finish(data.clone())',migration,'pooled adoption copied original allocation')
    add('render-active-adoption-omitted','context','self\n            .node_buffers\n            .values_mut()','std::iter::empty::<&mut RenderBuffer>()',migration,'pooled render capacity omitted during migration')
    add('render-pooled-adoption-omitted','context','.chain(self.buffer_pool.iter_mut())','.chain(std::iter::empty::<&mut RenderBuffer>())',migration,'pooled render capacity omitted during migration')
    add('render-pooled-reuse-charged-again','context','            Some(mut buffer) => {','            Some(mut buffer) => {\n                let _duplicate = self.node_retirement.as_ref().map(|host| host.reserve_inline(buffer.capacity()*4)).transpose()?;',pool,'pooled render reuse acquired duplicate admission')
    add('render-node-lease-double-charges','retirement','let bytes = std::mem::size_of_val(node) + HANDOFF_BYTES;','let bytes = std::mem::size_of_val(node) + HANDOFF_BYTES + self.render_buffer_samples()*4;',pool,'node lease duplicated render capacity credit')
    add('render-pool-detaches-owner','context','self.buffer_pool.push(buffer);','self.buffer_pool.push(RenderBuffer::new(None, buffer.capacity()).unwrap());',pool,'pooled render capacity returned with node credit')
    add('render-credit-returned-before-data','retirement','            let lease = self.lease.take().unwrap();','            let mut lease = self.lease.take().unwrap();\n            lease.permit.shrink_to(0);',foreign,'render credit returned before original CPU destructor')
    add('render-bulk-destroyed-inline','retirement','if self.lease.is_some() {','if false {',foreign,'render storage destroyed away from blocked original CPU')
    add('render-small-credit-omitted','storage','host.reserve_inline(bytes)','host.reserve_inline(0)',boundary,'small render buffer acquired artificial handoff')
    add('render-4096-boundary-inline','storage','Some(host) if bytes < 4096','Some(host) if bytes <= 4096',boundary,'bulk render buffer omitted CPU handoff credit')
    add('render-foreign-inline-accepted','storage','host.accepts_inline(permit)','{ let _ = permit; true }',foreign,'render buffer accepted foreign physical ledger')
    add('render-foreign-cpu-accepted','storage','value.accepts_physical_ledger(host)','{ let _ = value; true }',foreign,'render buffer accepted foreign physical ledger')
    method=wave.method(originals['storage'],'    pub(crate) fn resize(')
    start=method.index('        assert!(');end=method.index('        self.data_mut().resize',start)
    add('render-managed-resize-escapes-credit','storage',method,method[:start]+method[end:],'managed_resize_cannot_escape_admitted_capacity','managed render buffer resized past admitted capacity')
    add('render-node-mutated-before-admission','context','        let buffer = match self.take_render_buffer() {','        self.nodes.remove(&node_id);\n        let buffer = match self.take_render_buffer() {',refusal,'render refusal removed original node')
    add('render-overwrites-shared-mix','context','                output[i] += dest_buf[i];','                output[i] = dest_buf[i];',additive,'context rendering stopped adding to shared bus')
    add('render-partial-buffer-not-zeroed','context','            node_buf[..buffer_size].fill(0.0);','            // fault: stale partial output remains',additive,'partial render buffer retained stale samples')
    for name,key,before,after,assertion in cold.declarations({'nodes/wave_shaper.rs':originals['wave'],'context.rs':originals['context']}):
        rows.append(dict(name=name,changes=[('wave' if key=='nodes/wave_shaper.rs' else 'context',before,after)],test=cold.TEST,expected_assertion=assertion))
    assert len(rows)==23
    return rows


def before_context(raw):
    assert 'node_buffers: HashMap<AudioNodeId, Vec<f32>>' in raw
    assert raw.count('    test_wall_enabled: bool,') == 1
    raw = raw.replace('    test_wall_enabled: bool,', '    test_wall_enabled: bool,\n    #[cfg(test)]\n    test_dense_order: bool,', 1)
    raw = raw.replace('            test_wall_enabled: false,', '            test_wall_enabled: false,\n            #[cfg(test)]\n            test_dense_order: false,', 1)
    anchor = '        // Count edges per endpoint, then prefix-sum into CSR starts.'
    assert raw.count(anchor) == 1
    raw = raw.replace(anchor, DENSE_ORDER_TEST_CONTROL + anchor, 1)
    return raw+'''
impl AudioContext {
    #[cfg(test)]
    pub(crate) fn set_test_dense_order(&mut self) { self.test_dense_order = true; self.graph_dirty = true; }
    #[cfg(test)]
    pub(crate) fn render_buffer_identity(&self, id: AudioNodeId) -> Option<(usize,usize,usize)> { self.node_buffers.get(&id).map(|data|(data.as_ptr()as usize,data.len(),data.capacity())) }
    #[cfg(test)]
    pub(crate) fn render_pool_identity(&self) -> Vec<(usize,usize,usize)> { self.buffer_pool.iter().map(|data|(data.as_ptr()as usize,data.len(),data.capacity())).collect() }
    #[cfg(test)]
    pub(crate) fn reserve_native_render_buffer(&mut self, id: AudioNodeId, samples: usize) { let data=self.node_buffers.get_mut(&id).unwrap();data.reserve_exact(samples.saturating_sub(data.len())); }
    #[cfg(test)]
    pub(crate) fn observe_render_buffer(&mut self, _id: AudioNodeId, _hook: Arc<dyn Fn()+Send+Sync>) {}
}
'''


def before_retirement(raw):
    assert '_buffer: Option<Vec<f32>>' in raw
    return raw+'''
impl Host {
    pub(crate) fn render_buffer_samples(&self) -> usize { crate::audio_thread::RENDER_QUANTUM_FRAMES*self.channels.max(1)as usize }
    pub(crate) fn accepts_inline(&self, permit: &AudioAggregatePermit) -> bool { permit.belongs_to(&self.aggregate) }
}
'''


def pcm_receipt(log):
    decoder=json.JSONDecoder();rows={}
    for line in log.read_text().splitlines():
        if not line.startswith('MIGO_RENDER_PCM '):continue
        _,key,tail=line.split(' ',2);items=[]
        for _ in range(3):
            data,end=decoder.raw_decode(tail);items.append(data);tail=tail[end:].lstrip()
        assert key not in rows,key
        rows[key]=dict(pcm=items[0],collected=items[1],ended=items[2],clock=int(tail))
    assert len(rows)==216,(log,len(rows))
    for key,row in rows.items():
        _,channels,_,_,frames=map(int,key.split(':'))
        assert len(row['pcm'])==frames*channels+channels-1,key
    return rows


def main(work,target,before_directory,preflight=False):
    package,env,frozen=base.prepare(work,target)
    product=base.capacity.product;common=product.whole.support.clock.common
    names={'storage':'render_buffer.rs','context':'context.rs','retirement':'node_retirement.rs','wave':'nodes/wave_shaper.rs'}
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
            paths['retirement'].write_text(before_retirement((before_directory/'engine/crates/audio/src/node_retirement.rs').read_text()))
            binary=build('before-render-storage')
            for label,test,assertion in [('before-pooled-credit','pooled_capacity_outlives_node_and_reuse_keeps_original_pointer','pooled render capacity returned with node credit'),('before-migration-credit','migration_adopts_active_and_pooled_actual_capacity_without_copying','pooled render capacity omitted during migration')]:
                run(binary,label,PREFIX+test,101,assertion);red.append(dict(name=label,test=PREFIX+test,exit=101,expected_assertion=assertion,sha256=programs[-1]['sha256']))
            run(binary,'pcm-before',PCM);before_pcm=pcm_receipt(work/'pcm-before.log');assert before_pcm==current_pcm,'complete old context PCM/collection/clock changed'
            (work/'pcm-before-comparison.json').write_text(json.dumps(dict(configured_blocks=len(before_pcm),complete_pcm_samples=sum(len(row['pcm'])for row in before_pcm.values()),all_bits_and_collection_and_clock_equal=True,collection_ids_canonicalized=True,actual_before_program_sha256=programs[-1]['sha256'],old_numeric_context_body_unchanged=True,hashmap_dense_order_fixed_only_in_test_fixture=True,production_cycle_order_unchanged=True),indent=2)+'\n')
            programs[-1]['pruned']=True;binary.unlink()
        finally:
            for key in ['context','retirement']:paths[key].write_text(originals[key])
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
    (work/'summary.json').write_text(json.dumps(dict(programs=programs,controls=controls,original_red=red,suites=suites,root_inputs_unchanged=True,source_copies_restored=True,root_dev_profile_verified=True,isolated_release_is_not_root_shipping=True,preflight=preflight),indent=2)+'\n');print('Render storage verified',len(controls),'fresh faults',flush=True)


if __name__=='__main__':
    parser=argparse.ArgumentParser(description=__doc__);parser.add_argument('--output',type=Path,required=True);parser.add_argument('--target-dir',type=Path,required=True);parser.add_argument('--before-directory',type=Path,required=True);parser.add_argument('--preflight',action='store_true');args=parser.parse_args();main(args.output.resolve(),args.target_dir.resolve(),args.before_directory.resolve(),args.preflight)
