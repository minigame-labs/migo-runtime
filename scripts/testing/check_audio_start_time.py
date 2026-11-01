#!/usr/bin/env python3
"""A JS-only startTime skipped loading/autoplay and overwrote explicit zero seeks.

Send persistent configuration to the existing audio owner on both backends.
Prove start/resume/stop/end/source ordering with actual PCM and source ACKs;
retain loading, timeline, loop, seek and bounded IO regression faults.
"""
from __future__ import annotations
import argparse,json,re,shutil,subprocess,tomllib
from pathlib import Path
import check_audio_loading_controls as loading
import check_audio_io_renewal as io

ROOT=io.ROOT
codecs=io.codecs
public=io.public
phase=io.phase
clock=io.clock
EXTRA=['engine/crates/runtime-v8/src/audio/02_inner_audio_context.js',
       'platforms/apple/WebContent/PerformancePlus/test/source-load.test.mjs',
       'engine/crates/runtime-v8/snapshots/SNAPSHOT-full-linux-x86_64.bin',
       'engine/crates/runtime-v8/snapshots/SNAPSHOT-full-linux-x86_64.bin.manifest.json',
       'engine/crates/runtime-v8/snapshots/SNAPSHOT-worker-full-linux-x86_64.bin',
       'engine/crates/runtime-v8/snapshots/SNAPSHOT-worker-full-linux-x86_64.bin.manifest.json',
       'engine/crates/runtime-v8/src/audio/ops.rs','engine/crates/runtime-v8/src/audio/mod.rs',
       'engine/crates/core/src/runtime/service_audio.rs','engine/crates/core/src/runtime/service_audio_tests.rs',
       'engine/crates/core/src/runtime/service_ops.rs','contracts/runtime/op-boundary.json',
       'contracts/runtime/service-ops.json','platforms/apple/WebContent/PerformancePlus/src/lane-command.mjs',
       'platforms/apple/WebContent/PerformancePlus/src/service-ops.mjs',
       'platforms/apple/WebContent/PerformancePlus/src/lane-async.mjs',
       'scripts/gen-performance-plus-engine.py','scripts/testing/check_service_op_retirement.py',
       'scripts/test-runtime-op-boundary-contract.sh']

def native_declarations(originals):
    rows=[];p='inner_audio.rs';prefix='inner_audio::loading_control_tests::'
    order=prefix+'start_time_property_order_reaches_first_play_for_owned_and_cached_audio'
    zero=prefix+'explicit_loading_seek_even_zero_wins_and_stop_rearms_start_time'
    resume=prefix+'start_time_does_not_seek_a_paused_or_playing_source_and_resume_keeps_position'
    lifetime=prefix+'source_replacement_and_natural_end_rearm_the_persistent_start_time'
    def scoped(name,signature,before,after,test):
        io.seek.live.cutover.scoped(rows,originals,name,p,signature,before,after,test)
    scoped('start-time-configuration-discarded','    pub fn set_start_time(',
           'self.start_time = time.max(0.0);','',order)
    scoped('start-time-accepts-nonfinite','    pub fn set_start_time(',
           'if time.is_finite() {','if true {',order)
    scoped('start-time-first-play-ignored','    pub fn play(',
           'let initial_seek = self.start_pending && self.start_time > 0.0;',
           'let initial_seek = false;',order)
    scoped('start-time-overrides-explicit-zero','    pub fn seek(',
           '        self.start_pending = false;','',zero)
    scoped('start-time-stop-does-not-rearm','    pub fn stop(',
           '        self.start_pending = true;','',zero)
    scoped('start-time-new-source-does-not-rearm','    pub(crate) fn begin_load_for(',
           '        self.start_pending = true;','',lifetime)
    scoped('start-time-ended-does-not-rearm','    fn finish_playback(',
           '        self.start_pending = true;','',lifetime)
    scoped('start-time-resume-reapplies-property','    pub fn play(',
           'let initial_seek = self.start_pending && self.start_time > 0.0;',
           'let initial_seek = self.start_time > 0.0;',resume)
    scoped('start-time-setter-eagerly-seeks','    pub fn set_start_time(',
           '            self.start_time = time.max(0.0);',
           '            self.start_time = time.max(0.0); self.seek(self.start_time);',resume)
    scoped('start-time-configuration-allocates','    pub fn set_start_time(',
           '        if time.is_finite() {',
           '        std::hint::black_box(vec![0_u8;64]);\n        if time.is_finite() {',
           prefix+'setting_start_time_and_coalescing_loading_commands_have_no_steady_heap_activity')
    rejection='inner_audio::public_control_tests::configured_start_refusal_never_plays_unpositioned_pcm'
    scoped('start-time-source-start-is-optional-seek','    pub fn play(',
           'self.request_stream_control(self.start_time, ControlKind::Start)',
           'self.request_stream_control(self.start_time, ControlKind::Seek)',rejection)
    scoped('start-time-immediate-refusal-is-nonterminal','    fn request_stream_control(',
           'if matches!(kind, ControlKind::Restart | ControlKind::Start) {',
           'if kind == ControlKind::Restart {',rejection)
    scoped('start-time-async-refusal-is-nonterminal','    fn poll_stream_controls(',
           'if matches!(pending.kind, ControlKind::Restart | ControlKind::Start) {',
           'if pending.kind == ControlKind::Restart {',rejection)
    return rows

def candidate_declarations(originals):
    rows=[];p='inner_audio.rs'
    test='timed_m4a::live_loop_tests::loading_controls::real_file_http_start_time_property_order_and_zero_seek_keep_exact_suffixes'
    def scoped(name,signature,before,after):
        io.seek.live.cutover.scoped(rows,originals,name,p,signature,before,after,test)
    scoped('start-time-real-source-ignores-first-target','    pub fn play(',
           'let initial_seek = self.start_pending && self.start_time > 0.0;',
           'let initial_seek = false;')
    scoped('start-time-real-source-overrides-zero-seek','    pub fn seek(',
           '        self.start_pending = false;','')
    scoped('start-time-repeated-play-restarts-over-pending-seek','    pub fn play(',
           '            && self.stream_controls.is_empty()','')
    return rows

def native_rows(originals):return native_declarations(originals)+loading.native_rows(originals)
def candidate_rows(originals):return candidate_declarations(originals)+loading.candidate_rows(originals)

def freeze_extra(work,frozen):
    for rel in EXTRA:
        frozen[rel]=codecs.sha(ROOT/rel);dest=work/'source-inputs'/rel;dest.parent.mkdir(parents=True,exist_ok=True);shutil.copy2(ROOT/rel,dest)
    (work/'source-inputs.json').write_text(json.dumps(frozen,indent=2)+'\n')

def node(work):
    receipts=loading.node(work)
    source=(ROOT/EXTRA[0]).read_text();script=ROOT/EXTRA[1]
    anchors=[('start-time-facade-discards-configuration','    op_inner_audio_set_start_time(this.#id, time);',''),
        ('start-time-facade-seeks-from-stale-snapshot','    op_inner_audio_play(this.#id);',
         '    if (this.#buffered && this.#startTime > 0 && this.#currentTime === 0) op_inner_audio_seek(this.#id, this.#startTime);\n    op_inner_audio_play(this.#id);'),
        ('start-time-facade-accepts-nonfinite','    if (!Number.isFinite(time)) return;','')]
    import os
    for name,before,after in anchors:
        assert source.count(before)==1
        path=work/(name+'.js');path.write_text(source.replace(before,after));env=os.environ.copy();env['MIGO_LOADING_FACADE']=str(path)
        with(work/(name+'-test.log')).open('w')as out:
            result=subprocess.run(['node',str(script)],cwd=ROOT,env=env,stdout=out,stderr=subprocess.STDOUT,timeout=30)
        observed=(work/(name+'-test.log')).read_text();assert result.returncode==1 and'AssertionError'in observed and'SyntaxError'not in observed
        receipts.append({'name':name,'exit':1,'mutant_sha256':codecs.sha(path)})
    (work/'node-controls.json').write_text(json.dumps(receipts,indent=2)+'\n')
    return receipts

def retirement(work):
    import os,sys
    script=ROOT/'scripts/testing/check_service_op_retirement.py'
    codecs.run([sys.executable,str(script)],work/'retirement-positive.log',cwd=ROOT)
    source=(ROOT/'scripts/gen-performance-plus-engine.py').read_text()
    begin=source.index('    if (not isinstance(retired, list)')
    end=source.index('    if any(name in lanes for name in retired):',begin)
    anchors=[('retirement-forgets-record-validation',source[begin:end],''),
        ('retirement-allows-active-lane','    if any(name in lanes for name in retired):','    if False:'),
        ('retirement-does-not-keep-number','        if name in retired:\n            continue\n','')]
    receipts=[]
    for name,before,after in anchors:
        assert source.count(before)==1
        path=work/(name+'.py');path.write_text(source.replace(before,after));env=os.environ.copy();env['MIGO_RETIREMENT_GENERATOR']=str(path)
        with(work/(name+'-test.log')).open('w')as out:
            result=subprocess.run([sys.executable,str(script)],cwd=ROOT,env=env,stdout=out,stderr=subprocess.STDOUT,timeout=30)
        observed=(work/(name+'-test.log')).read_text();assert result.returncode==1 and'AssertionError'in observed and'SyntaxError'not in observed
        receipts.append({'name':name,'exit':1,'mutant_sha256':codecs.sha(path)})
    (work/'python-controls.json').write_text(json.dumps(receipts,indent=2)+'\n')
    return receipts

def native(work,target,positive_only):
    package,env,frozen=clock.prepare(work,target,list((ROOT/'scripts/testing').glob('check_audio_*.py'))+[ROOT/'scripts/testing/check_gitignore_contract.py']+[ROOT/p for p in EXTRA])
    originals={n:(package/'src'/n).read_text()for n in ['streaming.rs','inner_audio.rs','stream_control.rs']};rows=native_rows(originals)
    assert len(rows)==129 and len({r[0]for r in rows})==129
    binary=public.build_native(package,work,env,'positive');log=work/'positive-tests.log';assert clock.common.run([str(binary)],log,package,env)==0
    assert re.search(r'test result: ok\. 352 passed; 0 failed; 2 ignored;',log.read_text());io.program(work,'positive','native')
    js=node(work);python_controls=retirement(work)
    receipts=io.faults(work,package,env,originals,[]if positive_only else rows,lambda name:public.build_native(package,work,env,name),package/'src','native')
    assert all(codecs.sha(ROOT/p)==h and codecs.sha(work/'source-inputs'/p)==h for p,h in frozen.items())
    audio=ROOT/'engine/crates/audio/src';assert all(p.read_bytes()==(package/'src'/p.relative_to(audio)).read_bytes()for p in audio.rglob('*.rs'))
    summary={'positive_tests':352,'ignored_tests':2,'compiled_controls':len(receipts),'new_controls':0 if positive_only else 13,'retained_controls':0 if positive_only else 116,'controls':receipts,'node_controls':js,'python_controls':python_controls,'root_inputs_unchanged':True,'source_copies_restored':True,'root_lock_sha256':codecs.sha(ROOT/'engine/Cargo.lock')}
    (work/'summary.json').write_text(json.dumps(summary,indent=2)+'\n');print('native complete',352,len(receipts),flush=True)

def candidate(work,target,rubato,positive_only):
    env,frozen,retained,libraries,resampling=io.seek.live.prepare(work,target,rubato);freeze_extra(work,frozen);package=work/'probe'
    codecs.run(['cargo','metadata','--offline','--format-version','1'],work/'metadata.jsonl',cwd=package,env=env)
    identity=lambda p:(p['name'],p['version'],p.get('source'),p.get('checksum'));locked=tomllib.loads((ROOT/'engine/Cargo.lock').read_text())['package'];known={p['name']for p in locked};pins=set(map(identity,locked))
    assert all(p['name']not in known or identity(p)in pins for p in tomllib.loads((package/'Cargo.lock').read_text())['package'])
    names=['timed_m4a.rs','resampled_streaming.rs','indexed_seek.rs','streaming_transport.rs','producer_control.rs','isomp4_source.rs','inner_audio.rs','stream_control.rs','off_worker.rs'];originals={n:(work/'source'/n).read_text()for n in names};rows=candidate_rows(originals)
    assert len(rows)==119 and len({r[0]for r in rows})==119
    codecs.run(['cargo','check','--offline','--locked','--lib'],work/'normal-library-check.log',cwd=package,env=env)
    positives=phase.positive_profiles(work,env,[('cyclic','timed_m4a::loop_tests::',11),('cyclic-clock','resampled_streaming::loop_clock_tests::',2),('public-source','public_control_contracts::',3),('cutover-clock','streaming::cutover_tests::',6),('output-cutover','timed_m4a::cutover_tests::',5),('public-loop','timed_m4a::live_loop_tests::',12),('quota','isomp4_source::quota_tests::',3),('consumption','streaming::rendered_progress_tests::',1)])
    assert positives=={'release':318,'debug':315},positives
    for profile in ['release','debug']:io.program(work,'positive-'+profile,'candidate')
    receipts=io.faults(work,package,env,originals,[]if positive_only else rows,lambda name:phase.file.dsp.source.build(work,env,name,release=name=='paused-player-drains-past-prefetch',clean=False),work/'source','candidate')
    for row in receipts:row['profile']='release'if row['name']=='paused-player-drains-past-prefetch'else'debug'
    assert all(codecs.sha(ROOT/p)==h and codecs.sha(work/'source-inputs'/p)==h for p,h in frozen.items());assert all(codecs.sha(work/p)==h for p,h in retained.items())
    provenance=json.loads((work/'library-source-provenance.json').read_text())
    for row in provenance:assert phase.file.dsp.source.index_gate.timeline.source_tree(libraries[row['crate']])==row['source_files']
    for name,row in resampling.items():assert all(codecs.sha(work/'resampling-vendor'/f"{name}-{row['version']}"/p)==h for p,h in row['source_files'].items())
    summary={'positive_tests_per_profile':positives,'compiled_controls':len(receipts),'new_controls':0 if positive_only else 3,'retained_controls':0 if positive_only else 116,'controls':receipts,'root_inputs_unchanged':True,'source_copies_restored':True,'root_lock_sha256':codecs.sha(ROOT/'engine/Cargo.lock'),'libraries':len(provenance),'patches':sum(len(r['patches'])for r in provenance),'resampling_source_files_unchanged':True,'new_single_sources_per_profile':50,'new_cyclic_sources_per_profile':20,'scope':'Native startTime configuration reaches loading/autoplay and explicit start,stop/end replay;pause resumes and explicit zero seeks win.Source ACK guards exact FileHTTP PCM.Shipping codec/platform/device remain open.'}
    (work/'summary.json').write_text(json.dumps(summary,indent=2)+'\n');print('candidate complete',positives,len(receipts),flush=True)

if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--scope',choices=['native','candidate'],required=True);p.add_argument('--output',type=Path,required=True);p.add_argument('--target-dir',type=Path,required=True);p.add_argument('--rubato-cargo-home',type=Path,required=True);p.add_argument('--positive-only',action='store_true');a=p.parse_args()
    if a.scope=='native':native(a.output.resolve(),a.target_dir.resolve(),a.positive_only)
    else:candidate(a.output.resolve(),a.target_dir.resolve(),a.rubato_cargo_home.resolve(),a.positive_only)
