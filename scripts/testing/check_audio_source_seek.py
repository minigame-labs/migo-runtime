#!/usr/bin/env python3
"""Verify explicit source seek on the real pinned HTTP/file and CPU ownership path.

Container ReadAt is continuous; explicit seek resets the retained codec/DSP,
trims presentation preroll before interleaving and can reopen EOF. Preserve
invalid user targets, cancellation, bounded input and existing source resumes.
A by-value IO future once overflowed the default test stack. Pin at the owner
and borrow through cancellation; prove actual assertion failures, including the
future budget. Publication actor/player commands and product cutover remain open.
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

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location('source_seek_file', ROOT / 'scripts/testing/check_audio_mp4_file.py')
file = importlib.util.module_from_spec(spec)
spec.loader.exec_module(file)
transport = file.dsp
codecs = file.codecs


def declarations():
    matrix='real_http_and_file_seek_reuse_eof_owners_and_exact_dsp_suffixes'
    bad='malformed_seek_steps_and_fatal_index_errors_never_enter_source_io'
    deadline='source_deadline_is_not_renewed_and_a_real_reposition_must_replace_the_body'
    cpu='cpu_deadline_returns_before_the_running_seek_and_skips_late_decode'
    return [
        ('source-seek-reject-drops-owner','indexed_seek.rs','''            CpuResult::Rejected(kind) => Ok(Some(SeekResult {
                decoder,
                status: SeekStatus::Rejected(kind),
            })),''','''            CpuResult::Rejected(_) => Err(invalid("rejected target retired decoder")),''','rejected_target_preserves_pending_source_and_recycles_the_same_loan'),
        ('source-seek-reject-loses-kind','indexed_seek.rs','status: SeekStatus::Rejected(kind),','status: SeekStatus::Rejected(SeekErrorKind::Unseekable),','rejected_target_preserves_pending_source_and_recycles_the_same_loan'),
        ('source-seek-cache-step-skipped','indexed_seek.rs','let step = value.step(&[], false, pcm.buffer_mut(), &active.cancelled)?;','let step = Step {consumed:0,progress:Progress::NeedSource,sample_rate:0,channels:0};',matrix),
        ('source-seek-cache-pcm-lost','indexed_seek.rs','let (pcm, result) = outcome?;','let (mut pcm, result) = outcome?; pcm.buffer_mut().clear();',matrix),
        ('source-seek-unoffered-input-accepted','indexed_seek.rs','if step.consumed != 0 {','if false {',bad),
        ('source-seek-retained-index-replaced','indexed_seek.rs','if !matches!(request, M4aSourceRequest::ReadAt(_)) {','if false {',bad),
        ('source-seek-corruption-downgraded','indexed_seek.rs','Err(error) => return Err(invalid(error)),','Err(_) => return Ok((pcm, Some(CpuResult::Rejected(SeekErrorKind::OutOfRange)))),',bad),
        ('source-seek-stale-pcm-loan-accepted','indexed_seek.rs','if invalid_loan {','if false {','uninitialized_cancelled_closed_and_expired_controls_do_not_touch_io'),
        ('source-seek-cpu-deadline-renewed','indexed_seek.rs','tokio::time::timeout_at(deadline, &mut operation)','tokio::time::timeout_at(deadline + std::time::Duration::from_secs(1), &mut operation)',cpu),
        ('source-seek-late-cpu-decode','indexed_seek.rs','''        if Instant::now() >= deadline {
            return Err(timeout());
        }
        let value = slot''','''        let value = slot''',cpu),
        ('source-seek-child-deadline-renewed','indexed_seek.rs','source.resume_before(request, decoder, context)','source.resume_before(request, decoder, SeekContext { deadline: Instant::now() + std::time::Duration::from_secs(1), ..context })',deadline),
        ('source-seek-io-deadline-renewed','indexed_seek.rs','tokio::time::timeout_at(deadline, &mut resume)','tokio::time::timeout_at(deadline + std::time::Duration::from_secs(1), &mut resume)',deadline),
        ('source-seek-body-restart-lost','indexed_seek.rs','(resumed.decoder, true)','(resumed.decoder, false)',matrix),
        ('source-seek-false-body-ack-accepted','indexed_seek.rs','if !resumed.restart {','if false {',deadline),
        ('source-seek-resolved-request-repeated','indexed_seek.rs','step.progress = Progress::Yield;','',matrix),
        ('source-seek-caller-allocates','indexed_seek.rs','let active = state.clone();','std::hint::black_box(vec![0_u8;64]); let active = state.clone();','cached_seek_adds_no_caller_allocation_beyond_one_existing_cpu_handoff'),
        ('source-seek-io-future-copied','indexed_seek.rs','''                    tokio::pin!(resume);
                    let Some(resumed) =
                        while_active(tx, state, tokio::time::timeout_at(deadline, &mut resume))
                            .await''','''                    let Some(resumed) =
                        while_active(tx, state, tokio::time::timeout_at(deadline, resume))
                            .await''','seek_future_fits_the_default_stack_without_boxing_or_stack_overrides'),
    ]


def controls(work, env):
    rows=declarations()
    for name,file,before,after,test in rows:
        path=work/'source'/file;original=path.read_text();assert original.count(before)==1,(name,'mutation anchor changed')
        path.write_text(original.replace(before,after))
        try:
            binary=transport.source.build(work,env,name);log=work/f'{name}-test.log'
            with log.open('w')as out:r=subprocess.run([str(binary),'--exact','source_seek_contracts::'+test,'--nocapture'],cwd=work/'probe',env=env,stdout=out,stderr=subprocess.STDOUT,timeout=120)
            observed=log.read_text();assert r.returncode==101 and 'panicked at'in observed and '1 failed'in observed,(name,'must reject an executed assertion')
        finally:path.write_text(original)
        print('actual source seek mutation',name,': executable assertion rejected',flush=True)
    return len(rows)


def main(work,target,rubato,positive_only):
    env,frozen,_,libraries,new=file.prepare(work,target,rubato)
    env.update(CC='/usr/bin/clang',CXX='/usr/bin/clang++');env.pop('MIGO_AUDIO_CHECKS_BLESS',None)
    package=work/'probe';lib=package/'src/lib.rs'
    for name in ['decoder_seek_contracts','source_seek_contracts']:
        fixture=ROOT/'scripts/testing/fixtures/audio-codec-probe'/(name+'.rs')
        path=work/'source'/fixture.name;shutil.copy2(fixture,path)
        lib.write_text(lib.read_text()+f'\n#[cfg(test)] #[path={json.dumps(str(path))}] mod {name};\n')
    extra=[Path(__file__).resolve(),fixture,ROOT/'scripts/testing/check_gitignore_contract.py']
    extra+=sorted((ROOT/'scripts/testing').glob('check_audio_*.py'))
    frozen.update({str(p.relative_to(ROOT)):codecs.sha(p)for p in extra})
    for p in extra:
        dest=work/'source-inputs'/p.relative_to(ROOT);dest.parent.mkdir(parents=True,exist_ok=True);shutil.copy2(p,dest)
    (work/'source-inputs.json').write_text(json.dumps(frozen,indent=2)+'\n')
    retained={str(p.relative_to(work)):codecs.sha(p)for tree in [package/'src',work/'source']for p in tree.rglob('*.rs')}
    (work/'retained-sources.json').write_text(json.dumps(retained,indent=2)+'\n')
    codecs.run(['cargo','metadata','--offline','--format-version','1'],work/'candidate-metadata.jsonl',cwd=package,env=env)
    known={r['name']for r in tomllib.loads((ROOT/'engine/Cargo.lock').read_text())['package']}
    identity=lambda r:(r['name'],r['version'],r.get('source'),r.get('checksum'))
    pinned={identity(r)for r in tomllib.loads((ROOT/'engine/Cargo.lock').read_text())['package']}
    assert all(r['name']not in known or identity(r)in pinned for r in tomllib.loads((package/'Cargo.lock').read_text())['package']),'existing dependency identity drift'
    codecs.run(['cargo','check','--offline','--locked','--lib'],work/'normal-library-check.log',cwd=package,env=env)
    binary=transport.source.build(work,env,'positive')
    checks=[('source-seek','source_seek_contracts::',10),('seek','decoder_seek_contracts::',9),('file','isomp4_source_contracts::transport::local::',11),('package','isomp4_source_contracts::transport::local::package::',8),('directory','isomp4_source_contracts::transport::local::directory::',5),('setup','local_source::tests::',1),('transport','isomp4_source_contracts::transport::',11),('source','isomp4_source_contracts::',25),('range','network::http::range::tests::',18),('common','streaming_transport::tests::',3),('player','inner_audio::',42)]
    for label,prefix,count in checks:
        args=[str(binary),prefix,'--nocapture','--test-threads=1']
        if label=='file':args+=['--skip','isomp4_source_contracts::transport::local::package::','--skip','isomp4_source_contracts::transport::local::directory::']
        if label=='transport':args+=['--skip','isomp4_source_contracts::transport::local::']
        if label=='source':args+=['--skip','isomp4_source_contracts::transport::']
        log=work/f'positive-{label}.log';codecs.run(args,log,cwd=package,env=env)
        assert f'{count} passed; 0 failed'in log.read_text(),label
    count=0 if positive_only else controls(work,env)
    legacy=0 if positive_only else transport.controls(work,env)
    assert all(codecs.sha(ROOT/p)==h for p,h in frozen.items()),'Root inputs changed'
    assert all(codecs.sha(work/p)==h for p,h in retained.items()),'test source not restored'
    rows=json.loads((work/'library-source-provenance.json').read_text());assert len(rows)==9 and sum(len(r['patches'])for r in rows)==29
    for row in rows:assert transport.source.index_gate.timeline.source_tree(libraries[row['crate']])==row['source_files']
    for name,row in new.items():assert all(codecs.sha(work/'resampling-vendor'/f"{name}-{row['version']}"/p)==h for p,h in row['source_files'].items())
    summary={'new_source_seek_tests':10,'new_source_pcm_suffixes':120,'retained_decoder_tests':9,'retained_common_pcm_suffixes':416,'retained_dsp_pcm_suffixes':60,'positive_tests':sum(r[2]for r in checks),'new_seek_controls':count,'retained_dsp_controls':legacy,'compiled_controls':count+legacy,'positive_only':positive_only,'binary_sha256':codecs.sha(binary),'libraries':9,'patches':29,'root_inputs_unchanged':True,'source_copies_restored':True,'existing_dependency_identities_preserved':True,'root_lock_sha256':codecs.sha(ROOT/'engine/Cargo.lock'),'scope':'Candidate one-step explicit source seek over real pinned file/HTTP,owned CPU decoder,PCM loans and shared absolute deadline. Publication actor/player controls,product/native/device/peak acceptance remain open.'}
    (work/'summary.json').write_text(json.dumps(summary,indent=2)+'\n');print(json.dumps(summary,indent=2),flush=True)

if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--output',type=Path,required=True);p.add_argument('--target-dir',type=Path,required=True);p.add_argument('--rubato-cargo-home',type=Path,required=True);p.add_argument('--positive-only',action='store_true');a=p.parse_args();main(a.output.resolve(),a.target_dir.resolve(),a.rubato_cargo_home.resolve(),a.positive_only)
