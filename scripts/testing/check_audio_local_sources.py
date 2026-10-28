#!/usr/bin/env python3
"""Compile all sequential codecs against the same anchored Fs/Pack source.

The M4A reader used to own reusable file/IO policy under a format-specific
module. One source must retain pooled chunks, expose errors rather than EOF,
and feed the existing CPU/DSP/player without collecting a whole local asset.
"""
from __future__ import annotations
import argparse,importlib.util,json,shutil,subprocess,tomllib
from pathlib import Path
ROOT=Path(__file__).resolve().parents[2]
spec=importlib.util.spec_from_file_location('file_gate',ROOT/'scripts/testing/check_audio_mp4_file.py')
file=importlib.util.module_from_spec(spec);spec.loader.exec_module(file)
codecs=file.codecs

def fixtures(work,env):
    assets=work/'local-assets';assets.mkdir();(work/'cache').mkdir()
    inputs=work/'local-input';inputs.mkdir()
    for i,(rate,channels,frames)in enumerate([(44100,1,17),(48000,2,9737),(22050,1,9737),(48000,2,240137)]):
        source=inputs/f'{i}.wav';codecs.fixture_pcm(source,rate,channels,frames)
        for ext,args in [('aac',['-c:a','aac','-profile:a','aac_low','-b:a','128k','-f','adts']),('wav',['-c:a',['pcm_u8','pcm_s16le','pcm_s24le','pcm_f32le'][i]])]:
            codecs.run(['ffmpeg','-nostdin','-v','error','-i',str(source),*args,str(assets/f'{i}.{ext}')],work/f'local-{i}-{ext}-encode.log',cwd=work,env=env)
        mp3_source=source
        if i==2:
            mp3_source=inputs/'mpeg25.wav';codecs.fixture_pcm(mp3_source,8000,2,9737)
        codecs.run(['ffmpeg','-nostdin','-v','error','-i',str(mp3_source),'-c:a','libmp3lame','-q:a','4',str(assets/f'{i}.mp3')],work/f'local-{i}-mp3-encode.log',cwd=work,env=env)
    spec=importlib.util.spec_from_file_location('ogg_fixture',ROOT/'scripts/testing/check_audio_ogg_streaming.py');ogg=importlib.util.module_from_spec(spec);spec.loader.exec_module(ogg)
    inputs=work/'local-vorbis';inputs.mkdir();ogg.fixtures(inputs)
    for i,name in enumerate(['0.ogg','2.ogg','cross.ogg','max-window-8.ogg']):shutil.copy2(inputs/'fixtures'/name,assets/f'{i}.ogg')
    (assets/'bad.mp3').write_bytes(b'ID3\x04\x00\x00\x00\x00\x01')
    (assets/'bad.aac').write_bytes(b'\xff\xf1')
    for ext in ['wav','ogg']:(assets/f'bad.{ext}').write_bytes((assets/f'3.{ext}').read_bytes()[:-1])
    (work/'local-fixture-sha256.json').write_text(json.dumps({p.name:codecs.sha(p)for p in sorted(assets.iterdir())},indent=2)+'\n')
    env['W09_LOCAL_SOURCE_ASSETS']=str(assets)

def controls(work,env):
    declarations=[
        ('sequential-source-copies-pooled-chunks','local_source.rs','Some(chunk) => SourceRead::Data(chunk),','Some(chunk) => SourceRead::Data(chunk.as_ref().to_vec()),','sequential_chunks_retain_the_existing_two_slot_pool_owner'),
        ('sequential-source-collects-before-pcm','local_source.rs','''        self.chunk().await.map(|chunk| match chunk {
            Some(chunk) => SourceRead::Data(chunk),
            None => SourceRead::End,
        })''','''        let mut all=Vec::new();
        while let Some(chunk)=self.chunk().await? {all.extend_from_slice(chunk.as_ref());}
        Ok(if all.is_empty(){SourceRead::End}else{SourceRead::Data(all)})''','first_pcm_and_four_slot_backpressure_do_not_collect_a_whole_file'),
        ('sequential-io-error-becomes-eof','local_source.rs','        self.chunk().await.map(|chunk| match chunk {','        self.chunk().await.or(Ok(None)).map(|chunk| match chunk {','sequential_chunks_retain_the_existing_two_slot_pool_owner'),
        ('sequential-cpu-runs-on-io','off_worker.rs','''let (value, outcome) = cpu
            .run(move || {
                let outcome = step(&mut value);
                (value, outcome)
            })
            .await?;''','''let outcome=step(&mut value);let(value,outcome)=(value,outcome);''','every_format_backend_partition_and_rate_matches_whole_pcm'),
    ]
    for name,path,before,after,test in declarations:
        source=work/'source'/path;original=source.read_text();assert original.count(before)==1,(name,'anchor drift')
        source.write_text(original.replace(before,after))
        try:
            binary=file.dsp.source.build(work,env,name);log=work/f'{name}-test.log'
            with log.open('w')as out:r=subprocess.run([str(binary),'--exact','local_source_contracts::'+test,'--nocapture'],cwd=work/'probe',env=env,stdout=out,stderr=subprocess.STDOUT,timeout=120)
            observed=log.read_text();assert r.returncode==101 and 'panicked at'in observed and '1 failed'in observed,(name,'must reject an executed assertion')
        finally:source.write_text(original)
        print('actual sequential source fault',name,': executable assertion rejected',flush=True)
    return len(declarations)

def prepare(work,target,rubato,extra_inputs=()):
    extra=[Path(__file__).resolve(),ROOT/'scripts/testing/check_audio_ogg_streaming.py',ROOT/'scripts/testing/check_audio_metadata_skip.py']
    extra.extend(extra_inputs)
    env,frozen,_,libraries,new=file.prepare(work,target,rubato,extra)
    package=work/'probe';manifest=(package/'Cargo.toml').read_text()
    manifest=manifest.replace('"isomp4","id3v2","opt-simd"','"isomp4","id3v2","opt-simd","wav","pcm","ogg","vorbis"',1)
    manifest=manifest.replace('symphonia-codec-aac="=0.6.1"','symphonia-codec-aac={version="=0.6.1",features=["streaming"]}',1)
    dependencies='''symphonia-format-riff={version="=0.6.1",default-features=false,features=["wav","streaming"]}
symphonia-codec-pcm="=0.6.1"
symphonia-format-ogg={version="=0.6.1",features=["streaming"]}
symphonia-codec-vorbis="=0.6.1"
'''
    manifest=manifest.replace('\n[dependencies]\n','\n[dependencies]\n'+dependencies,1)
    manifest=manifest.replace('"m4a-file-probe"]','"m4a-file-probe","wave-streaming-probe","adts-streaming-probe","vorbis-streaming-probe"]\nwave-streaming-probe=[]\nadts-streaming-probe=[]\nvorbis-streaming-probe=[]',1)
    (package/'Cargo.toml').write_text(manifest)
    for name in ['wave_streaming.rs','adts_streaming.rs','vorbis_streaming.rs','local_source_contracts.rs']:shutil.copy2(ROOT/'scripts/testing/fixtures/audio-codec-probe'/name,work/'source'/name)
    lib=package/'src/lib.rs';lib.write_text(lib.read_text()+f'\n#[cfg(test)] #[path={json.dumps(str(work/"source/local_source_contracts.rs"))}] mod local_source_contracts;\n')
    fixtures(work,env)
    return env,frozen,libraries,new

def main(work,target,rubato,positive_only):
    env,frozen,libraries,new=prepare(work,target,rubato)
    package=work/'probe'
    retained={str(p.relative_to(work)):codecs.sha(p)for tree in [package/'src',work/'source']for p in tree.rglob('*.rs')}
    codecs.run(['cargo','metadata','--offline','--format-version','1'],work/'candidate-metadata.jsonl',cwd=package,env=env)
    selected=json.loads(next(line for line in(work/'candidate-metadata.jsonl').read_text().splitlines()if line.startswith('{"packages"')))['packages'];locked={}
    for row in tomllib.loads((ROOT/'engine/Cargo.lock').read_text())['package']:locked.setdefault(row['name'],set()).add(row['version'])
    assert all(row['name']not in locked or row['version']in locked[row['name']]for row in selected),'existing dependency drift'
    codecs.run(['cargo','check','--offline','--locked','--lib'],work/'normal-library-check.log',cwd=package,env=env)
    binary=file.dsp.source.build(work,env,'positive')
    checks=[('sequential','local_source_contracts::',5),('local','isomp4_source_contracts::transport::local::',11),('package','isomp4_source_contracts::transport::local::package::',8),('directory','isomp4_source_contracts::transport::local::directory::',5),('setup','local_source::tests::',1),('http','isomp4_source_contracts::transport::',11),('source','isomp4_source_contracts::',25),('range','network::http::range::tests::',18),('common','streaming_transport::tests::',3)]
    for label,prefix,count in checks:
        args=[str(binary),prefix,'--nocapture','--test-threads=1']
        if label=='local':args+=['--skip','isomp4_source_contracts::transport::local::package::','--skip','isomp4_source_contracts::transport::local::directory::']
        if label=='http':args+=['--skip','isomp4_source_contracts::transport::local::']
        if label=='source':args+=['--skip','isomp4_source_contracts::transport::']
        log=work/f'positive-{label}.log';codecs.run(args,log,cwd=package,env=env);assert f'{count} passed; 0 failed'in log.read_text(),label
    negative=0 if positive_only else controls(work,env)
    local_negative=0 if positive_only else file.controls(work,env)
    http_negative=0 if positive_only else file.dsp.controls(work,env)
    assert all(codecs.sha(ROOT/rel)==h for rel,h in frozen.items()),'Root changed'
    assert all(codecs.sha(work/rel)==h for rel,h in retained.items()),'source copy not restored'
    rows=json.loads((work/'library-source-provenance.json').read_text());assert len(rows)==9 and sum(len(r['patches'])for r in rows)==26
    for row in rows:assert file.dsp.source.index_gate.timeline.source_tree(libraries[row['crate']])==row['source_files']
    for name,row in new.items():assert all(codecs.sha(work/'resampling-vendor'/f"{name}-{row['version']}"/p)==h for p,h in row['source_files'].items())
    summary={'sequential_tests':5,'sequential_pcm_comparisons':256,'sequential_player_cases':16,'sequential_compiled_controls':negative,'local_compiled_controls':local_negative,'http_compiled_controls':http_negative,'m4a_local_tests':11,'m4a_package_tests':8,'m4a_directory_tests':5,'setup_tests':1,'m4a_http_tests':11,'legacy_source_tests':25,'range_tests':18,'common_tests':3,'binary_sha256':codecs.sha(binary),'positive_only':positive_only,'libraries':len(rows),'patches':26,'root_inputs_unchanged':True,'source_copies_restored':True,'existing_dependency_versions_preserved':True,'root_lock_sha256':codecs.sha(ROOT/'engine/Cargo.lock'),'scope':'Host MP3/WAV/AAC-LC ADTS/Ogg Vorbis common anchored file/VFS/directory/package source candidates with shared CPU/DSP/PCM/player and M4A regressions. Not writable spool/full playback controls/product cutover/native/device/maximum performance acceptance.'}
    (work/'summary.json').write_text(json.dumps(summary,indent=2)+'\n');print(json.dumps(summary,indent=2),flush=True)
if __name__=='__main__':
    parser=argparse.ArgumentParser(description=__doc__);parser.add_argument('--output',type=Path,required=True);parser.add_argument('--target-dir',type=Path,required=True);parser.add_argument('--rubato-cargo-home',type=Path,required=True);parser.add_argument('--positive-only',action='store_true');a=parser.parse_args();main(a.output.resolve(),a.target_dir.resolve(),a.rubato_cargo_home.resolve(),a.positive_only)
