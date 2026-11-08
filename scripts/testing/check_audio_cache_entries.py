#!/usr/bin/env python3
"""Physical cache buckets: admission, refusal and real original CPU disposal.

The original complete cache/LRU implementation is recompiled for true reds and
complete PCM comparisons. Faults change actual private Rust source, one at a time.
"""
from __future__ import annotations
import argparse,json,re,shutil
from pathlib import Path
import check_audio_output_ring as previous
base=previous.base
wave=previous.wave
ROOT=base.ROOT
PREFIX='cache::entries_storage::tests::'

def declarations(originals):
    rows=[]
    def add(name,key,before,after,test,message):
        assert before!=after and originals[key].count(before)==1,(name,key,'anchor drift')
        rows.append(dict(name=name,changes=[(key,before,after)],test=PREFIX+test,expected_assertion=message))
    charge='managed_cache_backing_is_charged_by_actual_bucket_allocation'
    initial='initial_refusal_precedes_buckets_and_retains_original_input'
    drop='empty_bulk_cache_drop_retains_buckets_until_original_cpu_destruction_after_close'
    growth='growth_moves_original_values_and_retains_both_bucket_allocations_until_cpu_drop'
    add('buckets-uncharged','buckets','host.reserve_inline(bytes)?','host.reserve_inline(0)?',charge,'cache buckets omitted actual physical allocation')
    add('buckets-before-admission','buckets','        let admission = host','        let _early=std::hint::black_box(Entries::with_capacity(layout.capacity));\n        let admission = host',initial,'cache backing allocated before physical admission')
    add('buckets-growth-unmanaged','buckets','.map(|host| Admission::reserve(host, layout.bytes))','.filter(|_| self.layout.bytes == 0).map(|host| Admission::reserve(host, layout.bytes))','growth_refusal_preserves_full_cache_before_lru_eviction_or_publication','cache bucket refusal evicted or published an entry')
    add('buckets-key-refusal-installs','cache','        let url = match Key::new(url, self.metadata_host.as_ref()) {','        if let Some(growth)=growth {growth.install(&mut self.entries);}\n        let growth=None::<entries_storage::Plan>;\n        let url = match Key::new(url, self.metadata_host.as_ref()) {','key_refusal_cancels_unused_bucket_ticket_without_changing_original_table','cache key refusal installed unused bucket plan')
    add('buckets-values-lost','buckets','replacement.data.extend(target.data.drain());','if target.data.len()>=128 {target.data.clear();}else{replacement.data.extend(target.data.drain());}',growth,'cache growth copied or discarded original PCM')
    add('buckets-physical-layout-lost','buckets','        self.data.clear();\n    }','        self.data.clear(); self.layout.bytes=0;\n    }','tombstone_churn_clear_and_hits_preserve_physical_bucket_ownership','tombstone or clear forgot physical bucket capacity')
    add('buckets-drop-on-caller','buckets','        // Release live PCM/key handles synchronously','        self.owner=Owner::Native;\n        // Release live PCM/key handles synchronously',drop,'cache buckets freed on caller before original CPU destruction')
    add('buckets-credit-before-destruction','retirement','            let lease = self.lease.take().unwrap();','            let mut lease = self.lease.take().unwrap(); lease.permit.shrink_to(0);',drop,'cache buckets returned physical credit before actual CPU destruction')
    add('buckets-late-handoff-allocation','buckets','        // Release live PCM/key handles synchronously','        let _late=std::hint::black_box(Box::new(7u8));\n        // Release live PCM/key handles synchronously',drop,'cache bucket drop allocated a late handoff')
    add('buckets-foreign-ledger-accepted','buckets','            if !accepts {','            if false {','table_ledger_identity_and_closed_host_refuse_without_allocation','cache table accepted foreign physical ledger')
    add('buckets-bulk-inline','buckets','if bytes < 4096 {','if true {',drop,'cache buckets freed on caller before original CPU destruction')
    add('buckets-eager-construction','cache','                Entries::new()','                Entries::with_capacity(Self::DEFAULT_CACHE_CAPACITY)','managed_constructor_is_lazy_before_any_bucket_admission','managed cache constructed unadmitted buckets')
    add('buckets-no-tombstone-headroom','buckets','            .checked_add(1)','            .checked_add(usize::from(self.is_empty()))',growth,'cache buckets exceeded admitted capacity')
    add('buckets-leaked-before-cpu','buckets','                _data: std::mem::take(&mut self.data),','                _data: {let original=std::mem::take(&mut self.data); std::mem::forget(original); HashMap::new()},',drop,'cache original buckets not actually freed on CPU')
    return rows

def original_cache(source):
    return source+'''\n#[path="cache_entries_storage.rs"] mod entries_storage;
impl AudioCache {
#[cfg(test)] fn bucket_bytes(&self)->usize {crate::graph_table_layout::Layout::for_entries::<(Key,CacheEntry)>(self.entries.capacity()).unwrap().bytes}
#[cfg(test)] fn observe_buckets(&mut self,_hook:Arc<dyn Fn()+Send+Sync>)->Arc<std::sync::atomic::AtomicUsize> {Arc::new(std::sync::atomic::AtomicUsize::new(0))}
#[cfg(test)] fn check_bucket_host(&self,_host:&crate::node_retirement::Host)->EngineResult<()> {Ok(())}
}
'''

def main(work,target,before_directory,preflight=False):
    package,env,frozen=base.prepare(work,target);product=base.capacity.product;common=product.whole.support.clock.common
    names={'buckets':'cache_entries_storage.rs','cache':'cache.rs','retirement':'node_retirement.rs'}
    paths={k:package/'src'/v for k,v in names.items()};originals={k:p.read_text()for k,p in paths.items()};rows=declarations(originals)
    for path in [Path(__file__).resolve()]:
        name=str(path.relative_to(ROOT));frozen[name]=common.sha(path);dest=work/'source-inputs'/name;dest.parent.mkdir(parents=True,exist_ok=True);shutil.copy2(path,dest)
    (work/'source-inputs.json').write_text(json.dumps(frozen,indent=2)+'\n')
    programs=[];controls=[];red=[];suites={}
    def build(label,release=False):
        binary=product.src.build(package,work,env,label,release);programs.append(dict(name=label,path=str(binary),sha256=common.sha(binary),pruned=False));return binary
    def run(binary,label,test='',expected=0,assertion=None,exact=True):
        args=[str(binary)]+((['--exact']if exact else[])+[test,'--nocapture']if test else[])
        code=common.run(args,work/(label+'.log'),package,env,timeout=900);(work/(label+'-exit.txt')).write_text(str(code)+'\n');output=(work/(label+'.log')).read_text();assert code==expected,(label,code)
        if expected:assert all(v in output for v in ['running 1 test','panicked at','1 failed',test,assertion]),label
        else:
            m=re.search(r'test result: ok\. ([1-9][0-9]*) passed; 0 failed; (\d+) ignored;',output);assert m,label;suites[label]=dict(passed=int(m[1]),ignored=int(m[2]))
    before_manifest=json.loads((before_directory/'root-sha256.json').read_text());changed=[n for n,h in before_manifest.items()if n.startswith('engine/crates/audio/')and n.endswith('.rs')and common.sha(ROOT/n)!=h]
    old_paths={n:package/n.removeprefix('engine/crates/audio/')for n in changed};saved={n:p.read_bytes()for n,p in old_paths.items()}
    clippy=['cargo','clippy','--lib','--tests','--offline','--locked','--message-format=json']
    try:
        assert common.run(clippy,work/'clippy.jsonl',package,env,timeout=600)==0
        for n,p in old_paths.items():p.write_bytes((before_directory/n).read_bytes())
        assert common.run(clippy,work/'clippy-before.jsonl',package,env,timeout=600)==0
    finally:
        for n,p in old_paths.items():p.write_bytes(saved[n])
    old,current=wave.diagnostics(work/'clippy-before.jsonl',package),wave.diagnostics(work/'clippy.jsonl',package);added=current-old
    delta=dict(before=sum(old.values()),after=sum(current.values()),new_diagnostics=sum(added.values()),actor_included=True,new=[dict(key=k,count=v)for k,v in added.items()]);(work/'clippy-delta.json').write_text(json.dumps(delta,indent=2)+'\n');assert not added,delta
    print('Stable source Clippy compared',delta,flush=True)
    positive=build('positive-debug');base.capacity.profile.verify(work/'positive-debug-build.jsonl',json.loads((work/'debug-profile-policy.json').read_text()),work/'debug-profile-artifacts.json')
    if preflight:
        run(positive,'buckets-preflight',PREFIX,exact=False)
        (work/'preflight-summary.json').write_text(json.dumps(dict(suites=suites,clippy=delta),indent=2)+'\n');print('Bucket preflight passed',flush=True);return
    run(positive,'full-debug')
    cache_keys=previous.previous
    old_gates=cache_keys.previous
    traces=[('ring',previous.PCM,previous.pcm_receipt),('cache',cache_keys.CACHE_PCM,cache_keys.cache_pcm_receipt)]
    for name,test,parser in [('pcm',old_gates.PCM,old_gates.pcm_receipt),('index',old_gates.INDEX_PCM,old_gates.index_pcm_receipt),('media',old_gates.MEDIA_PCM,old_gates.media_pcm_receipt),('inner',old_gates.INNER_PCM,old_gates.inner_pcm_receipt),('context',old_gates.CONTEXT_PCM,old_gates.context_pcm_receipt),('slot',old_gates.SLOT_PCM,old_gates.slot_pcm_receipt),('route',old_gates.ROUTE_PCM,old_gates.route_pcm_receipt),('mix',old_gates.MIX_PCM,old_gates.mix_pcm_receipt)]:traces.append((name,test,parser))
    current_pcm={}
    for name,test,parser in traces:run(positive,name+'-pcm-debug',test);current_pcm[name]=parser(work/(name+'-pcm-debug.log'))
    adapted=original_cache((before_directory/'engine/crates/audio/src/cache.rs').read_text());(work/'original-cache-adapted.rs').write_text(adapted)
    try:
        paths['cache'].write_text(adapted);binary=build('original-cache')
        for label,test,message in [('before-bucket-credit','managed_cache_backing_is_charged_by_actual_bucket_allocation','cache buckets omitted actual physical allocation'),('before-bucket-caller-drop','empty_bulk_cache_drop_retains_buckets_until_original_cpu_destruction_after_close','cache buckets freed on caller before original CPU destruction')]:
            run(binary,label,PREFIX+test,101,message);red.append(dict(name=label,test=PREFIX+test,exit=101,expected_assertion=message,sha256=programs[-1]['sha256']))
        for name,test,parser in traces:run(binary,name+'-pcm-before',test);assert parser(work/(name+'-pcm-before.log'))==current_pcm[name],name
        programs[-1]['pruned']=True;binary.unlink()
    finally:paths['cache'].write_text(originals['cache'])
    for row in rows:
        modified=set()
        try:
            for key,before,after in row['changes']:modified.add(key);paths[key].write_text(originals[key].replace(before,after))
            binary=build(row['name']);run(binary,row['name'],row['test'],101,row['expected_assertion']);controls.append({k:v for k,v in row.items()if k!='changes'}|dict(exit=101,sha256=programs[-1]['sha256']));programs[-1]['pruned']=True;binary.unlink();(work/'completed-controls.json').write_text(json.dumps(controls,indent=2)+'\n');print('Fresh cache bucket fault rejected',row['name'],flush=True)
        finally:
            for key in modified:paths[key].write_text(originals[key])
    release=build('positive-release',True);run(release,'full-release')
    for name,test,parser in traces:run(release,name+'-pcm-release',test);assert parser(work/(name+'-pcm-release.log'))==current_pcm[name],name
    product.whole.verify_inputs(work,frozen);assert all(p.read_text()==originals[k]for k,p in paths.items())
    for name in ['audio','shared','io','services','frame-wire']:
        src=ROOT/'engine/crates'/name/'src';private=package/'src'if name=='audio'else package/'local-deps'/name/'src';assert all((private/p.relative_to(src)).read_bytes()==p.read_bytes()for p in src.rglob('*')if p.is_file()),name
    counts={name:dict(blocks=len(data),samples=sum(len(row['pcm'])for row in data.values()))for name,data in current_pcm.items()}
    (work/'complete-pcm-comparison.json').write_text(json.dumps(dict(traces=counts,blocks=sum(v['blocks']for v in counts.values()),samples=sum(v['samples']for v in counts.values()),all_bits_state_clock_and_order_equal=True,original_cache_body_recompiled=True,lru_btree_node_ownership_still_open=True),indent=2)+'\n')
    (work/'summary.json').write_text(json.dumps(dict(programs=programs,controls=controls,original_red=red,suites=suites,clippy=delta,root_inputs_unchanged=True,source_copies_restored=True,root_dev_profile_verified=True,isolated_release_is_not_root_shipping=True,preflight=False),indent=2)+'\n');print('Cache bucket ownership verified',len(controls),'fresh faults',flush=True)

if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--output',type=Path,required=True);p.add_argument('--target-dir',type=Path,required=True);p.add_argument('--before-directory',type=Path,required=True);p.add_argument('--preflight',action='store_true');a=p.parse_args();main(a.output.resolve(),a.target_dir.resolve(),a.before_directory.resolve(),a.preflight)
