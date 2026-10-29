#!/usr/bin/env python3
"""Exercise a bounded single-response HTTP spool before decoder admission.

No validator/range or unknown length must never cause mixed representations or
an unbounded in-memory body. The sealed temporary file uses existing indexed
reader/codec/DSP; cancellation retains file, disk and memory owners through IO.
"""
from __future__ import annotations
import argparse,importlib.util,json,shutil,subprocess,tomllib
from pathlib import Path
ROOT=Path(__file__).resolve().parents[2]
spec=importlib.util.spec_from_file_location('spool_file',ROOT/'scripts/testing/check_audio_mp4_file.py');file=importlib.util.module_from_spec(spec);spec.loader.exec_module(file);codecs=file.codecs

def controls(work,env):
    # Source-level cancellation/deadline and credit-lifetime controls target the
    # local Session tests, independent of a codec's own EOF/error guards.
    before='''                check(&domain, &active, deadline)?;
                // Capture the whole BodyChunk, including its shared byte ticket.'''
    declarations=[
        ('spool-job-skips-final-cancel-check','spool_source.rs',before,'                // Deliberately omit the last cancellation/deadline check.','queued_cancelled_writes_keep_credit_and_cannot_reuse_or_seal'),
        ('spool-job-skips-absolute-deadline','spool_source.rs',before,'                // Deliberately omit the last cancellation/deadline check.','absolute_deadline_and_closed_domain_reject_setup_and_late_writes'),
        ('spool-job-loses-memory-credit','spool_source.rs','writer.append(chunk.as_ref())','writer.append(&chunk.data)','queued_cancelled_writes_keep_credit_and_cannot_reuse_or_seal'),
        ('spool-domain-close-wait-removed','spool_source.rs','_=domain.closed()=>Err(error("spool domain closed"))','_=std::future::pending::<()>()=>Err(error("spool domain closed"))','HTTP:cancellation_and_receiver_drop_abort_download_without_decoder_output'),
        ('spool-setup-write-seal-use-pack','spool_source.rs','            PoolKind::Fs,','            PoolKind::Pack,','spool_setup_write_seal_and_media_do_not_use_the_pack_class'),
        ('spool-media-uses-pack','local_source.rs','Self::Spool(_) => PoolKind::Fs,','Self::Spool(_) => PoolKind::Pack,','spool_setup_write_seal_and_media_do_not_use_the_pack_class'),
        ('spool-consumed-response-accepted','replay_body.rs','if response.decoded_body_bytes() != 0 {','if false {','HTTP:already_consumed_response_is_rejected_before_private_file_creation'),
        ('spool-body-credit-not-charged','source_chunk.rs','pools.reserve_bytes(data.len() as u64)','pools.reserve_bytes(0)','queued_cancelled_writes_keep_credit_and_cannot_reuse_or_seal'),
        ('sealed-admission-charges-media-credit','local_source.rs','''        Ok(Self::from_backing(
            Backing::Spool(spool),''','''        let _credit = scheduler.pools().reserve_bytes(FILE_CHUNK_BYTES as u64).map_err(error)?;
        Ok(Self::from_backing(
            Backing::Spool(spool),''','sealed_admission_keeps_the_deadline_owner_and_needs_no_io_credit'),
        ('sealed-admission-renews-deadline','local_source.rs','''        check_deadline(deadline).map_err(error)?;
        Ok(Self::from_backing(
            Backing::Spool(spool),''','''        check_deadline(Instant::now() + RESPONSE_HEADER_TIMEOUT).map_err(error)?;
        Ok(Self::from_backing(
            Backing::Spool(spool),''','sealed_admission_keeps_the_deadline_owner_and_needs_no_io_credit'),
        ('sealed-admission-ignores-closed-domain','local_source.rs','''        scheduler.ensure_open().map_err(error)?;
        check_deadline(deadline).map_err(error)?;
        Ok(Self::from_backing(
            Backing::Spool(spool),''','''        check_deadline(deadline).map_err(error)?;
        Ok(Self::from_backing(
            Backing::Spool(spool),''','sealed_admission_keeps_the_deadline_owner_and_needs_no_io_credit'),
    ]
    for name,filename,before,after,test in declarations:
        path=work/'source'/filename;source=path.read_text();assert source.count(before)==1,(name,'anchor drift');path.write_text(source.replace(before,after))
        try:
            binary=file.dsp.source.build(work,env,name);log=work/f'{name}-test.log'
            with log.open('w')as out:status=subprocess.run([str(binary),'--exact',('spool_contracts::'+test.removeprefix('HTTP:')if test.startswith('HTTP:')else'spool_source::tests::'+test),'--nocapture'],cwd=work/'probe',env=env,stdout=out,stderr=subprocess.STDOUT,timeout=30).returncode
            observed=log.read_text();assert status==101 and 'panicked at'in observed and '1 failed'in observed,(name,'must reject executed assertion',status)
        finally:path.write_text(source)
        print('actual spool source fault',name,': executed assertion rejected',flush=True)
    return len(declarations)

def include(work):
    package=work/'probe'
    for name in ['replay_body.rs','spool_source.rs','spool_contracts.rs']:shutil.copy2(ROOT/'scripts/testing/fixtures/audio-codec-probe'/name,work/'source'/name)
    lib=package/'src/lib.rs';lib.write_text(lib.read_text()+''.join(f'\n#[path={json.dumps(str(work/"source"/(name+".rs")))}] mod {name};\n'for name in ['replay_body','spool_source'])+f'\n#[cfg(test)] #[path={json.dumps(str(work/"source/spool_contracts.rs"))}] mod spool_contracts;\n')

def positive(work,env,binary,extra=()):
    checks=[*extra,('spool','spool_contracts::',5),('spool-owner','spool_source::tests::',4),('local','isomp4_source_contracts::transport::local::',11),('package','isomp4_source_contracts::transport::local::package::',8),('directory','isomp4_source_contracts::transport::local::directory::',5),('setup','local_source::tests::',1),('http','isomp4_source_contracts::transport::',11),('source','isomp4_source_contracts::',25),('range','network::http::range::tests::',18),('common','streaming_transport::tests::',3)]
    for label,prefix,count in checks:
        args=[str(binary),prefix,'--nocapture','--test-threads=1']
        if label=='local':args+=['--skip','isomp4_source_contracts::transport::local::package::','--skip','isomp4_source_contracts::transport::local::directory::']
        if label=='http':args+=['--skip','isomp4_source_contracts::transport::local::']
        if label=='source':args+=['--skip','isomp4_source_contracts::transport::']
        log=work/f'positive-{label}.log';codecs.run(args,log,cwd=work/'probe',env=env);assert f'{count} passed; 0 failed'in log.read_text(),label

def main(work,target,rubato,positive_only):
    env,frozen,_,libraries,new=file.prepare(work,target,rubato,[Path(__file__).resolve()]);package=work/'probe'
    include(work)
    retained={str(p.relative_to(work)):codecs.sha(p)for tree in [package/'src',work/'source']for p in tree.rglob('*.rs')}
    codecs.run(['cargo','metadata','--offline','--format-version','1'],work/'candidate-metadata.jsonl',cwd=package,env=env)
    selected=json.loads(next(l for l in(work/'candidate-metadata.jsonl').read_text().splitlines()if l.startswith('{"packages"')))['packages'];locked={}
    for row in tomllib.loads((ROOT/'engine/Cargo.lock').read_text())['package']:locked.setdefault(row['name'],set()).add(row['version'])
    assert all(r['name']not in locked or r['version']in locked[r['name']]for r in selected),'existing version drift'
    codecs.run(['cargo','check','--offline','--locked','--lib'],work/'normal-library-check.log',cwd=package,env=env)
    binary=file.dsp.source.build(work,env,'positive')
    positive(work,env,binary)
    negative=0 if positive_only else controls(work,env)
    local=0 if positive_only else file.controls(work,env)
    assert all(codecs.sha(ROOT/p)==h for p,h in frozen.items()),'Root changed';assert all(codecs.sha(work/p)==h for p,h in retained.items()),'source not restored'
    rows=json.loads((work/'library-source-provenance.json').read_text());assert len(rows)==9 and sum(len(r['patches'])for r in rows)==29
    for row in rows:assert file.dsp.source.index_gate.timeline.source_tree(libraries[row['crate']])==row['source_files']
    for name,row in new.items():assert all(codecs.sha(work/'resampling-vendor'/f"{name}-{row['version']}"/p)==h for p,h in row['source_files'].items())
    summary={'spool_http_tests':5,'spool_lifetime_tests':4,'spool_pcm_comparisons':32,'spool_compiled_controls':negative,'local_compiled_controls':local,'http_compiled_controls_recounted':0,'m4a_local_tests':11,'m4a_package_tests':8,'m4a_directory_tests':5,'setup_tests':1,'m4a_http_tests':11,'legacy_source_tests':25,'range_tests':18,'common_tests':3,'positive_only':positive_only,'binary_sha256':codecs.sha(binary),'libraries':9,'patches':29,'root_inputs_unchanged':True,'source_copies_restored':True,'existing_dependency_versions_preserved':True,'root_lock_sha256':codecs.sha(ROOT/'engine/Cargo.lock'),'scope':'Explicit pre-PCM single-response HTTP spool into sealed private file; M4A shared IO/CPU/DSP regressions. Not automatic product fallback dispatch, atomic snapshot of game-writable files, crash recovery, full playback controls, native/device or peak-performance acceptance.'}
    (work/'summary.json').write_text(json.dumps(summary,indent=2)+'\n');print(json.dumps(summary,indent=2),flush=True)
if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--output',type=Path,required=True);p.add_argument('--target-dir',type=Path,required=True);p.add_argument('--rubato-cargo-home',type=Path,required=True);p.add_argument('--positive-only',action='store_true');a=p.parse_args();main(a.output.resolve(),a.target_dir.resolve(),a.rubato_cargo_home.resolve(),a.positive_only)
