#!/usr/bin/env python3
"""Verify sandbox File capture through real services, Fs and DecodePool.

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

def controls(originals):
    rows=[]
    def add(name,changes,test,assertion):
        for path,a,z in changes:assert a!=z and originals[path].count(a)==1,(name,path,'anchor drift')
        rows.append(dict(name=name,changes=changes,test=TEST+test,expected_assertion=assertion))
    full='complete_snapshot_bytes_survive_source_replacement_after_sealing'
    limit='exact_source_limit_and_typed_oversize_keep_the_existing_error_class'
    retire='running_fs_read_keeps_the_original_memory_credit_until_the_job_returns'
    file='file';io='io';service='service'
    add('snapshot-limit-disabled',[(io,'if stamp.len > max_bytes {','if false {')],limit,'snapshot input limit lost its existing error class')
    add('typed-limit-tag-lost',[(io,'                SnapshotTooLarge,','                "snapshot exceeds limit",')],limit,'snapshot input limit lost its existing error class')
    add('running-source-owner-weakened',[(file,'let source = sealed.clone();','let source = Arc::downgrade(&sealed);'),(file,'source.reader().chunk_at(offset as u64, count)?','source.upgrade().ok_or_else(|| std::io::Error::other("source retired"))?.reader().chunk_at(offset as u64, count)?')],retire,'running original Fs read returned source credit before real completion')
    add('cancelled-source-still-read',[(file,'cancelled.load(Ordering::Acquire)','false')],retire,'cancelled source still read sealed bytes')
    add('closed-domain-ignored',[(file,'domain.is_closed()','false'),(file,'_ = domain.closed() =>','_ = std::future::pending::<()>() =>')],
        'closed_session_and_failed_capture_never_publish_partial_file_data','closed local source became a successful file')
    add('file-offset-shifted',[(file,'chunk_at(offset as u64, count)?','chunk_at(offset as u64 + 1, count)?')],full,'snapshot source refused complete bytes')
    add('captured-bytes-silenced',[(file,'data.extend_from_slice(chunk.as_ref());','data.resize(data.len() + chunk.as_ref().len(), 0);')],full,'snapshot bytes changed after the original source changed')
    a='''                let mut permit = reserve(&tx, MAX_ENCODED_AUDIO_BYTES)?;
                let prepared =
                    prepare_local_audio_source(sources.vfs, source, sources.local_file).await?;'''
    z='''                let prepared =
                    prepare_local_audio_source(sources.vfs, source, sources.local_file).await?;
                let mut permit = reserve(&tx, MAX_ENCODED_AUDIO_BYTES)?;'''
    add('file-open-before-input-admission',[(service,a,z)],'source_queue_saturation_precedes_any_filesystem_work','saturated source still started filesystem work')
    return rows

def owned_diagnostics(log,package):
    result=Counter()
    for key,count in wave.diagnostics(log,package).items():
        files=tuple(re.sub(r'^local-deps/(services|io)/',r'crates/\1/',n) for n in key[0])
        if any(n in ['crates/io/src/source_snapshot.rs','crates/services/src/audio.rs','crates/services/src/audio_tests.rs','crates/services/src/audio_local_file.rs','crates/services/src/audio_local_file_tests.rs'] for n in files):result[(files,*key[1:])]+=count
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
        steps += [dict(label='full-'+label,program=row,expected=0),dict(label='local-pcm-'+label,program=row,expected=0,test=NATIVE,dump=True)]
        row=other('services-'+label,'migo-services',release);steps.append(dict(label='services-'+label,program=row,expected=0,selector='audio::tests::'))
    row=other('source-snapshot','migo-io');steps.append(dict(label='source-snapshot',program=row,expected=0,selector='source_snapshot::tests::'))
    paths={'file':package/'local-deps/services/src/audio_local_file.rs','service':package/'local-deps/services/src/audio.rs','io':package/'local-deps/io/src/source_snapshot.rs'};originals={n:p.read_text() for n,p in paths.items()};faults=controls(originals)
    old=(before/'engine/crates/services/src/audio.rs').read_text();a=old.index('            let file =\n                tokio::task::spawn_blocking');z=old.index('\n        }\n        LocalAudioSource::Unconfined',a);branch=old[a:z];assert branch.endswith('Ok(tokio::fs::File::from_std(file))');branch=branch.replace('Ok(tokio::fs::File::from_std(file))','read_capped_local_audio(tokio::fs::File::from_std(file)).await')
    anchor='            file_stream::prepare(vfs, virtual_path, context).await';assert originals['service'].count(anchor)==1
    try:
        paths['service'].write_text(originals['service'].replace(anchor,'            let _ = context;\n'+branch+'.map(LocalAudioPrepared::Data)'));row=native('original-local-reader');steps.append(dict(label='original-local-reader',program=row,expected=101,test=NATIVE,expected_assertion='actual local product did not enter session Fs scheduling',role='original_runtime_red'))
    finally:paths['service'].write_text(originals['service'])
    for fault in faults:
        try:
            for n,a,z in fault['changes']:
                current=paths[n].read_text();assert current.count(a)==1;paths[n].write_text(current.replace(a,z))
            row=other(fault['name'],'migo-services');steps.append({k:v for k,v in fault.items() if k!='changes'}|dict(label=fault['name'],program=row,expected=101,role='fresh_source_fault'));print('Fresh fault prepared',fault['name'],flush=True)
        finally:
            for n,_,_ in fault['changes']:paths[n].write_text(originals[n])
    base.capacity.product.whole.verify_inputs(work,frozen)
    for name in ['audio','shared','io','services','frame-wire']:
        source=ROOT/'engine/crates'/name/'src';private=package/'src' if name=='audio' else package/'local-deps'/name/'src';assert all(p.read_bytes()==(private/p.relative_to(source)).read_bytes() for p in source.rglob('*') if p.is_file()),name
    (work/'execution-plan.json').write_text(json.dumps(dict(programs=programs,steps=steps,clippy=delta,owned_clippy=owned,source_inputs=frozen,private_sources_restored=True),indent=2)+'\n');print('Immutable local File execution plan prepared',flush=True)

def pcm(path):
    rows={}
    for line in path.read_text().splitlines():
        if line.startswith('MIGO_LOCAL_FILE_PCM '):
            _,key,values=line.split(' ',2);assert key not in rows;rows[key]=json.loads(values)
    assert len(rows)==12,(path,len(rows));return rows

def execute(work):
    plan=json.loads((work/'execution-plan.json').read_text());sha=lambda p:hashlib.sha256(p.read_bytes()).hexdigest()
    for n,h in plan['source_inputs'].items():assert sha(ROOT/n)==sha(work/'source-inputs'/n)==h,n
    suites={};faults=[];reds=[]
    for step in plan['steps']:
        p=step['program'];binary=Path(p['path']);assert sha(binary)==p['sha256'];args=[str(binary)]+(['--exact',step['test'],'--nocapture'] if step.get('test') else [step['selector'],'--nocapture'] if step.get('selector') else []);env=os.environ.copy()
        if step.get('dump'):env['MIGO_DUMP_LOCAL_FILE_PCM']='1'
        log=work/(step['label']+'.log')
        with log.open('w') as f:q=subprocess.run(args,cwd=work/'probe',env=env,stdout=f,stderr=subprocess.STDOUT,timeout=900)
        (work/(step['label']+'-exit.txt')).write_text(str(q.returncode)+'\n');text=log.read_text();assert q.returncode==step['expected'],(step['label'],q.returncode)
        if q.returncode:
            assert all(s in text for s in ['running 1 test','panicked at','1 failed',step['test'],step['expected_assertion']]),step['label'];(faults if step['role']=='fresh_source_fault' else reds).append(dict(name=step['label'],test=step['test'],expected_assertion=step['expected_assertion'],exit=101,sha256=p['sha256']))
        else:
            m=re.search(r'test result: ok\. ([1-9][0-9]*) passed; 0 failed; (\d+) ignored;',text);assert m,step['label'];suites[step['label']]=dict(passed=int(m[1]),ignored=int(m[2]))
        print('Executed',step['label'],q.returncode,flush=True)
    a=pcm(work/'local-pcm-debug.log');z=pcm(work/'local-pcm-release.log');assert a==z,'local source full PCM dev/O3 differs'
    (work/'summary.json').write_text(json.dumps(dict(programs=plan['programs'],suites=suites,controls=faults,original_red=reds,clippy=plan['clippy'],owned_clippy=plan['owned_clippy'],pcm=dict(outputs=len(a),samples=sum(map(len,a.values())),all_dev_o3_bits_equal=True),source_inputs_unchanged=True,private_sources_restored=True),indent=2)+'\n');print('Local File product capture verified',flush=True)

if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--output',type=Path,required=True);p.add_argument('--target-dir',type=Path);p.add_argument('--before-source',type=Path);p.add_argument('--before-clippy',type=Path);p.add_argument('--before-services-clippy',type=Path);p.add_argument('--execute',action='store_true');a=p.parse_args()
    if a.execute:execute(a.output.resolve())
    else:
        assert all([a.target_dir,a.before_source,a.before_clippy,a.before_services_clippy]);build(a.output.resolve(),a.target_dir.resolve(),a.before_source.resolve(),a.before_clippy.resolve(),a.before_services_clippy.resolve())
