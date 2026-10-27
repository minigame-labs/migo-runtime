#!/usr/bin/env python3
"""Resolve M4A indexes and indexed media through pinned HTTP and the common CPU lane.

Clone the existing services probe, replay published codec patches, and exercise
actual initial-body/index/reposition exchanges. The test PCM collector is not a
product cache or acceptance of DSP/player/disk fallback.
"""
from __future__ import annotations
import argparse
import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import tomllib

ROOT=Path(__file__).resolve().parents[2]
def module(name,path):
    spec=importlib.util.spec_from_file_location(name,path)
    value=importlib.util.module_from_spec(spec);spec.loader.exec_module(value);return value
range_gate=module('migo_mp4_source_range',ROOT/'scripts/testing/check_http_range_source.py')
index_gate=module('migo_mp4_source_index',ROOT/'scripts/testing/check_audio_mp4_index.py')
codecs=range_gate.codecs

def build(work,env,label,library=None):
    if library:
        codecs.run(['cargo','clean','--offline','--locked','--release','-p',library],work/f'{label}-library-clean.log',cwd=work/'probe',env=env)
    binary=range_gate.build(work,env,label)
    rows=[]
    for line in (work/f'{label}-build.jsonl').read_text().splitlines():
        try:r=json.loads(line)
        except ValueError:continue
        if r.get('reason')=='compiler-artifact':rows.append(r)
    for name in ['symphonia-core','symphonia-codec-aac','symphonia-format-isomp4','symphonia-metadata']:
        assert any(str(work/'vendor'/f'{name}-0.6.1') in r['package_id'] for r in rows),name
    if library:assert any(r['target']['name']==library.replace('-','_') and not r['fresh'] for r in rows)
    return binary

def controls(work,env):
    source=work/'source/isomp4_source.rs'
    common=work/'source/incremental_decoder.rs'
    transport=work/'source/streaming_transport.rs'
    vendor=work/'vendor/symphonia-format-isomp4-0.6.1/src/streaming/sequential.rs'
    budget='actual_source_metadata_atom_and_announced_index_budgets_fail_before_body_fetch'
    pcm='real_http_tail_indexes_install_on_cpu_and_keep_fragmented_pcm'
    cancellation='index_wait_cancellation_and_receiver_exit_release_real_pending_requests'
    eof='learned_length_and_external_movie_eof_do_not_accept_incomplete_initial_body'
    declarations=[
        ('atom-count-cap-removed',source,[('.filter(|n| *n <= self.limits.max_atoms)','.filter(|_| true)')],budget,None),
        ('skipped-byte-cap-removed',source,[('.filter(|n| *n <= self.limits.max_skipped_bytes)','.filter(|_| true)')],budget,None),
        ('index-body-cap-removed',source,[('.filter(|n| *n <= self.limits.index.max_index_bytes)','.filter(|_| true)')],budget,None),
        ('source-cancel-wait-removed',transport,[('_ = state.cancelled() => None,','_ = std::future::pending::<()>() => None,')],cancellation,None),
        ('source-receiver-exit-wait-removed',transport,[('_ = tx.closed() => {','_ = std::future::pending::<()>() => {')],cancellation,None),
        ('movie-parsed-on-io',source,[('let operation = decoder.with(move |value| {','let prepared = parse_movie(&bytes, options);\n                let operation = decoder.with(move |value| {'),('let parsed = parse_movie(&bytes, options)?;','let parsed = prepared?;')],pcm,None),
        ('learned-object-length-discarded',source,[('let length = self.reader.length();','let length = self.limits.index.object_len;')],eof,None),
        ('external-movie-not-required-at-eof',vendor,[('                            || !self.movie_seen\n','')],eof,'symphonia-format-isomp4'),
        ('pinned-length-bind-guard-removed',vendor,[('''        if !valid {
            self.failed = true;
            return decode_error("isomp4: invalid pinned object length");
        }''','')],'learned_library_length_is_monotonic_and_only_binds_pending_index','symphonia-format-isomp4'),
        ('index-scan-deadline-renewed',source,[('self.reader.read_exact_at(offset, len, deadline)','self.reader.read_exact_at(offset, len, Instant::now() + std::time::Duration::from_secs(2))')],'index_discovery_keeps_one_absolute_deadline_for_all_control_reads',None),
        ('index-install-discards-prefetched-input',common,[('        if reset_input {','        if true {')],pcm,None),
        ('cpu-index-wait-deadline-renewed',source,[('tokio::time::timeout_at(deadline, operation)','tokio::time::timeout_at(deadline + std::time::Duration::from_secs(2), operation)')],'cpu_index_wait_obeys_the_same_deadline_without_waiting_for_the_job',None),
    ]
    real = 'actual_read_at_reuses_media_extents_and_preserves_aac_and_container_end'
    bad = 'read_at_rejects_nonmedia_packets_limits_and_unknown_complete_lengths'
    declarations += [
        ('read-at-forgets-verified-media-cache',source,[('self.media_spans.iter().find(|span| {','self.media_spans.iter().find(|span| { false &&')],real,None),
        ('read-at-accepts-nonmedia-span',source,[('span.kind != AtomKind::MediaData\n                    || packet.offset','false\n                    || packet.offset')],bad,None),
        ('read-at-packet-crosses-media-end',source,[('                    || end > limit','                    || false')],bad,None),
        ('read-at-packet-in-media-header',source,[('                    || packet.offset < span.body_offset','                    || false')],bad,None),
        ('read-at-empty-packet-accepted',source,[('.filter(|end| *end > packet.offset)','.filter(|_| true)')],bad,None),
        ('read-at-discovery-budget-removed',source,[('.filter(|count| *count <= self.limits.max_atoms)','.filter(|_| true)')],bad,None),
        ('read-at-reposition-budget-removed',source,[('.filter(|n| *n <= self.limits.max_resumes)','.filter(|_| true)')],bad,None),
        ('read-at-pending-command-unchecked',source,[('value.source_request() != Some(M4aSourceRequest::ReadAt(packet))','false')],'read_at_cannot_resume_a_different_pending_command',None),
        ('read-at-retains-old-prefetched-input',work/'source/isomp4_streaming.rs',[('self.resume_source(true, |p| {','self.resume_source(false, |p| {')],real,None),
        ('read-at-resets-aac-state',common,[('        if reset_input {','        if reset_input {\n            self.decoder = None;')],real,None),
        ('read-at-finishes-at-mdat-not-object',source,[('let length = self.reader.length().ok_or(Error::UnknownLength)?;','let length = media.end.ok_or(Error::UnknownLength)?;')],real,None),
        ('read-at-renews-cpu-deadline',source,[('tokio::time::timeout_at(deadline, resume)','tokio::time::timeout_at(deadline + std::time::Duration::from_secs(2), resume)')],'read_at_cpu_resume_wait_shares_the_source_deadline',None),
        ('read-at-renews-media-setup-deadline',source,[('tokio::time::timeout_at(deadline, self.reader.chunk())','tokio::time::timeout_at(deadline + std::time::Duration::from_secs(2), self.reader.chunk())')],'read_at_pending_media_uses_the_absolute_setup_deadline',None),
        ('read-at-collects-complete-media-body',source,[('ReaderOwner::Media(stream) => stream.chunk().await,','ReaderOwner::Media(stream) => { let mut all = Vec::new(); while let Some(bytes) = stream.chunk().await? { all.extend_from_slice(&bytes); } Ok((!all.is_empty()).then(|| bytes::Bytes::from(all))) },')],'actual_read_at_short_ranges_and_held_body_deliver_before_tail_release',None),
    ]
    buffered = work/'source/isomp4_buffered_contracts.rs'
    binding = work/'source/isomp4_streaming.rs'
    buffer_lib = work/'vendor/symphonia-format-isomp4-0.6.1/src/streaming/sequential/buffered.rs'
    small = 'buffered::small_assets_reuse_one_response_without_validators_or_range'
    geometry = 'buffered::compaction_keeps_absolute_positions_and_avoids_redundant_io'
    quantum = 'buffered::buffered_index_scanning_yields_and_cancellation_stays_sticky'
    bounds = 'buffered::library_buffer_windows_reject_overflow_and_latch_failure'
    declarations += [
        ('buffered-history-discarded',binding,[('const RETAIN_ENCODED_HISTORY: bool = BUFFERED;','const RETAIN_ENCODED_HISTORY: bool = false;')],small,None),
        ('buffered-empty-push-compacts-prefix',common,[('        if data.is_empty() {\n            return Ok(0);\n        }\n','')],quantum,None),
        ('buffered-absolute-base-lost',binding,[('.checked_sub(head as u64)','.checked_sub(head as u64).map(|_| 0)')],geometry,None),
        ('buffered-rewind-uses-absolute-head',binding,[('.checked_sub(base)','.checked_sub(0)')],geometry,None),
        ('buffered-resume-resets-aac',common,[('BufferedSource::Resume(head) => {','BufferedSource::Resume(head) => { self.decoder = None;')],small,None),
        ('buffered-eof-asks-for-more-input',binding,[('            BufferedStep::NeedMore if eof => Err(symphonia::core::errors::Error::DecodeError(\n                "incomplete buffered source at EOF",\n            )),','            BufferedStep::NeedMore if eof => Ok(BufferedSource::NeedInput),')],'buffered::buffered_eof_quota_and_failed_state_do_not_hide_errors',None),
        ('buffered-index-overfills-prefetch',buffer_lib,[('if movie_end > limit {','if false {')],'buffered::genuine_misses_keep_pinned_http_and_large_indexes_stop_prefetch_at_capacity','symphonia-format-isomp4'),
        ('buffered-header-quantum-removed',buffer_lib,[('for _ in 0..HEADERS_PER_STEP {','for _ in 0..usize::MAX {')],quantum,'symphonia-format-isomp4'),
        ('buffered-probe-restarts-each-yield',buffer_lib,[('        self.buffered_probe = Some(probe);\n        Ok(BufferedStep::Yield)','        self.buffered_probe = None;\n        Ok(BufferedStep::Yield)')],quantum,'symphonia-format-isomp4'),
        ('buffered-window-error-not-sticky',buffer_lib,[('            self.failed = true;','            self.failed = false;')],bounds,'symphonia-format-isomp4'),
        ('buffered-window-cap-unchecked',buffer_lib,[('if input.len() > capacity {','if false {')],bounds,'symphonia-format-isomp4'),
    ]
    for label,path,changes,test,library in declarations:
        original=path.read_bytes();text=original.decode()
        for before,after in changes:
            assert text.count(before)==1,(label,'mutation anchor changed',before)
            text=text.replace(before,after)
        path.write_text(text)
        try:
            binary=build(work,env,label,library)
            log=work/f'{label}-test.log'
            with log.open('w')as stream:
                status=subprocess.run([str(binary),'--exact','isomp4_source_contracts::'+test,'--nocapture'],cwd=work/'probe',env=env,stdout=stream,stderr=subprocess.STDOUT,timeout=45).returncode
            observed=log.read_text()
            assert status==101 and 'panicked at' in observed and '1 failed' in observed,(label,'must fail an executed Rust assertion')
        finally:path.write_bytes(original)
        print('actual M4A HTTP source mutation',label,': executable assertion rejected',flush=True)
    return len(declarations)

def source_fixtures(directory):
    # Test-only layout surgery; independently require FFmpeg to preserve PCM.
    import struct
    U=lambda n:struct.pack('>I',n)
    atom=lambda kind,body:U(8+len(body))+kind+body
    src=(directory/'2-reorder.m4a').read_bytes()
    path=(b'moov',b'trak',b'mdia',b'minf',b'stbl')
    sizes=index_gate.timeline.payloads(src,path+(b'stsz',))[0][12:]
    sizes=list(struct.unpack('>'+str(len(sizes)//4)+'I',sizes))
    a=sizes[0];b=sum(sizes[1:4]);c=sum(sizes[4:])
    def top(data):
        rows=[];pos=0
        while pos<len(data):
            size=int.from_bytes(data[pos:pos+4],'big');assert size>=8
            rows.append((data[pos+4:pos+8],data[pos:pos+size]));pos+=size
        assert pos==len(data);return rows
    rows=top(src);ftyp=next(d for k,d in rows if k==b'ftyp');movie=next(d for k,d in rows if k==b'moov')
    media=next(d[8:]for k,d in rows if k==b'mdat');assert len(media)==a+b+c
    first=media[:c];second=media[c:c+a];third=media[c+a:]
    made=[]
    for multi in [False,True]:
        for tail in [False,True]:
            prefix=len(ftyp)+(0 if tail else len(movie))
            if multi:
                gap=atom(b'free',b'x'*32)
                md1=atom(b'mdat',first);md2=atom(b'mdat',third+second)
                start=prefix+len(md1)+len(gap)+8
                offsets=[start+b,start,prefix+8]
                payload=md1+gap+md2
            else:
                start=prefix+8;offsets=[start+c+b,start+c,start]
                payload=atom(b'mdat',first+third+second)
            table=U(0)+U(3)+b''.join(U(n)for n in offsets)
            changed=index_gate.timeline.rewrite(src,{path+(b'stco',):table})
            movie2=next(d for k,d in top(changed)if k==b'moov');assert len(movie2)==len(movie)
            result=ftyp+(b''if tail else movie2)+payload+(movie2 if tail else b'')
            name='2-'+('tail-'if tail else '')+('multi'if multi else'reverse')+'.m4a'
            (directory/name).write_bytes(result);made.append(name)
    oracle=[]
    for name in ['2-front.m4a']+made:
        result=subprocess.run(['ffmpeg','-nostdin','-hide_banner','-loglevel','error','-i',str(directory/name),'-f','f32le','-'],check=True,timeout=30,stdout=subprocess.PIPE)
        oracle.append(result.stdout)
    assert all(p==oracle[0]for p in oracle)
    (directory/'source-layouts.json').write_text(json.dumps({'layouts':made,'ffmpeg_pcm_identical':True},indent=2)+'\n')
    print('Generated4reverse/multi-mdat/front+tailfixtures;FFmpegPCMidentical',flush=True)

def buffered_fixtures(directory):
    import struct
    U=lambda n:struct.pack('>I',n)
    atom=lambda kind,body:U(8+len(body))+kind+body
    path=(b'moov',b'trak',b'mdia',b'minf',b'stbl')
    def top(data):
        rows=[];pos=0
        while pos<len(data):
            size=int.from_bytes(data[pos:pos+4],'big');assert size>=8
            rows.append((pos,data[pos+4:pos+8],data[pos:pos+size]));pos+=size
        assert pos==len(data);return rows
    def offsets(data):
        payload=index_gate.timeline.payloads(data,path+(b'stco',))[0]
        n=int.from_bytes(payload[4:8],'big');return list(struct.unpack('>'+str(n)+'I',payload[8:]))
    def changed(data,values):return index_gate.timeline.rewrite(data,{path+(b'stco',):U(0)+U(len(values))+b''.join(U(n)for n in values)})
    made=[]
    # A large prefix forces history compaction without moving audio packets
    # outside the retained final window. Absolute byte coordinates must survive.
    data=(directory/'1-reorder.m4a').read_bytes();gap=atom(b'free',b'x'*(128*1024))
    data=changed(data,[n+len(gap)for n in offsets(data)])
    at=next(p for p,k,d in top(data)if k==b'mdat');data=data[:at]+gap+data[at:]
    name='1-buffered-prefix.m4a';(directory/name).write_bytes(data);made.append((name,'1-front.m4a'))
    # The first chronological chunks are moved beyond an actual 128KiB mdat
    # hole. Backwards audio is then genuinely evicted and needs external IO.
    data=(directory/'2-reorder.m4a').read_bytes();old=offsets(data);hole=b'x'*(128*1024)
    data=changed(data,[old[0]+len(hole),old[1]+len(hole),old[2]])
    rows=top(data);pos,_,mdat=next(r for r in rows if r[1]==b'mdat');where=old[0]-(pos+8)
    payload=mdat[8:];new=atom(b'mdat',payload[:where]+hole+payload[where:])
    data=data[:pos]+new+data[pos+len(mdat):]
    name='2-buffered-miss.m4a';(directory/name).write_bytes(data);made.append((name,'2-front.m4a'))
    # Every AAC packet is its own chunk, in reverse physical order, so steady
    # decoding repeatedly exercises buffered resumes after the codec warms.
    data=(directory/'2-front.m4a').read_bytes();payload=index_gate.timeline.payloads(data,path+(b'stsz',))[0][12:]
    sizes=list(struct.unpack('>'+str(len(payload)//4)+'I',payload));old=offsets(data)[0]
    packets=[];at=old
    for size in sizes:packets.append(data[at:at+size]);at+=size
    def rewrite(delta):
        start=old+delta;ends=[];running=sum(sizes)
        for size in sizes:running-=size;ends.append(start+running)
        return index_gate.timeline.rewrite(data,{path+(b'stco',):U(0)+U(len(ends))+b''.join(U(n)for n in ends),path+(b'stsc',):U(0)+U(1)+U(1)+U(1)+U(1)})
    tmp=rewrite(0);oldpos=next(p for p,k,d in top(data)if k==b'mdat');newpos=next(p for p,k,d in top(tmp)if k==b'mdat')
    data=rewrite(newpos-oldpos);pos,_,mdat=next(r for r in top(data)if r[1]==b'mdat')
    data=data[:pos]+atom(b'mdat',b''.join(reversed(packets)))+data[pos+len(mdat):]
    name='2-buffered-many.m4a';(directory/name).write_bytes(data);made.append((name,'2-front.m4a'))
    data=(directory/'2-multi.m4a').read_bytes();gap=atom(b'free',b'x'*4096)
    data=changed(data,[n+len(gap)for n in offsets(data)])
    at=next(p for p,k,d in top(data)if k==b'mdat');data=data[:at]+gap+data[at:]
    assert len(data)+8<=65536
    data+=atom(b'free',b'x'*(65536-len(data)-8));assert len(data)==65536
    name='2-buffered-full.m4a';(directory/name).write_bytes(data);made.append((name,'2-front.m4a'))
    for name,reference in made:
        outputs=[]
        for asset in [name,reference]:
            outputs.append(subprocess.run(['ffmpeg','-nostdin','-hide_banner','-loglevel','error','-i',str(directory/asset),'-f','f32le','-'],check=True,timeout=30,stdout=subprocess.PIPE).stdout)
        assert outputs[0]==outputs[1],name
    (directory/'buffered-layouts.json').write_text(json.dumps({'layouts':made,'ffmpeg_pcm_identical':True},indent=2)+'\n')
    print('Generated4bounded-history/compaction/missfixtures;FFmpegPCMidentical',flush=True)

def prepare(work,target):
    env,frozen,_,_=range_gate.prepare(work,target)
    package=work/'probe';source=work/'source';source.mkdir()
    fixtures=ROOT/'scripts/testing/fixtures/audio-codec-probe'
    libraries=codecs.library_patches(ROOT,work)
    manifest=(package/'Cargo.toml').read_text()
    locked=tomllib.loads((ROOT/'engine/Cargo.lock').read_text())['package']
    def version(name):
        rows={p['version']for p in locked if p['name']==name};assert len(rows)==1;return rows.pop()
    deps='''symphonia={version="=0.6.1",default-features=false,features=["mp3","aac","isomp4","id3v2","opt-simd"]}
symphonia-bundle-mp3={version="=0.6.1",default-features=false,features=["streaming"]}
symphonia-codec-aac="=0.6.1"
symphonia-format-isomp4={version="=0.6.1",features=["streaming"]}
'''
    for name in ['minimp3-sys','lewton','hound','futures-util']:deps+=f'{name}="={version(name)}"\n'
    manifest=manifest.replace('\n[dependencies]\n','\n[dependencies]\n'+deps,1)
    dev=''
    for name in ['alloc','executor']:dev+=f'migo-{name}-probe={{path={json.dumps(str(ROOT/f"engine/testing/{name}-probe"))},default-features=false}}\n'
    manifest=manifest.replace('\n[dev-dependencies]\n','\n[dev-dependencies]\n'+dev,1)
    manifest=manifest.replace('\n[features]\n','\n[features]\ndefault=["m4a-streaming-probe","metadata-budgets"]\nm4a-streaming-probe=[]\nmetadata-budgets=[]\n',1)
    for name,path in libraries.items():manifest+=f'{name}={{path={json.dumps(str(path))}}}\n'
    known='cfg(feature, values("wave-streaming-probe", "adts-streaming-probe", "vorbis-streaming-probe", "phase-probe", "m4a-transport-probe"))'
    manifest+='\n[lints.rust]\nunexpected_cfgs={level="warn",check-cfg=['+json.dumps(known)+']}\n'
    (package/'Cargo.toml').write_text(manifest)
    root=package/'src/lib.rs';text=root.read_text()+'\nextern crate self as migo_services;\n'
    for name in ['decoder','limits','resampler','streaming','streaming_transport','off_worker']:
        path=ROOT/f'engine/crates/audio/src/{name}.rs'
        if name=='decoder':path=ROOT/'engine/crates/audio/src/decoder/mod.rs'
        if name in ['streaming','streaming_transport','off_worker']:
            shutil.copyfile(path,source/path.name);path=source/path.name
        text+=f'#[path={json.dumps(str(path))}] mod {name};\n'
    text+=f'#[cfg(test)] #[path={json.dumps(str(ROOT/"engine/crates/audio/src/mp3_fixture.rs"))}] mod mp3_fixture;\n'
    names={'push_decoder':'streaming.rs','candidate':'candidate.rs','isomp4_source':'isomp4_source.rs','isomp4_source_contracts':'isomp4_source_contracts.rs'}
    for name,file in names.items():
        shutil.copyfile(fixtures/file,source/(name+'.rs'))
        prefix='#[cfg(test)] 'if name.endswith('contracts')else''
        text+=f'{prefix}#[path={json.dumps(str(source/(name+".rs")))}] mod {name};\n'
    for file in ['incremental_decoder.rs','isomp4_streaming.rs','isomp4_buffered_contracts.rs']:shutil.copyfile(fixtures/file,source/file)
    text+='#[cfg(test)] #[global_allocator] static COUNTING: migo_alloc_probe::CountingAllocator = migo_alloc_probe::CountingAllocator::system();\n'
    (package/'src/lib.rs').write_text(text)
    # The shared off-worker tests and actual decoder use the same Tokio shape.
    (package/'Cargo.toml').write_text((package/'Cargo.toml').read_text().replace('"test-util"','"test-util", "rt-multi-thread"').replace('"fs", "io-util", "net"','"fs", "io-util", "net", "macros", "rt-multi-thread"'))
    extra=[Path(__file__).resolve(),ROOT/'scripts/testing/check_audio_mp4_index.py',ROOT/'engine/crates/audio/Cargo.toml']
    extra+=sorted((ROOT/'engine/crates/audio/src').rglob('*.rs'))
    extra+=sorted(fixtures.glob('*.rs'))
    for name in libraries:extra+=sorted((ROOT/f'engine/third_party/{name}-patches').glob('*'))
    frozen.update({str(p.relative_to(ROOT)):codecs.sha(p)for p in extra if p.is_file()})
    (work/'source-inputs.json').write_text(json.dumps(frozen,indent=2)+'\n')
    for rel in frozen:
        dst=work/'source-inputs'/rel;dst.parent.mkdir(parents=True,exist_ok=True);shutil.copyfile(ROOT/rel,dst)
    retained={str(p.relative_to(work)):codecs.sha(p)for tree in [package/'src',source]for p in tree.rglob('*.rs')}
    index_gate.fixtures(work/'fixtures');source_fixtures(work/'fixtures');buffered_fixtures(work/'fixtures');env['W09_M4A_FIXTURES']=str(work/'fixtures')
    return env,frozen,retained,libraries

def main(work,target,positive_only=False):
    env,frozen,retained,libraries=prepare(work,target)
    package=work/'probe'
    codecs.run(['cargo','metadata','--offline','--format-version','1'],work/'candidate-metadata.jsonl',cwd=package,env=env)
    codecs.run(['cargo','check','--offline','--locked','--lib'],work/'normal-library-check.log',cwd=package,env=env)
    positive=build(work,env,'positive')
    for label,selected,count in [('source','isomp4_source_contracts::',25),('range','network::http::range::tests::',18)]:
        log=work/f'positive-{label}.log'
        codecs.run([str(positive),selected,'--nocapture','--test-threads=1'],log,cwd=package,env=env)
        assert f'{count} passed; 0 failed'in log.read_text(),label
    observations=[]
    for line in (work/'positive-source.log').read_text().splitlines():
        if 'W09_M4A_BUFFERED_ABBA ' in line:
            observations.append(json.loads(line.split('W09_M4A_BUFFERED_ABBA ',1)[1]))
    assert len(observations)==16
    (work/'buffered-abba.json').write_text(json.dumps({'observations':observations,'scope':'Host loopback with30msperRange response;not device or wholepipeline performance'},indent=2)+'\n')
    negative=0 if positive_only else controls(work,env)
    assert all(codecs.sha(ROOT/rel)==h for rel,h in frozen.items()),'Root inputs changed'
    assert all(codecs.sha(work/rel)==h for rel,h in retained.items()),'source copy not restored'
    rows=json.loads((work/'library-source-provenance.json').read_text())
    for row in rows:assert index_gate.timeline.source_tree(libraries[row['crate']])==row['source_files'],row['crate']
    summary={'source_tests':25,'http_range_regression_tests':18,'http_pcm_comparisons':105,'legacy_http_pcm_comparisons':42,'buffered_http_pcm_comparisons':63,'buffered_tests':8,'encoded_cache_capacity':65536,'compiled_controls':negative,
             'binary_sha256':codecs.sha(positive),'libraries':len(rows),'patches':sum(len(r['patches'])for r in rows),
             'root_inputs_unchanged':True,'source_copies_restored':True,'root_lock_sha256':codecs.sha(ROOT/'engine/Cargo.lock'),
             'positive_only':positive_only,'scope':'Actual HTTP tail-index and ReadAt/common decoder/CPU lane; PCM collection is an oracle. No DSP/player/disk fallback/product/native/device/performance acceptance.'}
    (work/'summary.json').write_text(json.dumps(summary,indent=2)+'\n');print(json.dumps(summary,indent=2),flush=True)

if __name__=='__main__':
    parser=argparse.ArgumentParser(description=__doc__);parser.add_argument('--output',type=Path,required=True);parser.add_argument('--target-dir',type=Path,required=True);parser.add_argument('--positive-only',action='store_true')
    args=parser.parse_args();main(args.output.resolve(),args.target_dir.resolve(),args.positive_only)
