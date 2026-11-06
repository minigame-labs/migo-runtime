#!/usr/bin/env python3
"""Verify public controls over actual session-sealed HTTP M4A sources.

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


def controls(originals):
    rows=[]
    def add(name,changes,test,panic):
        for path,a,z in changes:assert a!=z and originals[path].count(a)==1,(name,path,'anchor drift')
        rows.append(dict(name=name,changes=changes,test=test,expected_assertion=panic))
    add('url-retained-before-capability',[('state','self.retain_after_eof && (!self.require_control_port || self.controls.get().is_some())','self.retain_after_eof')],POLICY,'URL retained an unselected source before its real control port')
    add('url-late-port-install-refused',[('state','if !self.accepts_controls() || self.is_cancelled() {','if !self.retains_source() || self.is_cancelled() {')],POLICY,'URL could not install its selected source port')
    add('sealed-http-controls-not-installed',[('file','    let controls = state\n        .accepts_controls()','    let controls = state\n        .retains_source()')],SEEK,'HTTP sealed source never activated its original control port')
    add('url-initial-loop-intent-lost',[('player','.filter(|s| s.accepts_controls())','.filter(|s| s.control_sender().is_some())')],LOADING,'controlled source published PCM outside its reported frame extent')
    add('url-loading-loop-update-lost',[('player','            if !self.shared.is_loaded()\n                && self\n                    .stream_state\n                    .as_ref()\n                    .is_some_and(|s| s.accepts_controls())\n            {\n                self.loop_intent = Some(enabled);\n            }\n','')],LOADING,'controlled source published PCM outside its reported frame extent')
    add('sealed-http-decoder-not-retained',[('file','FileDecoder::new(rate, length, m4a, true, file)','FileDecoder::new(rate, length, m4a, false, file)')],SEEK,'actual controlled file emitted Error')
    add('sealed-http-final-source-dropped-on-io',[('file','_owner: OffWorker<Arc<dyn AudioFile>>,','_owner: OffWorker<()>,'),('file','_owner: OffWorker::new(file.clone(), cpu.clone()),','_owner: OffWorker::new((), cpu.clone()),'),('index','_owner: OffWorker::new(file.clone(), f.host.audio_cpu()),','_owner: OffWorker::new((), f.host.audio_cpu()),')],PHYSICAL,'HTTP source slots did not physically free on original CPU retirement')
    add('retired-url-control-port-accepted',[('state','if !self.accepts_controls() || self.is_cancelled() {','if !self.accepts_controls() {')],POLICY,'retired URL installed a new source control port')
    add('fixed-mode-retention-needs-port',[('state','self.retain_after_eof && (!self.require_control_port || self.controls.get().is_some())','self.retain_after_eof && self.controls.get().is_some()')],POLICY,'fixed reusable mode lost its retention contract')
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
        for tag,test,dump in [('control-pcm',prior.prior.prior.prior.PREFIX,'control'),('origin-pcm',prior.prior.prior.prior.prior.WINDOW,'origin'),('local-pcm',index.NATIVE,'local'),('file-pcm',index.FULL,'file'),('indexed-pcm',index.INDEX_FULL,'indexed'),('indexed-large',index.INDEX_LARGE,'indexed'),('package-small',prior.prior.prior.prior.prior.product.SMALL,'package'),('package-large',prior.prior.prior.prior.prior.product.LARGE,'package')]:
            steps.append(dict(label=tag+'-'+label,program=row,expected=0,test=test,dump=dump,exact=tag!='control-pcm'))
        steps.append(dict(label='large-file-'+label,program=row,expected=0,test=index.LARGE))
    paths=dict(state=package/'src/streaming.rs',file=package/'src/streaming_file.rs',player=package/'src/inner_audio.rs',actor=package/'src/audio_thread.rs',index=package/'src/streaming_file_index.rs')
    originals={k:p.read_text() for k,p in paths.items()}
    old_actor=(before/'engine/crates/audio/src/audio_thread.rs').read_text()
    assert old_actor!=originals['actor'] and old_actor.replace('let state = StreamingState::new();\n                                    let rx = streaming::start_streaming_download_with_spool(', 'let state = StreamingState::new_url();\n                                    let rx = streaming::start_streaming_download_with_spool(')==originals['actor']
    try:
        paths['actor'].write_text(old_actor)
        row=binary('original-single-pass-url-actor')
        steps.append(dict(label=row['name'],program=row,expected=101,test=ACTUAL,expected_assertion='actual URL actor did not retain the sealed source at EOF',role='original_runtime_red'))
    finally:paths['actor'].write_text(originals['actor'])
    for fault in controls(originals):
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
    assert result['suites']['full-debug']==result['suites']['full-release']==dict(passed=900,ignored=3)
    assert result['suites']['http-control-pcm-debug']==result['suites']['http-control-pcm-release']==dict(passed=8,ignored=0)
    assert len(result['controls'])==9 and len(result['original_red'])==1
    path.write_text(json.dumps(result,indent=2)+'\n')
    print('HTTP spool source selection, complete PCM and original Fs/CPU custody verified',flush=True)


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
