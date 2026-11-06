#!/usr/bin/env python3
"""Verify actual bounded local M4A index IO, original CPU and player.

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

FILE_TEST='streaming_file::tests::'
SERVICE_TEST='audio::tests::file_stream::'
FULL=FILE_TEST+'sealed_file_cpu_publication_matches_complete_mature_pcm_for_every_sequential_codec'
LARGE=FILE_TEST+'actual_large_file_src_plays_all_pcm_beyond_both_old_whole_file_limits'
CUSTODY=FILE_TEST+'unpolled_producer_and_rejected_commands_keep_file_and_queue_credit_until_cpu_drop'
ERROR=FILE_TEST+'native_file_readiness_keeps_typed_errors_and_source_refusal_without_duplicate_events'

INDEX_FULL=FILE_TEST+'indexed::indexed_file_full_pcm_preserves_front_tail_co64_reorder_and_edits'
INDEX_LARGE=FILE_TEST+'indexed::actual_large_m4a_file_src_reaches_native_player_with_complete_pcm_and_identity'
INDEX_CPU=FILE_TEST+'indexed::original_cpu_really_destroys_the_exact_index_allocation_before_credit_returns'
INDEX_BAD=FILE_TEST+'indexed::malformed_or_oversized_tail_indexes_never_publish_successful_pcm_or_eof'
BOUNDS=SERVICE_TEST+'index_control_checks_bounds_and_admits_exact_storage_before_enqueuing'
BURST='streaming_file::index::tests::only_consumed_frames_renew_the_fixed_source_bound_reposition_burst'
FOREIGN='streaming_file::index::tests::original_consumer_progress_renews_without_banked_surplus_or_foreign_owners'
EXTENT='streaming_file::index::actual_extents::packet_offsets_are_checked_against_the_actual_media_atom_before_resume'

def controls(originals):
    rows=[]
    def add(name,changes,test,assertion,crate='audio'):
        for path,a,z in changes:assert a!=z and originals[path].count(a)==1,(name,path,'anchor drift')
        rows.append(dict(name=name,changes=changes,test=test,expected_assertion=assertion,crate=crate))
    add('index-input-admission-bypassed',[('file','.reserve_bytes(bytes as u64)','.reserve_bytes(0)')],BOUNDS,'index allocation bypassed existing IO byte admission','migo-services')
    add('index-control-zero-accepted',[('file','bytes == 0','false')],BOUNDS,'index control ignored its bounds before enqueue','migo-services')
    add('index-control-cancel-ignored',[('file','|| abort.is_cancelled()','|| false')],BOUNDS,'cancelled index was enqueued','migo-services')
    add('index-control-deadline-ignored',[('file','std::time::Instant::now() >= until','false')],BOUNDS,'expired index work returned successful bytes','migo-services')
    add('index-input-credit-returned-early',[('file','_lease: migo_io::pools::ByteTicket,','_lease: Option<migo_io::pools::ByteTicket>,'),('file','_lease: lease,','_lease: { drop(lease); None },')],INDEX_CPU,'index credit returned before original CPU destruction')
    add('index-budget-after-body-read',[('index','length > IndexOptions::default().max_index_bytes','false')],INDEX_BAD,'oversized index reached the unadmitted body reader')
    add('index-packet-outside-mdat-accepted',[('index','if span.kind != AtomKind::MediaData\n                    || packet.offset < span.body_offset','if false\n                    || packet.offset < span.body_offset')],EXTENT,'source accepted packet bytes outside actual mdat')
    add('index-reposition-work-mints-credit',[('index','let rendered = state.rendered_frames();','let rendered = state.bytes_downloaded.load(std::sync::atomic::Ordering::Relaxed);')],BURST,'non-render work minted local source reposition credit')
    add('index-reposition-counter-not-charged',[('index','.checked_sub(1)','.checked_sub(0)')],BURST,'non-render work minted local source reposition credit')
    add('index-reposition-foreign-owner-accepted',[('index','owner.as_ptr() != Arc::as_ptr(state)','false')],FOREIGN,'reposition credit accepted a foreign source')
    add('index-first-media-loan-lost',[('index','self.pending = Some(first);','drop(first); self.pending = None;')],INDEX_LARGE,'large M4A was refused by the actual native actor')
    add('index-media-resume-keeps-old-suffix',[('index','restart: true,','restart: false,')],INDEX_LARGE,'large M4A was refused by the actual native actor')
    add('publication-observer-before-pending',[('transport','result.is_pending()', 'true')],'streaming_transport::tests::publication_observer_reports_only_a_genuinely_pending_slot','ready publication slot was labeled blocked')
    return rows

def compare_clippy(before,after):
    raw_new=after-before;raw_removed=before-after;reworded=[]
    # rustc groups dead members in one diagnostic. Promoting rendered_frames
    # into the real source budget removes that member from the original group.
    # Preserve both raw Counters, and permit only this exact shrinking group.
    old=[k for k in raw_removed if k[2]=='dead_code' and 'rendered_frames' in k[3] and 'new_reusable' in k[3] and 'install_controls' in k[3]]
    new=[k for k in raw_new if k[2]=='dead_code' and k[3]=='associated items `new_reusable` and `install_controls` are never used']
    effective=raw_new.copy()
    if old or new:
        assert len(old)==len(new)==1 and raw_removed[old[0]]==raw_new[new[0]]==1
        a,z=old[0],new[0];assert a[1:3]==z[1:3] and set(z[4])<set(a[4]) and set(a[0])==set(z[0])=={'src/streaming.rs'}
        assert set(a[4])-set(z[4])=={'pub(crate) fn rendered_frames(&self) -> u64 {'}
        effective.subtract({z:1});effective+=Counter();reworded.append(dict(before=a,after=z,reason='Original unused-member group shrank: rendered_frames is now consumed by the actual File reposition budget. No new unused member or allow.'))
    result=dict(before=sum(before.values()),after=sum(after.values()),new_diagnostics=sum(effective.values()),new=[dict(key=k,count=v) for k,v in effective.items()],raw_counter_new=[dict(key=k,count=v) for k,v in raw_new.items()],raw_counter_removed=[dict(key=k,count=v) for k,v in raw_removed.items()],reworded_original_dead_code_groups=reworded)
    assert not effective,result
    return result

def owned_diagnostics(log,package):
    result=Counter()
    for key,count in wave.diagnostics(log,package).items():
        files=tuple(re.sub(r'^local-deps/(services|io|shared)/',r'crates/\1/',n) for n in key[0])
        if any(n.startswith(('crates/shared/src/audio_file','crates/shared/src/audio_channel','crates/shared/src/protocol/audio_cmd','crates/io/src/domain','crates/services/src/audio')) for n in files):result[(files,*key[1:])]+=count
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
    a=wave.diagnostics(before_clippy,before_clippy.parent/'probe');z=wave.diagnostics(work/'clippy.jsonl',package);delta=compare_clippy(a,z)
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
        steps += [dict(label='full-'+label,program=row,expected=0),dict(label='local-pcm-'+label,program=row,expected=0,test=NATIVE,dump='local'),dict(label='file-pcm-'+label,program=row,expected=0,test=FULL,dump='file'),dict(label='large-file-'+label,program=row,expected=0,test=LARGE),dict(label='indexed-pcm-'+label,program=row,expected=0,test=INDEX_FULL,dump='indexed'),dict(label='indexed-large-'+label,program=row,expected=0,test=INDEX_LARGE,dump='indexed')]
        row=other('services-'+label,'migo-services',release);steps.append(dict(label='services-'+label,program=row,expected=0,selector='audio::tests::'))
    row=other('source-snapshot','migo-io');steps.append(dict(label='source-snapshot',program=row,expected=0,selector='source_snapshot::tests::'))
    paths={'file':package/'local-deps/services/src/audio_file_stream.rs','service':package/'local-deps/services/src/audio.rs','channel':package/'local-deps/shared/src/audio_channel.rs','native':package/'src/streaming_file.rs','actor':package/'src/audio_thread.rs','index':package/'src/streaming_file_index.rs','transport':package/'src/streaming_transport.rs'};originals={n:p.read_text() for n,p in paths.items()};faults=controls(originals)
    # Restore the exact old actor's large-M4A refusal, including CPU custody.
    old=(before/'engine/crates/audio/src/audio_thread.rs').read_text();start=old.index('    if crate::decoder::is_m4a(source.prefix()) {',old.index('pub(crate) fn start_inner_file_stream('));end=old.index('    let (tx, rx)',start);branch=old[start:end]
    marker='    let (tx, rx) = streaming::stream_channel();';start=originals['actor'].index('pub(crate) fn start_inner_file_stream(');point=originals['actor'].index(marker,start)
    try:
        paths['actor'].write_text(originals['actor'][:point]+branch+originals['actor'][point:]);row=native('original-m4a-file-refusal');steps.append(dict(label='original-m4a-file-refusal',program=row,expected=101,test=INDEX_LARGE,expected_assertion='large M4A was refused by the actual native actor',role='original_runtime_red'))
    finally:paths['actor'].write_text(originals['actor'])
    for fault in faults:
        try:
            for n,a,z in fault['changes']:
                current=paths[n].read_text();assert current.count(a)==1;paths[n].write_text(current.replace(a,z))
            row=native(fault['name']) if fault['crate']=='audio' else other(fault['name'],fault['crate']);steps.append({k:v for k,v in fault.items() if k not in ['changes','crate']}|dict(label=fault['name'],program=row,expected=101,role='fresh_source_fault'));print('Fresh fault prepared',fault['name'],flush=True)
        finally:
            for n,_,_ in fault['changes']:paths[n].write_text(originals[n])
    base.capacity.product.whole.verify_inputs(work,frozen)
    for name in ['audio','shared','io','services','frame-wire']:
        source=ROOT/'engine/crates'/name/'src';private=package/'src' if name=='audio' else package/'local-deps'/name/'src';assert all(p.read_bytes()==(private/p.relative_to(source)).read_bytes() for p in source.rglob('*') if p.is_file()),name
    (work/'execution-plan.json').write_text(json.dumps(dict(programs=programs,steps=steps,clippy=delta,owned_clippy=owned,source_inputs=frozen,private_sources_restored=True),indent=2)+'\n');print('Immutable local File execution plan prepared',flush=True)

def pcm(path,prefix='MIGO_LOCAL_FILE_PCM ',expected=12):
    rows={}
    for line in path.read_text().splitlines():
        if prefix in line:
            harness, payload = line.split(prefix, 1)
            assert not harness or (harness.startswith('test ') and harness.endswith(' ... ')), (path, harness)
            key, values = payload.split(' ', 1)
            assert key not in rows
            rows[key] = json.loads(values)
    assert len(rows)==expected,(path,len(rows));return rows

def execute(work):
    plan=json.loads((work/'execution-plan.json').read_text());sha=lambda p:hashlib.sha256(p.read_bytes()).hexdigest()
    for n,h in plan['source_inputs'].items():assert sha(ROOT/n)==sha(work/'source-inputs'/n)==h,n
    suites={};faults=[];reds=[]
    for step in plan['steps']:
        p=step['program'];binary=Path(p['path']);assert sha(binary)==p['sha256'];args=[str(binary)]+(['--exact',step['test'],'--nocapture'] if step.get('test') else [step['selector'],'--nocapture'] if step.get('selector') else []);env=os.environ.copy()
        if step.get('dump'):env[{'local':'MIGO_DUMP_LOCAL_FILE_PCM','file':'MIGO_DUMP_FILE_STREAM_PCM','indexed':'MIGO_DUMP_INDEXED_FILE_PCM'}[step['dump']]]='1'
        log=work/(step['label']+'.log')
        with log.open('w') as f:q=subprocess.run(args,cwd=work/'probe',env=env,stdout=f,stderr=subprocess.STDOUT,timeout=900)
        (work/(step['label']+'-exit.txt')).write_text(str(q.returncode)+'\n');text=log.read_text();assert q.returncode==step['expected'],(step['label'],q.returncode)
        if q.returncode:
            assert all(s in text for s in ['running 1 test','panicked at','1 failed',step['test'],step['expected_assertion']]),step['label'];(faults if step['role']=='fresh_source_fault' else reds).append(dict(name=step['label'],test=step['test'],expected_assertion=step['expected_assertion'],exit=101,sha256=p['sha256']))
        else:
            m=re.search(r'test result: ok\. ([1-9][0-9]*) passed; 0 failed; (\d+) ignored;',text);assert m,step['label'];suites[step['label']]=dict(passed=int(m[1]),ignored=int(m[2]))
        print('Executed',step['label'],q.returncode,flush=True)
    a=pcm(work/'local-pcm-debug.log');z=pcm(work/'local-pcm-release.log');assert a==z,'local source full PCM dev/O3 differs'
    f=pcm(work/'file-pcm-debug.log','MIGO_FILE_STREAM_PCM ',8);g=pcm(work/'file-pcm-release.log','MIGO_FILE_STREAM_PCM ',8);assert f==g,'sealed file complete PCM dev/O3 differs'
    large={}
    for label in ['debug','release']:
        text=(work/('large-file-'+label+'.log')).read_text();m=re.search(r'MIGO_LARGE_FILE_PCM frames=(\d+) bits=(\d+) bounded_pcm_bytes=(\d+)',text);assert m and int(m[1])==16777345 and int(m[2])==1048576000 and int(m[3])<4*1024*1024;large[label]=dict(samples=int(m[1]),bits=int(m[2]),retained_pcm_bytes=int(m[3]),every_sample_checked=True,tail_and_end_checked=True)
    indexed={}
    for kind,count in [('pcm',40),('large',10)]:
        v=pcm(work/('indexed-'+kind+'-debug.log'),'MIGO_INDEXED_FILE_PCM ',count);w=pcm(work/('indexed-'+kind+'-release.log'),'MIGO_INDEXED_FILE_PCM ',count);assert v==w,'indexed M4A complete dev/O3 PCM bits differ';indexed[kind]=dict(outputs=len(v),samples=sum(map(len,v.values())),all_dev_o3_bits_equal=True)
    (work/'summary.json').write_text(json.dumps(dict(programs=plan['programs'],suites=suites,controls=faults,original_red=reds,clippy=plan['clippy'],owned_clippy=plan['owned_clippy'],pcm=dict(outputs=len(a),samples=sum(map(len,a.values())),all_dev_o3_bits_equal=True),file_pcm=dict(outputs=len(f),samples=sum(map(len,f.values())),all_dev_o3_bits_equal=True),large_file=large,indexed_file_pcm=indexed,source_inputs_unchanged=True,private_sources_restored=True),indent=2)+'\n');print('Actual indexed M4A File playback verified',flush=True)

if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--output',type=Path,required=True);p.add_argument('--target-dir',type=Path);p.add_argument('--before-source',type=Path);p.add_argument('--before-clippy',type=Path);p.add_argument('--before-services-clippy',type=Path);p.add_argument('--execute',action='store_true');a=p.parse_args()
    if a.execute:execute(a.output.resolve())
    else:
        assert all([a.target_dir,a.before_source,a.before_clippy,a.before_services_clippy]);build(a.output.resolve(),a.target_dir.resolve(),a.before_source.resolve(),a.before_clippy.resolve(),a.before_services_clippy.resolve())
