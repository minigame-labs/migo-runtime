#!/usr/bin/env python3
"""Output backing admission and last SPSC reference retain the original CPU.

Keep pinned ringbuf algorithms, the complete original callback and actor PCM.
Only fresh, exact single-test business panics count as rejected source faults.
"""
from __future__ import annotations
import argparse,json,re,shutil
from pathlib import Path
import check_audio_cache_keys as previous

base=previous.base
wave=previous.wave
ROOT=base.ROOT
PREFIX='output::ring::tests::'
PCM=PREFIX+'complete_native_and_integer_callbacks_match_original_kernel_and_clock_hints'

def pcm_receipt(log):
    decode=json.JSONDecoder();rows={}
    for line in log.read_text().splitlines():
        if not line.startswith('MIGO_OUTPUT_RING_PCM '):continue
        _,key,tail=line.split(' ',2);pcm,end=decode.raw_decode(tail);state,end=decode.raw_decode(tail[end:].lstrip())
        assert key not in rows
        rows[key]=dict(pcm=pcm,state=state)
    assert len(rows)==144,(log,len(rows))
    return rows

def declarations(originals):
    rows=[]
    def add(name,key,before,after,test,message):
        assert before!=after and originals[key].count(before)==1,(name,key,'anchor drift')
        rows.append(dict(name=name,changes=[(key,before,after)],test=PREFIX+test,expected_assertion=message))
    charged='actual_boxed_capacity_and_arc_cell_are_fully_charged'
    first='refusal_precedes_data_allocation_and_preserves_live_ring'
    drop='last_callback_owner_retains_credit_until_original_cpu_drop_after_close'
    retry='retry_refusal_is_lazy_and_preserves_existing_output_until_budget_recovers'
    add('ring-capacity-uncharged','ring','host.reserve_value_with_bytes::<HeapRb<f32>>(bytes)?','host.reserve_value_with_bytes::<HeapRb<f32>>(cell_bytes())?',charged,'output ring actual capacity omitted physical credit')
    add('ring-arc-cell-uncharged','ring','.checked_add(cell_bytes())','.checked_add(0)',charged,'output ring actual capacity omitted physical credit')
    anchor='    let reservation = match host {'
    add('ring-data-before-admission','ring',anchor,'    let _early=std::hint::black_box(HeapRb::<f32>::new(samples));\n'+anchor,first,'output ring backing allocated before admission')
    add('ring-admission-bypassed','ring','Some(host) => host.reserve_value_with_bytes::<HeapRb<f32>>(bytes)?,','Some(_host) => Reservation::unmanaged(),',first,'output ring allocation bypassed physical admission')
    add('ring-host-authority-lost','ring','host: host.cloned(),','host: None,',charged,'output ring lost its host authority')
    add('ring-channel-layout-lost','retirement','channels: channels.max(1),','channels: 1,',charged,'output host lost channel layout')
    add('ring-ends-copy-backing','ring','Self(self.0.clone())','Self(Some(Arc::new(Data {ring:Reservation::unmanaged().finish(HeapRb::new(self.as_ref().capacity().get())),host:None})))',charged,'output ring endpoints copied the backing')
    add('ring-drop-on-callback','retirement','if self.lease.is_some() {','if false {',drop,'output ring backing freed on callback caller')
    add('ring-credit-before-destruction','retirement','            let lease = self.lease.take().unwrap();','            let mut lease = self.lease.take().unwrap();\n            lease.permit.shrink_to(0);',drop,'output ring credit returned before actual CPU destruction')
    add('ring-late-drop-allocation','ring','        if let Some(cell) = self.0.take() {','        let _late=std::hint::black_box(Box::new(7u8));\n        if let Some(cell) = self.0.take() {',drop,'output ring final callback allocated a late handoff')
    add('ring-retry-unbounded','ring','if self.next.is_some_and(|next| now < next) {','if false {',retry,'output retry repeated denied factory before deadline')
    add('ring-retry-does-not-recover','ring','now < next','now <= next',retry,'output retry did not run at deadline')
    add('ring-silent-unmanaged','silent','ring::new(RING_BUFFER_FRAMES * channels, host)?','ring::new(RING_BUFFER_FRAMES * channels, None)?','replacement_overlap_and_silent_drop_keep_original_ring_tickets','silent output ring omitted physical credit')
    add('ring-native-silenced','output','let read = self.consumer.pop_slice(data);','let read = self.consumer.pop_slice(data); data.fill(0.0);','complete_native_and_integer_callbacks_match_original_kernel_and_clock_hints','managed output callback complete PCM changed')
    add('ring-converted-tail-lost','output','scratch[read..].fill(0.0);','scratch.fill(0.0);','complete_native_and_integer_callbacks_match_original_kernel_and_clock_hints','managed output callback complete PCM changed')
    return rows

def legacy_ring(current):
    # The original native allocation/split expression, with only the newer
    # factory/host/observation signatures bridged. No permit or CPU owner.
    head='''use crate::node_retirement::{Host,Reservation,Value};
use ringbuf::{HeapRb,HeapProd,HeapCons,traits::{Observer,Split}};
use shared::error::EngineResult;
use std::{alloc::Layout,sync::{Arc,atomic::AtomicUsize}};
pub(crate) type Producer=HeapProd<f32>;
pub(crate) type Consumer=HeapCons<f32>;
struct Data {ring:Value<HeapRb<f32>>,host:Option<Host>}
pub(super) fn new(samples:usize,_host:Option<&Host>)->EngineResult<(Producer,Consumer)>{Ok(HeapRb::<f32>::new(samples).split())}
pub(super) fn host(_producer:&Producer)->Option<Host>{None}
'''
    return head+wave.method(current,'fn cell_bytes()')+'\n'+current[current.index('/// One inline deadline'):]

def ports(source,after):
    suffix='\n#[path="output_oboe.rs"] mod oboe_source_probe;\n#[path="output_ohaudio.rs"] mod ohaudio_source_probe;\nconst _: () = {\n'
    for backend in ['oboe_source_probe','ohaudio_source_probe']:
        for name in ['new','sample_rate','channels','write','available','buffered','high_watermark','needs_data','sync','is_alive','pause_stream','resume_stream']+(['storage_host']if after else[]):
            suffix+='let _ = '+backend+'::AudioOutput::'+name+';\n'
    return source+suffix+'};\n'

def main(work,target,before_directory,preflight=False):
    package,env,frozen=base.prepare(work,target);product=base.capacity.product;common=product.whole.support.clock.common
    names={'ring':'output_ring.rs','retirement':'node_retirement.rs','output':'output.rs','silent':'output_silent.rs','actor':'audio_thread.rs'}
    paths={k:package/'src'/v for k,v in names.items()};originals={k:p.read_text()for k,p in paths.items()};rows=declarations(originals)
    for path in [Path(__file__).resolve()]:
        name=str(path.relative_to(ROOT));frozen[name]=common.sha(path);dest=work/'source-inputs'/name;dest.parent.mkdir(parents=True,exist_ok=True);shutil.copy2(path,dest)
    (work/'source-inputs.json').write_text(json.dumps(frozen,indent=2)+'\n')
    programs=[];controls=[];red=[];suites={}
    def build(label,release=False):
        binary=product.src.build(package,work,env,label,release);programs.append(dict(name=label,path=str(binary),sha256=common.sha(binary),pruned=False));return binary
    def run(binary,label,test='',expected=0,assertion=None):
        code=common.run([str(binary)]+(['--exact',test,'--nocapture']if test else[]),work/(label+'.log'),package,env,timeout=900);(work/(label+'-exit.txt')).write_text(str(code)+'\n');output=(work/(label+'.log')).read_text();assert code==expected,(label,code)
        if expected:assert all(v in output for v in ['running 1 test','panicked at','1 failed',test,assertion]),label
        else:
            m=re.search(r'test result: ok\. ([1-9][0-9]*) passed; 0 failed; (\d+) ignored;',output);assert m,label;suites[label]=dict(passed=int(m[1]),ignored=int(m[2]))
    before_manifest=json.loads((before_directory/'root-sha256.json').read_text());changed=[n for n,h in before_manifest.items()if n.startswith('engine/crates/audio/')and n.endswith('.rs')and common.sha(ROOT/n)!=h]
    old_paths={n:package/n.removeprefix('engine/crates/audio/')for n in changed};saved={n:p.read_bytes()for n,p in old_paths.items()}
    clippy=['cargo','clippy','--lib','--tests','--offline','--locked','--message-format=json']
    try:
        paths['output'].write_text(ports(originals['output'],True))
        assert common.run(clippy,work/'clippy.jsonl',package,env,timeout=600)==0
        for n,p in old_paths.items():p.write_bytes((before_directory/n).read_bytes())
        paths['output'].write_text(ports(paths['output'].read_text(),False))
        assert common.run(clippy,work/'clippy-before.jsonl',package,env,timeout=600)==0
    finally:
        for n,p in old_paths.items():p.write_bytes(saved[n])
        paths['output'].write_text(originals['output'])
    old,current=wave.diagnostics(work/'clippy-before.jsonl',package),wave.diagnostics(work/'clippy.jsonl',package);added=current-old
    delta=dict(before=sum(old.values()),after=sum(current.values()),new_diagnostics=sum(added.values()),actor_and_all_rust_backends_included=True,new=[dict(key=k,count=v)for k,v in added.items()]);(work/'clippy-delta.json').write_text(json.dumps(delta,indent=2)+'\n');assert not added,delta
    print('Stable source and all backend Clippy compared',delta,flush=True)
    positive=build('positive-debug');base.capacity.profile.verify(work/'positive-debug-build.jsonl',json.loads((work/'debug-profile-policy.json').read_text()),work/'debug-profile-artifacts.json');run(positive,'full-debug')
    traces=[('ring',PCM,pcm_receipt),('cache',previous.CACHE_PCM,previous.cache_pcm_receipt)]
    old=previous.previous
    for name,test,parser in [('pcm',old.PCM,old.pcm_receipt),('index',old.INDEX_PCM,old.index_pcm_receipt),('media',old.MEDIA_PCM,old.media_pcm_receipt),('inner',old.INNER_PCM,old.inner_pcm_receipt),('context',old.CONTEXT_PCM,old.context_pcm_receipt),('slot',old.SLOT_PCM,old.slot_pcm_receipt),('route',old.ROUTE_PCM,old.route_pcm_receipt),('mix',old.MIX_PCM,old.mix_pcm_receipt)]:traces.append((name,test,parser))
    current_pcm={}
    for name,test,parser in traces:run(positive,name+'-pcm-debug',test);current_pcm[name]=parser(work/(name+'-pcm-debug.log'))
    if not preflight:
        adapted=legacy_ring(originals['ring']);(work/'original-native-ring-adapted.rs').write_text(adapted)
        try:
            paths['ring'].write_text(adapted);binary=build('original-native-ring')
            for label,test,message in [('before-ring-admission','refusal_precedes_data_allocation_and_preserves_live_ring','output ring allocation bypassed physical admission'),('before-ring-callback-drop','last_callback_owner_retains_credit_until_original_cpu_drop_after_close','output ring backing freed on callback caller')]:
                run(binary,label,PREFIX+test,101,message);red.append(dict(name=label,test=PREFIX+test,exit=101,expected_assertion=message,sha256=programs[-1]['sha256']))
            for name,test,parser in traces:run(binary,name+'-pcm-before',test);assert parser(work/(name+'-pcm-before.log'))==current_pcm[name],name
            programs[-1]['pruned']=True;binary.unlink()
        finally:paths['ring'].write_text(originals['ring'])
        for row in rows:
            modified=set()
            try:
                for key,before,after in row['changes']:modified.add(key);paths[key].write_text(originals[key].replace(before,after))
                binary=build(row['name']);run(binary,row['name'],row['test'],101,row['expected_assertion']);controls.append({k:v for k,v in row.items()if k!='changes'}|dict(exit=101,sha256=programs[-1]['sha256']));programs[-1]['pruned']=True;binary.unlink();(work/'completed-controls.json').write_text(json.dumps(controls,indent=2)+'\n');print('Fresh output ring fault rejected',row['name'],flush=True)
            finally:
                for key in modified:paths[key].write_text(originals[key])
        release=build('positive-release',True);run(release,'full-release')
        for name,test,parser in traces:run(release,name+'-pcm-release',test);assert parser(work/(name+'-pcm-release.log'))==current_pcm[name],name
    product.whole.verify_inputs(work,frozen);assert all(p.read_text()==originals[k]for k,p in paths.items())
    for name in ['audio','shared','io','services','frame-wire']:
        src=ROOT/'engine/crates'/name/'src';private=package/'src'if name=='audio'else package/'local-deps'/name/'src';assert all((private/p.relative_to(src)).read_bytes()==p.read_bytes()for p in src.rglob('*')if p.is_file()),name
    counts={name:dict(blocks=len(data),samples=sum(len(row['pcm'])for row in data.values()))for name,data in current_pcm.items()}
    (work/'complete-pcm-comparison.json').write_text(json.dumps(dict(traces=counts,blocks=sum(v['blocks']for v in counts.values()),samples=sum(v['samples']for v in counts.values()),all_bits_state_clock_and_order_equal=True,original_callback_kernel_unchanged=True,other_backend_rust_bodies_checked_on_linux_host=True,native_android_ohos_device_acceptance=False),indent=2)+'\n')
    (work/'summary.json').write_text(json.dumps(dict(programs=programs,controls=controls,original_red=red,suites=suites,clippy=delta,root_inputs_unchanged=True,source_copies_restored=True,root_dev_profile_verified=True,isolated_release_is_not_root_shipping=True,preflight=preflight),indent=2)+'\n');print('Output ring ownership verified',len(controls),'fresh faults',flush=True)

if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--output',type=Path,required=True);p.add_argument('--target-dir',type=Path,required=True);p.add_argument('--before-directory',type=Path,required=True);p.add_argument('--preflight',action='store_true');a=p.parse_args();main(a.output.resolve(),a.target_dir.resolve(),a.before_directory.resolve(),a.preflight)
