#!/usr/bin/env python3
"""Verify session HTTP spool selection, complete PCM and original Fs/CPU custody.

Offline fresh builds precede SHA-bound loopback execution. No Git or shipping claim.
"""
from __future__ import annotations
import argparse, hashlib, json, os, re, shutil
from pathlib import Path
import check_audio_http_m4a as prior

index = prior.index
ROOT = index.ROOT
PREFIX = 'streaming_http::tests::'
PCM = prior.PCM
SPOOL = PREFIX+'preparation::'
SPOOL_PCM = SPOOL+'unpinned_and_range_ignoring_m4a_use_one_original_body_and_preserve_complete_pcm'
ACTOR = 'audio_thread::http_spool_tests::admitted_url_carries_session_spool_through_the_actual_audio_actor_and_source_identity'
SERVICE = 'audio::http_spool::tests::'
LARGE = PREFIX+'range_tail_plays_beyond_whole_file_limit_without_waiting_for_original_body_eof'
CUSTODY = PREFIX+'cancelled_real_index_handoff_retains_storage_and_credit_behind_the_original_cpu'


def controls(originals):
    rows = []
    def add(name, changes, test, panic, crate='migo-audio'):
        for path, a, z in changes:
            assert a != z and originals[path].count(a) == 1, (name, path, 'anchor drift')
        rows.append(dict(name=name, changes=changes, test=test, expected_assertion=panic, crate=crate))
    add('spool-session-capability-lost', [('command', 'spool: sources.local_file.as_ref().map(LocalAudioFile::http_spool),', 'spool: None,')], ACTOR, 'actual actor lost session HTTP spool capability')
    add('spool-unpinned-refused', [('prepare', 'Err(range::Error::NoStrongValidator) => {}', 'Err(range::Error::NoStrongValidator) => return Err(error("unpinned disabled")),')], SPOOL_PCM, 'spooled HTTP M4A failed complete PCM')
    add('spool-ignored-range-refused', [('prepare', 'Ok(()) | Err(range::Error::RangeUnsupported) => {}', 'Ok(()) => {}, Err(range::Error::RangeUnsupported) => return Err(error("spool disabled")),')], SPOOL_PCM, 'spooled HTTP M4A failed complete PCM')
    add('spool-invalid-range-recovers', [('prepare', 'Err(cause) => return Err(error(cause)),\n                }', 'Err(_) => {},\n                }')], SPOOL+'valid_pinned_range_stays_progressive_and_capability_errors_never_fallback', 'invalid pinned Range fell back to successful PCM')
    add('spool-prefix-discarded', [('prepare', 'response,\n        client,\n        prefix,', 'response,\n        client,\n        prefix: VecDeque::new(),')], SPOOL_PCM, 'spooled HTTP M4A failed complete PCM')
    add('spool-length-captured-after-prefix', [('prepare', 'let body = body_for_spool(client, response, prefix, length);', 'let length = response.content_length(); let body = body_for_spool(client, response, prefix, length);')], SPOOL_PCM, 'spooled HTTP M4A failed complete PCM')
    add('spool-seal-before-eof', [('services', '                .await?;\n        }\n        let owner = session.owner.clone();', '                .await?;\n            break;\n        }\n        let owner = session.owner.clone();')], SPOOL+'fallback_cannot_publish_before_seal_and_cancellation_wakes_the_original_body_wait', 'spool published Ready or PCM before sealing full response')
    add('spool-fs-custody-returned-early', [('services', '        let work = self\n            .scheduler', '        let value = op()?;\n        let work = self\n            .scheduler'), ('services', '                let value = op()?;\n                check', '                check')], SPOOL+'cancelled_spool_append_holds_actual_loan_capacity_until_the_original_fs_job_finishes', 'cancelled append released actual loan capacity or credit before the original Fs worker returned')
    add('spool-sealed-slots-not-admitted', [('io', 'ReadAtFile::with_budget(file, limit, pools)', 'ReadAtFile::new(file, limit)')], SERVICE+'sealed_http_media_slots_use_existing_admission_and_retain_both_slot_credits', 'HTTP sealed media slots bypassed existing IO byte admission', 'migo-services')
    add('spool-http-box-credit-returned-early', [('prepare', '_ticket: migo_io::pools::ByteTicket,', '_ticket: Option<migo_io::pools::ByteTicket>,'), ('prepare', '_ticket: ticket,', '_ticket: { drop(ticket); None },')], SPOOL+'actual_http_spool_chunks_admit_exact_box_capacity_and_hold_it_until_free', 'HTTP spool transport loan lost its actual allocation admission')
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
        for tag,test,dump in [('control-pcm',prior.prior.prior.PREFIX,'control'),('origin-pcm',prior.prior.prior.prior.WINDOW,'origin'),('local-pcm',index.NATIVE,'local'),('file-pcm',index.FULL,'file'),('indexed-pcm',index.INDEX_FULL,'indexed'),('indexed-large',index.INDEX_LARGE,'indexed'),('package-small',prior.prior.prior.prior.product.SMALL,'package'),('package-large',prior.prior.prior.prior.product.LARGE,'package')]:
            steps.append(dict(label=tag+'-'+label,program=row,expected=0,test=test,dump=dump,exact=tag!='control-pcm'))
        steps.append(dict(label='large-file-'+label,program=row,expected=0,test=index.LARGE))
        row=binary('services-'+label,'migo-services',release)
        steps.append(dict(label='services-range-'+label,program=row,expected=0,test='network::http::range::',exact=False))
        steps.append(dict(label='services-spool-'+label,program=row,expected=0,test=SERVICE,exact=False))
        row=binary('io-'+label,'migo-io',release)
        for tag,selector in [('source-snapshot','source_snapshot::'),('io-spool','spool::tests::')]:
            steps.append(dict(label=tag+'-'+label,program=row,expected=0,test=selector,exact=False))
    paths=dict(command=package/'local-deps/services/src/audio.rs', prepare=package/'src/streaming_http_prepare.rs', services=package/'local-deps/services/src/audio_http_spool.rs', io=package/'local-deps/io/src/spool.rs', actor=package/'src/audio_thread.rs')
    originals={k:p.read_text() for k,p in paths.items()}
    actor=originals['actor']
    call='streaming::start_streaming_download_with_spool(\n                                        cpu.clone(),\n                                        client,\n                                        url.clone(),\n                                        state.clone(),\n                                        sample_rate,\n                                        spool,\n                                    )'
    assert actor.count(call)==1
    original_call=call.replace('start_streaming_download_with_spool','start_streaming_download').replace('                                        spool,\n','')
    try:
        paths['actor'].write_text(actor.replace(call,original_call))
        row=binary('original-url-actor-without-session-spool')
        steps.append(dict(label=row['name'],program=row,expected=101,test=ACTOR,expected_assertion='actual actor lost session HTTP spool capability',role='original_runtime_red'))
    finally:
        paths['actor'].write_text(actor)
    for fault in controls(originals):
        try:
            for key in {c[0] for c in fault['changes']}:
                text=originals[key]
                for path,a,z in fault['changes']:
                    if path==key:text=text.replace(a,z)
                paths[key].write_text(text)
            row=binary(fault['name'],fault['crate'])
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
    prior.prior.prior.execute(work,baseline)
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
    assert result['suites']['full-debug']==result['suites']['full-release']==dict(passed=891,ignored=3)
    for profile in ['debug','release']:
        for name,count in [('services-range',30),('services-spool',4),('source-snapshot',29)]:
            assert result['suites'][name+'-'+profile]==dict(passed=count,ignored=0)
    assert len(result['controls'])==10 and len(result['original_red'])==1
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
