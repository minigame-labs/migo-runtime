#!/usr/bin/env python3
"""Build real product progressive-codec tests and fresh executed fault plans.

Builds and private source mutations stay offline in the sandbox. The separate
--execute phase only runs SHA-bound retained ELFs; loopback HTTP tests may need
the environment's local-socket permission. No Git command is used.
"""
from __future__ import annotations
import argparse
import hashlib
import json
from pathlib import Path
import re
import shutil
import subprocess
import os
import check_audio_command_ownership as base
import check_audio_oboe_callback as backend
import check_audio_waveshaper_storage as wave

ROOT=base.ROOT
PREFIX='streaming_codecs::tests::'
PCM=PREFIX+'partitioned_product_codecs_preserve_complete_pcm_and_loan_identity'
HTTP=PREFIX+'real_http_payload_selects_the_product_codec_and_preserves_all_pcm'


def ignore_contract():
    text=(ROOT/'.gitignore').read_text()
    required=[f'!engine/third_party/{name}-0.6.1/{file}' for name in ['symphonia-codec-aac','symphonia-format-isomp4'] for file in ['Cargo.lock','Cargo.toml.orig']]
    assert all(line in text.splitlines() for line in required), 'required published AAC metadata still ignored'
    # Withholding any required exception is a rejected ordinary-file rule set.
    assert all(not all(rule in text.replace(line+'\n','').splitlines() for rule in required) for line in required)


def declarations(originals):
    rows=[]
    def add(name,changes,test,message):
        for path,a,z in changes:assert a!=z and originals[path].count(a)==1,(name,path,'anchor drift')
        rows.append(dict(name=name,changes=changes,test=test if '::' in test else PREFIX+test,expected_assertion=message))
    complete='product progressive codec failed complete PCM'
    packet='streaming_codecs/packet.rs';auto='streaming_codecs/mod.rs';sniff='streaming_codecs/sniff.rs'
    factory='move || crate::streaming_codecs::AutoDecoder::new(target_sample_rate),'
    add('public-target-not-forwarded',[('streaming.rs',factory,'move || crate::streaming_codecs::AutoDecoder::new(48000),')],HTTP,'HTTP stream lost submitted format')
    add('vorbis-misrouted-as-mp3',[(sniff,'Kind::Vorbis','Kind::Mp3')],PCM,'product progressive complete PCM changed')
    add('adts-misrouted-as-mp3',[(sniff,'Kind::Adts','Kind::Mp3')],PCM,'product progressive complete PCM changed')
    add('sniff-prefix-not-replayed',[(auto,'replay: 0,','replay: 8,')],PCM,complete)
    add('metadata-body-treated-as-audio',[(sniff,'self.skip = bytes - 10;','self.skip = 0;')],
        'one_byte_headers_and_large_tags_never_use_tag_payload_as_format','ADTS metadata was decoded or dispatched as audio')
    add('metadata-limit-disabled',[(sniff,'.filter(|n| *n <= 4 * 1024 * 1024)','.filter(|_| true)')],
        'one_byte_headers_and_large_tags_never_use_tag_payload_as_format','oversized metadata was not refused at its header')
    add('whole-tag-boundary-regressed',[('decoder/mod.rs','for tag in 0..=32 {','for tag in 0..32 {')],
        'one_byte_headers_and_large_tags_never_use_tag_payload_as_format','whole-file tag boundary disagrees with streaming')
    add('six-channel-publication-split',[(packet,'ready.min(STEP_SAMPLES) / channels * channels','ready.min(STEP_SAMPLES)')],PCM,'packet publication split a channel frame')
    publish='pcm.extend_from_slice(&owner.output[owner.output_head..owner.output_head + n]);'
    add('publication-loan-swapped',[(packet,publish,'std::mem::swap(pcm, &mut owner.output);')],PCM,'stream codec replaced or grew the publication loan allocation')
    add('packet-pcm-silenced',[(packet,publish,'pcm.extend((0..n).map(|_| 0.0));')],PCM,'product progressive complete PCM changed')
    marker='''Stream::new(
                    owner.decoder.rate,
                    owner.target,'''
    add('sinc-target-fixed',[(packet,marker,marker.replace('owner.target,','44100,'))],PCM,'product progressive complete PCM changed')
    add('cancel-state-retained',[(auto,'self.codec = None;\n            self.error = Some(ErrorCode::Cancelled);','self.error = Some(ErrorCode::Cancelled);')],
        'original_cpu_cancel_retires_actual_packet_allocations_after_the_running_step','original CPU did not actually free packet capacity bytes')
    add('vorbis-buffered-packet-lost',[('streaming_codecs/vorbis.rs','self.0.packet(input)','None')],PCM,complete)
    add('wave-metadata-bounds-disabled',[('streaming_codecs/wave.rs','const MAX_TRAILING_BYTES: usize = 4 * 1024 * 1024;','const MAX_TRAILING_BYTES: usize = 8 * 1024 * 1024;'),('streaming_codecs/wave.rs','const MAX_DISCARDED_BYTES: Option<usize> = Some(4 * 1024 * 1024);','const MAX_DISCARDED_BYTES: Option<usize> = None;')],
        'codec_errors_and_cancellation_remain_terminal_without_pcm','progressive WAVE metadata budget was ignored')
    body=wave.method(originals[packet],'if owner.codec_done && !owner.filtered {')
    damaged=re.sub(r'owner.filtered = if let Some\(pipeline\)[\s\S]*?\n            };','owner.filtered = true;',body)
    assert body!=damaged
    add('sinc-flush-skipped',[(packet,body,damaged)],PCM,'product progressive complete PCM changed')
    return rows


def build(work:Path,target:Path,before_clippy:Path):
    ignore_contract()
    package,env,frozen=base.prepare(work,target)
    product=base.capacity.product;common=product.whole.support.clock.common
    output=package/'src/output.rs';saved=output.read_text()
    try:
        output.write_text(backend.ports(saved))
        assert common.run(['cargo','clippy','--lib','--tests','--offline','--locked','--message-format=json'],work/'clippy.jsonl',package,env,timeout=600)==0
    finally:output.write_text(saved)
    old=wave.diagnostics(before_clippy,before_clippy.parent/'probe');new=wave.diagnostics(work/'clippy.jsonl',package)
    delta=dict(before=sum(old.values()),after=sum(new.values()),new_diagnostics=sum((new-old).values()),new=[dict(key=k,count=v) for k,v in (new-old).items()]);(work/'clippy-delta.json').write_text(json.dumps(delta,indent=2)+'\n');assert not(new-old),delta
    print('Final stable Clippy Counter:',delta,flush=True)
    keys=['streaming.rs','streaming_codecs/mod.rs','streaming_codecs/packet.rs','streaming_codecs/sniff.rs','streaming_codecs/vorbis.rs','decoder/mod.rs','decoder/aac/incremental.rs','streaming_codecs/wave.rs'];paths={n:package/'src'/n for n in keys};originals={n:p.read_text() for n,p in paths.items()};controls=declarations(originals);programs=[];plan=[]
    def program(name,release=False):
        binary=product.src.build(package,work,env,name,release);row=dict(name=name,path=str(binary),sha256=common.sha(binary),pruned=False);programs.append(row);return row
    first=program('positive-debug');base.capacity.profile.verify(work/'positive-debug-build.jsonl',json.loads((work/'debug-profile-policy.json').read_text()),work/'debug-profile-artifacts.json')
    plan += [dict(label='full-debug',program=first,expected=0),dict(label='complete-pcm-debug',program=first,expected=0,test=PCM,dump=True)]
    try:
        p=paths['streaming.rs'];p.write_text(originals['streaming.rs'].replace('move || crate::streaming_codecs::AutoDecoder::new(target_sample_rate),','move || Mp3StreamDecoder::new(target_sample_rate),'))
        row=program('original-public-dispatch');plan.append(dict(label='original-public-dispatch',program=row,expected=101,test=HTTP,expected_assertion='real product HTTP codec failed',role='original_runtime_red'))
    finally:paths['streaming.rs'].write_text(originals['streaming.rs'])
    for control in controls:
        try:
            for key,a,z in control['changes']:
                current=paths[key].read_text();assert current.count(a)==1;paths[key].write_text(current.replace(a,z))
            row=program(control['name']);plan.append({k:v for k,v in control.items() if k!='changes'}|dict(label=control['name'],program=row,expected=101,role='fresh_source_fault'))
            (work/'partial-plan.json').write_text(json.dumps(plan,indent=2)+'\n');print('Fresh executable prepared:',control['name'],flush=True)
        finally:
            for key,_,_ in control['changes']:paths[key].write_text(originals[key])
    last=program('positive-release',True);plan += [dict(label='full-release',program=last,expected=0),dict(label='complete-pcm-release',program=last,expected=0,test=PCM,dump=True)]
    product.whole.verify_inputs(work,frozen)
    for name in ['audio','shared','io','services','frame-wire']:
        source=ROOT/'engine/crates'/name/'src';private=package/'src' if name=='audio' else package/'local-deps'/name/'src';assert all(p.read_bytes()==(private/p.relative_to(source)).read_bytes() for p in source.rglob('*') if p.is_file()),name
    (work/'execution-plan.json').write_text(json.dumps(dict(programs=programs,steps=plan,clippy=delta,source_inputs=frozen,private_sources_restored=True,source_fault_count=len(controls),root_dev_profile_verified=True),indent=2)+'\n');print('All immutable execution plans prepared',flush=True)


def pcm(path):
    rows={}
    for line in path.read_text().splitlines():
        if line.startswith('MIGO_PROGRESSIVE_PCM '):
            _,key,values=line.split(' ',2);assert key not in rows;rows[key]=json.loads(values)
    assert len(rows)==99,(path,len(rows));return rows


def execute(work:Path):
    plan=json.loads((work/'execution-plan.json').read_text());sha=lambda p:hashlib.sha256(p.read_bytes()).hexdigest()
    for n,h in plan['source_inputs'].items():assert sha(ROOT/n)==sha(work/'source-inputs'/n)==h,n
    suites={};controls=[];reds=[]
    for step in plan['steps']:
        row=step['program'];binary=Path(row['path']);assert sha(binary)==row['sha256'];command=[str(binary)]+(['--exact',step['test'],'--nocapture'] if step.get('test') else []);env=os.environ.copy()
        if step.get('dump'):env['MIGO_DUMP_PROGRESSIVE_PCM']='1'
        log=work/(step['label']+'.log')
        with log.open('w') as stream:result=subprocess.run(command,cwd=work/'probe',env=env,stdout=stream,stderr=subprocess.STDOUT,timeout=900)
        (work/(step['label']+'-exit.txt')).write_text(str(result.returncode)+'\n');text=log.read_text();assert result.returncode==step['expected'],(step['label'],result.returncode)
        if result.returncode:
            assert all(x in text for x in ['running 1 test','panicked at','1 failed',step['test'],step['expected_assertion']]),step['label']
            (controls if step['role']=='fresh_source_fault' else reds).append(dict(name=step['label'],test=step['test'],expected_assertion=step['expected_assertion'],exit=101,sha256=row['sha256']))
        else:
            match=re.search(r'test result: ok\. ([1-9][0-9]*) passed; 0 failed; (\d+) ignored;',text);assert match,step['label'];suites[step['label']]=dict(passed=int(match[1]),ignored=int(match[2]))
        print('Executed:',step['label'],result.returncode,flush=True)
    a=pcm(work/'complete-pcm-debug.log');b=pcm(work/'complete-pcm-release.log');assert a==b,'complete PCM bits dev/O3 differ'
    summary=dict(programs=plan['programs'],suites=suites,controls=controls,original_red=reds,clippy=plan['clippy'],pcm=dict(outputs=len(a),samples=sum(map(len,a.values())),all_dev_o3_bits_equal=True),root_inputs_unchanged=True,private_sources_restored=plan['private_sources_restored'],root_dev_profile_verified=True,isolated_o3_not_shipping=True,loopback_only_test_fixtures=True)
    (work/'summary.json').write_text(json.dumps(summary,indent=2)+'\n');print('Product progressive full suites and fresh assertions passed',flush=True)


if __name__=='__main__':
    parser=argparse.ArgumentParser(description=__doc__);parser.add_argument('--output',type=Path,required=True);parser.add_argument('--target-dir',type=Path);parser.add_argument('--before-clippy',type=Path);parser.add_argument('--execute',action='store_true');args=parser.parse_args()
    if args.execute:execute(args.output.resolve())
    else:
        assert args.target_dir and args.before_clippy
        build(args.output.resolve(),args.target_dir.resolve(),args.before_clippy.resolve())
