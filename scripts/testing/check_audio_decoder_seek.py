#!/usr/bin/env python3
"""Verify candidate decoder discontinuities with real AAC, Rubato and faults.

Container ReadAt is continuous; explicit seek resets the retained codec/DSP,
trims presentation preroll before interleaving and can reopen EOF. Preserve
invalid user targets, cancellation, bounded input and existing source resumes.
This is not actual source/player seek command integration or product cutover.
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
spec = importlib.util.spec_from_file_location('decoder_seek_transport', ROOT / 'scripts/testing/check_audio_mp4_transport.py')
transport = importlib.util.module_from_spec(spec)
spec.loader.exec_module(transport)
codecs = transport.codecs


def declarations():
    pcm = 'common_seek_resets_and_discards_preroll_across_eof_and_partitions'
    dsp = 'dsp_seek_matches_fresh_library_resampling_and_bypass'
    pending = 'dsp_seek_discards_pending_pcm_and_invalid_seek_keeps_it'
    return [
        ('seek-never-resets-codec','incremental_decoder.rs','            decoder.reset();','','explicit_seek_resets_the_retained_codec_once_and_container_rewinds_do_not'),
        ('seek-reconstructs-codec','incremental_decoder.rs','            decoder.reset();','            decoder.reset();\n            self.decoder = None;','explicit_seek_resets_the_retained_codec_once_and_container_rewinds_do_not'),
        ('seek-does-not-trim-preroll','incremental_decoder.rs','self.seek_target = Some(seeked.required_ts);','self.seek_target = None;',pcm),
        ('seek-keeps-finished','incremental_decoder.rs','        self.finished = false;','',pcm),
        ('seek-keeps-old-pcm-quota','incremental_decoder.rs','        self.total_samples = 0;','','complete_cache_seek_reuses_bytes_and_seek_does_not_bypass_pcm_quota'),
        ('seek-accepts-old-source-input','incremental_decoder.rs','        self.source_waiting = true;\n        self.finished = false;','        self.source_waiting = false;\n        self.finished = false;',pcm),
        ('seek-discards-unverified-cache','incremental_decoder.rs','        self.total_samples = 0;','        self.total_samples = 0;\n        self.input.clear();','complete_cache_seek_reuses_bytes_and_seek_does_not_bypass_pcm_quota'),
        ('invalid-seek-retires-live-request','incremental_decoder.rs','if !matches!(error, Error::SeekError(_)) {','if true {','invalid_seek_preserves_a_live_request_and_preindex_parser'),
        ('seek-ignores-packet-trim','incremental_decoder.rs','.checked_add(\n                        i64::try_from(packet.trim_start.get())\n                            .map_err(|_| "invalid packet trim")?,\n                    )','.checked_add(0)',pcm),
        ('seek-skip-uses-samples-as-frames','incremental_decoder.rs','let block = block.slice(skip..);','let block = block.slice((skip * self.channels as usize).min(block.frames())..);',pcm),
        ('seek-ends-preroll-before-target','incremental_decoder.rs','if skip < block.frames() {','if true {',pcm),
        ('dsp-seek-keeps-filter-history','resampled_streaming.rs','            resampler.reset();','',dsp),
        ('dsp-seek-keeps-old-pending-pcm','resampled_streaming.rs','        self.decoded.clear();\n        self.position = 0;\n        if let Some(resampler)','        if let Some(resampler)',pending),
        ('dsp-seek-keeps-decoder-eof','resampled_streaming.rs','        self.decoder_done = false;','',dsp),
        ('dsp-seek-never-suspends-source','resampled_streaming.rs','        self.decoder_done = false;\n        self.source_waiting = true;','        self.decoder_done = false;\n        self.source_waiting = false;',pending),
        ('dsp-pushes-before-cached-resume','resampled_streaming.rs','let consumed = if waiting {','let consumed = if false {',pending),
        ('invalid-dsp-seek-poisons-pending-pcm','resampled_streaming.rs','if !matches!(error, Error::SeekError(_)) {','if true {',pending),
        ('seek-allocates-per-command','incremental_decoder.rs','        self.seek_target = Some(seeked.required_ts);','        std::hint::black_box(vec![0_u8; 64]);\n        self.seek_target = Some(seeked.required_ts);','seek_and_cached_codec_dsp_steps_have_no_steady_heap_activity'),
    ]


def controls(work, env):
    rows = declarations()
    for name,file,before,after,test in rows:
        source=work/'source'/file;original=source.read_text()
        assert original.count(before)==1,(name,'mutation anchor changed')
        source.write_text(original.replace(before,after))
        try:
            binary=transport.source.build(work,env,name);log=work/f'{name}-test.log'
            with log.open('w') as out:
                r=subprocess.run([str(binary),'--exact','decoder_seek_contracts::'+test,'--nocapture'],cwd=work/'probe',env=env,stdout=out,stderr=subprocess.STDOUT,timeout=120)
            observed=log.read_text()
            assert r.returncode==101 and 'panicked at' in observed and '1 failed' in observed,(name,'must reject an executed assertion')
        finally:source.write_text(original)
        print('actual decoder seek mutation',name,': executable assertion rejected',flush=True)
    return len(rows)


def main(work,target,rubato,positive_only):
    env,frozen,_,libraries,new=transport.prepare(work,target,rubato)
    env.update(CC='/usr/bin/clang',CXX='/usr/bin/clang++');env.pop('MIGO_AUDIO_CHECKS_BLESS',None)
    package=work/'probe';fixture=ROOT/'scripts/testing/fixtures/audio-codec-probe/decoder_seek_contracts.rs'
    path=work/'source'/fixture.name;shutil.copy2(fixture,path)
    lib=package/'src/lib.rs';lib.write_text(lib.read_text()+f'\n#[cfg(test)] #[path={json.dumps(str(path))}] mod decoder_seek_contracts;\n')
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
    checks=[('seek','decoder_seek_contracts::',9),('transport','isomp4_source_contracts::transport::',11),('source','isomp4_source_contracts::',25),('range','network::http::range::tests::',18),('common','streaming_transport::tests::',3),('player','inner_audio::',42)]
    for label,prefix,count in checks:
        args=[str(binary),prefix,'--nocapture','--test-threads=1']
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
    summary={'new_seek_tests':9,'new_common_pcm_suffixes':416,'new_dsp_pcm_suffixes':60,'positive_tests':sum(r[2]for r in checks),'new_seek_controls':count,'retained_dsp_controls':legacy,'compiled_controls':count+legacy,'positive_only':positive_only,'binary_sha256':codecs.sha(binary),'libraries':9,'patches':29,'root_inputs_unchanged':True,'source_copies_restored':True,'existing_dependency_identities_preserved':True,'root_lock_sha256':codecs.sha(ROOT/'engine/Cargo.lock'),'scope':'Candidate bounded decoder codec/preroll and retained Rubato discontinuity over fixture/source protocol. No actual source/player seek command or product/native/device/peak acceptance.'}
    (work/'summary.json').write_text(json.dumps(summary,indent=2)+'\n');print(json.dumps(summary,indent=2),flush=True)

if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--output',type=Path,required=True);p.add_argument('--target-dir',type=Path,required=True);p.add_argument('--rubato-cargo-home',type=Path,required=True);p.add_argument('--positive-only',action='store_true');a=p.parse_args();main(a.output.resolve(),a.target_dir.resolve(),a.rubato_cargo_home.resolve(),a.positive_only)
