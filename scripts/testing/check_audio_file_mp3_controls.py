#!/usr/bin/env python3
"""Frozen sealed-File MP3 replay, public controls, complete PCM and CPU ownership.

Uses the existing native codec, transport and verification helpers. No Git access.
Builds and execution are separate so the same SHA-bound ELF can run with loopback.
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
import check_audio_streaming_mp3_gapless as prior

ROOT = prior.ROOT
DIRECT = 'streaming_mp3::control::tests::'
FILE = 'streaming_file::tests::mp3_controls::'
NATIVE = 'streaming_mp3_native::retained_replay_native_owner_is_replaced_and_freed_on_the_original_cpu'
sha = prior.index.base.capacity.product.whole.support.clock.common.sha


def faults(originals):
    rows = []
    def add(name, key, before, after, test, panic):
        assert originals[key].count(before) == 1, (name, 'anchor drift')
        rows.append(dict(name=name, key=key, before=before, after=after, test=test, expected_assertion=panic))
    seek = DIRECT+'retained_replay_seeks_and_exact_endpoint_match_every_complete_frozen_sample'
    loop = DIRECT+'repeated_mp3_cutovers_preserve_global_filter_history_and_complete_tail'
    capacity = DIRECT+'retained_cached_capacities_reuse_the_same_allocations_and_free_only_on_the_original_cpu'
    public = FILE+'public_mp3_seek_restart_and_exact_endpoint_keep_complete_gapless_pcm'
    add('replay-source-request-lost', 'control', 'owner.source_pending = !terminal && !cached;', 'owner.source_pending = false;', seek, 'uncached MP3 replay must request the original source offset zero')
    add('replay-original-cache-discarded', 'control', 'let cached = owner.validated && owner.eof && owner.encoded_origin == 0;', 'let cached = false;', capacity, 'cached MP3 unnecessarily reopened its source')
    add('replay-canonical-prefix-lost', 'control', '(0, start, (!enabled).then_some(1), p)', '(frame, start, (!enabled).then_some(1), p)', seek, 'retained MP3 complete suffix differs from frozen grid')
    add('replay-loop-stopped-after-one-cycle', 'control', '        self.cycles\n', '        false && self.cycles\n', loop, 'retained MP3 failed its complete replay presentation')
    anchor='        let history = owner.history.ok_or_else(unavailable)?;'
    add('replay-refusal-destroys-input', 'control', anchor, anchor+'\n        owner.buffer.clear();', DIRECT+'invalid_replay_requests_keep_encoded_input_pcm_clock_and_original_owners', 'rejected MP3 replay changed old state or allocations')
    add('replay-id3-loan-abandoned', 'stream', 'if progress == Progress::NeedInput && consumed < input.len() {', 'if false && progress == Progress::NeedInput && consumed < input.len() {', public, 'controlled file failed while publishing complete suffix')
    add('replay-cancel-keeps-actual-capacity', 'stream', '            self.retire(ErrorCode::Cancelled);', '            self.error = Some(ErrorCode::Cancelled);', capacity, 'original CPU did not really free retained MP3 capacity')
    add('replay-loop-resets-src', 'stream', '                if self.pipeline.is_none() {', '                if self.window.is_some() || self.pipeline.is_none() {', loop, 'MP3 loop cutover differs from the complete global grid')
    # Both CPU checks are necessary even when cached replay performs no read.
    before='if file.is_closed() {'
    assert originals['file'].count(before)==2
    rows.append(dict(name='replay-cached-capability-unchecked', key='file', before=before, after='if false && file.is_closed() {', test=FILE+'cached_mp3_cpu_step_revalidates_the_same_pinned_file_without_a_read', expected_assertion='cached MP3 CPU replay ignored its revoked pinned file'))
    return rows


def build(work, target, before, baseline):
    os.environ['CARGO_INCREMENTAL']='0'
    base=prior.index.base
    P,E,frozen=base.prepare(work,target)
    c=base.capacity.product.whole.support.clock.common
    for name in [str(Path(__file__).relative_to(ROOT)), 'scripts/testing/fixtures/audio-codec-probe/streaming_mp3_native.rs', prior.index.FIXTURE]:
        frozen[name]=sha(ROOT/name)
        q=work/'source-inputs'/name;q.parent.mkdir(parents=True,exist_ok=True);shutil.copy2(ROOT/name,q)
    q=work/prior.index.FIXTURE;q.parent.mkdir(parents=True,exist_ok=True);shutil.copy2(ROOT/prior.index.FIXTURE,q)
    (work/'runtime-fixture-inputs.json').write_text(json.dumps(dict(role='producer fixture bound before compilation',sha256={prior.index.FIXTURE:frozen[prior.index.FIXTURE]}),indent=2)+'\n')
    (work/'source-inputs.json').write_text(json.dumps(frozen,indent=2)+'\n')
    out=P/'src/output.rs';original=out.read_text()
    try:
        out.write_text(prior.index.backend.ports(original))
        assert c.run(['cargo','clippy','--lib','--tests','--offline','--locked','--message-format=json'],work/'clippy.jsonl',P,E,timeout=600)==0
    finally:out.write_text(original)
    a=prior.index.wave.diagnostics(baseline/'clippy.jsonl',baseline/'probe');z=prior.index.wave.diagnostics(work/'clippy.jsonl',P)
    assert a==z,dict(added=list((z-a).items()),removed=list((a-z).items()))
    delta=dict(before=sum(a.values()),after=sum(z.values()),added=0,removed=0,exact_counter_equal=True)
    (work/'clippy-delta.json').write_text(json.dumps(delta,indent=2)+'\n')
    programs=[];steps=[]
    def binary(name,release=False,env=None,native=False):
        path=base.capacity.product.src.build(P,work,E if env is None else env,name,release)
        if native:
            artifacts=[]
            for line in (work/(name+'-build.jsonl')).read_text().splitlines():
                try:r=json.loads(line)
                except ValueError:continue
                if r.get('reason')=='compiler-artifact' and r.get('target',{}).get('name')=='minimp3_sys':artifacts.append(r)
            assert len(artifacts)==1 and artifacts[0]['fresh'] is False,(name,'native did not really rebuild')
        row=dict(name=name,path=str(path),sha256=sha(path),fresh=False);programs.append(row);return row
    for release,label in [(False,'debug'),(True,'release')]:
        row=binary('positive-'+label,release)
        # Preserve the actual C object/archive before instrumented C faults.
        artifacts=[]
        for line in (work/(row['name']+'-build.jsonl')).read_text().splitlines():
            try:r=json.loads(line)
            except ValueError:continue
            if r.get('reason')=='build-script-executed' and 'minimp3-sys' in r['package_id']:artifacts.append(r)
        assert len(artifacts)==1
        out=Path(artifacts[0]['out_dir']);dest=work/'as-built-native'/row['name'];dest.mkdir(parents=True);files={}
        for p in out.rglob('*'):
            if p.is_file():
                n=str(p.relative_to(out));q=dest/n;q.parent.mkdir(parents=True,exist_ok=True);shutil.copy2(p,q);files[n]=dict(path=str(q),sha256=sha(q),bytes=q.stat().st_size)
        (dest/'receipt.json').write_text(json.dumps(dict(program_sha256=row['sha256'],program=row['name'],build_script_artifact=artifacts[0],files=files,captured_before_cpp_fault_builds=True),indent=2)+'\n')
        steps.append(dict(label='full-'+label,program=row,expected=0))
        for name,test in [('mp3-controls',DIRECT),('mp3-file-controls',FILE)]:
            steps.append(dict(label=name+'-'+label,program=row,expected=0,test=test,exact=False,controls_dump=label))
        for name,test in [('mp3-gapless',prior.PREFIX),('mp3-http',prior.HTTP)]:
            steps.append(dict(label=name+'-'+label,program=row,expected=0,test=test,exact=False,mp3_dump=label))
        for name,test,exact in [('http-pcm',prior.prior.prior.PCM,True),('spool-pcm',prior.prior.prior.SPOOL_PCM,True),('http-control-pcm',prior.prior.prior.CONTROL,False),('range-control-pcm',prior.prior.prior.RANGE,False),('wave-control-pcm',prior.prior.prior.WAVE,False),('http-wave-pcm',prior.prior.WAVE,False)]:
            steps.append(dict(label=name+'-'+label,program=row,expected=0,test=test,exact=exact))
        for name,test,dump in [('control-pcm',prior.prior.prior.prior.prior.prior.prior.PREFIX,'control'),('origin-pcm',prior.prior.prior.prior.prior.prior.prior.prior.WINDOW,'origin'),('local-pcm',prior.index.NATIVE,'local'),('file-pcm',prior.index.FULL,'file'),('indexed-pcm',prior.index.INDEX_FULL,'indexed'),('indexed-large',prior.index.INDEX_LARGE,'indexed'),('package-small',prior.prior.prior.prior.prior.prior.prior.prior.product.SMALL,'package'),('package-large',prior.prior.prior.prior.prior.prior.prior.prior.product.LARGE,'package')]:
            steps.append(dict(label=name+'-'+label,program=row,expected=0,test=test,dump=dump,exact=name!='control-pcm'))
        steps.append(dict(label='large-file-'+label,program=row,expected=0,test=prior.index.LARGE))
    paths=dict(control=P/'src/streaming_mp3_control.rs',stream=P/'src/streaming_mp3.rs',file=P/'src/streaming_file.rs',actor=P/'src/audio_thread.rs',native=work/'native/minimp3-sys-0.6.0/minimp3.c')
    originals={k:p.read_text() for k,p in paths.items()}
    old=(before/'engine/crates/audio/src/audio_thread.rs').read_text();assert old!=originals['actor']
    try:
        paths['actor'].write_text(old);row=binary('original-file-actor')
        steps.append(dict(label=row['name'],program=row,expected=101,test=FILE+'public_mp3_seek_restart_and_exact_endpoint_keep_complete_gapless_pcm',expected_assertion='actual MP3 actor did not activate source controls',role='original_runtime_red'))
    finally:paths['actor'].write_text(originals['actor'])
    for fault in faults(originals):
        try:
            paths[fault['key']].write_text(originals[fault['key']].replace(fault['before'],fault['after']))
            row=binary(fault['name']);steps.append(dict(label=row['name'],program=row,expected=101,test=fault['test'],expected_assertion=fault['expected_assertion'],role='fresh_source_fault'))
        finally:paths[fault['key']].write_text(originals[fault['key']])
    lib=P/'src/lib.rs';source=lib.read_text();private=work/'source/streaming_mp3_native.rs';private.parent.mkdir(exist_ok=True);shutil.copy2(ROOT/'scripts/testing/fixtures/audio-codec-probe/streaming_mp3_native.rs',private)
    instrument=E.copy();instrument.update(CFLAGS='-DMIGO_MINIMP3_TEST',MIGO_MP3_TEST_FIXTURES=str(P/'src/decoder/mp3/fixtures'))
    try:
        lib.write_text(source+'\n#[cfg(test)] #[path='+json.dumps(str(private))+'] mod streaming_mp3_native;\n')
        row=binary('retained-native-positive',env=instrument,native=True);steps.append(dict(label=row['name'],program=row,expected=0,test=NATIVE))
        anchor='void migo_mp3_presentation_free(migo_mp3_presentation *p) { free(p); }';assert originals['native'].count(anchor)==1
        paths['native'].write_text(originals['native'].replace(anchor,'void migo_mp3_presentation_free(migo_mp3_presentation *p) { (void)p; }'))
        row=binary('retained-native-real-free-lost',env=instrument,native=True);steps.append(dict(label=row['name'],program=row,expected=101,test=NATIVE,expected_assertion='original CPU did not free replaced native MP3 presentation',role='fresh_source_fault'))
    finally:lib.write_text(source);paths['native'].write_text(originals['native'])
    row=binary('restored-debug',native=True)
    steps.append(dict(label=row['name'],program=row,expected=0,test=DIRECT,exact=False))
    base.capacity.product.whole.verify_inputs(work,frozen)
    assert all(p.read_text()==originals[k] for k,p in paths.items())
    (work/'execution-plan.json').write_text(json.dumps(dict(programs=programs,steps=steps,clippy=delta,source_inputs=frozen,private_sources_restored=True),indent=2)+'\n')
    print('Frozen File MP3 plan built',len(programs),len(steps),flush=True)


def execute(work,baseline):
    plan=json.loads((work/'execution-plan.json').read_text());suites={};negatives=[]
    for name,h in plan['source_inputs'].items():assert sha(ROOT/name)==sha(work/'source-inputs'/name)==h,name
    for step in plan['steps']:
        binary=Path(step['program']['path']);assert sha(binary)==step['program']['sha256']
        env=os.environ.copy();env['RUST_TEST_THREADS']='1'
        for name in ['HTTP_WAVE','HTTP_SPOOL','HTTP_M4A','HTTP_CONTROL','RANGE_CONTROL','WAVE_CONTROL']:env['MIGO_DUMP_'+name+'_PCM']='1'
        if step.get('dump'):env[dict(control='MIGO_DUMP_FILE_CONTROL_PCM',origin='MIGO_DUMP_SINC_ORIGIN_PCM',local='MIGO_DUMP_LOCAL_FILE_PCM',file='MIGO_DUMP_FILE_STREAM_PCM',indexed='MIGO_DUMP_INDEXED_FILE_PCM',package='MIGO_DUMP_PACKAGE_FILE_PCM')[step['dump']]]='1'
        for key,folder,var in [('controls_dump','control-pcm','MIGO_MP3_CONTROL_PCM_DIR'),('mp3_dump','mp3-pcm','MIGO_MP3_GAPLESS_PCM_DIR')]:
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
    assert suites['full-debug']==suites['full-release']==dict(passed=954,ignored=3)
    pcm={}
    for folder,count in [('control-pcm',216),('mp3-pcm',116)]:
        a={p.name:p.read_bytes() for p in (work/folder/'debug').glob('*.f32le')};b={p.name:p.read_bytes() for p in (work/folder/'release').glob('*.f32le')}
        assert len(a)==count and a==b,(folder,len(a),len(b))
        if folder=='mp3-pcm':assert a=={p.name:p.read_bytes() for p in (baseline/folder/'debug').glob('*.f32le')}
        pcm[folder]=dict(outputs=len(a),samples=sum(len(v)//4 for v in a.values()),all_dev_o3_bits_equal=True,previous_bits_equal=folder=='mp3-pcm')
    for label,prefix,count in [('control-pcm','MIGO_FILE_CONTROL_PCM ',131),('origin-pcm','MIGO_SINC_ORIGIN_PCM ',36),('local-pcm','MIGO_LOCAL_FILE_PCM ',12),('file-pcm','MIGO_FILE_STREAM_PCM ',8),('indexed-pcm','MIGO_INDEXED_FILE_PCM ',40),('indexed-large','MIGO_INDEXED_FILE_PCM ',10),('package-small','MIGO_PACKAGE_FILE_PCM ',12),('package-large','MIGO_PACKAGE_INDEXED_PCM ',10),('http-pcm','MIGO_HTTP_M4A_PCM ',40),('spool-pcm','MIGO_HTTP_SPOOL_PCM ',80),('http-control-pcm','MIGO_HTTP_CONTROL_PCM ',234),('range-control-pcm','MIGO_RANGE_CONTROL_PCM ',116),('wave-control-pcm','MIGO_WAVE_CONTROL_PCM ',131),('http-wave-pcm','MIGO_HTTP_WAVE_PCM ',138),('http-control-pcm','MIGO_HTTP_ORDINARY_PCM ',8)]:
        a=prior.index.pcm(work/(label+'-debug.log'),prefix,count);b=prior.index.pcm(work/(label+'-release.log'),prefix,count)
        assert a==b==prior.index.pcm(baseline/(label+'-debug.log'),prefix,count),(label,'previous complete PCM changed')
        pcm[prefix.strip()]=dict(outputs=len(a),samples=sum(map(len,a.values())),all_dev_o3_bits_equal=True,previous_bits_equal=True)
    for profile in ['debug','release']:
        text=(work/('mp3-file-controls-'+profile+'.log')).read_text();assert 'MIGO_LARGE_MP3_CONTROL samples=16777728 encoded=3037011 complete_zero_pcm_checked=true reclaimed_then_source_replay=true' in text
    assert sum(r['role']=='fresh_source_fault' for r in negatives)==10 and sum(r['role']=='original_runtime_red' for r in negatives)==1
    result=dict(programs=plan['programs'],suites=suites,negatives=negatives,clippy=plan['clippy'],pcm=pcm,large_mp3_every_sample_and_tail_checked=True,source_replay_complexity='O(track), no index or speed claim',source_inputs_unchanged=True)
    (work/'summary.json').write_text(json.dumps(result,indent=2)+'\n');print('File MP3 controls verified',flush=True)


def main():
    parser=argparse.ArgumentParser(description=__doc__);parser.add_argument('--phase',choices=['build','execute'],required=True);parser.add_argument('--work',type=Path,required=True);parser.add_argument('--target',type=Path);parser.add_argument('--before',type=Path);parser.add_argument('--baseline',type=Path,required=True);args=parser.parse_args()
    if args.phase=='build':
        assert args.target and args.before;build(args.work,args.target,args.before,args.baseline)
    else:execute(args.work,args.baseline)
if __name__=='__main__':main()
