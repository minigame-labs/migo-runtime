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
        ('partial-status-admitted','replay_body.rs','response.status() != http::StatusCode::OK','false',response),
        ('partial-header-admitted','replay_body.rs','response.headers().contains_key(http::header::CONTENT_RANGE)','false',response),
        ('consumed-body-admitted','replay_body.rs','response.decoded_body_bytes() != 0','false',response),
        ('decoded-byte-space-treated-as-range','http_source.rs','format == Some(Format::M4a) && !decoded && !body.complete()','format == Some(Format::M4a)','decoded_m4a_uses_one_decoded_response_without_byte_ranges'),
        ('sequential-always-spooled','http_source.rs','if matches!(\n        format,','if false && matches!(\n        format,','known_sequential_formats_keep_one_response_and_do_not_create_spool'),
        ('fallback-renews-deadline','http_source.rs','let reader = spool_source::body(body, owner, limits, tx, state).await?;','let mut limits = limits; limits.deadline = Instant::now() + std::time::Duration::from_secs(3);\n    let reader = spool_source::body(body, owner, limits, tx, state).await?;','fallback_keeps_original_absolute_deadline_and_disk_limit'),
        ('fallback-reopens-object','http_source.rs','spool_source::body(body, owner, limits, tx, state)','spool_source::http(client.clone(), body.response().url().to_string(), owner, limits, tx, state)',pcm),
    ]
def prefix_declarations():
    base='http_prefix_contracts::'
    fallback=base+'oversized_prefix_spools_the_exact_consumed_prefix_once'
    credit=base+'replay_keeps_payload_pointer_order_and_credit_after_source_drop'
    return [
        ('prefix-parser-inline','http_prefix.rs','let operation = cpu.run(job);','let operation = std::future::ready(Ok::<_, shared::error::EngineError>(job()));',base+'unknown_sequential_formats_publish_pcm_before_the_original_body_eof'),
        ('prefix-scratch-unreserved','http_prefix.rs','.reserve_bytes(SCRATCH_BYTES)','.reserve_bytes(1)',base+'partial_prefix_body_cancel_deadline_and_scratch_admission_fail_without_disk'),
        ('prefix-byte-limit-expanded','http_prefix.rs','const MAX_PREFIX_BYTES: u64 = 64 * 1024;','const MAX_PREFIX_BYTES: u64 = 256 * 1024;',fallback),
        ('prefix-read-limit-expanded','http_prefix.rs','const MAX_PREFIX_READS: usize = 128;','const MAX_PREFIX_READS: usize = 4096;',base+'tiny_http_frames_hit_the_read_count_bound_and_keep_exact_fallback_bytes'),
        ('sequential-replay-discarded','replay_body.rs','if let Some(chunk) = self.prefix.pop_front() {','self.prefix.clear();\n        if let Some(chunk) = self.prefix.pop_front() {',credit),
        ('indexed-replay-discarded','isomp4_transport.rs','self.replay = prefix;','drop(prefix);',base+'complete_m4a_prefix_uses_cached_bytes_without_disk_or_range_get'),
        ('remaining-length-replaces-original','replay_body.rs','let length = self.expected.or(self.eof.then_some(self.received));','let length = self.response.content_length();',fallback),
        ('prefix-cpu-cancel-unlinked','http_prefix.rs','let Some(result) =\n            while_active(tx, state, tokio::time::timeout_at(deadline, operation)).await','let unlinked_state = StreamingState::new();\n        let Some(result) =\n            while_active(tx, &unlinked_state, tokio::time::timeout_at(deadline, operation)).await',base+'cancelled_active_cpu_probe_keeps_credit_until_worker_exit_and_stops_reads'),
        ('active-prefix-cancel-flag-lost','http_prefix.rs','self.0.store(false, Ordering::Release);','self.0.store(true, Ordering::Release);',base+'cancelled_active_cpu_probe_keeps_credit_until_worker_exit_and_stops_reads'),
        ('prefix-cpu-deadline-removed','http_prefix.rs','if Instant::now() >= deadline {','if false {',base+'late_cpu_probe_checks_original_deadline_before_read_without_waiter_repoll'),
        ('complete-cache-disabled','http_source.rs','if format == Some(Format::M4a)\n        && body.complete()','if false && format == Some(Format::M4a)\n        && body.complete()',base+'complete_m4a_prefix_uses_cached_bytes_without_disk_or_range_get'),
        ('spool-prefix-discarded','spool_source.rs','let (mut response, expected, mut prefix) = body.into_parts();','let (mut response, expected, mut prefix) = body.into_parts(); prefix.clear();',fallback),
        ('prefix-slices-copy-payload','source_chunk.rs','data: self.data.slice(start..end),','data: Bytes::copy_from_slice(&self.data[start..end]),',credit),
    ]

def validate(base):
    for name,file,before,_,_ in declarations()+prefix_declarations():assert (base/file).read_text().count(before)==1,(name,'anchor drift')

def main(work,target,rubato,positive_only):
    fixtures=ROOT/'scripts/testing/fixtures/audio-codec-probe';validate(fixtures)
    extra=[Path(__file__).resolve(),Path(spool.__file__).resolve(),ROOT/'scripts/testing/check_audio_format_sources.py',ROOT/'scripts/testing/check_gitignore_contract.py']
    env,frozen,libraries,new=local.prepare(work,target,rubato,extra);package=work/'probe'
    env.pop('MIGO_AUDIO_CHECKS_BLESS',None);runtime=work/'runtime-tmp';runtime.mkdir();env.update(TMPDIR=str(runtime),TMP=str(runtime),TEMP=str(runtime),CC='/usr/bin/clang',CXX='/usr/bin/clang++')
    fixture=Path('platforms/apple/WebContent/PerformancePlus/test/fixtures/audio-check-answers.json')
    for dest in [work/fixture,work/'source-inputs'/fixture]:dest.parent.mkdir(parents=True,exist_ok=True);shutil.copy2(ROOT/fixture,dest)
    frozen[str(fixture)]=codecs.sha(ROOT/fixture);(work/'source-inputs.json').write_text(json.dumps(frozen,indent=2)+'\n')
    original=(work/'fixtures/1-tail.m4a').read_bytes();padding=(100*1024+8).to_bytes(4,'big')+b'free'+bytes(100*1024)
    timeline=local.file.dsp.source.index_gate.timeline;path=(b'moov',b'trak',b'mdia',b'minf',b'stbl',b'stco');table=timeline.payloads(original,path)[0]
    changed=table[:8]+b''.join((int.from_bytes(table[i:i+4],'big')+len(padding)).to_bytes(4,'big')for i in range(8,len(table),4))
    padded=work/'prefix-padded.m4a';padded.write_bytes(padding+timeline.rewrite(original,{path:changed}));env['W09_PREFIX_PADDED']=str(padded)
    for label,path in [('original',work/'fixtures/1-tail.m4a'),('padded',padded)]:
        codecs.run(['ffmpeg','-nostdin','-v','error','-i',str(path),'-f','f32le',str(work/f'prefix-{label}.pcm')],work/f'prefix-{label}-oracle.log',cwd=work,env=env)
    assert (work/'prefix-original.pcm').read_bytes()==(work/'prefix-padded.pcm').read_bytes(),'independent padded-file PCM oracle'
    spool.include(work);lib=package/'src/lib.rs'
    for name,testing in [('http_prefix',False),('http_prefix_contracts',True),('format_source',False),('format_source_contracts',True),('auto_source_contracts',True),('http_source',False),('http_source_contracts',True),('spool_format_contracts',True)]:
        path=work/'source'/f'{name}.rs';shutil.copy2(fixtures/f'{name}.rs',path)
        lib.write_text(lib.read_text()+'\n'+('#[cfg(test)] 'if testing else'')+f'#[path={json.dumps(str(path))}] mod {name};\n')
    retained={str(p.relative_to(work)):codecs.sha(p)for tree in [package/'src',work/'source']for p in tree.rglob('*.rs')};validate(work/'source')
    codecs.run(['cargo','metadata','--offline','--format-version','1'],work/'candidate-metadata.jsonl',cwd=package,env=env)
    selected=json.loads(next(l for l in(work/'candidate-metadata.jsonl').read_text().splitlines()if l.startswith('{"packages"')))['packages'];locked={}
    for row in tomllib.loads((ROOT/'engine/Cargo.lock').read_text())['package']:locked.setdefault(row['name'],set()).add(row['version'])
    assert all(r['name']not in locked or r['version']in locked[r['name']]for r in selected),'existing dependency drift'
    codecs.run(['cargo','check','--offline','--locked','--lib'],work/'normal-library-check.log',cwd=package,env=env)
    binary=local.file.dsp.source.build(work,env,'positive')
    checks=[('prefix','http_prefix_contracts::',8),('selection','http_source_contracts::',10),('format','format_source_contracts::',7),('automatic','auto_source_contracts::',7),('spool','spool_contracts::',5),('spool-owner','spool_source::tests::',4),('sequential','local_source_contracts::',5),('http','isomp4_source_contracts::transport::',11),('source','isomp4_source_contracts::',25),('range','network::http::range::tests::',18),('common','streaming_transport::tests::',3),('local','isomp4_source_contracts::transport::local::',11),('package','isomp4_source_contracts::transport::local::package::',8),('directory','isomp4_source_contracts::transport::local::directory::',5),('setup','local_source::tests::',1),('spool-formats','spool_format_contracts::',3)]
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
        for name,file,before,after,test in declarations()+prefix_declarations():
            path=work/'source'/file;original=path.read_text();assert original.count(before)==1;path.write_text(original.replace(before,after))
            try:
                mutant=local.file.dsp.source.build(work,env,name);log=work/f'{name}-test.log'
                with log.open('w')as output:r=subprocess.run([str(mutant),'--exact',test if '::'in test else'http_source_contracts::'+test,'--nocapture'],cwd=package,env=env,stdout=output,stderr=subprocess.STDOUT,timeout=120)
                observed=log.read_text();assert r.returncode==101 and'panicked at'in observed and'1 failed'in observed,(name,'require executed assertion',r.returncode)
            finally:path.write_text(original)
            faults+=1;print('actual HTTP selection fault',name,': executed assertion rejected',flush=True)
    spool_faults=0 if positive_only else spool.controls(work,env)
    assert all(codecs.sha(ROOT/p)==h for p,h in frozen.items())and all(codecs.sha(work/p)==h for p,h in retained.items())
    rows=json.loads((work/'library-source-provenance.json').read_text());assert len(rows)==9 and sum(len(r['patches'])for r in rows)==29
    for row in rows:assert local.file.dsp.source.index_gate.timeline.source_tree(libraries[row['crate']])==row['source_files']
    for name,row in new.items():assert all(codecs.sha(work/'resampling-vendor'/f"{name}-{row['version']}"/p)==h for p,h in row['source_files'].items())
    summary={'selection_tests':10,'prefix_tests':8,'prefix_pcm_comparisons':27,'selection_pcm_comparisons':100,'positive_tests':sum(row[2]for row in checks),'compiled_controls':faults+spool_faults,'selection_controls':0 if positive_only else len(declarations()),'prefix_controls':0 if positive_only else len(prefix_declarations()),'spool_controls':spool_faults,'positive_only':positive_only,'binary_sha256':codecs.sha(binary),'libraries':9,'patches':29,'root_inputs_unchanged':True,'source_copies_restored':True,'existing_dependency_versions_preserved':True,'root_lock_sha256':codecs.sha(ROOT/'engine/Cargo.lock'),'scope':'Bounded library-native network prefix identification/replay, early sequential PCM, cached complete M4A without GET, exact original-response spool fallback, CPU cancellation/deadline and retained payload credit. Product/native/device/peak-performance acceptance remains open.'}
    (work/'summary.json').write_text(json.dumps(summary,indent=2)+'\n');print(json.dumps(summary,indent=2),flush=True)
if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--output',type=Path,required=True);p.add_argument('--target-dir',type=Path,required=True);p.add_argument('--rubato-cargo-home',type=Path,required=True);p.add_argument('--positive-only',action='store_true');a=p.parse_args();main(a.output.resolve(),a.target_dir.resolve(),a.rubato_cargo_home.resolve(),a.positive_only)
