#!/usr/bin/env python3
"""Frozen pinned HTTP MP3 controls, original reader/CPU, complete PCM and refusals.

No Git. Build offline first, then execute the immutable SHA-bound test programs.
"""
from __future__ import annotations
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import check_audio_file_mp3_controls as prior
ROOT=prior.ROOT
index=prior.prior.index
HTTP='streaming_http::tests::mp3_controls::'
ACTOR='audio_thread::http_spool_tests::actual_mp3_url_actor_reopens_the_same_pinned_source_after_eof'
PUBLIC=HTTP+'public_mp3_seek_restart_and_exact_endpoint_keep_complete_gapless_pcm'
DEADLINE=HTTP+'mp3_control_keeps_one_absolute_deadline_through_the_first_real_media_body'
HELD=HTTP+'mp3_seek_while_original_response_is_held_preserves_the_complete_nonconstant_tail'
sha=prior.sha


def faults(originals):
    rows=[]
    def add(name,key,before,after,test,panic):
        assert originals[key].count(before)==1,(name,key,'anchor drift')
        rows.append(dict(name=name,key=key,before=before,after=after,test=test,expected_assertion=panic))
    add('http-mp3-retention-lost','codecs','            retained_length: Some(length),\n            retain_mp3: true,','            retained_length: Some(length),\n            retain_mp3: false,',HTTP+'mp3_refusals_and_source_ack_preserve_the_old_revision_and_cached_pcm','invalid HTTP MP3 seek mutated the old source')
    add('http-mp3-request-hidden','codecs','Some(Codec::Mp3(d)) => d.source_request().map(HttpRequest::Mp3),','Some(Codec::Mp3(_)) => None,',HELD,'actual controlled file emitted Error')
    add('http-mp3-offset-shifted','resume','HttpRequest::Wave(offset) | HttpRequest::Mp3(offset) => offset,','HttpRequest::Wave(offset) => offset, HttpRequest::Mp3(offset) => offset + 1,',HELD,'actual controlled file emitted Error')
    body='''        let first = tokio::time::timeout_at(deadline, stream.chunk())
            .await
            .map_err(|_| EngineError::new(ErrorCode::Timeout))?
            .map_err(error)?
            .ok_or_else(|| error("HTTP M4A missing media bytes"))?;'''
    add('http-mp3-ack-before-body','http',body,'        let _=stream; let first=Bytes::new();',DEADLINE,'MP3 source acknowledged before its first real media read')
    add('http-mp3-old-suffix-retained','http','            restart: true,','            restart: false,',HELD,'actual controlled file emitted Error')
    add('http-mp3-deadline-renewed','control','self.resolve_request(request, decoder, context.state, deadline),','self.resolve_request(request, decoder, context.state, Instant::now()+RESPONSE_HEADER_TIMEOUT),',DEADLINE,'MP3 control renewed the original body deadline')
    add('http-mp3-fallback-spooled','prepare','    if wave || mp3 {','    if wave {',HTTP+'unreliable_range_mp3_keeps_complete_original_body_and_never_activates_controls','MP3 fallback attempted to spool its original progressive response')
    add('http-mp3-loop-cache-lost','mp3','let cached = self.encoded_origin == 0;','let cached = false;',HTTP+'public_mp3_loops_keep_global_phase_and_reuse_the_original_cached_window','controlled file failed while publishing complete suffix')
    add('http-mp3-cancel-keeps-owner','codecs','''        if cancel.load(Ordering::Acquire) {
            self.codec = None;''','''        if cancel.load(Ordering::Acquire) {
            let _ = &self.codec;''',HTTP+'retained_http_mp3_capacity_is_destroyed_only_by_the_original_cpu','original CPU did not destroy retained HTTP MP3 capacity')
    probe='''reader.read_exact_into(
                        0,
                        &mut probe,
                        deadline.min(Instant::now() + RESPONSE_HEADER_TIMEOUT),
                    )'''
    add('http-mp3-range-probe-skipped','prepare',probe,'async { Ok::<(), range::Error>(()) }',HTTP+'unreliable_range_mp3_keeps_complete_original_body_and_never_activates_controls','unreliable MP3 Range exposed source controls')
    return rows


def capture_native(work,row):
    rows=[]
    for line in (work/(row['name']+'-build.jsonl')).read_text().splitlines():
        try:r=json.loads(line)
        except ValueError:continue
        if r.get('reason')=='build-script-executed' and 'minimp3-sys' in r['package_id']:rows.append(r)
    assert len(rows)==1
    out=Path(rows[0]['out_dir']);dest=work/'as-built-native'/row['name'];dest.mkdir(parents=True);files={}
    for p in out.rglob('*'):
        if p.is_file():
            n=str(p.relative_to(out));q=dest/n;q.parent.mkdir(parents=True,exist_ok=True);shutil.copy2(p,q);files[n]=dict(path=str(q),sha256=sha(q),bytes=q.stat().st_size)
    (dest/'receipt.json').write_text(json.dumps(dict(program=row['name'],program_sha256=row['sha256'],files=files,build_script_artifact=rows[0],captured_before_cpp_fault_builds=True,cpp_faults_this_phase=False),indent=2)+'\n')


def build(work,target,before,baseline):
    os.environ['CARGO_INCREMENTAL']='0';P,E,frozen=index.base.prepare(work,target);c=index.base.capacity.product.whole.support.clock.common
    for n in [str(Path(__file__).relative_to(ROOT)),index.FIXTURE]:
        frozen[n]=sha(ROOT/n);q=work/'source-inputs'/n;q.parent.mkdir(parents=True,exist_ok=True);shutil.copy2(ROOT/n,q)
    q=work/index.FIXTURE;q.parent.mkdir(parents=True,exist_ok=True);shutil.copy2(ROOT/index.FIXTURE,q)
    (work/'runtime-fixture-inputs.json').write_text(json.dumps(dict(role='producer fixture bound before compilation',sha256={index.FIXTURE:frozen[index.FIXTURE]}),indent=2)+'\n');(work/'source-inputs.json').write_text(json.dumps(frozen,indent=2)+'\n')
    out=P/'src/output.rs';s=out.read_text()
    try:
        out.write_text(index.backend.ports(s));assert c.run(['cargo','clippy','--lib','--tests','--offline','--locked','--message-format=json'],work/'clippy.jsonl',P,E,timeout=600)==0
    finally:out.write_text(s)
    a=index.wave.diagnostics(baseline/'clippy.jsonl',baseline/'probe');z=index.wave.diagnostics(work/'clippy.jsonl',P);assert a==z,dict(added=list((z-a).items()),removed=list((a-z).items()))
    delta=dict(before=sum(a.values()),after=sum(z.values()),added=0,removed=0,exact_counter_equal=True);(work/'clippy-delta.json').write_text(json.dumps(delta,indent=2)+'\n')
    programs=[];steps=[]
    def binary(name,release=False):
        p=index.base.capacity.product.src.build(P,work,E,name,release);row=dict(name=name,path=str(p),sha256=sha(p),fresh=False);programs.append(row);return row
    old_plan=json.loads((baseline/'execution-plan.json').read_text())
    for release,label in [(False,'debug'),(True,'release')]:
        row=binary('positive-'+label,release);capture_native(work,row)
        for previous in old_plan['steps']:
            if previous['program']['name']=='positive-'+label:
                step=dict(previous);step['program']=row;steps.append(step)
        steps.append(dict(label='http-mp3-'+label,program=row,expected=0,test=HTTP,exact=False,http_mp3_dump=label))
        steps.append(dict(label='http-mp3-actor-'+label,program=row,expected=0,test=ACTOR))
    paths={k:P/'src'/name for k,name in [('prepare','streaming_http_prepare.rs'),('codecs','streaming_codecs/mod.rs'),('resume','streaming_http_wave.rs'),('http','streaming_http.rs'),('control','streaming_http_control.rs'),('mp3','streaming_mp3.rs')]};originals={k:p.read_text() for k,p in paths.items()}
    old=(before/'engine/crates/audio/src/streaming_http_prepare.rs').read_text();assert old!=originals['prepare']
    try:
        paths['prepare'].write_text(old);row=binary('original-http-mp3-selector');steps.append(dict(label=row['name'],program=row,expected=101,test=PUBLIC,expected_assertion='pinned MP3 URL never activated its real source controls',role='original_runtime_red'))
    finally:paths['prepare'].write_text(originals['prepare'])
    for f in faults(originals):
        try:
            paths[f['key']].write_text(originals[f['key']].replace(f['before'],f['after']));row=binary(f['name']);steps.append(dict(label=row['name'],program=row,expected=101,test=f['test'],expected_assertion=f['expected_assertion'],role='fresh_source_fault'))
        finally:paths[f['key']].write_text(originals[f['key']])
    row=binary('restored-debug');steps.append(dict(label=row['name'],program=row,expected=0,test=HTTP,exact=False))
    index.base.capacity.product.whole.verify_inputs(work,frozen);assert all(p.read_text()==originals[k] for k,p in paths.items())
    (work/'execution-plan.json').write_text(json.dumps(dict(programs=programs,steps=steps,clippy=delta,source_inputs=frozen,private_sources_restored=True),indent=2)+'\n');print('Pinned HTTP MP3 plan built',len(programs),len(steps),flush=True)


def execute(work,baseline):
    plan=json.loads((work/'execution-plan.json').read_text());suites={};negatives=[]
    for name,h in plan['source_inputs'].items():assert sha(ROOT/name)==sha(work/'source-inputs'/name)==h,name
    for step in plan['steps']:
        binary=Path(step['program']['path']);assert sha(binary)==step['program']['sha256']
        env=os.environ.copy();env['RUST_TEST_THREADS']='1'
        for name in ['HTTP_WAVE','HTTP_SPOOL','HTTP_M4A','HTTP_CONTROL','RANGE_CONTROL','WAVE_CONTROL']:env['MIGO_DUMP_'+name+'_PCM']='1'
        if step.get('dump'):env[dict(control='MIGO_DUMP_FILE_CONTROL_PCM',origin='MIGO_DUMP_SINC_ORIGIN_PCM',local='MIGO_DUMP_LOCAL_FILE_PCM',file='MIGO_DUMP_FILE_STREAM_PCM',indexed='MIGO_DUMP_INDEXED_FILE_PCM',package='MIGO_DUMP_PACKAGE_FILE_PCM')[step['dump']]]='1'
        for key,folder,var in [('controls_dump','control-pcm','MIGO_MP3_CONTROL_PCM_DIR'),('mp3_dump','mp3-pcm','MIGO_MP3_GAPLESS_PCM_DIR'),('http_mp3_dump','http-mp3-pcm','MIGO_HTTP_MP3_PCM_DIR')]:
            if step.get(key):
                directory=work/folder/step[key];directory.mkdir(parents=True,exist_ok=True);env[var]=str(directory)
        cmd=[str(binary)]
        if step.get('test'):cmd+=(['--exact'] if step.get('exact',True) else [])+[step['test'],'--nocapture']
        path=work/(step['label']+'.log')
        with path.open('w') as log:r=subprocess.run(cmd,cwd=work/'probe',env=env,stdout=log,stderr=subprocess.STDOUT,timeout=900)
        (work/(step['label']+'-exit.txt')).write_text(str(r.returncode)+'\n');content=path.read_text()
        assert r.returncode==step['expected'],(step['label'],r.returncode)
        if r.returncode:
            assert all(n in content for n in ['running 1 test','1 failed','panicked at',step['test'],step['expected_assertion']]),step['label']
            negatives.append(dict(name=step['label'],test=step['test'],panic=step['expected_assertion'],exit=101,role=step['role'],sha256=step['program']['sha256']))
        else:
            match=re.search(r'test result: ok\. ([1-9][0-9]*) passed; 0 failed; (\d+) ignored;',content);assert match,step['label']
            suites[step['label']]=dict(passed=int(match[1]),ignored=int(match[2]))
        print('Executed',step['label'],r.returncode,flush=True)
    assert suites['full-debug']==suites['full-release']==dict(passed=967,ignored=3)
    pcm={}
    for folder,count in [('control-pcm',216),('mp3-pcm',116)]:
        a={p.name:p.read_bytes() for p in (work/folder/'debug').glob('*.f32le')};b={p.name:p.read_bytes() for p in (work/folder/'release').glob('*.f32le')}
        assert len(a)==count and a==b,(folder,len(a),len(b))
        assert a=={p.name:p.read_bytes() for p in (baseline/folder/'debug').glob('*.f32le')},(folder,'previous complete PCM changed')
        pcm[folder]=dict(outputs=len(a),samples=sum(len(v)//4 for v in a.values()),all_dev_o3_bits_equal=True,previous_bits_equal=True)
    for label,prefix,count in [('control-pcm','MIGO_FILE_CONTROL_PCM ',131),('origin-pcm','MIGO_SINC_ORIGIN_PCM ',36),('local-pcm','MIGO_LOCAL_FILE_PCM ',12),('file-pcm','MIGO_FILE_STREAM_PCM ',8),('indexed-pcm','MIGO_INDEXED_FILE_PCM ',40),('indexed-large','MIGO_INDEXED_FILE_PCM ',10),('package-small','MIGO_PACKAGE_FILE_PCM ',12),('package-large','MIGO_PACKAGE_INDEXED_PCM ',10),('http-pcm','MIGO_HTTP_M4A_PCM ',40),('spool-pcm','MIGO_HTTP_SPOOL_PCM ',80),('http-control-pcm','MIGO_HTTP_CONTROL_PCM ',234),('range-control-pcm','MIGO_RANGE_CONTROL_PCM ',116),('wave-control-pcm','MIGO_WAVE_CONTROL_PCM ',131),('http-wave-pcm','MIGO_HTTP_WAVE_PCM ',138),('http-control-pcm','MIGO_HTTP_ORDINARY_PCM ',8)]:
        a=index.pcm(work/(label+'-debug.log'),prefix,count);b=index.pcm(work/(label+'-release.log'),prefix,count)
        assert a==b==index.pcm(baseline/(label+'-debug.log'),prefix,count),(label,'previous complete PCM changed')
        pcm[prefix.strip()]=dict(outputs=len(a),samples=sum(map(len,a.values())),all_dev_o3_bits_equal=True,previous_bits_equal=True)
    for profile in ['debug','release']:
        text=(work/('mp3-file-controls-'+profile+'.log')).read_text();assert 'MIGO_LARGE_MP3_CONTROL samples=16777728 encoded=3037011 complete_zero_pcm_checked=true reclaimed_then_source_replay=true' in text
    assert sum(r['role']=='fresh_source_fault' for r in negatives)==10 and sum(r['role']=='original_runtime_red' for r in negatives)==1
    a={p.name:p.read_bytes() for p in (work/'http-mp3-pcm/debug').glob('*.f32le')};b={p.name:p.read_bytes() for p in (work/'http-mp3-pcm/release').glob('*.f32le')}
    assert len(a)==105 and a==b,('HTTP MP3 full PCM',len(a),len(b))
    canonical={k:v for k,v in a.items() if k.startswith('file-')};assert len(canonical)==84
    old={p.name:p.read_bytes() for p in (baseline/'control-pcm/debug').glob('*.f32le')};assert all(v==old[k] for k,v in canonical.items())
    pcm['http-mp3']=dict(outputs=len(a),samples=sum(len(v)//4 for v in a.values()),canonical_file_outputs=84,canonical_file_bits_equal=True,all_dev_o3_bits_equal=True,complete_files_reread=True)
    for profile in ['debug','release']:
        assert 'MIGO_LARGE_HTTP_MP3_CONTROL samples=16777728 encoded=3037011 complete_zero_pcm_checked=true original_held_before_eof=true reclaimed_then_source_replay=true' in (work/('http-mp3-'+profile+'.log')).read_text()
    result=dict(programs=plan['programs'],suites=suites,negatives=negatives,clippy=plan['clippy'],pcm=pcm,large_mp3_every_sample_and_tail_checked=True,source_replay_complexity='Pinned HTTP O(track) with same Reader, no index or speed claim',source_inputs_unchanged=True)
    (work/'summary.json').write_text(json.dumps(result,indent=2)+'\n');print('Pinned HTTP MP3 controls verified',flush=True)


def main():
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--phase',choices=['build','execute'],required=True);p.add_argument('--work',type=Path,required=True);p.add_argument('--target',type=Path);p.add_argument('--before',type=Path);p.add_argument('--baseline',type=Path,required=True);a=p.parse_args();work=a.work.resolve()
    if a.phase=='build':
        assert a.target and a.before;build(work,a.target,a.before,a.baseline.resolve())
    else:execute(work,a.baseline.resolve())
if __name__=='__main__':main()
