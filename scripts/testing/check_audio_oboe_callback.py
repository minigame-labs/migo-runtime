#!/usr/bin/env python3
"""Oboe callback Box: physical admission, raw context handoff and original CPU backing.

The original complete Oboe backend is recompiled for true reds and
complete PCM comparisons. Faults change actual private Rust source, one at a time.
"""
from __future__ import annotations
import argparse,json,re,shutil,tomllib
from pathlib import Path
import check_audio_output_ring as previous
import check_audio_cache_lru as lru
import check_audio_output_sync as sync
import check_audio_cpal_errors as cpal
import check_audio_ohaudio_errors as ohaudio
base=previous.base
wave=previous.wave
ROOT=base.ROOT
PREFIX='output::oboe_source_probe::callback_storage::tests::'
PCM=PREFIX+'complete_native_handoff_pcm_and_refill_trace'

def error_pcm_receipt(log):
    decoder=json.JSONDecoder();rows={}
    for line in log.read_text().splitlines():
        if not line.startswith('MIGO_OBOE_BOX_PCM '):continue
        _,key,tail=line.split(' ',2);pcm,end=decoder.raw_decode(tail);state,end=decoder.raw_decode(tail[end:].lstrip());assert key not in rows;rows[key]=dict(pcm=pcm,state=state)
    assert len(rows)==48,(log,len(rows));return rows

CPP_INPUTS=['engine/crates/audio/native/oboe_bridge.cpp','engine/crates/audio/native/oboe_bridge.h','scripts/testing/fixtures/oboe/bridge_test.cpp','scripts/testing/fixtures/oboe/oboe/Oboe.h','scripts/test-oboe-backend-contract.sh']
PROBE_ROOT='engine/testing/alloc-probe'

def allocator_verification(work,env,before_directory,common,programs):
    """Compare the dev-only allocator itself, then test the exact private copy.

    A dependency's Clippy is not a substitute for linting its own test target.
    Both versions live in private packages; Root source is never restored.
    """
    root=tomllib.loads((ROOT/'engine/Cargo.toml').read_text())
    manifest=(ROOT/PROBE_ROOT/'Cargo.toml').read_text()
    for key in ['version','license']:
        manifest=manifest.replace(key+'.workspace = true',key+' = '+json.dumps(root['workspace']['package'][key]))
    manifest+='\n[workspace]\n[profile.dev]\n'
    dev=root['profile']['dev']
    for key,value in dev.items():
        if key not in ['package','build-override']:
            manifest+=key+' = '+json.dumps(value)+'\n'
    old,current=work/'allocator-before',work/'allocator-current'
    for package in [old,current]:
        package.mkdir()
        shutil.copytree(ROOT/PROBE_ROOT/'src',package/'src')
        (package/'Cargo.toml').write_text(manifest)
    (old/'src/lib.rs').write_bytes((before_directory/PROBE_ROOT/'src/lib.rs').read_bytes())
    (old/'src/deallocation_watch.rs').unlink()
    identities=lambda data:{(p['name'],p['version'],p.get('source'),p.get('checksum'))for p in data['package']}
    lock_policy={}
    for label,package in [('before',old),('current',current)]:
        assert common.run(['cargo','check','--lib','--offline','--message-format=json'],work/('allocator-'+label+'-prepare.jsonl'),package,env,timeout=300)==0
        selected=tomllib.loads((package/'Cargo.lock').read_text())
        assert identities(selected)<=identities(tomllib.loads((ROOT/'engine/Cargo.lock').read_text()))
        lock_policy[label]=dict(all_identities_from_root=True,selected_packages=len(selected['package']))
        assert common.run(['cargo','clippy','--lib','--tests','--offline','--locked','--message-format=json'],work/('allocator-'+label+'-clippy.jsonl'),package,env,timeout=300)==0
    previous,current_diags=wave.diagnostics(work/'allocator-before-clippy.jsonl',old),wave.diagnostics(work/'allocator-current-clippy.jsonl',current)
    added=current_diags-previous
    delta=dict(before=sum(previous.values()),after=sum(current_diags.values()),new_diagnostics=sum(added.values()),new=[dict(key=k,count=v)for k,v in added.items()])
    (work/'allocator-clippy-delta.json').write_text(json.dumps(delta,indent=2)+'\n');assert not added,delta
    (current/'src/lib.rs').touch()
    build=work/'allocator-positive-build.jsonl'
    assert common.run(['cargo','test','--lib','--offline','--locked','--no-run','--message-format=json'],build,current,env,timeout=300)==0
    rows=[]
    for line in build.read_text().splitlines():
        try:row=json.loads(line)
        except ValueError:continue
        if row.get('reason')=='compiler-artifact' and row.get('executable') and row.get('target',{}).get('name')=='migo_alloc_probe':rows.append(row)
    assert len(rows)==1 and rows[0]['fresh'] is False
    binary=work/'allocator-positive-tests';shutil.copy2(rows[0]['executable'],binary)
    programs.append(dict(name='allocator-positive',path=str(binary),sha256=common.sha(binary),pruned=False,role='dev_only_address_specific_deallocation_observer'))
    assert common.run([str(binary),'--nocapture'],work/'allocator-positive.log',current,env,timeout=60)==0
    assert 'test result: ok. 15 passed; 0 failed; 0 ignored;' in (work/'allocator-positive.log').read_text()
    sources={str(p.relative_to(ROOT)):common.sha(p)for p in (ROOT/PROBE_ROOT/'src').rglob('*')if p.is_file()}
    for name,h in sources.items():assert common.sha(current/'src'/Path(name).relative_to(PROBE_ROOT+'/src'))==h
    receipt=dict(clippy=delta,tests=dict(passed=15,ignored=0,new_identity_tests=4),build_fresh=False,lock_policy=lock_policy,source_sha256=sources,private_source_equal_root=True,dev_dependency_only=True)
    (work/'allocator-verification.json').write_text(json.dumps(receipt,indent=2)+'\n')
    print('Allocator own-source Clippy compared and 4 identity tests passed',delta,flush=True)
    return receipt

def declarations(originals):
    rows=[]
    def add(name,key,before,after,test,message):
        assert before!=after and originals[key].count(before)==1,(name,key,'anchor drift')
        rows.append(dict(name=name,changes=[(key,before,after)],test=PREFIX+test,expected_assertion=message))
    charged='actual_box_layout_and_production_charge_match_real_allocation'
    first='admission_precedes_factory_and_all_production_preparation_allocations'
    final='final_native_release_frees_box_before_refund_and_bulk_on_original_cpu'
    cancel='cancelling_before_handoff_destroys_payload_once_after_box_free_without_heap'
    handoff='admitted_callback_handoff_and_first_steady_render_work_at_full_budget_after_close'
    add('box-capacity-uncharged','storage','host.reserve_inline(bytes)?','host.reserve_inline(0)?',charged,'Oboe callback Box omitted physical credit')
    add('box-layout-undercharged','storage','std::alloc::Layout::new::<Data>().size()','std::mem::size_of::<OutputCallback>()',charged,'Oboe callback Box allocation layout mismatch')
    anchor='        let permit = host.reserve_inline(bytes)?;'
    add('box-before-admission','storage',anchor,'        let _early=std::hint::black_box(Box::new([0u8;32]));\n'+anchor,first,'Oboe preparation allocated before callback Box admission')
    add('box-live-permit-lost','storage','            _permit: self.permit,','            _permit: None,',charged,'Oboe callback Box omitted physical credit')
    anchor='let data = unsafe { *Box::from_raw(pointer) };'
    add('box-refund-before-physical-free','storage',anchor+'\n    drop(data);','drop(unsafe{Box::from_raw(pointer)});',final,'Oboe callback refund preceded actual Box free or payload drop')
    add('box-credit-returned-early','storage',anchor,'let mut boxed=unsafe{Box::from_raw(pointer)};drop(boxed._permit.take());let data=*boxed;',final,'Oboe callback refund preceded actual Box free or payload drop')
    add('box-payload-forgotten','storage','    drop(data);','    std::mem::forget(data);',cancel,'Oboe callback payload was not destroyed')
    add('box-late-cleanup-allocates','storage',anchor,'let _late=std::hint::black_box(Box::new(7u8));\n    '+anchor,cancel,'Oboe callback cancellation allocated cleanup')
    add('box-handoff-refunds','storage','Box::into_raw(self.0.take().unwrap())','{let mut data=self.0.take().unwrap();drop(data._permit.take());Box::into_raw(data)}',handoff,'Oboe handoff lost live Box credit')
    add('box-handoff-allocates','storage','Box::into_raw(self.0.take().unwrap())','{let _extra=std::hint::black_box(Box::new(7u8));Box::into_raw(self.0.take().unwrap())}',handoff,'Oboe native handoff allocated')
    callback='            callback,'
    add('box-payload-copied','storage',callback,'            callback:{let(_p,c)=crate::output::ring::new(1,None).unwrap();let _=callback;OutputCallback::new(c,crate::output::AudioSync::new(),0)},','raw_handoff_keeps_original_ring_and_sync_addresses','Oboe callback Box copied its ring')
    admission='    let admission = callback_storage::Admission::new(host)?;'
    rows.append(dict(name='box-production-unmanaged',changes=[('storage','impl Admission {','impl Admission { pub(super) fn unmanaged()->Self{Self{permit:None}}'),('backend',admission,'    let admission = callback_storage::Admission::unmanaged();')],test=PREFIX+'physical_refusal_preserves_old_prepared_output_and_original_pcm',expected_assertion='Oboe callback capacity bypassed physical refusal'))
    sync='    let sync = AudioSync::with_host(Some(host))?;'
    rows.append(dict(name='box-production-admits-late',changes=[('backend',admission,''),('backend',sync,sync+'\n'+admission)],test=PREFIX+first,expected_assertion='Oboe preparation allocated before callback Box admission'))
    add('box-native-render-silenced','backend','    callback.render_native(samples);','    callback.render_native(samples); samples.fill(0.0);','complete_native_handoff_pcm_and_refill_trace','Oboe complete callback Box PCM changed')
    add('box-release-leaked','storage',anchor,'let _=pointer;return;\n    '+anchor,cancel,'Oboe callback payload was not destroyed')
    add('box-unused-admission-omits-credit','storage','permit: Some(permit),','permit: None,','unused_admission_refunds_without_allocating_and_closed_host_refuses','Oboe unused callback admission omitted credit')
    for row in rows:
        for key,before,after in row['changes']:assert before!=after and originals[key].count(before)==1,(row['name'],key,'anchor drift')
    assert len(rows)==16
    return rows

PROBES=['#[cfg(all(test, target_os = "linux", not(target_env = "ohos")))]\n#[path = "output_'+name+'.rs"]\nmod '+name+'_source_probe;\n' for name in ['oboe','ohaudio']]
def ports(source):
    for probe in PROBES:
        assert source.count(probe)<=1
        source=source.replace(probe,'')
    return previous.ports(source,True)

def original_backend(source):
    # The entire original production file remains a byte-identical prefix.
    method=wave.method(source,'pub fn new(host:')
    prefix=method[method.index('const CHANNELS:'):method.index('let callbacks = Callbacks {')]
    text=source+r'''
#[path="output_oboe_callback_storage.rs"] mod callback_storage;
struct Prepared {producer:ring::Producer,sync:AudioSync,callback:Box<OutputCallback>}
fn prepare(host:&crate::node_retirement::Host)->EngineResult<Prepared>{
'''+prefix+'Ok(Prepared{producer,sync,callback})}\n'+r'''
fn into_callbacks(callback:Box<OutputCallback>)->Callbacks{Callbacks{context:Box::into_raw(callback).cast(),render,release}}
#[cfg(test)]fn observe_callback(_callback:&mut Box<OutputCallback>,_before:std::sync::Arc<dyn Fn()+Send+Sync>,_after:std::sync::Arc<dyn Fn()+Send+Sync>){}
#[cfg(test)]const _:()={
'''
    for name in ['new','sample_rate','channels','write','available','buffered','high_watermark','needs_data','sync','is_alive','pause_stream','resume_stream','storage_host']:text+='let _ = AudioOutput::'+name+';\n'
    return text+'};\n'

def cpp_contract(work,env,common,programs):
    executable=work/'oboe-cpp-bridge-asan-ubsan-tests'
    source=work/'source-inputs'
    command=['/usr/bin/clang++','-std=c++17','-pthread','-O1','-g','-fsanitize=address,undefined','-Wall','-Wextra','-Werror','-I'+str(source/'scripts/testing/fixtures/oboe'),'-I'+str(source/'engine/crates/audio/native'),str(source/'scripts/testing/fixtures/oboe/bridge_test.cpp'),str(source/'engine/crates/audio/native/oboe_bridge.cpp'),'-o',str(executable)]
    assert common.run(command,work/'cpp-bridge-build.log',work,env,timeout=180)==0
    code=common.run([str(executable)],work/'cpp-bridge-test.log',work,env,timeout=60)
    output=(work/'cpp-bridge-test.log').read_text()
    if code:assert code==1 and 'LeakSanitizer has encountered a fatal error' in output and 'does not work under ptrace' in output,('C++ contract failed',code)
    row=dict(name='oboe-cpp-bridge-asan-ubsan',path=str(executable),sha256=common.sha(executable),pruned=False,role='unchanged_cpp_bridge_host_ownership_contract')
    programs.append(row)
    contract=dict(command=command,exit=code,asan=True,ubsan=True,leak_checks_disabled=False,permission_pending=bool(code),source_inputs={name:common.sha(source/name)for name in CPP_INPUTS},program_sha256=row['sha256'],native_android_device_acceptance=False)
    (work/'cpp-bridge-contract.json').write_text(json.dumps(contract,indent=2)+'\n')
    if code:(work/'cpp-bridge-deferred.json').write_text(json.dumps(row,indent=2)+'\n')
    print('C++ bridge sanitizer contract '+('awaits same-SHA replay outside ptrace sandbox' if code else 'passed'),flush=True)

def main(work,target,before_directory,preflight=False):
    package,env,frozen=base.prepare(work,target);product=base.capacity.product;common=product.whole.support.clock.common
    names={'storage':'output_oboe_callback_storage.rs','backend':'output_oboe.rs','output':'output.rs'}
    paths={k:package/'src'/v for k,v in names.items()};originals={k:p.read_text()for k,p in paths.items()};rows=declarations(originals)
    for path in [Path(__file__).resolve()]+[ROOT/name for name in CPP_INPUTS]+list((ROOT/PROBE_ROOT/'src').rglob('*.rs')):
        name=str(path.relative_to(ROOT));frozen[name]=common.sha(path);dest=work/'source-inputs'/name;dest.parent.mkdir(parents=True,exist_ok=True);shutil.copy2(path,dest)
    (work/'source-inputs.json').write_text(json.dumps(frozen,indent=2)+'\n')
    programs=[];controls=[];red=[];suites={}
    allocator=allocator_verification(work,env,before_directory,common,programs)
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
        paths['output'].write_text(ports(originals['output']))
        assert common.run(clippy,work/'clippy.jsonl',package,env,timeout=600)==0
        for n,p in old_paths.items():p.write_bytes((before_directory/n).read_bytes())
        paths['output'].write_text(ports((before_directory/'engine/crates/audio/src/output.rs').read_text()))
        assert common.run(clippy,work/'clippy-before.jsonl',package,env,timeout=600)==0
    finally:
        for n,p in old_paths.items():p.write_bytes(saved[n])
        paths['output'].write_text(originals['output'])
    old,current=wave.diagnostics(work/'clippy-before.jsonl',package),wave.diagnostics(work/'clippy.jsonl',package);added=current-old
    delta=dict(before=sum(old.values()),after=sum(current.values()),new_diagnostics=sum(added.values()),actor_and_all_rust_backend_bodies_included=True,allocator_own_source=allocator['clippy'],new=[dict(key=k,count=v)for k,v in added.items()]);(work/'clippy-delta.json').write_text(json.dumps(delta,indent=2)+'\n');assert not added,delta
    print('Stable source Clippy compared',delta,flush=True)
    positive=build('positive-debug');base.capacity.profile.verify(work/'positive-debug-build.jsonl',json.loads((work/'debug-profile-policy.json').read_text()),work/'debug-profile-artifacts.json')
    if preflight:
        run(positive,'errors-preflight',PREFIX,exact=False)
        (work/'preflight-summary.json').write_text(json.dumps(dict(suites=suites,clippy=delta),indent=2)+'\n');print('Oboe callback Box preflight passed',flush=True);return
    run(positive,'full-debug')
    cpp_contract(work,env,common,programs)
    cache_keys=previous.previous
    old_gates=cache_keys.previous
    traces=[('oboe',PCM,error_pcm_receipt),('ohaudio',ohaudio.PCM,ohaudio.error_pcm_receipt),('error',cpal.PCM,cpal.error_pcm_receipt),('sync',sync.PCM,sync.sync_pcm_receipt),('lru',lru.PCM,lru.lru_pcm_receipt),('ring',previous.PCM,previous.pcm_receipt),('cache',cache_keys.CACHE_PCM,cache_keys.cache_pcm_receipt)]
    for name,test,parser in [('pcm',old_gates.PCM,old_gates.pcm_receipt),('index',old_gates.INDEX_PCM,old_gates.index_pcm_receipt),('media',old_gates.MEDIA_PCM,old_gates.media_pcm_receipt),('inner',old_gates.INNER_PCM,old_gates.inner_pcm_receipt),('context',old_gates.CONTEXT_PCM,old_gates.context_pcm_receipt),('slot',old_gates.SLOT_PCM,old_gates.slot_pcm_receipt),('route',old_gates.ROUTE_PCM,old_gates.route_pcm_receipt),('mix',old_gates.MIX_PCM,old_gates.mix_pcm_receipt)]:traces.append((name,test,parser))
    current_pcm={}
    for name,test,parser in traces:run(positive,name+'-pcm-debug',test);current_pcm[name]=parser(work/(name+'-pcm-debug.log'))
    adapted=original_backend((before_directory/'engine/crates/audio/src/output_oboe.rs').read_text());(work/'original-oboe-adapted.rs').write_text(adapted)
    try:
        paths['backend'].write_text(adapted);binary=build('original-oboe')
        for label,test,message in [('before-box-credit','actual_box_layout_and_production_charge_match_real_allocation','Oboe callback Box omitted physical credit'),('before-box-refusal','physical_refusal_preserves_old_prepared_output_and_original_pcm','Oboe callback capacity bypassed physical refusal')]:
            run(binary,label,PREFIX+test,101,message);red.append(dict(name=label,test=PREFIX+test,exit=101,expected_assertion=message,sha256=programs[-1]['sha256']))
        for name,test,parser in traces:run(binary,name+'-pcm-before',test);assert parser(work/(name+'-pcm-before.log'))==current_pcm[name],name
        programs[-1]['pruned']=True;binary.unlink()
    finally:paths['backend'].write_text(originals['backend'])
    for row in rows:
        modified=set()
        try:
            for key,before,after in row['changes']:
                content=paths[key].read_text() if key in modified else originals[key]
                assert content.count(before)==1,(row['name'],key,'fault anchor drift')
                modified.add(key);paths[key].write_text(content.replace(before,after))
            binary=build(row['name']);run(binary,row['name'],row['test'],101,row['expected_assertion']);controls.append({k:v for k,v in row.items()if k!='changes'}|dict(exit=101,sha256=programs[-1]['sha256']));programs[-1]['pruned']=True;binary.unlink();(work/'completed-controls.json').write_text(json.dumps(controls,indent=2)+'\n');print('Fresh Oboe callback fault rejected',row['name'],flush=True)
        finally:
            for key in modified:paths[key].write_text(originals[key])
    release=build('positive-release',True);run(release,'full-release')
    for name,test,parser in traces:run(release,name+'-pcm-release',test);assert parser(work/(name+'-pcm-release.log'))==current_pcm[name],name
    cpp=json.loads((work/'cpp-bridge-contract.json').read_text());assert cpp['exit']==0 and not cpp['permission_pending'], 'C++ sanitizer replay still pending'
    product.whole.verify_inputs(work,frozen);assert all(p.read_text()==originals[k]for k,p in paths.items())
    for name in ['audio','shared','io','services','frame-wire']:
        src=ROOT/'engine/crates'/name/'src';private=package/'src'if name=='audio'else package/'local-deps'/name/'src';assert all((private/p.relative_to(src)).read_bytes()==p.read_bytes()for p in src.rglob('*')if p.is_file()),name
    counts={name:dict(blocks=len(data),samples=sum(len(row['pcm'])for row in data.values()))for name,data in current_pcm.items()}
    (work/'complete-pcm-comparison.json').write_text(json.dumps(dict(traces=counts,blocks=sum(v['blocks']for v in counts.values()),samples=sum(v['samples']for v in counts.values()),all_bits_state_clock_and_order_equal=True,original_complete_oboe_body_prefix_unchanged=True,actual_rust_render_release_and_independent_pcm_oracle=True,cpp_bridge_asan_ubsan_contract_passed=True,small_callback_box_credit_after_real_free=True,original_cpu_backing_retirement_observed=True,ohos_callback_box_nested_notifier_and_other_metadata_still_open=True),indent=2)+'\n')
    (work/'summary.json').write_text(json.dumps(dict(programs=programs,controls=controls,original_red=red,suites=suites,clippy=delta,allocator=allocator,cpp_contract=cpp,root_inputs_unchanged=True,source_copies_restored=True,root_dev_profile_verified=True,isolated_release_is_not_root_shipping=True,preflight=False),indent=2)+'\n');print('Oboe callback Box ownership verified',len(controls),'fresh faults',flush=True)

if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--output',type=Path,required=True);p.add_argument('--target-dir',type=Path,required=True);p.add_argument('--before-directory',type=Path,required=True);p.add_argument('--preflight',action='store_true');a=p.parse_args();main(a.output.resolve(),a.target_dir.resolve(),a.before_directory.resolve(),a.preflight)
