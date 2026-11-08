#!/usr/bin/env python3
"""Verify pinned HTTP controls over the original reader and shared ACK handshake.

Offline fresh builds precede SHA-bound loopback execution. No Git or shipping claim.
"""
from __future__ import annotations
import argparse, hashlib, json, os, re, shutil
from pathlib import Path
import check_audio_http_spool as prior

index = prior.index
ROOT = index.ROOT
PREFIX = 'streaming_http::tests::'
PCM = prior.PCM
CONTROL = PREFIX+'controls::'
SEEK = CONTROL+'public_http_seek_restart_and_exact_endpoint_preserve_complete_pcm'
LOADING = CONTROL+'loop_intent_before_the_late_http_port_keeps_the_last_requested_mode'
POLICY = CONTROL+'url_retention_requires_a_real_port_and_preserves_existing_single_pass_and_fixed_modes'
PHYSICAL = CONTROL+'sealed_http_eof_retains_actual_slot_capacity_until_original_cpu_destruction'
ACTUAL = 'audio_thread::http_spool_tests::actual_url_actor_keeps_the_original_sealed_source_for_public_seek_and_restart'
SPOOL = PREFIX+'preparation::'
SPOOL_PCM = SPOOL+'unpinned_and_range_ignoring_m4a_use_one_original_body_and_preserve_complete_pcm'
ACTOR = 'audio_thread::http_spool_tests::admitted_url_carries_session_spool_through_the_actual_audio_actor_and_source_identity'
SERVICE = 'audio::http_spool::tests::'
LARGE = PREFIX+'range_tail_plays_beyond_whole_file_limit_without_waiting_for_original_body_eof'
CUSTODY = PREFIX+'cancelled_real_index_handoff_retains_storage_and_credit_behind_the_original_cpu'


RANGE = PREFIX+'range_controls::'
SEEK = RANGE+'public_http_seek_restart_and_exact_endpoint_preserve_complete_pcm'
ACK = RANGE+'real_range_controls_reject_without_change_and_wait_for_actual_consumer_ack'
READ = RANGE+'interrupted_range_read_keeps_every_byte_and_original_reader_budget'
FAILED = RANGE+'returned_range_validation_and_body_errors_never_become_interruptions'
DEADLINE = RANGE+'public_range_seek_keeps_original_deadline_through_the_first_real_body_chunk'
ACTUAL = 'audio_thread::http_spool_tests::actual_url_actor_reopens_the_same_pinned_range_source_after_eof'

def controls(originals, package):
    rows=[]
    def add(name,changes,test,panic):
        for path,a,z in changes:assert a!=z and originals[path].count(a)==1,(name,path,'anchor drift')
        rows.append(dict(name=name,changes=changes,test=test,expected_assertion=panic))
    add('range-controls-not-installed',[('adapter','if state.accepts_controls() {','if false {')],SEEK,'HTTP sealed source never activated its original control port')
    add('range-decoder-not-retained',[('codec','if self.retained_length.is_some() {','if false {')],SEEK,'actual controlled file emitted Error')
    add('range-consumer-ack-skipped',[('handshake','tokio::time::timeout_at(deadline, receipt)','tokio::time::timeout_at(deadline, async { drop(receipt); Ok::<(), oneshot::error::RecvError>(()) })')],ACK,'pinned HTTP dropped actual consumer ACK')
    add('range-read-not-recovered',[('adapter','stream.resume_after_interruption().map_err(error)?','()')],READ,'pinned HTTP did not recover its interrupted read')
    a=originals['adapter'];begin=a.index('        if self.interrupted {');end=a.index('\n    }\n}\nimpl ControlledSource',begin)
    add('range-returned-error-recovered',[('adapter',a[begin:end],'        self.interrupted = false; self.failed = false; Ok(())')],FAILED,'completed HTTP error was recovered as an interruption')
    add('range-control-body-deadline-renewed',[('adapter','self.resolve_request(request, decoder, context.state, deadline)','self.resolve_request(request, decoder, context.state, deadline + std::time::Duration::from_secs(30))')],DEADLINE,'HTTP control renewed its original media deadline')
    import check_audio_file_controls as file
    for fault in file.controls(package):
        if fault['name'] in ['file-consumer-ack-skipped','file-capability-not-rechecked-after-ack','file-media-deadline-renewed']:
            key='handshake' if fault['path']=='streaming_source_control.rs' else 'file_adapter'
            add(fault['name'],[(key,fault['before'],fault['after'])],fault['test'],fault['expected_assertion'])
    return rows


def build(work, target, before, baseline):
    os.environ['CARGO_INCREMENTAL'] = '0'
    base = index.base
    package, env, frozen = base.prepare(work, target)
    common = base.capacity.product.whole.support.clock.common
    for name in [index.FIXTURE, str(Path(__file__).relative_to(ROOT))]:
        frozen[name] = common.sha(ROOT/name)
        dest = work/'source-inputs'/name
        dest.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(ROOT/name, dest)
    dest = work/index.FIXTURE
    dest.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(ROOT/index.FIXTURE, dest)
    (work/'source-inputs.json').write_text(json.dumps(frozen, indent=2)+'\n')
    output = package/'src/output.rs'
    saved = output.read_text()
    try:
        output.write_text(index.backend.ports(saved))
        assert common.run(['cargo','clippy','--lib','--tests','--offline','--locked','--message-format=json'],work/'clippy.jsonl',package,env,timeout=600)==0
    finally:
        output.write_text(saved)
    a=index.wave.diagnostics(baseline/'clippy.jsonl',baseline/'probe')
    z=index.wave.diagnostics(work/'clippy.jsonl',package)
    assert not(z-a),(z-a)
    delta=dict(before=sum(a.values()),after=sum(z.values()),new_diagnostics=0,removed=[dict(key=k,count=v) for k,v in (a-z).items()])
    assert common.run(['cargo','clippy','-p','migo-services','--lib','--tests','--offline','--locked','--message-format=json'],work/'services-clippy.jsonl',package,env,timeout=600)==0
    a=index.wave.diagnostics(baseline/'services-clippy.jsonl',baseline/'probe')
    z=index.wave.diagnostics(work/'services-clippy.jsonl',package)
    select=lambda c:type(c)({k:v for k,v in c.items() if any(n.endswith(('network/http/mod.rs', 'src/audio.rs', 'src/audio_file_stream.rs', 'src/audio_http_spool.rs')) for n in k[0])})
    a,z=select(a),select(z)
    assert not(z-a),(z-a)
    delta['services_http']=dict(before=sum(a.values()),after=sum(z.values()),new_diagnostics=0)
    (work/'clippy-delta.json').write_text(json.dumps(delta,indent=2)+'\n')
    print('Stable HTTP M4A Clippy',delta,flush=True)
    programs,steps=[],[]
    def binary(name,crate='migo-audio',release=False):
        if crate=='migo-audio':
            path=base.capacity.product.src.build(package,work,env,name,release)
        else:
            (package/'local-deps'/crate.removeprefix('migo-')/'src/lib.rs').touch()
            cmd=['cargo','test','-p',crate,'--lib','--offline','--locked','--no-run','--message-format=json']
            if release:cmd.append('--release')
            log=work/(name+'-build.jsonl')
            assert common.run(cmd,log,package,env,timeout=600)==0
            rows=[]
            for line in log.read_text().splitlines():
                try:r=json.loads(line)
                except ValueError:continue
                if r.get('reason')=='compiler-artifact' and r.get('executable') and r.get('target',{}).get('name')==crate.replace('-','_'):rows.append(r)
            assert len(rows)==1 and rows[0]['fresh'] is False
            if release:assert rows[0]['profile']['opt_level']=='3'
            path=work/(name+'-tests');shutil.copy2(rows[0]['executable'],path)
        row=dict(name=name,path=str(path),sha256=common.sha(path),fresh=False)
        programs.append(row);return row
    for release,label in [(False,'debug'),(True,'release')]:
        row=binary('positive-'+label,release=release)
        steps.append(dict(label='full-'+label,program=row,expected=0))
        steps.append(dict(label='http-pcm-'+label,program=row,expected=0,test=PCM))
        steps.append(dict(label='spool-pcm-'+label,program=row,expected=0,test=SPOOL_PCM))
        steps.append(dict(label='http-control-pcm-'+label,program=row,expected=0,test=CONTROL,exact=False))
        steps.append(dict(label='range-control-pcm-'+label,program=row,expected=0,test=RANGE,exact=False))
        for tag,test,dump in [('control-pcm',prior.prior.prior.prior.PREFIX,'control'),('origin-pcm',prior.prior.prior.prior.prior.WINDOW,'origin'),('local-pcm',index.NATIVE,'local'),('file-pcm',index.FULL,'file'),('indexed-pcm',index.INDEX_FULL,'indexed'),('indexed-large',index.INDEX_LARGE,'indexed'),('package-small',prior.prior.prior.prior.prior.product.SMALL,'package'),('package-large',prior.prior.prior.prior.prior.product.LARGE,'package')]:
            steps.append(dict(label=tag+'-'+label,program=row,expected=0,test=test,dump=dump,exact=tag!='control-pcm'))
        steps.append(dict(label='large-file-'+label,program=row,expected=0,test=index.LARGE))
    paths={k:package/'src'/n for k,n in dict(adapter='streaming_http_control.rs',codec='streaming_codecs/mod.rs',handshake='streaming_source_control.rs',file_adapter='streaming_file_control.rs',prepare='streaming_http_prepare.rs').items()}
    originals={k:p.read_text() for k,p in paths.items()}
    original=(before/'engine/crates/audio/src/streaming_http_prepare.rs').read_text()
    assert original!=originals['prepare']
    try:
        paths['prepare'].write_text(original)
        row=binary('original-single-pass-range-selector')
        steps.append(dict(label=row['name'],program=row,expected=101,test=ACTUAL,expected_assertion='actual URL actor did not reopen the retained Range source',role='original_runtime_red'))
    finally:paths['prepare'].write_text(originals['prepare'])
    for fault in controls(originals,package):
        try:
            for key in {c[0] for c in fault['changes']}:
                text=originals[key]
                for path,a,z in fault['changes']:
                    if path==key:text=text.replace(a,z)
                paths[key].write_text(text)
            row=binary(fault['name'])
            steps.append(dict(label=fault['name'],program=row,expected=101,test=fault['test'],expected_assertion=fault['expected_assertion'],role='fresh_source_fault'))
        finally:
            for key in {c[0] for c in fault['changes']}:paths[key].write_text(originals[key])
    base.capacity.product.whole.verify_inputs(work,frozen)
    for name in ['audio','shared','io','services','frame-wire']:
        src=ROOT/'engine/crates'/name/'src';private=package/'src' if name=='audio' else package/'local-deps'/name/'src'
        assert all(p.read_bytes()==(private/p.relative_to(src)).read_bytes() for p in src.rglob('*') if p.is_file()),name
    (work/'execution-plan.json').write_text(json.dumps(dict(programs=programs,steps=steps,clippy=delta,source_inputs=frozen,private_sources_restored=True),indent=2)+'\n')
    print('Immutable HTTP M4A execution plan prepared',flush=True)


def execute(work, baseline):
    os.environ['MIGO_DUMP_HTTP_SPOOL_PCM']='1'
    os.environ['MIGO_DUMP_HTTP_M4A_PCM']='1'
    os.environ['MIGO_DUMP_HTTP_CONTROL_PCM']='1'
    os.environ['MIGO_DUMP_RANGE_CONTROL_PCM']='1'
    prior.prior.prior.prior.execute(work,baseline)
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
    assert result['suites']['full-debug']==result['suites']['full-release']==dict(passed=909,ignored=3)
    assert result['suites']['http-control-pcm-debug']==result['suites']['http-control-pcm-release']==dict(passed=8,ignored=0)
    assert len(result['controls'])==9 and len(result['original_red'])==1
    a=index.pcm(work/'range-control-pcm-debug.log','MIGO_RANGE_CONTROL_PCM ',116)
    b=index.pcm(work/'range-control-pcm-release.log','MIGO_RANGE_CONTROL_PCM ',116)
    assert a==b
    for key,value in a.items():assert value==old[key.removeprefix('range/')],key
    result['range_control_pcm']=dict(outputs=116,samples=sum(map(len,a.values())),all_dev_o3_bits_equal=True,previous_file_complete_traversals_equal=116)
    assert result['suites']['range-control-pcm-debug']==result['suites']['range-control-pcm-release']==dict(passed=8,ignored=0)
    path.write_text(json.dumps(result,indent=2)+'\n')
    print('Pinned Range controls, complete PCM and original reader/CPU custody verified',flush=True)


if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__)
    p.add_argument('--output',type=Path,required=True)
    p.add_argument('--target-dir',type=Path)
    p.add_argument('--before-source',type=Path)
    p.add_argument('--baseline-native',type=Path,required=True)
    p.add_argument('--execute',action='store_true')
    a=p.parse_args()
    if a.execute:execute(a.output.resolve(),a.baseline_native.resolve())
    else:
        assert a.target_dir and a.before_source
        build(a.output.resolve(),a.target_dir.resolve(),a.before_source.resolve(),a.baseline_native.resolve())
