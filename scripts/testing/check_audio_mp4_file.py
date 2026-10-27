#!/usr/bin/env python3
"""Validate anchored immutable-file M4A through the shared source/DSP/player.

A local read must not reopen a pathname, run disk IO on an async/audio worker,
reuse a failed/cancelled reader, or replace the common MP4 parser/PCM loop.
The finite file buffer pool and real filesystem mutations make these observable.
"""
from __future__ import annotations
import argparse,importlib.util,json,shutil,subprocess,tomllib
from pathlib import Path
ROOT=Path(__file__).resolve().parents[2]
spec=importlib.util.spec_from_file_location('mp4_file_dsp',ROOT/'scripts/testing/check_audio_mp4_transport.py')
dsp=importlib.util.module_from_spec(spec);spec.loader.exec_module(dsp)
codecs=dsp.codecs

def controls(work,env):
    declarations=[
        ('cancelled-file-reader-reused','isomp4_file.rs','if self.failed || self.domain.is_closed() {','if self.domain.is_closed() {','cancelled_file_wait_leaves_io_worker_free_and_reader_failed'),
        ('file-io-runs-inline','isomp4_file.rs','        let position = self.position;','        std::thread::sleep(self.delay);\n        let position = self.position;','cancelled_file_wait_leaves_io_worker_free_and_reader_failed'),
        ('file-drop-does-not-cancel-queued-read','isomp4_file.rs','self.0.store(false, Ordering::Release);','self.0.store(true, Ordering::Release);','cancelled_file_wait_leaves_io_worker_free_and_reader_failed'),
        ('file-expired-deadline-still-submits','isomp4_file.rs','self.failed = true;\n        if Instant::now() >= deadline {','self.failed = true;\n        if false {','one_absolute_file_deadline_and_closed_domain_reject_new_and_queued_io'),
        ('file-queued-control-ignores-deadline','isomp4_file.rs','// Do not rely on the receiver polling its timeout promptly.\n                    check_deadline(deadline)?;','// Deadline check deliberately removed.','expired_queued_control_skips_disk_even_without_polling_timeout'),
        ('file-late-control-completion-accepted','isomp4_file.rs','fn check_deadline(deadline: Instant) -> std::io::Result<()> {\n    if Instant::now() >= deadline {','fn check_deadline(deadline: Instant) -> std::io::Result<()> {\n    if false {','late_control_completion_is_rejected_without_polling_timeout'),
        ('file-requires-no-code-namespace','isomp4_file.rs','if !path.starts_with("/code/") {','if false {','local_source_admission_uses_vfs_anchored_readonly_code_handles'),
    ]
    declarations += [
        ('file-closed-domain-data-published','isomp4_file.rs','                    let result = file.chunk_at(position, len);\n                    #[cfg(test)]\n                    if let Some(hook) = after_io {\n                        hook();\n                    }\n                    if domain.is_closed() || !read_active.load(Ordering::Acquire) {','                    let result = file.chunk_at(position, len);\n                    #[cfg(test)]\n                    if let Some(hook) = after_io {\n                        hook();\n                    }\n                    if false {','domain_close_after_disk_read_discards_late_data'),
    ]
    for name,file,before,after,test in declarations:
        path=work/'source'/file;original=path.read_text();assert original.count(before)==1,(name,'anchor changed')
        path.write_text(original.replace(before,after))
        try:
            binary=dsp.source.build(work,env,name);log=work/f'{name}-test.log'
            with log.open('w')as out:r=subprocess.run([str(binary),'--exact','isomp4_source_contracts::transport::local::'+test,'--nocapture'],cwd=work/'probe',env=env,stdout=out,stderr=subprocess.STDOUT,timeout=30)
            observed=log.read_text();assert r.returncode==101 and 'panicked at' in observed and '1 failed' in observed,(name,'must fail an executed assertion')
        finally:path.write_text(original)
        print('actual local source fault',name,': executable assertion rejected',flush=True)
    path=work/'source/isomp4_file.rs';original=path.read_text()
    before='let file = tokio::time::timeout_at(deadline, task)\n            .await\n            .map_err(|_| error("file setup deadline exceeded"))?'
    assert original.count(before)==1
    path.write_text(original.replace(before,'let file = task.await'))
    try:
        name='file-setup-ignores-timeout';binary=dsp.source.build(work,env,name);log=work/f'{name}-test.log'
        with log.open('w')as out:r=subprocess.run([str(binary),'--exact','isomp4_file::tests::file_setup_timeout_does_not_block_async_worker','--nocapture'],cwd=work/'probe',env=env,stdout=out,stderr=subprocess.STDOUT,timeout=30)
        observed=log.read_text();assert r.returncode==101 and 'panicked at' in observed and '1 failed' in observed,(name,'must fail an executed assertion')
    finally:path.write_text(original)
    print('actual local source fault',name,': executable assertion rejected',flush=True)
    return len(declarations)+1

def main(work,target,rubato,positive_only):
    env,frozen,_,libraries,new=dsp.prepare(work,target,rubato);package=work/'probe';files=ROOT/'scripts/testing/fixtures/audio-codec-probe'
    for name in ['isomp4_file.rs','isomp4_file_contracts.rs']:shutil.copy2(files/name,work/'source'/name)
    lib=package/'src/lib.rs';lib.write_text(lib.read_text()+f'\n#[path={json.dumps(str(work/"source/isomp4_file.rs"))}] mod isomp4_file;\n')
    manifest=(package/'Cargo.toml').read_text().replace('"m4a-transport-probe"]','"m4a-transport-probe","m4a-file-probe"]\nm4a-file-probe=[]',1);(package/'Cargo.toml').write_text(manifest)
    for name in ['user','cache','tmp']:(work/'sandbox'/name).mkdir(parents=True)
    (work/'sandbox/user/clip.m4a').write_bytes((work/'fixtures/1-tail.m4a').read_bytes())
    frozen[str(Path(__file__).relative_to(ROOT))]=codecs.sha(Path(__file__))
    for rel in ['scripts/testing/check_audio_mp4_transport.py','engine/crates/io/Cargo.toml']:
        frozen[rel]=codecs.sha(ROOT/rel)
    (work/'source-inputs.json').write_text(json.dumps(frozen,indent=2)+'\n')
    for rel in frozen:
        to=work/'source-inputs'/rel;to.parent.mkdir(parents=True,exist_ok=True);shutil.copy2(ROOT/rel,to)
    retained={str(p.relative_to(work)):codecs.sha(p)for tree in [package/'src',work/'source']for p in tree.rglob('*.rs')}
    codecs.run(['cargo','metadata','--offline','--format-version','1'],work/'candidate-metadata.jsonl',cwd=package,env=env)
    selected=json.loads(next(x for x in (work/'candidate-metadata.jsonl').read_text().splitlines()if x.startswith('{"packages"')))['packages'];locked={}
    for row in tomllib.loads((ROOT/'engine/Cargo.lock').read_text())['package']:locked.setdefault(row['name'],set()).add(row['version'])
    assert all(row['name']not in locked or row['version']in locked[row['name']]for row in selected),'existing dependency version drift'
    codecs.run(['cargo','check','--offline','--locked','--lib'],work/'normal-library-check.log',cwd=package,env=env)
    positive=dsp.source.build(work,env,'positive')
    for label,prefix,count in [('local','isomp4_source_contracts::transport::local::',11),('setup','isomp4_file::tests::',1),('http','isomp4_source_contracts::transport::',11),('source','isomp4_source_contracts::',25),('range','network::http::range::tests::',18),('common','streaming_transport::tests::',3)]:
        args=[str(positive),prefix,'--nocapture','--test-threads=1']
        if label=='http':args+=['--skip','isomp4_source_contracts::transport::local::']
        if label=='source':args+=['--skip','isomp4_source_contracts::transport::']
        log=work/f'positive-{label}.log';codecs.run(args,log,cwd=package,env=env);assert f'{count} passed; 0 failed'in log.read_text(),label
    negative=0 if positive_only else controls(work,env)
    http_controls=0 if positive_only else dsp.controls(work,env)
    assert all(codecs.sha(ROOT/p)==h for p,h in frozen.items()),'Root changed'
    assert all(codecs.sha(work/p)==h for p,h in retained.items()),'source copy not restored'
    rows=json.loads((work/'library-source-provenance.json').read_text())
    for row in rows:assert dsp.source.index_gate.timeline.source_tree(libraries[row['crate']])==row['source_files']
    for name,row in new.items():assert all(codecs.sha(work/'resampling-vendor'/f"{name}-{row['version']}"/p)==h for p,h in row['source_files'].items())
    summary={'local_tests':11,'file_setup_tests':1,'local_pcm_comparisons':34,'local_player_cases':5,'http_dsp_tests':11,'http_dsp_pcm_comparisons':61,'legacy_source_tests':25,'range_tests':18,'common_transport_tests':3,'local_compiled_controls':negative,'http_compiled_controls':http_controls,'positive_only':positive_only,'binary_sha256':codecs.sha(positive),'libraries':len(rows),'patches':sum(len(r['patches'])for r in rows),'root_inputs_unchanged':True,'source_copies_restored':True,'existing_dependency_versions_preserved':True,'root_lock_sha256':codecs.sha(ROOT/'engine/Cargo.lock'),'scope':'Host immutable anchored file and HTTP candidates through shared codec/index/DSP/player; no pack/writable-file spool/product/native/device or performance acceptance'}
    (work/'summary.json').write_text(json.dumps(summary,indent=2)+'\n');print(json.dumps(summary,indent=2),flush=True)
if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--output',type=Path,required=True);p.add_argument('--target-dir',type=Path,required=True);p.add_argument('--rubato-cargo-home',type=Path,required=True);p.add_argument('--positive-only',action='store_true');a=p.parse_args()
    main(a.output.resolve(),a.target_dir.resolve(),a.rubato_cargo_home.resolve(),a.positive_only)
