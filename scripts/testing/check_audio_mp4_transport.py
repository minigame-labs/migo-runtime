#!/usr/bin/env python3
"""Prove M4A source suspensions preserve the shared DSP and PCM publication path.

An index wait or media reposition must not lose queued PCM, reset AAC/sinc state,
keep an old HTTP suffix, or bypass the same four-slot backpressure. This gate
compiles the production transport; the codec/DSP still remain opt-in candidates.
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
spec=importlib.util.spec_from_file_location('m4a_transport_source',ROOT/'scripts/testing/check_audio_mp4_source.py')
source=importlib.util.module_from_spec(spec);spec.loader.exec_module(source)
codecs=source.codecs

def controls(work,env):
    declarations=[
        ('source-resume-resets-sinc','resampled_streaming.rs','        self.source_waiting = false;','        self.resampler = None;\n        self.source_waiting = false;','http_index_rewinds_and_buffered_sources_keep_exact_dsp_pcm'),
        ('source-wait-drops-pending-pcm','resampled_streaming.rs','        if self.position < self.decoded.len() {','        if false {','http_index_rewinds_and_buffered_sources_keep_exact_dsp_pcm'),
        ('index-install-discards-http-suffix','isomp4_transport.rs','restart: false','restart: true','coalesced_input_preserves_index_suffix_and_replaces_repositioned_suffix'),
        ('media-resume-keeps-old-http-suffix','streaming_transport.rs',"                        continue 'download;","                        // deliberately keep previous body suffix",'coalesced_input_preserves_index_suffix_and_replaces_repositioned_suffix'),
        ('ready-reports-source-rate','resampled_streaming.rs','                self.target_rate\n','                self.decoder.rate\n','http_index_rewinds_and_buffered_sources_keep_exact_dsp_pcm'),
        ('sinc-tail-not-flushed','resampled_streaming.rs','        if self.decoder_done {','        if self.decoder_done { return Ok(self.outcome(0, Progress::Finished));','http_index_rewinds_and_buffered_sources_keep_exact_dsp_pcm'),
        ('source-wait-never-published','resampled_streaming.rs','            self.source_waiting = progress == push_decoder::Progress::NeedSource;','            self.source_waiting = false;','http_index_rewinds_and_buffered_sources_keep_exact_dsp_pcm'),
        ('composed-m4a-step-allocates','resampled_streaming.rs','        let result = self.step_inner(input, eof, pcm, cancel);','        std::hint::black_box(vec![0_u8;64]);\n        let result = self.step_inner(input, eof, pcm, cancel);','bounded_m4a_resampling_steps_have_no_steady_rust_allocation'),
        ('source-cancel-wait-removed','streaming_transport.rs','_ = state.cancelled() => None,','_ = std::future::pending::<()>() => None,','pending_index_wait_cancels_and_receiver_exit_stops_publication'),
        ('source-receiver-exit-wait-removed','streaming_transport.rs','_ = tx.closed() => {','_ = std::future::pending::<()>() => {','pending_index_wait_cancels_and_receiver_exit_stops_publication'),
        ('cached-source-requires-etag','isomp4_transport.rs','identity: Some(range::Identity::capture(&response)),','identity: Some(Ok(range::Identity::capture(&response).map_err(error)?)),','cached_dsp_needs_no_validator_or_range_and_true_misses_fail_closed'),
    ]
    declarations += [
        ('yield-drops-offered-suffix','streaming_transport.rs','Progress::Yield => tokio::task::yield_now().await,','Progress::Yield => break,','coalesced_input_preserves_index_suffix_and_replaces_repositioned_suffix'),
        ('invalid-source-resume-not-sticky','resampled_streaming.rs','            self.failed = true;\n            return Err("composed decoder has no drained source request".into());','            return Err("composed decoder has no drained source request".into());','invalid_source_resume_and_decode_failures_are_sticky'),
        ('source-error-published-as-done','streaming_transport.rs','StreamMsg::Error(error.to_string())','StreamMsg::Done','cached_dsp_needs_no_validator_or_range_and_true_misses_fail_closed'),
        ('media-collected-before-first-dsp-pcm','isomp4_source.rs','ReaderOwner::Media(stream) => stream.chunk().await,','ReaderOwner::Media(stream) => { let mut all=Vec::new(); while let Some(chunk)=stream.chunk().await? { all.extend_from_slice(&chunk); } Ok((!all.is_empty()).then(|| bytes::Bytes::from(all))) },','first_dsp_pcm_from_read_at_does_not_wait_for_media_eof'),
    ]
    for name,file,before,after,test in declarations:
        path=work/'source'/file;original=path.read_text();assert original.count(before)==1,(name,'anchor changed')
        path.write_text(original.replace(before,after))
        try:
            binary=source.build(work,env,name)
            log=work/f'{name}-test.log'
            with log.open('w') as stream:
                result=subprocess.run([str(binary),'--exact','isomp4_source_contracts::transport::'+test,'--nocapture'],cwd=work/'probe',env=env,stdout=stream,stderr=subprocess.STDOUT,timeout=90)
            observed=log.read_text();assert result.returncode==101 and 'panicked at' in observed and '1 failed' in observed,(name,'must reject in an executed Rust assertion')
        finally:path.write_text(original)
        print('actual M4A DSP mutation',name,': executable assertion rejected',flush=True)
    return len(declarations)

def prepare(work,target,rubato_home):
    env,frozen,_,libraries=source.prepare(work,target)
    package=work/'probe';manifest=(package/'Cargo.toml').read_text()
    manifest=manifest.replace('\n[dependencies]\n','\n[dependencies]\nrubato={version="=5.0.1",default-features=false}\n',1)
    manifest=manifest.replace('default=["m4a-streaming-probe","metadata-budgets"]','default=["m4a-streaming-probe","metadata-budgets","m4a-transport-probe"]\nm4a-transport-probe=[]')
    existing={r['name']for r in tomllib.loads((ROOT/'engine/Cargo.lock').read_text())['package']}
    new={};patches=''
    for directory in sorted((rubato_home/'registry/src').glob('*/*')):
        cargo=directory/'Cargo.toml'
        if not cargo.exists():continue
        row=tomllib.loads(cargo.read_text())['package'];name=row['name']
        if name in existing:continue
        proof=source.range_gate.transport.published_source(directory,rubato_home,row)
        retained=work/'resampling-vendor'/directory.name;shutil.copytree(directory,retained)
        patches+=f'{name}={{path={json.dumps(str(retained))}}}\n'
        new[name]={'version':row['version'],'checksum':proof['package'],'source_files':proof['files']}
    assert new.get('rubato',{}).get('version')=='5.0.1'
    manifest=manifest.replace('\n[lints.rust]\n','\n'+patches+'\n[lints.rust]\n')
    (work/'resampling-paths.json').write_text(json.dumps(new,indent=2)+'\n')
    (package/'Cargo.toml').write_text(manifest)
    lib=package/'src/lib.rs';text=lib.read_text()
    for name in ['cache','inner_audio']:
        path=ROOT/f'engine/crates/audio/src/{name}.rs';text+=f'#[path={json.dumps(str(path))}] mod {name};\n'
    for name in ['source_chunk','bandlimited','resampled_streaming','isomp4_transport']:
        file=name+'.rs';shutil.copy2(ROOT/'scripts/testing/fixtures/audio-codec-probe'/file,work/'source'/file)
        text+=f'#[path={json.dumps(str(work/"source"/file))}] mod {name};\n'
    shutil.copy2(ROOT/'scripts/testing/fixtures/audio-codec-probe/isomp4_transport_contracts.rs',work/'source/isomp4_transport_contracts.rs')
    shutil.copy2(ROOT/'scripts/testing/fixtures/audio-codec-probe/streaming_player_contracts.rs',work/'source/streaming_player_contracts.rs')
    text+=f'\n#[cfg(test)] #[path={json.dumps(str(work/"source/streaming_player_contracts.rs"))}] mod streaming_player_contracts;\n'
    lib.write_text(text)
    extra=[Path(__file__).resolve(),ROOT/'scripts/testing/check_audio_mp4_source.py',ROOT/'scripts/testing/check_audio_streaming_transport.py']
    frozen.update({str(p.relative_to(ROOT)):codecs.sha(p)for p in extra})
    (work/'source-inputs.json').write_text(json.dumps(frozen,indent=2)+'\n')
    for rel in frozen:
        dst=work/'source-inputs'/rel;dst.parent.mkdir(parents=True,exist_ok=True);shutil.copy2(ROOT/rel,dst)
    retained={str(p.relative_to(work)):codecs.sha(p)for tree in [package/'src',work/'source']for p in tree.rglob('*.rs')}
    codecs.run(['ffmpeg','-nostdin','-v','error','-f','lavfi','-i','anullsrc=r=8000:cl=mono','-t','2201.6','-c:a','aac','-profile:a','aac_low','-movflags','+faststart',str(work/'fixtures/long-front.m4a')],work/'long-encode.log',cwd=work,env=env)
    # A test-only non-audio tail forces an unaccepted suffix at media rewind.
    # Keep an independent FFmpeg PCM oracle for this layout change.
    import hashlib,struct
    base=work/'fixtures/2-buffered-miss.m4a';extended=work/'fixtures/2-coalesced-rewind.m4a'
    extended.write_bytes(base.read_bytes()+struct.pack('>I4s',256*1024+8,b'free')+bytes(256*1024))
    hashes=[]
    for fixture in [base,extended]:
        decoded=subprocess.run(['ffmpeg','-nostdin','-v','error','-i',str(fixture),'-f','f32le','-'],capture_output=True,check=True,timeout=30)
        hashes.append(hashlib.sha256(decoded.stdout).hexdigest())
    assert hashes[0]==hashes[1],'coalesced tail changed independent PCM'
    (work/'coalesced-fixture-oracle.json').write_text(json.dumps({'encoded_sha256':codecs.sha(extended),'ffmpeg_pcm_sha256':hashes[0],'same_as_original':True},indent=2)+'\n')
    # Range preparation first resolves a smaller services closure. Restore the
    # full Root cohort before adding codecs/DSP so unused existing pins are not
    # silently discarded and upgraded on this second resolution.
    (package/'Cargo.lock').write_bytes((ROOT/'engine/Cargo.lock').read_bytes())
    return env,frozen,retained,libraries,new

def main(work,target,rubato_home,positive_only):
    env,frozen,retained,libraries,new=prepare(work,target,rubato_home)
    package=work/'probe'
    codecs.run(['cargo','metadata','--offline','--format-version','1'],work/'candidate-metadata.jsonl',cwd=package,env=env)
    selected=json.loads(next(line for line in (work/'candidate-metadata.jsonl').read_text().splitlines() if line.startswith('{"packages"')))['packages']
    locked={}
    for row in tomllib.loads((ROOT/'engine/Cargo.lock').read_text())['package']:
        locked.setdefault(row['name'],set()).add(row['version'])
    assert all(row['name'] not in locked or row['version'] in locked[row['name']] for row in selected),'existing dependency version drift'
    codecs.run(['cargo','check','--offline','--locked','--lib'],work/'normal-library-check.log',cwd=package,env=env)
    binary=source.build(work,env,'positive')
    for label,prefix,count in [('transport','isomp4_source_contracts::transport::',11),('source','isomp4_source_contracts::',36),('range','network::http::range::tests::',18),('common','streaming_transport::tests::',3)]:
        if label=='source':continue # Run the 25 legacy source groups separately below, without repeating the DSP/player groups.
        log=work/f'positive-{label}.log'
        codecs.run([str(binary),prefix,'--nocapture','--test-threads=1'],log,cwd=package,env=env)
        assert f'{count} passed; 0 failed' in log.read_text(),label
    source_log=work/'positive-source.log'
    codecs.run([str(binary),'isomp4_source_contracts::','--skip','isomp4_source_contracts::transport::','--nocapture','--test-threads=1'],source_log,cwd=package,env=env)
    assert '25 passed; 0 failed' in source_log.read_text()
    negative=0 if positive_only else controls(work,env)
    assert all(codecs.sha(ROOT/p)==h for p,h in frozen.items()),'Root changed'
    assert all(codecs.sha(work/p)==h for p,h in retained.items()),'source copy not restored'
    rows=json.loads((work/'library-source-provenance.json').read_text())
    for row in rows:assert source.index_gate.timeline.source_tree(libraries[row['crate']])==row['source_files']
    for name,row in new.items():
        directory=work/'resampling-vendor'/f"{name}-{row['version']}"
        assert all(codecs.sha(directory/rel)==h for rel,h in row['source_files'].items()),(name,'Rubato cohort source drift')
    summary={'m4a_transport_tests':11,'legacy_source_tests':25,'http_range_tests':18,'common_transport_tests':3,'http_dsp_pcm_comparisons':61,'player_comparisons':7,'compiled_controls':negative,'positive_only':positive_only,'binary_sha256':codecs.sha(binary),'libraries':len(rows),'patches':sum(len(r['patches'])for r in rows),'root_inputs_unchanged':True,'source_copies_restored':True,'resampling_source_files_unchanged':True,'existing_dependency_versions_preserved':True,'root_lock_sha256':codecs.sha(ROOT/'engine/Cargo.lock'),'scope':'Host shared HTTP/CPU/DSP/PcmPool/player candidate; no product codec switch, fallback, native/device or performance acceptance'}
    (work/'summary.json').write_text(json.dumps(summary,indent=2)+'\n');print(json.dumps(summary,indent=2),flush=True)

if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--output',type=Path,required=True);p.add_argument('--target-dir',type=Path,required=True);p.add_argument('--rubato-cargo-home',type=Path,required=True);p.add_argument('--positive-only',action='store_true');a=p.parse_args()
    main(a.output.resolve(),a.target_dir.resolve(),a.rubato_cargo_home.resolve(),a.positive_only)
