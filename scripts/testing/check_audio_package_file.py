#!/usr/bin/env python3
"""Verify pinned package capture through the existing File/CPU/player pipeline.

Build offline without Git; execute only SHA-bound retained test programs.
Complete PCM, original physical admission and fresh executed faults are required.
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
import check_audio_file_index as index

ROOT=index.ROOT
IO='source_snapshot::package::tests::'
SERVICE='audio::tests::package_file::'
SMALL='audio_thread::decode_pool::tests::package_file_service_reaches_the_actual_decode_pool_with_complete_pcm_and_identity'
LARGE=index.FILE_TEST+'indexed::actual_large_package_m4a_src_reaches_native_player_with_complete_pcm_and_identity'

def controls(originals):
    rows=[]
    def add(name,key,a,z,test,panic,crate='migo-io'):
        assert a!=z and originals[key].count(a)==1,(name,'anchor drift')
        rows.append(dict(name=name,key=key,before=a,after=z,test=test,expected_assertion=panic,crate=crate))
    limit=IO+'length_and_memory_limits_precede_reader_allocation_and_never_fall_back'
    add('package-length-after-allocation','package','if length > max_bytes {','if false {',SERVICE+'oversized_package_keeps_the_typed_source_limit_before_any_file_command','left: \"AudioFileError\"','migo-services')
    add('package-scratch-admission-bypassed','package','.reserve_bytes(limits.max_scratch_bytes()? as u64)','.reserve_bytes(0)',limit,'package reader allocated before scratch/media admission')
    add('package-media-admission-bypassed','package','.reserve_bytes(MEMORY_BYTES)','.reserve_bytes(0)',limit,'package reader allocated before scratch/media admission')
    a='''                reader
                    .copy_to(&mut Output {'''
    start=originals['package'].index(a);end=originals['package'].index('                check_deadline(Some(deadline))?;',start)
    body=originals['package'][start:end]
    add('package-crc-error-ignored','package',body,body.replace('                reader','                let _ = reader',1).replace('.map_err(io::Error::other)?;','.map_err(io::Error::other);'),IO+'bad_whole_entry_crc_never_publishes_a_sealed_source','bad package CRC published a sealed source')
    output=IO+'every_package_output_block_checks_cancellation_and_the_same_deadline'
    add('package-block-cancel-ignored','package','check(self.scheduler, self.cancelled).map_err(io::Error::other)?;','let _ = (self.scheduler, self.cancelled);',output,'cancelled package wrote a payload block')
    add('package-interrupted-cancel-retried','package','check(self.scheduler, self.cancelled).map_err(io::Error::other)?;','check(self.scheduler, self.cancelled)?;',output,'cancelled package retried until deadline')
    add('package-block-deadline-ignored','package','check_deadline(Some(self.deadline))?;','let _ = self.deadline;',output,'expired package wrote a payload block')
    a='''                check_deadline(Some(deadline))?;
                check(&worker, &cancelled)?;
                // Destroy both'''
    add('package-final-deadline-ignored','package',a,a.replace('check_deadline(Some(deadline))?;','let _ = deadline;'),IO+'an_empty_package_still_checks_the_original_deadline_after_the_running_open','expired running package published a sealed source')
    add('package-mount-selection-omitted','capture','context\n                .mounts\n                .as_ref()', 'None::<&Arc<shared::vfs::MountTable>>',SERVICE+'small_package_keeps_exact_encoded_bytes_and_the_original_whole_file_profile','package source selected stale VFS bytes','migo-services')
    return rows

def build(work,target,before,before_native,before_services,before_io):
    os.environ['CARGO_INCREMENTAL']='0'
    base=index.base;package,env,frozen=base.prepare(work,target);common=base.capacity.product.whole.support.clock.common
    for n in [index.FIXTURE,str(Path(__file__).relative_to(ROOT))]:
        frozen[n]=common.sha(ROOT/n);p=work/'source-inputs'/n;p.parent.mkdir(parents=True,exist_ok=True);shutil.copy2(ROOT/n,p)
    p=work/index.FIXTURE;p.parent.mkdir(parents=True,exist_ok=True);shutil.copy2(ROOT/index.FIXTURE,p)
    (work/'source-inputs.json').write_text(json.dumps(frozen,indent=2)+'\n')
    output=package/'src/output.rs';saved=output.read_text()
    try:
        output.write_text(index.backend.ports(saved));assert common.run(['cargo','clippy','--lib','--tests','--offline','--locked','--message-format=json'],work/'clippy.jsonl',package,env,timeout=600)==0
    finally:output.write_text(saved)
    diagnostics={}
    for label,old,new,root in [('native',before_native,work/'clippy.jsonl',before_native.parent/'probe')]:
        a=index.wave.diagnostics(old,root);z=index.wave.diagnostics(new,package);assert not(z-a),(label,z-a)
        diagnostics[label]=dict(before=sum(a.values()),after=sum(z.values()),new_diagnostics=0,raw_removed=[dict(key=k,count=v) for k,v in (a-z).items()])
    for crate,label,old in [('migo-services','services',before_services),('migo-io','io',before_io)]:
        flags=['--no-default-features','--features','zip-extract'] if crate=='migo-io' else []
        assert common.run(['cargo','clippy','-p',crate,*flags,'--lib','--tests','--offline','--locked','--message-format=json'],work/(label+'-clippy.jsonl'),package,env,timeout=600)==0
        a=index.wave.diagnostics(old,before_io.parent/'probe' if label=='io' else ROOT/'engine');z=index.wave.diagnostics(work/(label+'-clippy.jsonl'),package)
        def owned(counter):
            result=Counter()
            for key,value in counter.items():
                files=tuple(re.sub(r'^local-deps/(services|io|shared)/',r'crates/\1/',n) for n in key[0])
                if any(n.startswith(('crates/io/src/source_snapshot','crates/services/src/audio')) for n in files):result[(files,*key[1:])]+=value
            return result
        a,z=owned(a),owned(z);assert not(z-a),(label,z-a)
        diagnostics[label]=dict(before=sum(a.values()),after=sum(z.values()),new_diagnostics=0,removed=[dict(key=k,count=v) for k,v in (a-z).items()])
    (work/'clippy-delta.json').write_text(json.dumps(diagnostics,indent=2)+'\n');print('Stable Clippy has no new diagnostics',diagnostics,flush=True)
    programs=[];steps=[]
    def binary(name,crate='migo-audio',release=False):
        if crate=='migo-audio':p=base.capacity.product.src.build(package,work,env,name,release)
        else:
            folder='io' if crate=='migo-io' else 'services';(package/'local-deps'/folder/'src/lib.rs').touch();cmd=['cargo','test','-p',crate,'--lib','--offline','--locked','--no-run','--message-format=json']
            if crate=='migo-io':cmd+=['--no-default-features','--features','zip-extract']
            if release:cmd+=['--release']
            log=work/(name+'-build.jsonl');assert common.run(cmd,log,package,env,timeout=900)==0
            artifacts=[]
            for line in log.read_text().splitlines():
                try:x=json.loads(line)
                except ValueError:continue
                if x.get('reason')=='compiler-artifact' and x.get('executable') and x.get('target',{}).get('name')==crate.replace('-','_'):artifacts.append(x)
            assert len(artifacts)==1 and artifacts[0]['fresh'] is False
            if release:assert artifacts[0]['profile']['opt_level']=='3'
            p=work/(name+'-tests');shutil.copy2(artifacts[0]['executable'],p)
        row=dict(name=name,path=str(p),sha256=common.sha(p),fresh=False);programs.append(row);return row
    for release,label in [(False,'debug'),(True,'release')]:
        row=binary('positive-'+label,release=release);steps.append(dict(label='full-'+label,program=row,expected=0))
        for tag,test,dump in [('local-pcm',index.NATIVE,'local'),('file-pcm',index.FULL,'file'),('indexed-pcm',index.INDEX_FULL,'indexed'),('indexed-large',index.INDEX_LARGE,'indexed'),('package-small',SMALL,'package'),('package-large',LARGE,'package')]:steps.append(dict(label=tag+'-'+label,program=row,expected=0,test=test,dump=dump))
        steps.append(dict(label='large-file-'+label,program=row,expected=0,test=index.LARGE))
        for crate,tag,selector in [('migo-services','services','audio::tests::'),('migo-io','source-snapshot','source_snapshot::')]:steps.append(dict(label=tag+'-'+label,program=binary(tag+'-'+label,crate,release),expected=0,selector=selector))
    paths={'package':package/'local-deps/io/src/source_snapshot/package.rs','capture':package/'local-deps/services/src/audio_local_file.rs'};originals={k:p.read_text() for k,p in paths.items()}
    faults=controls(originals)
    try:
        # Actual ordinary-file original capture, preserving new API callers.
        paths['capture'].write_bytes((before/'engine/crates/services/src/audio_local_file.rs').read_bytes());row=binary('original-package-vfs-refusal','migo-services');steps.append(dict(label='original-package-vfs-refusal',program=row,expected=101,test=SERVICE+'small_package_keeps_exact_encoded_bytes_and_the_original_whole_file_profile',expected_assertion='package source selected stale VFS bytes',role='original_runtime_red'))
    finally:paths['capture'].write_text(originals['capture'])
    for fault in faults:
        p=paths[fault['key']]
        try:
            p.write_text(originals[fault['key']].replace(fault['before'],fault['after']));row=binary(fault['name'],fault['crate']);steps.append(dict(label=fault['name'],program=row,expected=101,test=fault['test'],expected_assertion=fault['expected_assertion'],role='fresh_source_fault'));print('Fresh fault prepared',fault['name'],flush=True)
        finally:p.write_text(originals[fault['key']])
    base.capacity.product.whole.verify_inputs(work,frozen)
    for name in ['audio','shared','io','services','frame-wire']:
        src=ROOT/'engine/crates'/name/'src';private=package/'src' if name=='audio' else package/'local-deps'/name/'src'
        assert all(p.read_bytes()==(private/p.relative_to(src)).read_bytes() for p in src.rglob('*') if p.is_file()),name
    (work/'execution-plan.json').write_text(json.dumps(dict(programs=programs,steps=steps,clippy=diagnostics,source_inputs=frozen,private_sources_restored=True),indent=2)+'\n');print('Immutable package execution plan prepared',flush=True)

def execute(work):
    plan=json.loads((work/'execution-plan.json').read_text());sha=lambda p:hashlib.sha256(p.read_bytes()).hexdigest();env=os.environ.copy();env['RUST_TEST_THREADS']='1'
    for n,h in plan['source_inputs'].items():assert sha(ROOT/n)==sha(work/'source-inputs'/n)==h,n
    suites={};faults=[];reds=[]
    for step in plan['steps']:
        p=step['program'];binary=Path(p['path']);assert sha(binary)==p['sha256'];cmd=[str(binary)]+(['--exact',step['test'],'--nocapture'] if step.get('test') else [step['selector'],'--nocapture'] if step.get('selector') else [])
        e=env.copy()
        if step.get('dump'):e[dict(local='MIGO_DUMP_LOCAL_FILE_PCM',file='MIGO_DUMP_FILE_STREAM_PCM',indexed='MIGO_DUMP_INDEXED_FILE_PCM',package='MIGO_DUMP_PACKAGE_FILE_PCM')[step['dump']]]='1'
        with (work/(step['label']+'.log')).open('w') as log:q=subprocess.run(cmd,cwd=work/'probe',env=e,stdout=log,stderr=subprocess.STDOUT,timeout=900)
        (work/(step['label']+'-exit.txt')).write_text(str(q.returncode)+'\n');text=(work/(step['label']+'.log')).read_text();assert q.returncode==step['expected'],(step['label'],q.returncode)
        if q.returncode:
            assert all(s in text for s in ['running 1 test','panicked at','1 failed',step['test'],step['expected_assertion']]),step['label'];(faults if step['role']=='fresh_source_fault' else reds).append(dict(name=step['label'],test=step['test'],expected_assertion=step['expected_assertion'],exit=101,sha256=p['sha256']))
        else:
            m=re.search(r'test result: ok\. ([1-9][0-9]*) passed; 0 failed; (\d+) ignored;',text);assert m,step['label'];suites[step['label']]=dict(passed=int(m[1]),ignored=int(m[2]))
        print('Executed',step['label'],q.returncode,flush=True)
    pcm={}
    for label,prefix,count in [('local-pcm','MIGO_LOCAL_FILE_PCM ',12),('file-pcm','MIGO_FILE_STREAM_PCM ',8),('indexed-pcm','MIGO_INDEXED_FILE_PCM ',40),('indexed-large','MIGO_INDEXED_FILE_PCM ',10),('package-small','MIGO_PACKAGE_FILE_PCM ',12),('package-large','MIGO_PACKAGE_INDEXED_PCM ',10)]:
        a=index.pcm(work/(label+'-debug.log'),prefix,count);z=index.pcm(work/(label+'-release.log'),prefix,count);assert a==z,(label,'complete dev/O3 PCM differs');pcm[label]=dict(outputs=len(a),samples=sum(map(len,a.values())),all_dev_o3_bits_equal=True)
    large={}
    for label in ['debug','release']:
        m=re.search(r'MIGO_LARGE_FILE_PCM frames=(\d+) bits=(\d+) bounded_pcm_bytes=(\d+)',(work/('large-file-'+label+'.log')).read_text());assert m and int(m[1])==16777345 and int(m[2])==1048576000 and int(m[3])<4*1024*1024;large[label]=dict(samples=int(m[1]),bits=int(m[2]),retained_pcm_bytes=int(m[3]),every_sample_checked=True,tail_and_end_checked=True)
    (work/'summary.json').write_text(json.dumps(dict(programs=plan['programs'],suites=suites,controls=faults,original_red=reds,clippy=plan['clippy'],pcm=pcm,large_file=large,source_inputs_unchanged=True,private_sources_restored=True),indent=2)+'\n');print('Actual package audio source verified',flush=True)

if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--output',type=Path,required=True);p.add_argument('--target-dir',type=Path);p.add_argument('--before-source',type=Path);p.add_argument('--before-clippy',type=Path);p.add_argument('--before-services-clippy',type=Path);p.add_argument('--before-io-clippy',type=Path);p.add_argument('--execute',action='store_true');a=p.parse_args()
    if a.execute:execute(a.output.resolve())
    else:
        assert all([a.target_dir,a.before_source,a.before_clippy,a.before_services_clippy,a.before_io_clippy]);build(a.output.resolve(),a.target_dir.resolve(),a.before_source.resolve(),a.before_clippy.resolve(),a.before_services_clippy.resolve(),a.before_io_clippy.resolve())
