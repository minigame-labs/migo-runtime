#!/usr/bin/env python3
"""Shared AudioSync cell: pre-admission and physical credit through its real free.

The original complete output implementation is recompiled for true reds and
complete PCM comparisons. Faults change actual private Rust source, one at a time.
"""
from __future__ import annotations
import argparse,json,re,shutil
from pathlib import Path
import check_audio_output_ring as previous
import check_audio_cache_lru as lru
base=previous.base
wave=previous.wave
ROOT=base.ROOT
PREFIX='output::sync_storage::tests::'
PCM=PREFIX+'complete_managed_callback_pcm_and_refill_state_trace'

def sync_pcm_receipt(log):
    decoder=json.JSONDecoder();rows={}
    for line in log.read_text().splitlines():
        if not line.startswith('MIGO_OUTPUT_SYNC_PCM '):continue
        _,key,tail=line.split(' ',2);pcm,end=decoder.raw_decode(tail);state,end=decoder.raw_decode(tail[end:].lstrip());assert key not in rows;rows[key]=dict(pcm=pcm,state=state)
    assert len(rows)==120,(log,len(rows));return rows

def declarations(originals):
    rows=[]
    def add(name,key,before,after,test,message):
        assert before!=after and originals[key].count(before)==1,(name,key,'anchor drift')
        rows.append(dict(name=name,changes=[(key,before,after)],test=PREFIX+test,expected_assertion=message))
    charged='actual_arc_cell_layout_and_one_physical_charge_match_allocator'
    first='refusal_precedes_state_factory_and_arc_allocation'
    drop='last_foreign_callback_frees_real_cell_before_credit_refund_without_allocation'
    clones='clones_share_state_without_heap_or_new_credit_after_host_close'
    callback='callback_policy_and_bound_notifier_work_under_full_budget_without_heap'
    add('sync-header-uncharged','storage','host.reserve_inline(bytes)','host.reserve_inline(0)',charged,'AudioSync cell omitted physical credit')
    add('sync-arc-head-uncharged','storage','host.reserve_inline(bytes)','host.reserve_inline(bytes-std::mem::size_of::<[AtomicUsize;2]>())',charged,'AudioSync cell omitted physical credit')
    add('sync-admission-bypassed','storage','host.map(|host| host.reserve_inline(bytes))','host.filter(|_|false).map(|host| host.reserve_inline(bytes))',first,'AudioSync metadata bypassed physical admission')
    anchor='        let permit = host.map(|host| host.reserve_inline(bytes)).transpose()?;'
    assert originals['storage'].count(anchor)==1
    rows.append(dict(name='sync-factory-before-admission',changes=[('storage',anchor,'        let state=state();\n'+anchor),('storage','            state: state(),','            state,')],test=PREFIX+first,expected_assertion='AudioSync state factory ran before admission'))
    add('sync-arc-before-admission','storage',anchor,'        let _early=std::hint::black_box(Arc::new([0u8;std::mem::size_of::<Data>()]));\n'+anchor,first,'AudioSync Arc allocated before admission')
    add('sync-clone-allocates','storage','Self(self.0.clone())','{let value=Self(self.0.clone());let _extra=std::hint::black_box(Box::new(7u8));value}',clones,'AudioSync clone allocated new state')
    add('sync-refund-before-arc-free','storage','drop(Arc::into_inner(cell));','drop(cell);',drop,'AudioSync refund preceded actual Arc/state destruction')
    add('sync-credit-refunded-early','storage','if let Some(cell) = self.0.take() {','if let Some(mut cell) = self.0.take() { if let Some(data)=Arc::get_mut(&mut cell){drop(data._permit.take());}',drop,'AudioSync refund preceded actual Arc/state destruction')
    add('sync-live-permit-lost','storage','            _permit: permit,','            _permit: None,',charged,'AudioSync cell omitted physical credit')
    add('sync-final-cell-leaked','storage','drop(Arc::into_inner(cell));','std::mem::forget(cell);',drop,'AudioSync final state was not actually destroyed')
    add('sync-late-cleanup-allocates','storage','        if let Some(cell) = self.0.take() {','        let _late=std::hint::black_box(Box::new(7u8));\n        if let Some(cell) = self.0.take() {',drop,'AudioSync final callback allocated cleanup')
    add('sync-silent-unmanaged','silent','let sync = AudioSync::with_host(host)?;','let sync = AudioSync::with_host(None)?;','silent_output_uses_admitted_sync_and_a_retained_sync_clone_keeps_its_credit','silent output omitted managed AudioSync')
    add('sync-level-not-shared','output','self.inner.buffer_level.store(level, Ordering::Relaxed);','self.inner.buffer_level.store(0, Ordering::Relaxed);',clones,'AudioSync clone lost shared state')
    add('sync-refill-signal-lost','output','self.inner.needs_data.store(true, Ordering::Release);','self.inner.needs_data.store(false, Ordering::Release);',callback,'AudioSync callback lost refill signal')
    add('sync-callback-max-lost','output','self.inner.max_callback.fetch_max(len, Ordering::Relaxed);','self.inner.max_callback.fetch_max(0, Ordering::Relaxed);',clones,'AudioSync lost callback maximum')
    add('sync-bound-notifier-lost','output','            notifier.notify();','            let _ = notifier;',callback,'AudioSync callback lost bound notifier')
    return rows

def original_output(source):
    return source+r'''
#[path="output_sync_storage.rs"] mod sync_storage;
impl AudioSync {
    pub(super) fn with_host(_host:Option<&crate::node_retirement::Host>)->shared::error::EngineResult<Self>{Ok(Self::new())}
    #[cfg(test)] fn storage_bytes(&self)->usize{0}
    #[cfg(test)] fn observe_storage(&mut self,_before:Arc<dyn Fn()+Send+Sync>,_after:Arc<dyn Fn()+Send+Sync>){}
}
'''

def main(work,target,before_directory,preflight=False):
    package,env,frozen=base.prepare(work,target);product=base.capacity.product;common=product.whole.support.clock.common
    names={'storage':'output_sync_storage.rs','output':'output.rs','silent':'output_silent.rs'}
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
        paths['output'].write_text(previous.ports(originals['output'],True))
        assert common.run(clippy,work/'clippy.jsonl',package,env,timeout=600)==0
        for n,p in old_paths.items():p.write_bytes((before_directory/n).read_bytes())
        paths['output'].write_text(previous.ports(paths['output'].read_text(),True))
        assert common.run(clippy,work/'clippy-before.jsonl',package,env,timeout=600)==0
    finally:
        for n,p in old_paths.items():p.write_bytes(saved[n])
        paths['output'].write_text(originals['output'])
    old,current=wave.diagnostics(work/'clippy-before.jsonl',package),wave.diagnostics(work/'clippy.jsonl',package);added=current-old
    delta=dict(before=sum(old.values()),after=sum(current.values()),new_diagnostics=sum(added.values()),actor_and_all_rust_backend_bodies_included=True,new=[dict(key=k,count=v)for k,v in added.items()]);(work/'clippy-delta.json').write_text(json.dumps(delta,indent=2)+'\n');assert not added,delta
    print('Stable source Clippy compared',delta,flush=True)
    positive=build('positive-debug');base.capacity.profile.verify(work/'positive-debug-build.jsonl',json.loads((work/'debug-profile-policy.json').read_text()),work/'debug-profile-artifacts.json')
    if preflight:
        run(positive,'sync-preflight',PREFIX,exact=False)
        (work/'preflight-summary.json').write_text(json.dumps(dict(suites=suites,clippy=delta),indent=2)+'\n');print('AudioSync preflight passed',flush=True);return
    run(positive,'full-debug')
    cache_keys=previous.previous
    old_gates=cache_keys.previous
    traces=[('sync',PCM,sync_pcm_receipt),('lru',lru.PCM,lru.lru_pcm_receipt),('ring',previous.PCM,previous.pcm_receipt),('cache',cache_keys.CACHE_PCM,cache_keys.cache_pcm_receipt)]
    for name,test,parser in [('pcm',old_gates.PCM,old_gates.pcm_receipt),('index',old_gates.INDEX_PCM,old_gates.index_pcm_receipt),('media',old_gates.MEDIA_PCM,old_gates.media_pcm_receipt),('inner',old_gates.INNER_PCM,old_gates.inner_pcm_receipt),('context',old_gates.CONTEXT_PCM,old_gates.context_pcm_receipt),('slot',old_gates.SLOT_PCM,old_gates.slot_pcm_receipt),('route',old_gates.ROUTE_PCM,old_gates.route_pcm_receipt),('mix',old_gates.MIX_PCM,old_gates.mix_pcm_receipt)]:traces.append((name,test,parser))
    current_pcm={}
    for name,test,parser in traces:run(positive,name+'-pcm-debug',test);current_pcm[name]=parser(work/(name+'-pcm-debug.log'))
    adapted=original_output((before_directory/'engine/crates/audio/src/output.rs').read_text());(work/'original-output-adapted.rs').write_text(adapted)
    try:
        paths['output'].write_text(adapted);binary=build('original-output')
        for label,test,message in [('before-sync-credit','actual_arc_cell_layout_and_one_physical_charge_match_allocator','AudioSync cell omitted physical credit'),('before-sync-refusal','rejected_new_sync_preserves_old_shared_state_and_original_credit','AudioSync replacement ignored physical refusal')]:
            run(binary,label,PREFIX+test,101,message);red.append(dict(name=label,test=PREFIX+test,exit=101,expected_assertion=message,sha256=programs[-1]['sha256']))
        for name,test,parser in traces:run(binary,name+'-pcm-before',test);assert parser(work/(name+'-pcm-before.log'))==current_pcm[name],name
        programs[-1]['pruned']=True;binary.unlink()
    finally:paths['output'].write_text(originals['output'])
    for row in rows:
        modified=set()
        try:
            for key,before,after in row['changes']:
                content=paths[key].read_text() if key in modified else originals[key]
                assert content.count(before)==1,(row['name'],key,'fault anchor drift')
                modified.add(key);paths[key].write_text(content.replace(before,after))
            binary=build(row['name']);run(binary,row['name'],row['test'],101,row['expected_assertion']);controls.append({k:v for k,v in row.items()if k!='changes'}|dict(exit=101,sha256=programs[-1]['sha256']));programs[-1]['pruned']=True;binary.unlink();(work/'completed-controls.json').write_text(json.dumps(controls,indent=2)+'\n');print('Fresh AudioSync fault rejected',row['name'],flush=True)
        finally:
            for key in modified:paths[key].write_text(originals[key])
    release=build('positive-release',True);run(release,'full-release')
    for name,test,parser in traces:run(release,name+'-pcm-release',test);assert parser(work/(name+'-pcm-release.log'))==current_pcm[name],name
    product.whole.verify_inputs(work,frozen);assert all(p.read_text()==originals[k]for k,p in paths.items())
    for name in ['audio','shared','io','services','frame-wire']:
        src=ROOT/'engine/crates'/name/'src';private=package/'src'if name=='audio'else package/'local-deps'/name/'src';assert all((private/p.relative_to(src)).read_bytes()==p.read_bytes()for p in src.rglob('*')if p.is_file()),name
    counts={name:dict(blocks=len(data),samples=sum(len(row['pcm'])for row in data.values()))for name,data in current_pcm.items()}
    (work/'complete-pcm-comparison.json').write_text(json.dumps(dict(traces=counts,blocks=sum(v['blocks']for v in counts.values()),samples=sum(v['samples']for v in counts.values()),all_bits_state_clock_and_order_equal=True,original_output_body_recompiled=True,original_audio_sync_numeric_methods_unchanged=True,small_sync_cell_credit_after_real_arc_free=True,nested_notifier_and_native_backend_metadata_still_open=True),indent=2)+'\n')
    (work/'summary.json').write_text(json.dumps(dict(programs=programs,controls=controls,original_red=red,suites=suites,clippy=delta,root_inputs_unchanged=True,source_copies_restored=True,root_dev_profile_verified=True,isolated_release_is_not_root_shipping=True,preflight=False),indent=2)+'\n');print('AudioSync ownership verified',len(controls),'fresh faults',flush=True)

if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--output',type=Path,required=True);p.add_argument('--target-dir',type=Path,required=True);p.add_argument('--before-directory',type=Path,required=True);p.add_argument('--preflight',action='store_true');a=p.parse_args();main(a.output.resolve(),a.target_dir.resolve(),a.before_directory.resolve(),a.preflight)
