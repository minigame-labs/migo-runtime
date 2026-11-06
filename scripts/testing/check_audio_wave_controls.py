#!/usr/bin/env python3
"""Verify public sealed WAVE controls using the mature RIFF port and original CPU.

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
WAVE = 'streaming_file::tests::wave_controls::'
SEEK = WAVE+'public_wave_seek_restart_and_exact_endpoint_keep_every_pcm_sample'
LOOP = WAVE+'wave_loops_with_metadata_keep_global_sinc_phase_and_consumer_cutovers'
LIVE = WAVE+'live_wave_source_seek_discards_the_original_unaccepted_encoded_suffix'
ACK = WAVE+'wave_refusal_and_actual_consumer_ack_preserve_original_revision'
DEADLINE = WAVE+'wave_absolute_control_deadline_covers_the_first_actual_media_read'
PHYSICAL = 'streaming_codecs::wave::retirement::wave_eof_retains_actual_capacities_until_original_cpu_really_destroys_them'
ACTUAL = WAVE+'actual_large_wave_reclaims_pcm_then_reopens_the_sealed_source_and_plays_the_full_tail'

def controls(originals, package):
    rows=[]
    def add(name,changes,test,panic):
        for path,a,z in changes:assert a!=z and originals[path].count(a)==1,(name,path,'anchor drift')
        rows.append(dict(name=name,changes=changes,test=test,expected_assertion=panic))
    add('wave-decoder-not-retained',[('wave','        d.retain();','        // retained source omitted')],SEEK,'actual controlled file emitted Error')
    a=originals['wave'];begin=a.index('    fn resume_buffered(');end=a.index('    fn validate_codec(',begin)
    # Force real File IO for a seek already covered by retained bytes.
    add('wave-retained-buffer-replay-disabled',[('wave',a[begin:end],a[begin:end].split('{',1)[0]+'{ let _=(input,head); Ok(None) }\n')],WAVE+'retained_wave_restarts_while_both_original_file_slots_are_in_flight','retained WAVE restart requested saturated source IO')
    add('wave-buffered-tail-coordinate-lost',[('wave','.checked_add(self.tail_in_window as u64)','.checked_add(0)')],PHYSICAL,'WAVE EOF restart did not preserve retained input coordinates')
    add('wave-consumer-ack-skipped',[('handshake','tokio::time::timeout_at(deadline, receipt)','tokio::time::timeout_at(deadline, async { drop(receipt); Ok::<(), oneshot::error::RecvError>(()) })')],ACK,'WAVE dropped actual consumer ACK')
    add('wave-old-encoded-suffix-retained',[('file_control','let restart = request.is_some() || terminal;','let restart = terminal;')],LIVE,'WAVE control complete PCM')
    add('wave-control-read-deadline-renewed',[('file_wave','.loan(offset, count, false, state, deadline)','.loan(offset, count, false, state, deadline + std::time::Duration::from_secs(30))')],DEADLINE,'file control media read renewed or ignored its original deadline')
    add('wave-cancel-keeps-original-allocations',[('packet','        if cancel.load(Ordering::Acquire) {\n            self.owner = None;','        if cancel.load(Ordering::Acquire) {\n            // physical storage incorrectly survives cancellation')],PHYSICAL,'original CPU did not really free retained replay buffers')
    add('wave-library-resume-accepts-wrong-offset',[('riff','if self.failed || self.pending != Some(offset) {','if self.failed {')],'wave::streaming::seek_tests::mismatched_resume_and_parser_errors_cannot_restart_the_representation','assertion failed: p.resume_at(offset + 4).is_err()')
    rows[-1]['crate']='riff'
    add('wave-unknown-source-controls-installed',[('handshake','if state.timeline().is_some() {','if true {')],WAVE+'unknown_length_wave_keeps_single_pass_eof_and_existing_buffered_controls','unknown WAVE installed unavailable source controls')
    add('wave-source-bound-not-checked',[('riff','self.object_len.is_some_and(|length| end > length)','self.object_len.is_some_and(|_length| false)')],WAVE+'truncated_pinned_wave_refuses_before_ready_or_source_control_installation','truncated pinned WAVE published successful PCM or Ready')
    return rows


def prepare_libraries(package,work,env,before,delta):
    base=index.base
    common=base.capacity.product.whole.support.clock.common
    libraries=base.capacity.product.whole.support.codecs.library_patches(ROOT,work)
    library_proof={}
    for name,folder in libraries.items():
        root=ROOT/'engine/third_party'/folder.name
        if name=='symphonia-bundle-mp3':
            assert not root.exists();continue
        inventory=lambda folder:{str(p.relative_to(folder)):common.sha(p) for p in folder.rglob('*') if p.is_file()}
        assert inventory(root)==inventory(folder),name
        library_proof[name]=inventory(root)
    (work/'product-library-replay.json').write_text(json.dumps(library_proof,indent=2)+'\n')
    assert len(library_proof)==8
    root_riff=libraries['symphonia-format-riff'];private_riff=package/'private-vendor'/root_riff.name;shutil.copytree(root_riff,private_riff);libraries['symphonia-format-riff']=private_riff
    manifest=package/'Cargo.toml';text=manifest.read_text().replace(str(ROOT/'engine/third_party/symphonia-format-riff-0.6.1'),str(private_riff)).replace('[workspace]\n','[workspace]\nmembers=[".","private-vendor/symphonia-format-riff-0.6.1"]\ndefault-members=["."]\n')
    # Workspace membership bypasses the dependency wildcard. Match the recorded
    # Root role explicitly, including any future member-specific policy.
    role=json.loads((work/'debug-profile-policy.json').read_text())['expected']['symphonia-format-riff']
    text+='\n[profile.dev.package."symphonia-format-riff"]\nopt-level='+role['opt_level']+'\n'
    manifest.write_text(text)
    source=private_riff/'src/wave/streaming.rs';final=source.read_text()
    try:
        source.write_text((before/'engine/third_party/symphonia-format-riff-0.6.1/src/wave/streaming.rs').read_text())
        assert common.run(['cargo','clippy','-p','symphonia-format-riff','--lib','--tests','--all-features','--offline','--locked','--message-format=json'],work/'riff-clippy-before.jsonl',package,env,timeout=600)==0
    finally:source.write_text(final)
    assert common.run(['cargo','clippy','-p','symphonia-format-riff','--lib','--tests','--all-features','--offline','--locked','--message-format=json'],work/'riff-clippy-current.jsonl',package,env,timeout=600)==0
    a=index.wave.diagnostics(work/'riff-clippy-before.jsonl',package);z=index.wave.diagnostics(work/'riff-clippy-current.jsonl',package);assert not(z-a),(z-a)
    delta['riff_all_features']=dict(before=sum(a.values()),after=sum(z.values()),new_diagnostics=0,removed=[dict(key=k,count=v) for k,v in (a-z).items()])
    (work/'clippy-delta.json').write_text(json.dumps(delta,indent=2)+'\n')
    print('RIFF all-features stable Clippy',delta['riff_all_features'],flush=True)
    return libraries,library_proof


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
    libraries,library_proof=prepare_libraries(package,work,env,before,delta)
    programs,steps=[],[]
    def binary(name,crate='migo-audio',release=False):
        if crate=='migo-audio':
            path=base.capacity.product.src.build(package,work,env,name,release)
        else:
            (libraries['symphonia-format-riff']/'src/lib.rs').touch() if crate=='symphonia-format-riff' else (package/'local-deps'/crate.removeprefix('migo-')/'src/lib.rs').touch()
            cmd=['cargo','test','-p',crate,'--lib','--offline','--locked','--no-run','--message-format=json']
            if crate=='symphonia-format-riff':cmd.append('--all-features')
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
        steps.append(dict(label='wave-control-pcm-'+label,program=row,expected=0,test=WAVE,exact=False))
        for tag,test,dump in [('control-pcm',prior.prior.prior.prior.PREFIX,'control'),('origin-pcm',prior.prior.prior.prior.prior.WINDOW,'origin'),('local-pcm',index.NATIVE,'local'),('file-pcm',index.FULL,'file'),('indexed-pcm',index.INDEX_FULL,'indexed'),('indexed-large',index.INDEX_LARGE,'indexed'),('package-small',prior.prior.prior.prior.prior.product.SMALL,'package'),('package-large',prior.prior.prior.prior.prior.product.LARGE,'package')]:
            steps.append(dict(label=tag+'-'+label,program=row,expected=0,test=test,dump=dump,exact=tag!='control-pcm'))
        steps.append(dict(label='large-file-'+label,program=row,expected=0,test=index.LARGE))
    for release,label in [(False,'debug'),(True,'release')]:
        row=binary('riff-'+label,crate='symphonia-format-riff',release=release)
        steps.append(dict(label='riff-'+label,program=row,expected=0,exact=False))
    paths={k:package/'src'/n for k,n in dict(wave='streaming_codecs/wave.rs',packet='streaming_codecs/packet.rs',handshake='streaming_source_control.rs',file_wave='streaming_file_wave.rs',file_control='streaming_file_control.rs',actor='audio_thread.rs').items()}
    paths['riff']=libraries['symphonia-format-riff']/'src/wave/streaming.rs'
    originals={k:p.read_text() for k,p in paths.items()}
    original=(before/'engine/crates/audio/src/audio_thread.rs').read_text();assert original!=originals['actor']
    try:
        paths['actor'].write_text(original)
        row=binary('original-single-pass-wave-selector')
        steps.append(dict(label=row['name'],program=row,expected=101,test=ACTUAL,expected_assertion='actual WAVE selector did not activate source controls',role='original_runtime_red'))
    finally:paths['actor'].write_text(originals['actor'])
    for fault in controls(originals,package):
        try:
            for key in {c[0] for c in fault['changes']}:
                text=originals[key]
                for path,a,z in fault['changes']:
                    if path==key:text=text.replace(a,z)
                paths[key].write_text(text)
            row=binary(fault['name'],crate='symphonia-format-riff' if fault.get('crate')=='riff' else 'migo-audio')
            steps.append(dict(label=fault['name'],program=row,expected=101,test=fault['test'],expected_assertion=fault['expected_assertion'],role='fresh_source_fault'))
        finally:
            for key in {c[0] for c in fault['changes']}:paths[key].write_text(originals[key])
    assert all(common.sha(ROOT/'engine/third_party'/libraries[name].name/n)==common.sha(libraries[name]/n)==h for name,proof in library_proof.items() for n,h in proof.items())
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
    os.environ['MIGO_DUMP_WAVE_CONTROL_PCM']='1'
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
    assert result['suites']['full-debug']==result['suites']['full-release']==dict(passed=920,ignored=3)
    assert result['suites']['http-control-pcm-debug']==result['suites']['http-control-pcm-release']==dict(passed=8,ignored=0)
    assert len(result['controls'])==10 and len(result['original_red'])==1
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
    path.write_text(json.dumps(result,indent=2)+'\n')
    print('Sealed WAVE controls, complete PCM and original CPU custody verified',flush=True)


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
