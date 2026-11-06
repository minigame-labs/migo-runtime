#!/usr/bin/env python3
"""Cache LRU links in admitted buckets: no tree nodes, original CPU disposal.

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
PREFIX='cache::lru::tests::'
PCM=PREFIX+'complete_growth_lru_replacement_and_eof_pcm_trace'

def lru_pcm_receipt(log):
    decoder=json.JSONDecoder();rows={}
    for line in log.read_text().splitlines():
        if not line.startswith('MIGO_CACHE_LRU_PCM '):continue
        _,key,tail=line.split(' ',2);pcm,end=decoder.raw_decode(tail);tail=tail[end:].lstrip();state,end=decoder.raw_decode(tail);tail=tail[end:].lstrip();order,end=decoder.raw_decode(tail)
        assert key not in rows;rows[key]=dict(pcm=pcm,state=state,order=order)
    assert len(rows)==21,(log,len(rows));return rows

def declarations(originals):
    rows=[]
    def add(name,key,before,after,test,message,count=1):
        assert before!=after and originals[key].count(before)==count,(name,key,'anchor drift')
        rows.append(dict(name=name,changes=[(key,before,after)],test=PREFIX+test,expected_assertion=message))
    links='head_middle_tail_singleton_remove_and_clear_keep_bidirectional_order'
    topology='cache LRU topology invalid'
    hot='hits_use_no_heap_under_full_budget_and_closed_original_cpu'
    drop='final_lru_metadata_waits_for_original_cpu_and_real_bucket_free_after_close'
    refusal='bucket_refusal_keeps_complete_old_lru_and_original_incoming_pcm'
    add('lru-touch-skipped','cache','self.order_index.touch(&mut self.entries, url);','let _ = url;',links,'cache LRU order changed')
    add('lru-head-detach-lost','lru','self.first = links.newer.clone();','self.first = None;',links,topology)
    add('lru-tail-detach-lost','lru','self.last = links.older.clone();','self.last = None;',links,topology)
    add('lru-older-neighbor-lost','lru','.newer = links.newer.clone();','.newer = None;',links,topology)
    add('lru-newer-neighbor-lost','lru','.older = links.older.clone();','.older = None;',links,topology)
    add('lru-appended-older-lost','lru','.links = Links { older, newer: None };','.links = Links { older:None, newer:None };',links,topology)
    add('lru-previous-tail-link-lost','lru','.newer = Some(key.clone());','.newer = None;',links,topology)
    add('lru-empty-head-lost','lru','self.first = Some(key.clone());','self.first = None;',links,topology)
    add('lru-appended-tail-lost','lru','self.last = Some(key);','self.last = None;',links,topology)
    add('lru-url-deep-copied','lru','        let older = self.last.take();','        let key=Key::new(key.as_str().to_owned(),None).unwrap();\n        let older = self.last.take();',hot,'cache LRU hit allocated or freed metadata')
    add('lru-buckets-freed-on-caller','buckets','        // Release live PCM/key handles synchronously','        self.owner=Owner::Native;\n        // Release live PCM/key handles synchronously',drop,'cache LRU metadata freed on caller before original CPU')
    add('lru-credit-before-cpu','retirement','            let lease = self.lease.take().unwrap();','            let mut lease=self.lease.take().unwrap(); lease.permit.shrink_to(0);',drop,'cache LRU credit returned before original CPU destruction')
    add('lru-bucket-backing-leaked','buckets','                _data: std::mem::take(&mut self.data),','                _data: {let original=std::mem::take(&mut self.data); std::mem::forget(original); std::collections::HashMap::new()},',drop,'cache LRU backing was not actually freed on original CPU')
    add('lru-link-bytes-uncharged','buckets','let layout = Layout::for_entries::<(Key, CacheEntry)>(\n            required','let layout = Layout::for_entries::<(Key, [usize;3])>(\n            required','embedded_links_are_in_the_actual_admitted_bucket_allocation','cache LRU links omitted from physical bucket layout')
    add('lru-eviction-before-admission','cache','        let growth = match self.entries.reserve(self.metadata_host.as_ref()) {','        if !self.entries.is_empty() && self.entries.len()==self.entries.capacity() {if let Some(key)=self.order_index.pop(&mut self.entries){self.entries.remove(key.as_str());}}\n        let growth = match self.entries.reserve(self.metadata_host.as_ref()) {',refusal,'cache LRU refusal changed original cache')
    add('lru-growth-admission-bypassed','buckets','.map(|host| Admission::reserve(host, layout.bytes))','.filter(|_| self.layout.bytes==0).map(|host| Admission::reserve(host, layout.bytes))',refusal,'cache LRU refusal changed original cache')
    add('lru-counter-overflow','cache','self.access_counter = self.access_counter.wrapping_add(1);','self.access_counter += 1;','counter_wrap_preserves_true_lru_and_does_not_interrupt_hits','cache LRU counter overflow interrupted hit',count=2)
    add('lru-hit-value-lost','cache','self.entries.get(url).map(|entry| Arc::clone(&entry.audio))','self.entries.get(url).filter(|_|false).map(|entry| Arc::clone(&entry.audio))',hot,'cache LRU hit changed cached PCM')
    add('lru-clear-keeps-tail','lru','        self.last = None;','        let _ = &self.last;',links,topology)
    return rows

def original_cache(source,package):
    return source+'\nmod lru { #[path='+json.dumps(str(package/'src/cache_lru_tests.rs'))+'] mod tests; }\n'+r'''
impl AudioCache {
#[cfg(test)] fn lru_keys(&self)->impl Iterator<Item=&Key> {self.order_index.values()}
#[cfg(test)] fn lru_iter(&self)->impl Iterator<Item=(&u64,&Key)> {self.order_index.iter()}
#[cfg(test)] fn lru_empty(&self)->bool {self.order_index.is_empty()}
#[cfg(test)] fn lru_valid(&self)->bool {self.order_index.len()==self.entries.len() && self.order_index.iter().all(|(n,k)|self.entries.get(k.as_str()).is_some_and(|e|e.access_order==*n))}
}
'''
def main(work,target,before_directory,preflight=False):
    package,env,frozen=base.prepare(work,target);product=base.capacity.product;common=product.whole.support.clock.common
    names={'lru':'cache_lru.rs','buckets':'cache_entries_storage.rs','cache':'cache.rs','retirement':'node_retirement.rs'}
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
        (work/'preflight-summary.json').write_text(json.dumps(dict(suites=suites,clippy=delta),indent=2)+'\n');print('LRU preflight passed',flush=True);return
    run(positive,'full-debug')
    cache_keys=previous.previous
    old_gates=cache_keys.previous
    traces=[('lru',PCM,lru_pcm_receipt),('ring',previous.PCM,previous.pcm_receipt),('cache',cache_keys.CACHE_PCM,cache_keys.cache_pcm_receipt)]
    for name,test,parser in [('pcm',old_gates.PCM,old_gates.pcm_receipt),('index',old_gates.INDEX_PCM,old_gates.index_pcm_receipt),('media',old_gates.MEDIA_PCM,old_gates.media_pcm_receipt),('inner',old_gates.INNER_PCM,old_gates.inner_pcm_receipt),('context',old_gates.CONTEXT_PCM,old_gates.context_pcm_receipt),('slot',old_gates.SLOT_PCM,old_gates.slot_pcm_receipt),('route',old_gates.ROUTE_PCM,old_gates.route_pcm_receipt),('mix',old_gates.MIX_PCM,old_gates.mix_pcm_receipt)]:traces.append((name,test,parser))
    current_pcm={}
    for name,test,parser in traces:run(positive,name+'-pcm-debug',test);current_pcm[name]=parser(work/(name+'-pcm-debug.log'))
    adapted=original_cache((before_directory/'engine/crates/audio/src/cache.rs').read_text(),package);(work/'original-cache-adapted.rs').write_text(adapted)
    try:
        paths['cache'].write_text(adapted);binary=build('original-cache')
        for label,test,message in [('before-lru-hit-allocation','hits_use_no_heap_under_full_budget_and_closed_original_cpu','cache LRU hit allocated or freed metadata'),('before-lru-caller-drop','final_lru_metadata_waits_for_original_cpu_and_real_bucket_free_after_close','cache LRU metadata freed on caller before original CPU'),('before-lru-counter-debug','counter_wrap_preserves_true_lru_and_does_not_interrupt_hits','cache LRU counter overflow interrupted hit')]:
            run(binary,label,PREFIX+test,101,message);red.append(dict(name=label,test=PREFIX+test,exit=101,expected_assertion=message,sha256=programs[-1]['sha256']))
        for name,test,parser in traces:run(binary,name+'-pcm-before',test);assert parser(work/(name+'-pcm-before.log'))==current_pcm[name],name
        programs[-1]['pruned']=True;binary.unlink()
        binary=build('original-cache-release',True)
        label='before-lru-counter-release';test=PREFIX+'counter_wrap_preserves_true_lru_and_does_not_interrupt_hits';message='cache LRU counter wrap evicted most recent entry'
        run(binary,label,test,101,message);red.append(dict(name=label,test=test,exit=101,expected_assertion=message,sha256=programs[-1]['sha256']))
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
    (work/'complete-pcm-comparison.json').write_text(json.dumps(dict(traces=counts,blocks=sum(v['blocks']for v in counts.values()),samples=sum(v['samples']for v in counts.values()),all_bits_state_clock_and_order_equal=True,original_cache_body_recompiled=True,lru_nodes_eliminated_and_links_in_admitted_buckets=True,diagnostic_counter_wrap_does_not_change_recency=True),indent=2)+'\n')
    (work/'summary.json').write_text(json.dumps(dict(programs=programs,controls=controls,original_red=red,suites=suites,clippy=delta,root_inputs_unchanged=True,source_copies_restored=True,root_dev_profile_verified=True,isolated_release_is_not_root_shipping=True,preflight=False),indent=2)+'\n');print('Cache LRU ownership verified',len(controls),'fresh faults',flush=True)

if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--output',type=Path,required=True);p.add_argument('--target-dir',type=Path,required=True);p.add_argument('--before-directory',type=Path,required=True);p.add_argument('--preflight',action='store_true');a=p.parse_args();main(a.output.resolve(),a.target_dir.resolve(),a.before_directory.resolve(),a.preflight)
