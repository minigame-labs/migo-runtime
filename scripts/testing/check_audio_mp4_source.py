#!/usr/bin/env python3
"""Resolve real M4A tail indexes through pinned HTTP and the common CPU lane.

Clone the existing services probe, replay published codec patches, and exercise
actual initial-body/index exchanges. The test PCM collector is not a product
cache or acceptance of DSP/player/ReadAt/disk fallback.
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

def main(work,target):
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
    known='cfg(feature, values("wave-streaming-probe", "adts-streaming-probe", "vorbis-streaming-probe", "phase-probe"))'
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
    for file in ['incremental_decoder.rs','isomp4_streaming.rs']:shutil.copyfile(fixtures/file,source/file)
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
    index_gate.fixtures(work/'fixtures');env['W09_M4A_FIXTURES']=str(work/'fixtures')
    codecs.run(['cargo','metadata','--offline','--format-version','1'],work/'candidate-metadata.jsonl',cwd=package,env=env)
    codecs.run(['cargo','check','--offline','--locked','--lib'],work/'normal-library-check.log',cwd=package,env=env)
    positive=build(work,env,'positive')
    for label,selected,count in [('source','isomp4_source_contracts::',9),('range','network::http::range::tests::',16)]:
        log=work/f'positive-{label}.log'
        codecs.run([str(positive),selected,'--nocapture','--test-threads=1'],log,cwd=package,env=env)
        assert f'{count} passed; 0 failed'in log.read_text(),label
    negative=controls(work,env)
    assert all(codecs.sha(ROOT/rel)==h for rel,h in frozen.items()),'Root inputs changed'
    assert all(codecs.sha(work/rel)==h for rel,h in retained.items()),'source copy not restored'
    rows=json.loads((work/'library-source-provenance.json').read_text())
    for row in rows:assert index_gate.timeline.source_tree(libraries[row['crate']])==row['source_files'],row['crate']
    summary={'source_tests':9,'http_range_regression_tests':16,'http_pcm_comparisons':16,'compiled_controls':negative,
             'binary_sha256':codecs.sha(positive),'libraries':len(rows),'patches':sum(len(r['patches'])for r in rows),
             'root_inputs_unchanged':True,'source_copies_restored':True,'root_lock_sha256':codecs.sha(ROOT/'engine/Cargo.lock'),
             'scope':'Actual HTTP tail-index/common decoder/CPU lane only; test PCM collection is an oracle. No ReadAt/DSP/player/disk fallback/product/native/device/performance acceptance.'}
    (work/'summary.json').write_text(json.dumps(summary,indent=2)+'\n');print(json.dumps(summary,indent=2),flush=True)

if __name__=='__main__':
    parser=argparse.ArgumentParser(description=__doc__);parser.add_argument('--output',type=Path,required=True);parser.add_argument('--target-dir',type=Path,required=True)
    args=parser.parse_args();main(args.output.resolve(),args.target_dir.resolve())
