#!/usr/bin/env python3
"""Replay codecs and prove the common automatic local-source factory.

Directory path names do not prove immutable bytes. Select one admitted backend,
snapshot only the explicit managed-mutable contract, and retain source deadlines
and resident credit through the existing codec/CPU/DSP pipeline.
"""
from __future__ import annotations
import argparse, importlib.util, json, shutil, subprocess, tomllib
from pathlib import Path
ROOT=Path(__file__).resolve().parents[2]
spec=importlib.util.spec_from_file_location('automatic_local',ROOT/'scripts/testing/check_audio_local_sources.py')
local=importlib.util.module_from_spec(spec);spec.loader.exec_module(local);codecs=local.codecs

def declarations():
    return [
        ('mutable-source-downgraded-to-direct','DirectoryOwnership::Snapshot => {\n                    let deadline',
         'DirectoryOwnership::Snapshot => {\n                    if snapshot.max_bytes > 0 { return Self::open_directory(pin, &scheduler).await; }\n                    let deadline',
         'failed_snapshot_admission_never_falls_back_to_direct_read'),
        ('automatic-package-uses-fs','Self::build_source(scheduler, PoolKind::Pack, 0, deadline, move || {',
         'Self::build_source(scheduler, PoolKind::Fs, 0, deadline, move || {',
         'automatic_package_setup_uses_pack_without_native_work_at_admission'),
        ('automatic-package-renews-deadline','Self::open_package_at(pin, &scheduler, snapshot.deadline).await',
         'Self::open_package_at(pin, &scheduler, Instant::now() + RESPONSE_HEADER_TIMEOUT).await',
         'automatic_package_setup_keeps_the_original_deadline_without_repoll'),
        ('snapshot-media-credit-charged-twice','Self::Spool(v) => v.reader().transient_media_bytes(),',
         'Self::Spool(_) => FILE_CHUNK_BYTES as u64,',
         'snapshot_media_uses_persistent_credit_without_charging_it_again'),
    ]

def validate(path):
    text=path.read_text()
    for name,before,_,_ in declarations():assert text.count(before)==1,(name,'anchor drift')

def main(work,target,rubato,positive_only):
    validate(ROOT/'scripts/testing/fixtures/audio-codec-probe/local_source.rs')
    env,frozen,libraries,new=local.prepare(work,target,rubato,[Path(__file__).resolve()]);package=work/'probe'
    env.pop('MIGO_AUDIO_CHECKS_BLESS',None)
    runtime=work/'runtime-tmp';runtime.mkdir();env.update(TMPDIR=str(runtime),TMP=str(runtime),TEMP=str(runtime),CC='/usr/bin/clang',CXX='/usr/bin/clang++')
    fixture=Path('platforms/apple/WebContent/PerformancePlus/test/fixtures/audio-check-answers.json');destination=work/fixture;destination.parent.mkdir(parents=True,exist_ok=True);shutil.copy2(ROOT/fixture,destination)
    for path in [ROOT/fixture,ROOT/'scripts/testing/check_gitignore_contract.py']:
        relative=str(path.relative_to(ROOT));frozen[relative]=codecs.sha(path);dest=work/'source-inputs'/relative;dest.parent.mkdir(parents=True,exist_ok=True);shutil.copy2(path,dest)
    (work/'source-inputs.json').write_text(json.dumps(frozen,indent=2)+'\n')
    source=work/'source/auto_source_contracts.rs';shutil.copy2(ROOT/'scripts/testing/fixtures/audio-codec-probe/auto_source_contracts.rs',source)
    lib=package/'src/lib.rs';lib.write_text(lib.read_text()+f'\n#[cfg(test)] #[path={json.dumps(str(source))}] mod auto_source_contracts;\n')
    retained={str(p.relative_to(work)):codecs.sha(p)for tree in [package/'src',work/'source']for p in tree.rglob('*.rs')}
    validate(work/'source/local_source.rs')
    codecs.run(['cargo','metadata','--offline','--format-version','1'],work/'candidate-metadata.jsonl',cwd=package,env=env)
    selected=json.loads(next(l for l in(work/'candidate-metadata.jsonl').read_text().splitlines()if l.startswith('{"packages"')))['packages'];locked={}
    for row in tomllib.loads((ROOT/'engine/Cargo.lock').read_text())['package']:locked.setdefault(row['name'],set()).add(row['version'])
    assert all(r['name']not in locked or r['version']in locked[r['name']]for r in selected),'existing dependency drift'
    codecs.run(['cargo','check','--offline','--locked','--lib'],work/'normal-library-check.log',cwd=package,env=env)
    binary=local.file.dsp.source.build(work,env,'positive')
    checks=[('automatic','auto_source_contracts::',7),('sequential','local_source_contracts::',5),
            ('local','isomp4_source_contracts::transport::local::',11),('package','isomp4_source_contracts::transport::local::package::',8),
            ('directory','isomp4_source_contracts::transport::local::directory::',5),('setup','local_source::tests::',1)]
    for label,prefix,count in checks:
        command=[str(binary),prefix,'--nocapture','--test-threads=1']
        if label=='local':command+=['--skip','isomp4_source_contracts::transport::local::package::','--skip','isomp4_source_contracts::transport::local::directory::']
        log=work/f'positive-{label}.log';codecs.run(command,log,cwd=package,env=env);assert f'{count} passed; 0 failed'in log.read_text(),label
    observed=(work/'positive-automatic.log').read_text();assert 'W09_AUTO_SOURCE_PCM 192 exact comparisons'in observed
    count=0
    if not positive_only:
        path=work/'source/local_source.rs'
        for name,before,after,test in declarations():
            original=path.read_text();assert original.count(before)==1;path.write_text(original.replace(before,after))
            try:
                mutant=local.file.dsp.source.build(work,env,name);log=work/f'{name}-test.log'
                with log.open('w')as output:r=subprocess.run([str(mutant),'--exact','auto_source_contracts::'+test,'--nocapture'],cwd=package,env=env,stdout=output,stderr=subprocess.STDOUT,timeout=60)
                observed=log.read_text();assert r.returncode==101 and'panicked at'in observed and'1 failed'in observed,(name,'require executed assertion',r.returncode)
            finally:path.write_text(original)
            count+=1;print('actual automatic audio source fault',name,': executed assertion rejected',flush=True)
    assert all(codecs.sha(ROOT/p)==h for p,h in frozen.items())and all(codecs.sha(work/p)==h for p,h in retained.items())
    rows=json.loads((work/'library-source-provenance.json').read_text());assert len(rows)==9 and sum(len(r['patches'])for r in rows)==29
    for row in rows:assert local.file.dsp.source.index_gate.timeline.source_tree(libraries[row['crate']])==row['source_files']
    for name,row in new.items():assert all(codecs.sha(work/'resampling-vendor'/f"{name}-{row['version']}"/p)==h for p,h in row['source_files'].items())
    summary={'automatic_tests':7,'automatic_pcm_comparisons':192,'compiled_controls':count,'positive_only':positive_only,'binary_sha256':codecs.sha(binary),'libraries':9,'patches':29,'root_inputs_unchanged':True,'source_copies_restored':True,'existing_dependency_versions_preserved':True,'root_lock_sha256':codecs.sha(ROOT/'engine/Cargo.lock'),'scope':'Automatic local directory/package dispatch and explicit immutable/mutable ownership, source snapshots connected to common codecs/CPU/DSP, deadlines and resident credit. Existing local/M4A regression suites retained. Not automatic format probing, HTTP fallback/product/native/device/maximum-performance acceptance.'}
    (work/'summary.json').write_text(json.dumps(summary,indent=2)+'\n');print(json.dumps(summary,indent=2),flush=True)

if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--output',type=Path,required=True);p.add_argument('--target-dir',type=Path,required=True);p.add_argument('--rubato-cargo-home',type=Path,required=True);p.add_argument('--positive-only',action='store_true');a=p.parse_args();main(a.output.resolve(),a.target_dir.resolve(),a.rubato_cargo_home.resolve(),a.positive_only)
