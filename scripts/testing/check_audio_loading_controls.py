#!/usr/bin/env python3
"""Loading play/seek must survive readiness without becoming sticky autoplay.

Late completion used to erase commands or restart a user-paused source. Apply
generation-owned intent once at real CanPlay through the existing source ACK;
compile faults in coalescing, lifetime, ordering and the first audible suffix.
"""
from __future__ import annotations
import argparse,json,re,shutil,subprocess,tomllib
from pathlib import Path
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
       'engine/crates/runtime-v8/snapshots/SNAPSHOT-worker-full-linux-x86_64.bin.manifest.json']

def native_declarations(originals):
    rows=[];p='inner_audio.rs';prefix='inner_audio::loading_control_tests::'
    transport=prefix+'last_loading_transport_wins_over_autoplay_without_mutating_the_property'
    seek=prefix+'loading_seek_waits_for_metadata_and_stop_cancels_only_the_earlier_target'
    stream=prefix+'short_and_prefilled_streams_apply_loading_controls_once_at_canplay'
    lifetime=prefix+'source_replacement_rejects_old_completion_without_erasing_new_controls'
    def whole(name,before,after,test):
        assert originals[p].count(before)==1,(name,'mutation anchor drift')
        rows.append((name,p,before,after,test))
    for label,state in [('play','Playing'),('pause','Paused'),('stop','Stopped')]:
        whole('loading-'+label+'-discarded',f'controls.playback = Some(PlaybackState::{state});','',transport)
    whole('loading-stop-keeps-earlier-seek','            controls.seek = None;','',seek)
    whole('loading-seek-discarded','controls.seek = Some(time.max(0.0));','',seek)
    io.seek.live.cutover.scoped(rows,originals,'loading-nonfinite-seek-overwrites-intent',p,'    pub fn seek(',
        'if !time.is_finite() {','if false {',seek)
    whole('loading-new-source-keeps-old-controls',
        'self.loading_controls = Some(LoadingControls::default());',
        'self.loading_controls.get_or_insert_with(LoadingControls::default);',lifetime)
    whole('loading-controls-apply-repeatedly','self.loading_controls.take()',
        'self.loading_controls.as_ref().map(|c| LoadingControls { playback:c.playback, seek:c.seek })',stream)
    whole('loading-metadata-is-mistaken-for-canplay','if self.stream_canplay_sent && self.shared.is_loaded() {',
        'if self.shared.sample_rate() != 0 {',stream)
    whole('loading-stream-never-applies-controls','''        if self.stream_canplay_sent && self.shared.is_loaded() {
            self.apply_loading_controls();
        }''','',stream)
    for name,signature in [('owned','    pub(crate) fn complete_load('),('cached','    pub(crate) fn complete_cached_load(')]:
        io.seek.live.cutover.scoped(rows,originals,'loading-'+name+'-never-applies-controls',p,signature,
            '        self.apply_loading_controls();','',transport)
    whole('loading-autoplay-overrides-explicit-transport','controls.playback.unwrap_or_else(',
        'None::<PlaybackState>.unwrap_or_else(',transport)
    return rows

def candidate_declarations(originals):
    p='inner_audio.rs';rows=[]
    test='timed_m4a::live_loop_tests::loading_controls::real_file_http_loading_play_pause_stop_and_seek_keep_exact_single_suffixes'
    for name,before,after in [
        ('loading-real-source-ignores-seek','controls.seek = Some(time.max(0.0));',''),
    ]:
        assert originals[p].count(before)==1,(name,'mutation anchor drift')
        rows.append((name,p,before,after,test))
    io.seek.live.cutover.scoped(rows,originals,'loading-real-source-mixes-before-ack',p,'    pub fn process(',
        'if !self.stream_controls.is_empty() {','if false {',test)
    return rows

def native_rows(originals):return native_declarations(originals)+io.native_declarations(originals)+io.seek.native_rows(originals)
def candidate_rows(originals):return candidate_declarations(originals)+io.candidate_declarations(originals)+io.seek.candidate_rows(originals)

def freeze_extra(work,frozen):
    for rel in EXTRA:
        frozen[rel]=codecs.sha(ROOT/rel);dest=work/'source-inputs'/rel;dest.parent.mkdir(parents=True,exist_ok=True);shutil.copy2(ROOT/rel,dest)
    (work/'source-inputs.json').write_text(json.dumps(frozen,indent=2)+'\n')

def node(work):
    source=(ROOT/EXTRA[0]).read_text();script=ROOT/EXTRA[1]
    codecs.run(['node',str(script)],work/'facade-positive.log',cwd=ROOT)
    mutants=[('loading-facade-sticky-autoplay',source.replace('    op_inner_audio_play(this.#id);',
        '    if (!this.#buffered) { this.#autoplay = true; op_inner_audio_set_autoplay(this.#id,true); return Promise.resolve(); }\n    op_inner_audio_play(this.#id);')),
        ('loading-facade-loses-unready-play',source.replace('    op_inner_audio_play(this.#id);',
        '    if (!this.#buffered) return Promise.resolve();\n    op_inner_audio_play(this.#id);'))]
    import os
    receipts=[]
    for name,text in mutants:
        assert text!=source;path=work/(name+'.js');path.write_text(text);env=os.environ.copy();env['MIGO_LOADING_FACADE']=str(path)
        with (work/(name+'-test.log')).open('w')as out:
            result=subprocess.run(['node',str(script)],cwd=ROOT,env=env,stdout=out,stderr=subprocess.STDOUT,timeout=30)
        observed=(work/(name+'-test.log')).read_text();assert result.returncode==1 and'AssertionError'in observed and'SyntaxError'not in observed
        receipts.append({'name':name,'exit':1,'mutant_sha256':codecs.sha(path)})
    (work/'node-controls.json').write_text(json.dumps(receipts,indent=2)+'\n')
    return receipts

def native(work,target,positive_only):
    package,env,frozen=clock.prepare(work,target,list((ROOT/'scripts/testing').glob('check_audio_*.py'))+[ROOT/'scripts/testing/check_gitignore_contract.py']+[ROOT/p for p in EXTRA])
    originals={n:(package/'src'/n).read_text()for n in ['streaming.rs','inner_audio.rs','stream_control.rs']};rows=native_rows(originals)
    assert len(rows)==116 and len({r[0]for r in rows})==116
    binary=public.build_native(package,work,env,'positive');log=work/'positive-tests.log';assert clock.common.run([str(binary)],log,package,env)==0
    assert re.search(r'test result: ok\. 345 passed; 0 failed; 2 ignored;',log.read_text());io.program(work,'positive','native')
    js=node(work)
    receipts=io.faults(work,package,env,originals,[]if positive_only else rows,lambda name:public.build_native(package,work,env,name),package/'src','native')
    assert all(codecs.sha(ROOT/p)==h and codecs.sha(work/'source-inputs'/p)==h for p,h in frozen.items())
    audio=ROOT/'engine/crates/audio/src';assert all(p.read_bytes()==(package/'src'/p.relative_to(audio)).read_bytes()for p in audio.rglob('*.rs'))
    summary={'positive_tests':345,'ignored_tests':2,'compiled_controls':len(receipts),'new_controls':0 if positive_only else 13,'retained_controls':0 if positive_only else 103,'controls':receipts,'node_controls':js,'root_inputs_unchanged':True,'source_copies_restored':True,'root_lock_sha256':codecs.sha(ROOT/'engine/Cargo.lock')}
    (work/'summary.json').write_text(json.dumps(summary,indent=2)+'\n');print('native complete',345,len(receipts),flush=True)

def candidate(work,target,rubato,positive_only):
    env,frozen,retained,libraries,resampling=io.seek.live.prepare(work,target,rubato);freeze_extra(work,frozen);package=work/'probe'
    codecs.run(['cargo','metadata','--offline','--format-version','1'],work/'metadata.jsonl',cwd=package,env=env)
    identity=lambda p:(p['name'],p['version'],p.get('source'),p.get('checksum'));locked=tomllib.loads((ROOT/'engine/Cargo.lock').read_text())['package'];known={p['name']for p in locked};pins=set(map(identity,locked))
    assert all(p['name']not in known or identity(p)in pins for p in tomllib.loads((package/'Cargo.lock').read_text())['package'])
    names=['timed_m4a.rs','resampled_streaming.rs','indexed_seek.rs','streaming_transport.rs','producer_control.rs','isomp4_source.rs','inner_audio.rs','stream_control.rs','off_worker.rs'];originals={n:(work/'source'/n).read_text()for n in names};rows=candidate_rows(originals)
    assert len(rows)==116 and len({r[0]for r in rows})==116
    codecs.run(['cargo','check','--offline','--locked','--lib'],work/'normal-library-check.log',cwd=package,env=env)
    positives=phase.positive_profiles(work,env,[('cyclic','timed_m4a::loop_tests::',11),('cyclic-clock','resampled_streaming::loop_clock_tests::',2),('public-source','public_control_contracts::',3),('cutover-clock','streaming::cutover_tests::',6),('output-cutover','timed_m4a::cutover_tests::',5),('public-loop','timed_m4a::live_loop_tests::',10),('quota','isomp4_source::quota_tests::',3),('consumption','streaming::rendered_progress_tests::',1)])
    assert positives=={'release':309,'debug':306},positives
    for profile in ['release','debug']:io.program(work,'positive-'+profile,'candidate')
    receipts=io.faults(work,package,env,originals,[]if positive_only else rows,lambda name:phase.file.dsp.source.build(work,env,name,release=name=='paused-player-drains-past-prefetch',clean=False),work/'source','candidate')
    for row in receipts:row['profile']='release'if row['name']=='paused-player-drains-past-prefetch'else'debug'
    assert all(codecs.sha(ROOT/p)==h and codecs.sha(work/'source-inputs'/p)==h for p,h in frozen.items());assert all(codecs.sha(work/p)==h for p,h in retained.items())
    provenance=json.loads((work/'library-source-provenance.json').read_text())
    for row in provenance:assert phase.file.dsp.source.index_gate.timeline.source_tree(libraries[row['crate']])==row['source_files']
    for name,row in resampling.items():assert all(codecs.sha(work/'resampling-vendor'/f"{name}-{row['version']}"/p)==h for p,h in row['source_files'].items())
    summary={'positive_tests_per_profile':positives,'compiled_controls':len(receipts),'new_controls':0 if positive_only else 2,'retained_controls':0 if positive_only else 114,'controls':receipts,'root_inputs_unchanged':True,'source_copies_restored':True,'root_lock_sha256':codecs.sha(ROOT/'engine/Cargo.lock'),'libraries':len(provenance),'patches':sum(len(r['patches'])for r in provenance),'resampling_source_files_unchanged':True,'new_single_sources_per_profile':40,'new_cyclic_sources_per_profile':4,'scope':'Generation-owned loading transport/seek applied once at actual CanPlay;Source loop+seek ACK and single/cyclic PCM retained. Shipping codec and loading startTime generalization remain open.'}
    (work/'summary.json').write_text(json.dumps(summary,indent=2)+'\n');print('candidate complete',positives,len(receipts),flush=True)

if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--scope',choices=['native','candidate'],required=True);p.add_argument('--output',type=Path,required=True);p.add_argument('--target-dir',type=Path,required=True);p.add_argument('--rubato-cargo-home',type=Path,required=True);p.add_argument('--positive-only',action='store_true');a=p.parse_args()
    if a.scope=='native':native(a.output.resolve(),a.target_dir.resolve(),a.positive_only)
    else:candidate(a.output.resolve(),a.target_dir.resolve(),a.rubato_cargo_home.resolve(),a.positive_only)
