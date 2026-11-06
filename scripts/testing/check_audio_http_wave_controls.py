#!/usr/bin/env python3
"""Verify pinned HTTP WAVE controls over the original Range/CPU/publication path."""
from __future__ import annotations
import argparse, hashlib, json, os, shutil
from pathlib import Path
import check_audio_wave_controls as prior
index=prior.index
ROOT=index.ROOT
WAVE='streaming_http::tests::wave_controls::'
SEEK=WAVE+'public_wave_seek_restart_and_exact_endpoint_keep_every_pcm_sample'
EARLY=WAVE+'wave_source_seek_before_original_response_eof_preserves_the_entire_nonconstant_tail'
ACK=WAVE+'wave_refusals_and_actual_consumer_ack_preserve_the_original_revision'
DEADLINE=WAVE+'wave_control_keeps_the_original_deadline_through_the_first_real_media_body'
UNKNOWN=WAVE+'unknown_wave_length_never_activates_controls_and_keeps_complete_pcm'
TRUNCATED=WAVE+'truncated_wave_extent_refuses_before_ready_or_controls'
CPU='streaming_codecs::tests::http_wave_retirement::http_wave_eof_retains_actual_capacities_until_original_cpu_really_destroys_them'
ACTUAL='audio_thread::http_spool_tests::actual_url_actor_reopens_the_same_pinned_wave_source_after_eof'
LEGACY_SUFFIX='streaming_http::tests::source_reposition_discards_large_unaccepted_http_suffix_and_preserves_all_pcm'

def faults(originals):
    rows=[]
    def add(name,key,a,z,test,panic):
        assert a!=z and originals[key].count(a)==1,(name,key,'anchor drift')
        rows.append(dict(name=name,key=key,before=a,after=z,test=test,expected_assertion=panic))
    add('http-wave-controls-omitted','adapter','if state.accepts_controls() {','if false {',SEEK,'pinned WAVE never installed its real source controls')
    add('http-wave-decoder-not-retained','codec','packet::PacketStream::wave(self.target, length)','packet::PacketStream::new(self.target)',SEEK,'actual controlled file emitted Error')
    add('http-wave-source-bound-widened','codec','packet::PacketStream::wave(self.target, length)','packet::PacketStream::wave(self.target, length + 1)',TRUNCATED,'truncated HTTP WAVE published Ready or PCM')
    add('http-wave-port-before-clock','adapter','Controls::deferred(None)','Controls::for_source(state, None)?',UNKNOWN,'unknown HTTP WAVE activated source controls')
    add('http-wave-request-type-lost','codec','Some(Codec::Wave(d)) => d.wave_request().map(HttpRequest::Wave),','Some(Codec::Wave(_)) => None,',EARLY,'HTTP WAVE source preparation failed before original EOF')
    add('http-wave-resume-offset-changed','wave','decoder.resume_wave(offset)','decoder.resume_wave(offset + 2)',EARLY,'HTTP WAVE source preparation failed before original EOF')
    add('http-wave-media-deadline-renewed','adapter','self.resolve_request(request, decoder, context.state, deadline)','self.resolve_request(request, decoder, context.state, deadline + std::time::Duration::from_secs(30))',DEADLINE,'HTTP WAVE renewed its original media deadline')
    add('http-common-media-retains-old-input','source','        Ok(SourceResume {\n            decoder,\n            restart: true,\n        })','        Ok(SourceResume {\n            decoder,\n            restart: false,\n        })',LEGACY_SUFFIX,'coalesced HTTP M4A failed complete PCM')
    add('http-wave-consumer-ack-skipped','handshake','tokio::time::timeout_at(deadline, receipt)','tokio::time::timeout_at(deadline, async { drop(receipt); Ok::<(), oneshot::error::RecvError>(()) })',ACK,'HTTP WAVE dropped actual consumer ACK')
    add('http-wave-cancel-retains-original-allocations','codec','        if cancel.load(Ordering::Acquire) {\n            self.codec = None;','        if cancel.load(Ordering::Acquire) {\n            // incorrectly retain the original codec allocations',CPU,'original CPU did not really free HTTP WAVE retained buffers')
    return rows


def build(work,target,before,baseline):
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
    (work/'clippy-delta.json').write_text(json.dumps(delta,indent=2)+'\n');print('Stable HTTP WAVE Clippy',delta,flush=True)
    libs=base.capacity.product.whole.support.codecs.library_patches(ROOT,work);proof={}
    for name,folder in libs.items():
        root=ROOT/'engine/third_party'/folder.name
        if name=='symphonia-bundle-mp3':assert not root.exists();continue
        inventory=lambda d:{str(p.relative_to(d)):c.sha(p) for p in d.rglob('*') if p.is_file()}
        proof[name]=inventory(root);assert proof[name]==inventory(folder),name
    assert len(proof)==8;(work/'product-library-replay.json').write_text(json.dumps(proof,indent=2)+'\n')
    programs=[];steps=[]
    def binary(name,release=False):
        p=base.capacity.product.src.build(P,work,E,name,release)
        row=dict(name=name,path=str(p),sha256=c.sha(p),fresh=False);programs.append(row);return row
    for release,label in [(False,'debug'),(True,'release')]:
        row=binary('positive-'+label,release)
        steps.append(dict(label='full-'+label,program=row,expected=0))
        for tag,test,exact in [('http-pcm',prior.PCM,True),('spool-pcm',prior.SPOOL_PCM,True),('http-control-pcm',prior.CONTROL,False),('range-control-pcm',prior.RANGE,False),('wave-control-pcm',prior.WAVE,False),('http-wave-pcm',WAVE,False)]:
            steps.append(dict(label=tag+'-'+label,program=row,expected=0,test=test,exact=exact))
        for tag,test,dump in [('control-pcm',prior.prior.prior.prior.prior.PREFIX,'control'),('origin-pcm',prior.prior.prior.prior.prior.prior.WINDOW,'origin'),('local-pcm',index.NATIVE,'local'),('file-pcm',index.FULL,'file'),('indexed-pcm',index.INDEX_FULL,'indexed'),('indexed-large',index.INDEX_LARGE,'indexed'),('package-small',prior.prior.prior.prior.prior.prior.product.SMALL,'package'),('package-large',prior.prior.prior.prior.prior.prior.product.LARGE,'package')]:
            steps.append(dict(label=tag+'-'+label,program=row,expected=0,test=test,dump=dump,exact=tag!='control-pcm'))
        steps.append(dict(label='large-file-'+label,program=row,expected=0,test=index.LARGE))
    paths={k:P/'src'/n for k,n in dict(codec='streaming_codecs/mod.rs',packet='streaming_codecs/packet.rs',adapter='streaming_http_control.rs',source='streaming_http.rs',wave='streaming_http_wave.rs',handshake='streaming_source_control.rs',prepare='streaming_http_prepare.rs').items()};originals={k:p.read_text() for k,p in paths.items()}
    for name,key,old,test,panic in [('original-http-wave-single-pass-selector','prepare','streaming_http_prepare.rs',ACTUAL,'actual URL WAVE actor did not reopen the pinned source'),('original-packet-ready-before-src-prepared','packet','streaming_codecs/packet.rs',EARLY,'HTTP WAVE early source seek was refused before original EOF')]:
        original=(before/'engine/crates/audio/src'/old).read_text();assert original!=originals[key]
        try:
            paths[key].write_text(original);row=binary(name);steps.append(dict(label=name,program=row,expected=101,test=test,expected_assertion=panic,role='original_runtime_red'))
        finally:paths[key].write_text(originals[key])
    for fault in faults(originals):
        p=paths[fault['key']]
        try:
            p.write_text(originals[fault['key']].replace(fault['before'],fault['after']));row=binary(fault['name']);steps.append(dict(label=row['name'],program=row,expected=101,test=fault['test'],expected_assertion=fault['expected_assertion'],role='fresh_source_fault'))
        finally:p.write_text(originals[fault['key']])
    base.capacity.product.whole.verify_inputs(work,frozen)
    for name in ['audio','shared','io','services','frame-wire']:
        root=ROOT/'engine/crates'/name/'src';private=P/'src' if name=='audio' else P/'local-deps'/name/'src';assert all(c.sha(p)==c.sha(private/p.relative_to(root)) for p in root.rglob('*') if p.is_file())
    assert len(programs)==14 and len(steps)==44
    (work/'execution-plan.json').write_text(json.dumps(dict(programs=programs,steps=steps,clippy=delta,source_inputs=frozen,private_sources_restored=True),indent=2)+'\n');print('Immutable HTTP WAVE plan prepared',flush=True)


def execute(work, baseline):
    os.environ['MIGO_DUMP_HTTP_WAVE_PCM']='1'
    os.environ['MIGO_DUMP_HTTP_SPOOL_PCM']='1'
    os.environ['MIGO_DUMP_HTTP_M4A_PCM']='1'
    os.environ['MIGO_DUMP_HTTP_CONTROL_PCM']='1'
    os.environ['MIGO_DUMP_RANGE_CONTROL_PCM']='1'
    os.environ['MIGO_DUMP_WAVE_CONTROL_PCM']='1'
    prior.prior.prior.prior.prior.execute(work,baseline)
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
    assert result['suites']['full-debug']==result['suites']['full-release']==dict(passed=932,ignored=3)
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
    print('Sealed WAVE controls, complete PCM and original CPU custody verified',flush=True)

if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--output',type=Path,required=True);p.add_argument('--target-dir',type=Path);p.add_argument('--before-source',type=Path);p.add_argument('--baseline-native',type=Path,required=True);p.add_argument('--execute',action='store_true');a=p.parse_args()
    if a.execute:execute(a.output.resolve(),a.baseline_native.resolve())
    else:
        assert a.target_dir and a.before_source
        build(a.output.resolve(),a.target_dir.resolve(),a.before_source.resolve(),a.baseline_native.resolve())
