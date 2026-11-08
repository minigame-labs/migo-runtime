#!/usr/bin/env python3
"""Verify large local files through real Fs, CPU, publication and player.

The build phase is offline and never uses Git. --execute only runs retained,
SHA-bound ELFs; original Audio HTTP regressions need loopback test permission.
"""
from __future__ import annotations
import argparse
from collections import Counter
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import check_audio_command_ownership as base
import check_audio_oboe_callback as backend
import check_audio_waveshaper_storage as wave

ROOT=base.ROOT
FIXTURE='platforms/apple/WebContent/PerformancePlus/test/fixtures/audio-check-answers.json'
NATIVE='audio_thread::decode_pool::tests::sandbox_file_service_reaches_the_actual_decode_pool_with_complete_pcm_and_identity'
TEST='audio::tests::local_file::'

FILE_TEST='streaming_file::tests::'
SERVICE_TEST='audio::tests::file_stream::'
FULL=FILE_TEST+'sealed_file_cpu_publication_matches_complete_mature_pcm_for_every_sequential_codec'
LARGE=FILE_TEST+'actual_large_file_src_plays_all_pcm_beyond_both_old_whole_file_limits'
CUSTODY=FILE_TEST+'unpolled_producer_and_rejected_commands_keep_file_and_queue_credit_until_cpu_drop'
ERROR=FILE_TEST+'native_file_readiness_keeps_typed_errors_and_source_refusal_without_duplicate_events'

def controls(originals):
    rows=[]
    def add(name,changes,test,assertion,crate='audio'):
        for path,a,z in changes:assert a!=z and originals[path].count(a)==1,(name,path,'anchor drift')
        rows.append(dict(name=name,changes=changes,test=test,expected_assertion=assertion,crate=crate))
    bounds=SERVICE_TEST+'invalid_cancelled_expired_and_closed_file_requests_cannot_become_successful_bytes'
    add('file-loan-owner-weakened',[('file','_file: Arc<File>,','_file: std::sync::Weak<File>,'),('file','_file: self','_file: Arc::downgrade(&self)')],SERVICE_TEST+'returned_file_loan_keeps_original_source_credit_until_the_last_loan_drops','file loan released the sealed object before consumption','migo-services')
    add('file-request-bounds-disabled',[('file','length == 0','false')],bounds,'file read ignored bounds before enqueue','migo-services')
    add('file-request-cancel-disabled',[('file','|| cancelled.is_cancelled()','|| false')],bounds,'cancelled file read was enqueued','migo-services')
    add('file-request-deadline-disabled',[('file','if std::time::Instant::now() >= deadline {','if false {')],bounds,'expired queued file work returned successful bytes','migo-services')
    add('file-offset-shifted',[('file','.chunk_at(offset, length)','.chunk_at(offset + 1, length)')],SERVICE_TEST+'file_capability_keeps_complete_bytes_and_the_captured_prefix_after_source_changes','sealed file loan changed complete bytes','migo-services')
    add('file-source-caller-destructor',[('native','_owner: OffWorker<Arc<dyn AudioFile>>,','_owner: OffWorker<()>,'),('native','_owner: OffWorker::new(file.clone(), cpu.clone()),','_owner: OffWorker::new((), cpu.clone()),')],CUSTODY,'file owner freed on caller before original CPU destruction')
    add('file-rejected-input-caller-destructor',[('channel','reply(resp, source, permit, bytes, error)','{ let _ = resp.send(Err(error)); drop((source, permit)); None }')],CUSTODY,'file command returned original queue credit before CPU destruction')
    add('file-discarded-input-caller-destructor',[('channel','AudioCmd::InnerAudioLoadFile { source, .. } => Box::new(source),','AudioCmd::InnerAudioLoadFile { source, .. } => { drop(source); Box::new(()) },')],CUSTODY,'file owner freed on caller before original CPU destruction')
    add('file-empty-format-untyped',[('native','step.progress == transport::Progress::Finished && step.sample_rate == 0','false')],ERROR,'local file loading failure lost its typed cause')
    add('file-error-cause-lost',[('native','*error.lock().unwrap() = Some(cause.clone());','*error.lock().unwrap() = None;')],ERROR,'local file loading failure lost its typed cause')
    add('file-readiness-ack-early',[('actor','reply: resp,','reply: if source.prefix().starts_with(b"RIFF") { let _ = resp.send(Ok(())); tokio::sync::oneshot::channel().0 } else { resp },')],ERROR,'local file replied before native CanPlay')
    add('file-rpc-class-lost',[('service','''    async fn answer_file(self) -> Result<T, ServiceError> {
        match self.0.await.map_err(|_| response_closed())? {
            Ok(value) => Ok(value),
            Err(error) if error.code == ErrorCode::InvalidArgument => {''','''    async fn answer_file(self) -> Result<T, ServiceError> {
        match self.0.await.map_err(|_| response_closed())? {
            Ok(value) => Ok(value),
            Err(error) if false => {''')],SERVICE_TEST+'actual_large_file_command_holds_a_sealed_capability_and_waits_for_native_readiness','local file codec failure lost its original class','migo-services')
    return rows

def owned_diagnostics(log,package):
    result=Counter()
    for key,count in wave.diagnostics(log,package).items():
        files=tuple(re.sub(r'^local-deps/(services|io|shared)/',r'crates/\1/',n) for n in key[0])
        if any(n.startswith(('crates/shared/src/audio_file','crates/shared/src/audio_channel','crates/shared/src/protocol/audio_cmd','crates/io/src/domain','crates/services/src/audio')) for n in files):result[(files,*key[1:])]+=count
    return result

def build(work,target,before,before_clippy,before_services_clippy):
    package,env,frozen=base.prepare(work,target)
    common=base.capacity.product.whole.support.clock.common
    p=work/FIXTURE;p.parent.mkdir(parents=True,exist_ok=True);shutil.copy2(ROOT/FIXTURE,p)
    p=work/'source-inputs'/FIXTURE;p.parent.mkdir(parents=True,exist_ok=True);shutil.copy2(ROOT/FIXTURE,p);frozen[FIXTURE]=common.sha(ROOT/FIXTURE);(work/'source-inputs.json').write_text(json.dumps(frozen,indent=2)+'\n')
    output=package/'src/output.rs';saved=output.read_text()
    try:
        output.write_text(backend.ports(saved));assert common.run(['cargo','clippy','--lib','--tests','--offline','--locked','--message-format=json'],work/'clippy.jsonl',package,env,timeout=600)==0
    finally:output.write_text(saved)
    a=wave.diagnostics(before_clippy,before_clippy.parent/'probe');z=wave.diagnostics(work/'clippy.jsonl',package);delta=dict(before=sum(a.values()),after=sum(z.values()),new_diagnostics=sum((z-a).values()),new=[dict(key=k,count=v) for k,v in (z-a).items()]);assert not(z-a),delta
    assert common.run(['cargo','clippy','-p','migo-services','--lib','--tests','--offline','--locked','--message-format=json'],work/'services-clippy.jsonl',package,env,timeout=600)==0
    a=owned_diagnostics(before_services_clippy,ROOT/'engine');z=owned_diagnostics(work/'services-clippy.jsonl',package);owned=dict(before=sum(a.values()),after=sum(z.values()),new_diagnostics=sum((z-a).values()),new=[dict(key=k,count=v) for k,v in (z-a).items()]);assert not(z-a),owned
    (work/'clippy-delta.json').write_text(json.dumps(dict(native=delta,owned_source=owned),indent=2)+'\n');print('Stable Clippy',delta,owned,flush=True)
    programs=[];steps=[]
    def native(name,release=False):
        binary=base.capacity.product.src.build(package,work,env,name,release);row=dict(name=name,path=str(binary),sha256=common.sha(binary),fresh=False);programs.append(row);return row
    def other(name,crate,release=False):
        folder='services' if crate=='migo-services' else 'io';(package/'local-deps'/folder/'src/lib.rs').touch();log=work/(name+'-build.jsonl');cmd=['cargo','test','-p',crate,'--lib','--offline','--locked','--no-run','--message-format=json']
        if crate=='migo-io':cmd+=['--no-default-features','--features','zip-extract']
        if release:cmd+=['--release']
        assert common.run(cmd,log,package,env,timeout=600)==0
        rows=[]
        for line in log.read_text().splitlines():
            try:x=json.loads(line)
            except ValueError:continue
            if x.get('reason')=='compiler-artifact' and x.get('executable') and x.get('target',{}).get('name')==crate.replace('-','_'):rows.append(x)
        assert len(rows)==1 and rows[0]['fresh'] is False
        if release:assert rows[0]['profile']['opt_level']=='3'
        binary=work/(name+'-tests');shutil.copy2(rows[0]['executable'],binary);row=dict(name=name,path=str(binary),sha256=common.sha(binary),fresh=False);programs.append(row);return row
    for release,label in [(False,'debug'),(True,'release')]:
        row=native('positive-'+label,release)
        steps += [dict(label='full-'+label,program=row,expected=0),dict(label='local-pcm-'+label,program=row,expected=0,test=NATIVE,dump='local'),dict(label='file-pcm-'+label,program=row,expected=0,test=FULL,dump='file'),dict(label='large-file-'+label,program=row,expected=0,test=LARGE)]
        row=other('services-'+label,'migo-services',release);steps.append(dict(label='services-'+label,program=row,expected=0,selector='audio::tests::'))
    row=other('source-snapshot','migo-io');steps.append(dict(label='source-snapshot',program=row,expected=0,selector='source_snapshot::tests::'))
    paths={'file':package/'local-deps/services/src/audio_file_stream.rs','service':package/'local-deps/services/src/audio.rs','channel':package/'local-deps/shared/src/audio_channel.rs','native':package/'src/streaming_file.rs','actor':package/'src/audio_thread.rs'};originals={n:p.read_text() for n,p in paths.items()};faults=controls(originals)
    # Restore the previous whole-file profile: same captured source, original
    # bounded collector and original DecodePool. The real product must refuse it.
    anchor='if sealed.reader().len()<=MAX_ENCODED_AUDIO_BYTES as u64 {'
    assert originals['file'].count(anchor)==1
    try:
        paths['file'].write_text(originals['file'].replace(anchor,'if true {'))
        row=native('original-whole-file-profile');steps.append(dict(label='original-whole-file-profile',program=row,expected=101,test=LARGE,expected_assertion='large local file was rejected before source handoff',role='original_runtime_red'))
    finally:paths['file'].write_text(originals['file'])
    for fault in faults:
        try:
            for n,a,z in fault['changes']:
                current=paths[n].read_text();assert current.count(a)==1;paths[n].write_text(current.replace(a,z))
            row=native(fault['name']) if fault['crate']=='audio' else other(fault['name'],fault['crate']);steps.append({k:v for k,v in fault.items() if k not in ['changes','crate']}|dict(label=fault['name'],program=row,expected=101,role='fresh_source_fault'));print('Fresh fault prepared',fault['name'],flush=True)
        finally:
            for n,_,_ in fault['changes']:paths[n].write_text(originals[n])
    base.capacity.product.whole.verify_inputs(work,frozen)
    for name in ['audio','shared','io','services','frame-wire']:
        source=ROOT/'engine/crates'/name/'src';private=package/'src' if name=='audio' else package/'local-deps'/name/'src';assert all(p.read_bytes()==(private/p.relative_to(source)).read_bytes() for p in source.rglob('*') if p.is_file()),name
    (work/'execution-plan.json').write_text(json.dumps(dict(programs=programs,steps=steps,clippy=delta,owned_clippy=owned,source_inputs=frozen,private_sources_restored=True),indent=2)+'\n');print('Immutable local File execution plan prepared',flush=True)

def pcm(path,prefix='MIGO_LOCAL_FILE_PCM ',expected=12):
    rows={}
    for line in path.read_text().splitlines():
        if line.startswith(prefix):
            _,key,values=line.split(' ',2);assert key not in rows;rows[key]=json.loads(values)
    assert len(rows)==expected,(path,len(rows));return rows

def execute(work):
    plan=json.loads((work/'execution-plan.json').read_text());sha=lambda p:hashlib.sha256(p.read_bytes()).hexdigest()
    for n,h in plan['source_inputs'].items():assert sha(ROOT/n)==sha(work/'source-inputs'/n)==h,n
    suites={};faults=[];reds=[]
    for step in plan['steps']:
        p=step['program'];binary=Path(p['path']);assert sha(binary)==p['sha256'];args=[str(binary)]+(['--exact',step['test'],'--nocapture'] if step.get('test') else [step['selector'],'--nocapture'] if step.get('selector') else []);env=os.environ.copy()
        if step.get('dump'):env['MIGO_DUMP_LOCAL_FILE_PCM' if step['dump']=='local' else 'MIGO_DUMP_FILE_STREAM_PCM']='1'
        log=work/(step['label']+'.log')
        with log.open('w') as f:q=subprocess.run(args,cwd=work/'probe',env=env,stdout=f,stderr=subprocess.STDOUT,timeout=900)
        (work/(step['label']+'-exit.txt')).write_text(str(q.returncode)+'\n');text=log.read_text();assert q.returncode==step['expected'],(step['label'],q.returncode)
        if q.returncode:
            assert all(s in text for s in ['running 1 test','panicked at','1 failed',step['test'],step['expected_assertion']]),step['label'];(faults if step['role']=='fresh_source_fault' else reds).append(dict(name=step['label'],test=step['test'],expected_assertion=step['expected_assertion'],exit=101,sha256=p['sha256']))
        else:
            m=re.search(r'test result: ok\. ([1-9][0-9]*) passed; 0 failed; (\d+) ignored;',text);assert m,step['label'];suites[step['label']]=dict(passed=int(m[1]),ignored=int(m[2]))
        print('Executed',step['label'],q.returncode,flush=True)
    a=pcm(work/'local-pcm-debug.log');z=pcm(work/'local-pcm-release.log');assert a==z,'local source full PCM dev/O3 differs'
    f=pcm(work/'file-pcm-debug.log','MIGO_FILE_STREAM_PCM ',8);g=pcm(work/'file-pcm-release.log','MIGO_FILE_STREAM_PCM ',8);assert f==g,'sealed file complete PCM dev/O3 differs'
    large={}
    for label in ['debug','release']:
        text=(work/('large-file-'+label+'.log')).read_text();m=re.search(r'MIGO_LARGE_FILE_PCM frames=(\d+) bits=(\d+) bounded_pcm_bytes=(\d+)',text);assert m and int(m[1])==16777345 and int(m[2])==1048576000 and int(m[3])<4*1024*1024;large[label]=dict(samples=int(m[1]),bits=int(m[2]),retained_pcm_bytes=int(m[3]),every_sample_checked=True,tail_and_end_checked=True)
    (work/'summary.json').write_text(json.dumps(dict(programs=plan['programs'],suites=suites,controls=faults,original_red=reds,clippy=plan['clippy'],owned_clippy=plan['owned_clippy'],pcm=dict(outputs=len(a),samples=sum(map(len,a.values())),all_dev_o3_bits_equal=True),file_pcm=dict(outputs=len(f),samples=sum(map(len,f.values())),all_dev_o3_bits_equal=True),large_file=large,source_inputs_unchanged=True,private_sources_restored=True),indent=2)+'\n');print('Large sealed File product playback verified',flush=True)

if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--output',type=Path,required=True);p.add_argument('--target-dir',type=Path);p.add_argument('--before-source',type=Path);p.add_argument('--before-clippy',type=Path);p.add_argument('--before-services-clippy',type=Path);p.add_argument('--execute',action='store_true');a=p.parse_args()
    if a.execute:execute(a.output.resolve())
    else:
        assert all([a.target_dir,a.before_source,a.before_clippy,a.before_services_clippy]);build(a.output.resolve(),a.target_dir.resolve(),a.before_source.resolve(),a.before_clippy.resolve(),a.before_services_clippy.resolve())
