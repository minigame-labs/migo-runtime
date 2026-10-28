#!/usr/bin/env python3
"""Prove pre-PCM HTTP capability selection without reopening or mixing bodies.

An Accept-Ranges header is not a successful range exchange. Only missing
capability may select the original response's bounded spool; representation
errors must remain errors, and already admitted decoders must not retry.
"""
from __future__ import annotations
import argparse, importlib.util, json, shutil, subprocess, tomllib
from pathlib import Path
ROOT=Path(__file__).resolve().parents[2]
def module(name,path):
    spec=importlib.util.spec_from_file_location(name,path);value=importlib.util.module_from_spec(spec);spec.loader.exec_module(value);return value
local=module('http_source_local',ROOT/'scripts/testing/check_audio_local_sources.py')
spool=module('http_source_spool',ROOT/'scripts/testing/check_audio_spool.py')
codecs=local.codecs

def declarations():
    pcm='m4a_actual_range_capability_or_original_response_spool_preserves_pcm'
    response='unread_complete_response_is_required_before_admission'
    return [
        ('prepared-probe-ignores-playback-cancel','format_source.rs','let Some(found) = streaming_transport::while_active(\n        tx,\n        state,','let unlinked_state = StreamingState::new();\n    let Some(found) = streaming_transport::while_active(\n        tx,\n        &unlinked_state,','queued_unknown_probe_stops_on_playback_cancel_and_receiver_exit'),
        ('range-unavailable-does-not-spool','http_source.rs','Err(range::Error::RangeUnsupported) => {}','Err(range::Error::RangeUnsupported) => return Err(error("range unavailable")),',pcm),
        ('missing-validator-does-not-spool','http_source.rs','Err(range::Error::NoStrongValidator) => {}','Err(range::Error::NoStrongValidator) => return Err(error("no validator")),',pcm),
        ('range-errors-downgraded-to-spool','http_source.rs','Err(range::Error::RangeUnsupported) => {}\n                    Err(e) => return Err(error(e)),','Err(_) => {},','changed_or_incomplete_range_is_an_error_without_fallback_or_decoder'),
        ('range-capability-not-demonstrated','http_source.rs','reader.read_exact_at(0, 1, probe_deadline)','std::future::ready(Ok::<_, range::Error>(Vec::<u8>::new()))',pcm),
        ('range-probe-reader-discarded','isomp4_transport.rs','let source = IndexSource::new(reader, limits);','let source = IndexSource::new(range::Reader::new(client.clone(), GateKind::AudioStream, range::Identity::capture(&response).unwrap(), range::Limits::default()), limits);',pcm),
        ('partial-status-admitted','http_source.rs','response.status() != StatusCode::OK','false',response),
        ('partial-header-admitted','http_source.rs','response.headers().contains_key(CONTENT_RANGE)','false',response),
        ('consumed-body-admitted','http_source.rs','response.decoded_body_bytes() != 0','false',response),
        ('decoded-byte-space-treated-as-range','http_source.rs','format == Some(Format::M4a) && !response.has_decoded_representation()','format == Some(Format::M4a)','decoded_m4a_uses_one_decoded_response_without_byte_ranges'),
        ('sequential-always-spooled','http_source.rs','if matches!(\n        format,','if false && matches!(\n        format,','known_sequential_formats_keep_one_response_and_do_not_create_spool'),
        ('fallback-renews-deadline','http_source.rs','let reader = spool_source::response(response, owner, limits, tx, state).await?;','let mut limits = limits; limits.deadline = Instant::now() + std::time::Duration::from_secs(3);\n    let reader = spool_source::response(response, owner, limits, tx, state).await?;','fallback_keeps_original_absolute_deadline_and_disk_limit'),
        ('fallback-reopens-object','http_source.rs','spool_source::response(response, owner, limits, tx, state)','spool_source::http(client.clone(), response.url().to_string(), owner, limits, tx, state)',pcm),
    ]
def validate(base):
    for name,file,before,_,_ in declarations():assert (base/file).read_text().count(before)==1,(name,'anchor drift')

def main(work,target,rubato,positive_only):
    fixtures=ROOT/'scripts/testing/fixtures/audio-codec-probe';validate(fixtures)
    extra=[Path(__file__).resolve(),Path(spool.__file__).resolve(),ROOT/'scripts/testing/check_audio_format_sources.py',ROOT/'scripts/testing/check_gitignore_contract.py']
    env,frozen,libraries,new=local.prepare(work,target,rubato,extra);package=work/'probe'
    env.pop('MIGO_AUDIO_CHECKS_BLESS',None);runtime=work/'runtime-tmp';runtime.mkdir();env.update(TMPDIR=str(runtime),TMP=str(runtime),TEMP=str(runtime),CC='/usr/bin/clang',CXX='/usr/bin/clang++')
    fixture=Path('platforms/apple/WebContent/PerformancePlus/test/fixtures/audio-check-answers.json')
    for dest in [work/fixture,work/'source-inputs'/fixture]:dest.parent.mkdir(parents=True,exist_ok=True);shutil.copy2(ROOT/fixture,dest)
    frozen[str(fixture)]=codecs.sha(ROOT/fixture);(work/'source-inputs.json').write_text(json.dumps(frozen,indent=2)+'\n')
    spool.include(work);lib=package/'src/lib.rs'
    for name,testing in [('format_source',False),('format_source_contracts',True),('auto_source_contracts',True),('http_source',False),('http_source_contracts',True),('spool_format_contracts',True)]:
        path=work/'source'/f'{name}.rs';shutil.copy2(fixtures/f'{name}.rs',path)
        lib.write_text(lib.read_text()+'\n'+('#[cfg(test)] 'if testing else'')+f'#[path={json.dumps(str(path))}] mod {name};\n')
    retained={str(p.relative_to(work)):codecs.sha(p)for tree in [package/'src',work/'source']for p in tree.rglob('*.rs')};validate(work/'source')
    codecs.run(['cargo','metadata','--offline','--format-version','1'],work/'candidate-metadata.jsonl',cwd=package,env=env)
    selected=json.loads(next(l for l in(work/'candidate-metadata.jsonl').read_text().splitlines()if l.startswith('{"packages"')))['packages'];locked={}
    for row in tomllib.loads((ROOT/'engine/Cargo.lock').read_text())['package']:locked.setdefault(row['name'],set()).add(row['version'])
    assert all(r['name']not in locked or r['version']in locked[r['name']]for r in selected),'existing dependency drift'
    codecs.run(['cargo','check','--offline','--locked','--lib'],work/'normal-library-check.log',cwd=package,env=env)
    binary=local.file.dsp.source.build(work,env,'positive')
    checks=[('selection','http_source_contracts::',10),('format','format_source_contracts::',7),('automatic','auto_source_contracts::',7),('spool','spool_contracts::',5),('spool-owner','spool_source::tests::',4),('sequential','local_source_contracts::',5),('http','isomp4_source_contracts::transport::',11),('source','isomp4_source_contracts::',25),('range','network::http::range::tests::',18),('common','streaming_transport::tests::',3),('local','isomp4_source_contracts::transport::local::',11),('package','isomp4_source_contracts::transport::local::package::',8),('directory','isomp4_source_contracts::transport::local::directory::',5),('setup','local_source::tests::',1),('spool-formats','spool_format_contracts::',3)]
    for label,prefix,count in checks:
        command=[str(binary),prefix,'--nocapture','--test-threads=1']
        if label=='local':command+=['--skip','isomp4_source_contracts::transport::local::package::','--skip','isomp4_source_contracts::transport::local::directory::']
        if label=='http':command+=['--skip','isomp4_source_contracts::transport::local::']
        if label=='source':command+=['--skip','isomp4_source_contracts::transport::']
        log=work/f'positive-{label}.log';codecs.run(command,log,cwd=package,env=env);assert f'{count} passed; 0 failed'in log.read_text(),label
    observed=(work/'positive-selection.log').read_text()
    for name,count in [('SEQUENTIAL',32),('M4A',48),('UNKNOWN',20)]:assert f'W09_HTTP_SELECT_{name}_PCM {count}'in observed
    faults=0
    if not positive_only:
        for name,file,before,after,test in declarations():
            path=work/'source'/file;original=path.read_text();assert original.count(before)==1;path.write_text(original.replace(before,after))
            try:
                mutant=local.file.dsp.source.build(work,env,name);log=work/f'{name}-test.log'
                with log.open('w')as output:r=subprocess.run([str(mutant),'--exact','http_source_contracts::'+test,'--nocapture'],cwd=package,env=env,stdout=output,stderr=subprocess.STDOUT,timeout=120)
                observed=log.read_text();assert r.returncode==101 and'panicked at'in observed and'1 failed'in observed,(name,'require executed assertion',r.returncode)
            finally:path.write_text(original)
            faults+=1;print('actual HTTP selection fault',name,': executed assertion rejected',flush=True)
    assert all(codecs.sha(ROOT/p)==h for p,h in frozen.items())and all(codecs.sha(work/p)==h for p,h in retained.items())
    rows=json.loads((work/'library-source-provenance.json').read_text());assert len(rows)==9 and sum(len(r['patches'])for r in rows)==27
    for row in rows:assert local.file.dsp.source.index_gate.timeline.source_tree(libraries[row['crate']])==row['source_files']
    for name,row in new.items():assert all(codecs.sha(work/'resampling-vendor'/f"{name}-{row['version']}"/p)==h for p,h in row['source_files'].items())
    summary={'selection_tests':10,'selection_pcm_comparisons':100,'positive_tests':sum(row[2]for row in checks),'compiled_controls':faults,'positive_only':positive_only,'binary_sha256':codecs.sha(binary),'libraries':9,'patches':27,'root_inputs_unchanged':True,'source_copies_restored':True,'existing_dependency_versions_preserved':True,'root_lock_sha256':codecs.sha(ROOT/'engine/Cargo.lock'),'scope':'Pre-PCM automatic HTTP capability selection: known sequential streaming, demonstrated M4A range, original-response bounded spool and unknown-container library identification. No product/native/device/peak-performance acceptance.'}
    (work/'summary.json').write_text(json.dumps(summary,indent=2)+'\n');print(json.dumps(summary,indent=2),flush=True)
if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--output',type=Path,required=True);p.add_argument('--target-dir',type=Path,required=True);p.add_argument('--rubato-cargo-home',type=Path,required=True);p.add_argument('--positive-only',action='store_true');a=p.parse_args();main(a.output.resolve(),a.target_dir.resolve(),a.rubato_cargo_home.resolve(),a.positive_only)
