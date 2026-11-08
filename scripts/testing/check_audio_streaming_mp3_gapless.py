#!/usr/bin/env python3
"""Verify bounded progressive MP3 presentation using the existing minimp3_ex core.

Build offline from frozen sources; execute retained SHA-bound binaries separately.
Only fresh single-test business assertions qualify as counterevidence.
"""
from __future__ import annotations
import argparse, hashlib, json, os, re, shutil, subprocess
from pathlib import Path
import check_audio_http_wave_controls as prior
index=prior.index
ROOT=index.ROOT
PREFIX='streaming_mp3::tests::gapless::'
PCM=PREFIX+'complete_gapless_pcm_matches_frozen_presentation_and_independent_sinc'
CLOCK=PREFIX+'authoritative_clock_is_ready_before_pcm_and_before_original_body_eof'
LONG=PREFIX+'long_declared_stream_consumes_every_sample_with_bounded_resident_capacity'
TRUNCATED=PREFIX+'declared_gapless_truncation_is_terminal_and_never_publishes_done'
CPU=PREFIX+'actual_spare_capacities_stay_owned_until_original_cpu_cancellation'
NATIVE='streaming_mp3_native::native_stream_owner_is_really_freed_on_the_original_cpu'
OOM='streaming_mp3_native::native_constructor_failure_retires_the_stream_without_pcm'
HTTP='streaming_http::tests::mp3_gapless::'


def faults(originals):
    rows=[]
    def add(name,key,a,z,test,panic):
        assert a!=z and originals[key].count(a)==1,(name,key,'anchor drift')
        rows.append(dict(name=name,key=key,before=a,after=z,test=test,expected_assertion=panic))
    add('stream-tag-not-skipped','stream','if skip_tag {','if false {',PCM,'gapless stream complete PCM differs from frozen oracle')
    add('stream-raw-frames-instead-of-presentation','stream','self.frames\n                .decode_presented(&self.buffer[..size], presentation)?','{ let _=presentation; self.frames.decode(&self.buffer[..size]) }',PCM,'progressive MP3 did not complete its presentation')
    add('stream-clock-frame-count-shifted','stream','(samples / owner.source_format.1 as usize) as u64,','(samples / owner.source_format.1 as usize) as u64 + 1,',CLOCK,'progressive MP3 clock differs from declared gapless presentation')
    add('stream-whole-pcm-ceiling-imposed','stream','.begin(&self.buffer[..frame.length], &frame, true)?','.begin(&self.buffer[..frame.length], &frame, false)?',LONG,'progressive MP3 imposed a whole-file duration ceiling')
    add('stream-cancel-keeps-physical-capacity','stream','            self.retire(ErrorCode::Cancelled);','            self.error=Some(ErrorCode::Cancelled);',CPU,'original CPU did not really free progressive MP3 capacity')
    add('stream-native-delay-ignored','native','    *expected = p->state.detected_samples;','    p->state.to_skip = 0;\n    *expected = p->state.detected_samples;',PCM,'gapless stream complete PCM differs from frozen oracle')
    add('stream-native-padding-retained','ex','out_samples = dec->detected_samples - dec->cur_sample;','out_samples = out_samples;',PCM,'progressive MP3 did not complete its presentation')
    add('stream-declared-eof-unchecked','presentation','if unsafe { migo_mp3_presentation_finish(self.native.as_ptr()) } < 0 {','if false {',TRUNCATED,'truncated MP3 must validate its native presentation before SRC flushing')
    add('stream-id3-incremental-skip-lost','stream','    fn skip_id3(&mut self) -> EngineResult<Option<Progress>> {','    fn skip_id3(&mut self) -> EngineResult<Option<Progress>> { return Ok(None);',PREFIX+'incremental_id3_skip_ignores_embedded_sync_and_never_retains_the_whole_tag','ID3 prefix was not skipped as bounded metadata')
    return rows


def build(work,target,before,baseline,before_metadata=None):
    os.environ['CARGO_INCREMENTAL']='0'
    base=index.base;P,E,frozen=base.prepare(work,target)
    c=base.capacity.product.whole.support.clock.common
    for n in [index.FIXTURE,str(Path(__file__).relative_to(ROOT))]:
        frozen[n]=c.sha(ROOT/n);q=work/'source-inputs'/n;q.parent.mkdir(parents=True,exist_ok=True);shutil.copy2(ROOT/n,q)
    q=work/index.FIXTURE;q.parent.mkdir(parents=True,exist_ok=True);shutil.copy2(ROOT/index.FIXTURE,q)
    (work/'source-inputs.json').write_text(json.dumps(frozen,indent=2)+'\n')
    out=P/'src/output.rs';s=out.read_text()
    try:out.write_text(index.backend.ports(s));assert c.run(['cargo','clippy','--lib','--tests','--offline','--locked','--message-format=json'],work/'clippy.jsonl',P,E,timeout=600)==0
    finally:out.write_text(s)
    a=index.wave.diagnostics(baseline/'clippy.jsonl',baseline/'probe');z=index.wave.diagnostics(work/'clippy.jsonl',P);assert not(z-a),list((z-a).items())
    delta=dict(before=sum(a.values()),after=sum(z.values()),new_diagnostics=0,removed=[dict(key=k,count=v) for k,v in (a-z).items()])
    assert c.run(['cargo','clippy','-p','migo-services','--lib','--tests','--offline','--locked','--message-format=json'],work/'services-clippy.jsonl',P,E,timeout=600)==0
    a=index.wave.diagnostics(baseline/'services-clippy.jsonl',baseline/'probe');z=index.wave.diagnostics(work/'services-clippy.jsonl',P)
    select=lambda counter:type(counter)({k:v for k,v in counter.items() if any(p.endswith(('network/http/mod.rs','src/audio.rs','src/audio_file_stream.rs','src/audio_http_spool.rs')) for p in k[0])})
    a,z=select(a),select(z);assert not(z-a);delta['services_http']=dict(before=sum(a.values()),after=sum(z.values()),new_diagnostics=0)
    (work/'clippy-delta.json').write_text(json.dumps(delta,indent=2)+'\n');print('Stable progressive MP3 Clippy',delta,flush=True)
    libs=base.capacity.product.whole.support.codecs.library_patches(ROOT,work);proof={}
    for name,folder in libs.items():
        root=ROOT/'engine/third_party'/folder.name
        if name=='symphonia-bundle-mp3':assert not root.exists();continue
        inventory=lambda d:{str(p.relative_to(d)):c.sha(p) for p in d.rglob('*') if p.is_file()}
        proof[name]=inventory(root);assert proof[name]==inventory(folder),name
    assert len(proof)==8;(work/'product-library-replay.json').write_text(json.dumps(proof,indent=2)+'\n')
    programs=[];steps=[]
    def binary(name,release=False,env=None,native=False):
        p=base.capacity.product.src.build(P,work,E if env is None else env,name,release)
        if native:
            artifacts=[]
            for line in (work/(name+'-build.jsonl')).read_text().splitlines():
                try:r=json.loads(line)
                except ValueError:continue
                if r.get('reason')=='compiler-artifact' and r.get('target',{}).get('name')=='minimp3_sys':artifacts.append(r)
            assert len(artifacts)==1 and artifacts[0]['fresh'] is False,(name,'native must really rebuild')
        row=dict(name=name,path=str(p),sha256=c.sha(p),fresh=False);programs.append(row);return row
    for release,label in [(False,'debug'),(True,'release')]:
        row=binary('positive-'+label,release)
        steps.append(dict(label='full-'+label,program=row,expected=0))
        for tag,test in [('mp3-gapless',PREFIX),('mp3-http',HTTP)]:steps.append(dict(label=tag+'-'+label,program=row,expected=0,test=test,exact=False,mp3_dump=label))
        for tag,test,exact in [('http-pcm',prior.prior.PCM,True),('spool-pcm',prior.prior.SPOOL_PCM,True),('http-control-pcm',prior.prior.CONTROL,False),('range-control-pcm',prior.prior.RANGE,False),('wave-control-pcm',prior.prior.WAVE,False),('http-wave-pcm',prior.WAVE,False)]:
            steps.append(dict(label=tag+'-'+label,program=row,expected=0,test=test,exact=exact))
        for tag,test,dump in [('control-pcm',prior.prior.prior.prior.prior.prior.PREFIX,'control'),('origin-pcm',prior.prior.prior.prior.prior.prior.prior.WINDOW,'origin'),('local-pcm',index.NATIVE,'local'),('file-pcm',index.FULL,'file'),('indexed-pcm',index.INDEX_FULL,'indexed'),('indexed-large',index.INDEX_LARGE,'indexed'),('package-small',prior.prior.prior.prior.prior.prior.prior.product.SMALL,'package'),('package-large',prior.prior.prior.prior.prior.prior.prior.product.LARGE,'package')]:
            steps.append(dict(label=tag+'-'+label,program=row,expected=0,test=test,dump=dump,exact=tag!='control-pcm'))
        steps.append(dict(label='large-file-'+label,program=row,expected=0,test=index.LARGE))
    native=work/'native/minimp3-sys-0.6.0'
    paths=dict(stream=P/'src/streaming_mp3.rs',presentation=P/'src/decoder/mp3/presentation.rs',native=native/'minimp3.c',ex=native/'minimp3/minimp3_ex.h')
    originals={k:p.read_text() for k,p in paths.items()}
    old=(before/'engine/crates/audio/src/streaming_mp3.rs').read_text();assert old!=originals['stream']
    try:
        paths['stream'].write_text(old);row=binary('original-raw-progressive-mp3');steps.append(dict(label=row['name'],program=row,expected=101,test=PCM,expected_assertion='gapless stream complete PCM differs from frozen oracle',role='original_runtime_red'))
    finally:paths['stream'].write_text(originals['stream'])
    if before_metadata is not None:
        original=(before_metadata/'engine/crates/audio/src/streaming_mp3.rs').read_text();assert original!=originals['stream']
        try:
            paths['stream'].write_text(original);row=binary('original-pre-id3-bootstrap');steps.append(dict(label=row['name'],program=row,expected=101,test=PREFIX+'incremental_id3_skip_ignores_embedded_sync_and_never_retains_the_whole_tag',expected_assertion='ID3 prefix was not skipped as bounded metadata',role='original_runtime_red'))
        finally:paths['stream'].write_text(originals['stream'])
    for f in faults(originals):
        try:
            paths[f['key']].write_text(originals[f['key']].replace(f['before'],f['after']));row=binary(f['name'],native=f['key'] in ['native','ex']);steps.append(dict(label=row['name'],program=row,expected=101,test=f['test'],expected_assertion=f['expected_assertion'],role='fresh_source_fault'))
        finally:paths[f['key']].write_text(originals[f['key']])
    lib=P/'src/lib.rs';source=lib.read_text();extra=ROOT/'scripts/testing/fixtures/audio-codec-probe/streaming_mp3_native.rs';private=work/'source/streaming_mp3_native.rs';private.parent.mkdir(exist_ok=True);shutil.copy2(extra,private)
    instrument=E.copy();instrument.update(CFLAGS='-DMIGO_MINIMP3_TEST',MIGO_MP3_TEST_FIXTURES=str(P/'src/decoder/mp3/fixtures'))
    try:
        lib.write_text(source+'\n#[cfg(test)] #[path='+json.dumps(str(private))+'] mod streaming_mp3_native;\n')
        row=binary('native-owner-positive',env=instrument,native=True);steps.append(dict(label=row['name'],program=row,expected=0,test=NATIVE))
        a='void migo_mp3_presentation_free(migo_mp3_presentation *p) { free(p); }';assert originals['native'].count(a)==1
        paths['native'].write_text(originals['native'].replace(a,'void migo_mp3_presentation_free(migo_mp3_presentation *p) { (void)p; }'))
        row=binary('native-owner-leak',env=instrument,native=True);steps.append(dict(label=row['name'],program=row,expected=101,test=NATIVE,expected_assertion='original CPU did not really free native MP3 presentation',role='fresh_source_fault'))
        paths['native'].write_text(originals['native'].replace('malloc(sizeof(*p))','NULL'))
        row=binary('native-allocation-failure-positive',env=instrument,native=True);steps.append(dict(label=row['name'],program=row,expected=0,test=OOM))
    finally:
        lib.write_text(source);paths['native'].write_text(originals['native'])
    base.capacity.product.whole.verify_inputs(work,frozen)
    for name in ['audio','shared','io','services','frame-wire']:
        root=ROOT/'engine/crates'/name/'src';private=P/'src' if name=='audio' else P/'local-deps'/name/'src';assert all(c.sha(p)==c.sha(private/p.relative_to(root)) for p in root.rglob('*') if p.is_file())
    assert all(p.read_text()==originals[k] for k,p in paths.items())
    (work/'execution-plan.json').write_text(json.dumps(dict(programs=programs,steps=steps,clippy=delta,source_inputs=frozen,private_sources_restored=True),indent=2)+'\n');print('Immutable progressive MP3 plan prepared',len(programs),len(steps),flush=True)


def _execute_plan(work, baseline):
    plan = json.loads((work / 'execution-plan.json').read_text())
    sha = lambda p: hashlib.sha256(p.read_bytes()).hexdigest()
    for name, digest in plan['source_inputs'].items():
        assert sha(ROOT/name) == sha(work/'source-inputs'/name) == digest, name
    suites, faults, reds = {}, [], []
    for step in plan['steps']:
        program = step['program']
        binary = Path(program['path'])
        assert sha(binary) == program['sha256']
        env = os.environ.copy()
        env['RUST_TEST_THREADS'] = '1'
        if step.get('dump'):
            env[dict(control='MIGO_DUMP_FILE_CONTROL_PCM', origin='MIGO_DUMP_SINC_ORIGIN_PCM', local='MIGO_DUMP_LOCAL_FILE_PCM', file='MIGO_DUMP_FILE_STREAM_PCM', indexed='MIGO_DUMP_INDEXED_FILE_PCM', package='MIGO_DUMP_PACKAGE_FILE_PCM')[step['dump']]] = '1'
        if step.get('mp3_dump'):
            directory=work/'mp3-pcm'/step['mp3_dump'];directory.mkdir(parents=True,exist_ok=True)
            env['MIGO_MP3_GAPLESS_PCM_DIR']=str(directory)
        cmd = [str(binary)]
        if step.get('test'):
            cmd += (['--exact'] if step.get('exact', True) else []) + [step['test'], '--nocapture']
        with (work/(step['label']+'.log')).open('w') as log:
            result = subprocess.run(cmd, cwd=work/'probe', env=env, stdout=log, stderr=subprocess.STDOUT, timeout=900)
        (work/(step['label']+'-exit.txt')).write_text(str(result.returncode)+'\n')
        text = (work/(step['label']+'.log')).read_text()
        assert result.returncode == step['expected'], (step['label'], result.returncode)
        if result.returncode:
            assert all(s in text for s in ['running 1 test', '1 failed', 'panicked at', step['test'], step['expected_assertion']]), step['label']
            (faults if step['role']=='fresh_source_fault' else reds).append(dict(name=step['label'], test=step['test'], expected_assertion=step['expected_assertion'], exit=101, sha256=program['sha256']))
        else:
            match = re.search(r'test result: ok\. ([1-9][0-9]*) passed; 0 failed; (\d+) ignored;', text)
            assert match
            suites[step['label']] = dict(passed=int(match[1]), ignored=int(match[2]))
        print('Executed', step['label'], result.returncode, flush=True)
    pcm = {}
    for label, prefix, count in [('control-pcm','MIGO_FILE_CONTROL_PCM ',131), ('origin-pcm','MIGO_SINC_ORIGIN_PCM ',36), ('local-pcm','MIGO_LOCAL_FILE_PCM ',12), ('file-pcm','MIGO_FILE_STREAM_PCM ',8), ('indexed-pcm','MIGO_INDEXED_FILE_PCM ',40), ('indexed-large','MIGO_INDEXED_FILE_PCM ',10), ('package-small','MIGO_PACKAGE_FILE_PCM ',12), ('package-large','MIGO_PACKAGE_INDEXED_PCM ',10)]:
        a = index.pcm(work/(label+'-debug.log'), prefix, count)
        b = index.pcm(work/(label+'-release.log'), prefix, count)
        assert a == b, (label, 'dev/O3 bits differ')
        if label != 'control-pcm':
            assert a == index.pcm(baseline/(label+'-debug.log'), prefix, count), (label, 'previous full PCM changed')
        pcm[label] = dict(outputs=len(a), samples=sum(map(len,a.values())), all_dev_o3_bits_equal=True, previous_product_bits_unchanged=label!='control-pcm')
    large = {}
    for label in ['debug', 'release']:
        match = re.search(r'MIGO_LARGE_FILE_PCM frames=(\d+) bits=(\d+) bounded_pcm_bytes=(\d+)', (work/('large-file-'+label+'.log')).read_text())
        assert match and int(match[1]) == 16777345 and int(match[2]) == 1048576000 and int(match[3]) < 4*1024*1024
        large[label] = dict(samples=int(match[1]), bits=int(match[2]), retained_pcm_bytes=int(match[3]), every_sample_checked=True, tail_and_end_checked=True)
    (work/'summary.json').write_text(json.dumps(dict(programs=plan['programs'], suites=suites, controls=faults, original_red=reds, clippy=plan['clippy'], pcm=pcm, large_file=large, baseline_pcm_path=str(baseline), source_inputs_unchanged=True, private_sources_restored=True), indent=2)+'\n')
    print('Actual File controls and complete PCM verified', flush=True)




def execute(work, baseline):
    os.environ['MIGO_DUMP_HTTP_WAVE_PCM']='1'
    os.environ['MIGO_DUMP_HTTP_SPOOL_PCM']='1'
    os.environ['MIGO_DUMP_HTTP_M4A_PCM']='1'
    os.environ['MIGO_DUMP_HTTP_CONTROL_PCM']='1'
    os.environ['MIGO_DUMP_RANGE_CONTROL_PCM']='1'
    os.environ['MIGO_DUMP_WAVE_CONTROL_PCM']='1'
    _execute_plan(work,baseline)
    path=work/'summary.json';result=json.loads(path.read_text())
    current=index.pcm(work/'control-pcm-debug.log','MIGO_FILE_CONTROL_PCM ',131)
    assert current==index.pcm(baseline/'control-pcm-debug.log','MIGO_FILE_CONTROL_PCM ',131)
    result['pcm']['control-pcm']['previous_product_bits_unchanged']=True
    a=index.pcm(work/'http-pcm-debug.log','MIGO_HTTP_M4A_PCM ',40)
    b=index.pcm(work/'http-pcm-release.log','MIGO_HTTP_M4A_PCM ',40)
    assert a==b==index.pcm(baseline/'http-pcm-debug.log','MIGO_HTTP_M4A_PCM ',40)
    result['http_pcm']=dict(outputs=40,samples=sum(map(len,a.values())),all_dev_o3_bits_equal=True,previous_http_bits_equal=True)
    a=index.pcm(work/'spool-pcm-debug.log','MIGO_HTTP_SPOOL_PCM ',80)
    b=index.pcm(work/'spool-pcm-release.log','MIGO_HTTP_SPOOL_PCM ',80)
    assert a==b
    canonical=lambda rows,mode:{k.removeprefix(mode+'/').replace('edit-version-one/','edit-v1/'):v for k,v in rows.items() if k.startswith(mode+'/')}
    expected=index.pcm(baseline/'indexed-pcm-debug.log','MIGO_INDEXED_FILE_PCM ',40)
    for mode in ['no-tag','ignored']:assert canonical(a,mode)==expected
    result['spool_pcm']=dict(outputs=80,samples=sum(map(len,a.values())),all_dev_o3_bits_equal=True,previous_indexed_file_bits_equal=True)
    a=index.pcm(work/'http-control-pcm-debug.log','MIGO_HTTP_CONTROL_PCM ',234)
    b=index.pcm(work/'http-control-pcm-release.log','MIGO_HTTP_CONTROL_PCM ',234)
    assert a==b,'HTTP control dev/O3 bits changed'
    old=index.pcm(baseline/'control-pcm-debug.log','MIGO_FILE_CONTROL_PCM ',131)
    compared=0
    for key,value in a.items():
        if key.startswith(('no-tag/','ignored/')):
            original=key.split('/',1)[1];assert value==old[original],key;compared+=1
    assert compared==232
    result['http_control_pcm']=dict(outputs=234,samples=sum(map(len,a.values())),all_dev_o3_bits_equal=True,previous_file_complete_traversals_equal=232,loading_intent_complete_outputs=2)
    a=index.pcm(work/'http-control-pcm-debug.log','MIGO_HTTP_ORDINARY_PCM ',8)
    b=index.pcm(work/'http-control-pcm-release.log','MIGO_HTTP_ORDINARY_PCM ',8)
    assert a==b==index.pcm(baseline/'file-pcm-debug.log','MIGO_FILE_STREAM_PCM ',8)
    result['ordinary_http_pcm']=dict(outputs=8,samples=sum(map(len,a.values())),all_dev_o3_bits_equal=True,previous_file_bits_equal=True)
    assert result['suites']['full-debug']==result['suites']['full-release']==dict(passed=941,ignored=3)
    assert result['suites']['http-control-pcm-debug']==result['suites']['http-control-pcm-release']==dict(passed=8,ignored=0)
    assert len(result['controls'])==10 and len(result['original_red'])==2
    a=index.pcm(work/'range-control-pcm-debug.log','MIGO_RANGE_CONTROL_PCM ',116)
    b=index.pcm(work/'range-control-pcm-release.log','MIGO_RANGE_CONTROL_PCM ',116)
    assert a==b
    for key,value in a.items():assert value==old[key.removeprefix('range/')],key
    result['range_control_pcm']=dict(outputs=116,samples=sum(map(len,a.values())),all_dev_o3_bits_equal=True,previous_file_complete_traversals_equal=116)
    assert result['suites']['range-control-pcm-debug']==result['suites']['range-control-pcm-release']==dict(passed=8,ignored=0)
    a=index.pcm(work/'wave-control-pcm-debug.log','MIGO_WAVE_CONTROL_PCM ',131)
    b=index.pcm(work/'wave-control-pcm-release.log','MIGO_WAVE_CONTROL_PCM ',131)
    assert a==b,'WAVE control dev/O3 complete PCM changed'
    result['wave_control_pcm']=dict(outputs=len(a),samples=sum(map(len,a.values())),all_dev_o3_bits_equal=True,independent_mature_pcm_and_global_loop_reference=True)
    assert result['suites']['wave-control-pcm-debug']==result['suites']['wave-control-pcm-release']==dict(passed=10,ignored=0)
    result['large_wave_control']={}
    for profile in ['debug','release']:
        text=(work/('wave-control-pcm-'+profile+'.log')).read_text();assert 'MIGO_LARGE_WAVE_CONTROL samples=8388737 bits=1048576000 complete_tail=true' in text
        result['large_wave_control'][profile]=dict(samples=8388737,bits=1048576000,every_sample_checked=True,reclaimed_then_source_restart=True)
    a=index.pcm(work/'http-wave-pcm-debug.log','MIGO_HTTP_WAVE_PCM ',138)
    b=index.pcm(work/'http-wave-pcm-release.log','MIGO_HTTP_WAVE_PCM ',138)
    assert a==b,'HTTP WAVE complete PCM dev/O3 changed'
    file=index.pcm(baseline/'wave-control-pcm-debug.log','MIGO_WAVE_CONTROL_PCM ',131)
    canonical={k:v for k,v in a.items() if k in file};assert len(canonical)==123
    assert all(v==file[k] for k,v in canonical.items())
    result['http_wave_pcm']=dict(outputs=138,samples=sum(map(len,a.values())),canonical_file_outputs=123,canonical_file_bits_equal=True,all_dev_o3_bits_equal=True)
    assert result['suites']['http-wave-pcm-debug']==result['suites']['http-wave-pcm-release']==dict(passed=10,ignored=0)
    result['large_http_wave_control']={}
    for profile in ['debug','release']:
        text=(work/('http-wave-pcm-'+profile+'.log')).read_text();assert 'MIGO_LARGE_HTTP_WAVE_CONTROL samples=8388737 bits=1048576000 complete_tail=true' in text
        result['large_http_wave_control'][profile]=dict(samples=8388737,every_sample_checked=True,original_response_held_before_eof=True,reclaimed_then_source_restart=True)
    path.write_text(json.dumps(result,indent=2)+'\n')
    files={}
    for label in ['debug','release']:
        rows={p.name:p for p in (work/'mp3-pcm'/label).glob('*.f32le')}
        assert len(rows)==116,(label,len(rows));files[label]=rows
    assert files['debug'].keys()==files['release'].keys()
    evidence={}
    for name,p in files['debug'].items():
        raw=p.read_bytes();assert raw==files['release'][name].read_bytes(),name
        evidence[name]=dict(sha256=hashlib.sha256(raw).hexdigest(),samples=len(raw)//4)
    result['mp3_gapless_pcm']=dict(outputs=116,samples=sum(x['samples'] for x in evidence.values()),all_dev_o3_bits_equal=True,complete_files_reread=True,files=evidence)
    for label in ['debug','release']:
        assert 'samples=16777728' in (work/('mp3-gapless-'+label+'.log')).read_text()
    path.write_text(json.dumps(result,indent=2)+'\n')
    print('Progressive MP3 presentation, complete PCM and original CPU custody verified',flush=True)

if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--output',type=Path,required=True);p.add_argument('--target-dir',type=Path);p.add_argument('--before-source',type=Path);p.add_argument('--before-metadata-source',type=Path);p.add_argument('--baseline-native',type=Path,required=True);p.add_argument('--execute',action='store_true');a=p.parse_args()
    if a.execute:execute(a.output.resolve(),a.baseline_native.resolve())
    else:
        assert a.target_dir and a.before_source
        build(a.output.resolve(),a.target_dir.resolve(),a.before_source.resolve(),a.baseline_native.resolve(),a.before_metadata_source.resolve() if a.before_metadata_source else None)
