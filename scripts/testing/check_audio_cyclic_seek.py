#!/usr/bin/env python3
"""Cyclic public seeks must retain queued mode, canonical grid and source ACK.

The ordinary codec seek intentionally rejects cyclic decoders. Reusing a loop
toggle at seek(duration) instead chooses an extra cycle. Exercise the explicit
trait operation, exact empty endpoint and real File/HTTP PCM, then rebuild
violations before retaining the existing actor/publication/loop contracts.
"""
from __future__ import annotations
import argparse,json,re,tomllib
from pathlib import Path
import check_audio_live_loop as live

ROOT=live.ROOT
codecs=live.codecs
public=live.public
phase=live.phase
clock=live.clock

def native_declarations(originals):
    rows=[];prefix='inner_audio::live_loop_tests::cyclic_seek::';p='inner_audio.rs'
    first=prefix+'cyclic_seek_waits_for_ack_and_fresh_pcm_in_every_play_state'
    end=prefix+'queued_disable_then_seek_duration_has_one_empty_terminal_suffix'
    math=prefix+'seek_descriptor_is_integer_bounded_and_distinct_from_cycle_cutover'
    def change(name,path,signature,before,after,test):live.cutover.scoped(rows,originals,name,path,signature,before,after,test)
    request='    fn request_stream_control('
    change('cyclic-public-seek-uses-legacy-operation',p,request,'if matches!(kind, ControlKind::Seek | ControlKind::Start) && managed {','if false {',first)
    change('cyclic-seek-forgets-queued-mode',p,request,
        '''enabled: self
                        .stream_controls
                        .desired_loop(self.shared.loop_enabled()),
                    seek_target:''',
        'enabled: self.shared.loop_enabled(),\n                    seek_target:',end)
    change('cyclic-seek-loses-input-target',p,request,'seek_target: Some(target as u64),','seek_target: None,',first)
    change('cyclic-seek-uses-old-consumer-position',p,request,
        'output_start: position.frame + u64::from(position.phase != 0),','output_start: self.shared.position_frames(),',first)
    change('cyclic-seek-notification-suppressed',p,'    fn accept_as(',
        '            notify_seek,\n            self.traversal,','            notify_seek && self.traversal.is_none(),\n            self.traversal,',first)
    change('cyclic-seek-single-extent-adds-a-cycle','streaming.rs','    pub(crate) fn seek(',
        'StreamPresentation::single(clock)','StreamPresentation::repeated(clock, Some(2))?',math)
    change('cyclic-seek-continuous-mode-gets-end','streaming.rs','    pub(crate) fn seek(',
        'StreamPresentation::repeated(clock, None)?','StreamPresentation::single(clock)',math)
    change('cyclic-seek-output-origin-rounded-down','streaming.rs','    pub(crate) fn seek(',
        'position.frame + u64::from(position.phase != 0)','position.frame',math)
    change('cyclic-seek-input-origin-uses-float','streaming.rs','    pub(crate) fn seek(',
        'clock.position(target)?','clock.position((target as f64) as u64)?',math)
    change('cyclic-seek-request-mapping-unchecked','stream_control.rs','    pub(crate) fn traversal(',
        'if traversal.output_start() != self.output_start {','if false {',math)
    change('cyclic-seek-after-later-stop-announces-events',p,'    fn poll_stream_controls(',
        '!quiet\n                                && matches!(pending.kind, ControlKind::Seek | ControlKind::Start)','matches!(pending.kind, ControlKind::Seek | ControlKind::Start)',prefix+'queued_cyclic_seek_stop_play_preserves_last_mode_and_zero_position')
    return rows

def candidate_declarations(originals):
    rows=[];prefix='timed_m4a::live_loop_tests::cyclic_seek::'
    actor=prefix+'real_file_http_cyclic_seek_restart_and_disabled_endpoints_match_pcm'
    codec=prefix+'cyclic_codec_seek_matches_continuous_and_single_reference_at_boundaries'
    protocol=prefix+'malformed_and_expired_cyclic_source_requests_preserve_the_old_owner'
    heap=prefix+'cached_cyclic_seek_refusals_retirement_and_repeated_changes_keep_pcm_and_heap_contracts'
    def change(name,path,signature,before,after,test):live.cutover.scoped(rows,originals,name,path,signature,before,after,test)
    def whole(name,path,before,after,test):
        assert originals[path].count(before)==1,(name,'mutation anchor drift')
        rows.append((name,path,before,after,test))
    op='decoder.seek_loop(Timestamp::new(frame as i64), request.enabled)'
    whole('cyclic-source-dispatches-ordinary-seek','producer_control.rs',op,'decoder.seek(Timestamp::new(frame as i64))',actor)
    whole('cyclic-source-loses-media-target','producer_control.rs',op,'decoder.seek_loop(Timestamp::ZERO, request.enabled)',actor)
    whole('cyclic-source-forgets-requested-off-mode','producer_control.rs',op,'decoder.seek_loop(Timestamp::new(frame as i64), true)',actor)
    whole('cyclic-source-command-target-unchecked','producer_control.rs',
        '.is_some_and(|frame| i64::try_from(frame).ok() != Some(target))','.is_some_and(|_| false)',protocol)
    change('cyclic-single-seek-keeps-old-loop-state','resampled_streaming.rs','    pub(crate) fn seek_single_from(',
        '        self.looping = None;','',codec)
    change('cyclic-single-seek-loses-dsp-grid','resampled_streaming.rs','    pub(crate) fn seek_single_from(',
        'self.seek_from(ts, clock)?','self.seek_from(ts, None)?',codec)
    seek='    fn seek_loop('
    change('cyclic-codec-seek-forgets-continuous-mode','timed_m4a.rs',seek,
        'self.cutover(traversal.output_start(), true)?','self.cutover(traversal.output_start(), false)?',codec)
    change('cyclic-codec-seek-rounds-output-position','timed_m4a.rs',seek,
        'self.cutover(traversal.output_start(), true)?','self.cutover(traversal.output_start() / 16 * 16, true)?',codec)
    change('cyclic-codec-seek-reuses-single-mode','timed_m4a.rs',seek,
        'let mut seeked = self.cutover(traversal.output_start(), true)?;','let mut seeked = self.seek_single(target)?;',codec)
    change('cyclic-codec-off-seek-rewinds-to-zero','timed_m4a.rs',seek,
        'return self.seek_single(target);','return self.seek_single(Timestamp::ZERO);',codec)
    change('cyclic-codec-refuses-valid-off-seek','timed_m4a.rs',seek,
        'return self.seek_single(target);','return Err(Error::SeekError(SeekErrorKind::Unseekable));',codec)
    change('cyclic-terminal-bypass-survives-continuous-restart','timed_m4a.rs','    pub(crate) fn cutover(',
        '        self.terminal_seek = false;','',codec)
    change('cyclic-codec-seek-clamps-invalid-input','timed_m4a.rs',seek,
        'crate::streaming::StreamTraversal::seek(clock, frame, enabled)',
        'crate::streaming::StreamTraversal::seek(clock, frame.min(clock.seek_frame(f64::MAX).unwrap() as u64), enabled)',heap)
    change('cyclic-codec-seek-allocates-per-operation','timed_m4a.rs',seek,
        '        use symphonia::core::errors::{Error, SeekErrorKind};',
        '        std::hint::black_box(vec![0_u8;64]);\n        use symphonia::core::errors::{Error, SeekErrorKind};',heap)
    return rows

def native_rows(originals):return native_declarations(originals)+live.native_rows(originals)
def candidate_rows(originals):return candidate_declarations(originals)+live.candidate_rows(originals)

def native(work,target,positive_only):
    package,env,frozen=clock.prepare(work,target,list((ROOT/'scripts/testing').glob('check_audio_*.py'))+[ROOT/'scripts/testing/check_gitignore_contract.py'])
    originals={n:(package/'src'/n).read_text()for n in ['streaming.rs','inner_audio.rs','stream_control.rs']}
    rows=native_rows(originals);assert len(rows)==98 and len({r[0]for r in rows})==98
    binary=public.build_native(package,work,env,'positive');log=work/'positive-tests.log'
    assert clock.common.run([str(binary)],log,package,env)==0
    assert re.search(r'test result: ok\. 334 passed; 0 failed; 2 ignored;',log.read_text())
    receipts=public.faults(work,package,env,originals,[]if positive_only else rows,lambda name:public.build_native(package,work,env,name),package/'src')
    assert all(codecs.sha(ROOT/p)==h and codecs.sha(work/'source-inputs'/p)==h for p,h in frozen.items())
    audio=ROOT/'engine/crates/audio/src';assert all(p.read_bytes()==(package/'src'/p.relative_to(audio)).read_bytes()for p in audio.rglob('*.rs'))
    summary={'positive_tests':334,'ignored_tests':2,'new_tests':8,'compiled_controls':len(receipts),'new_controls':0 if positive_only else 11,'retained_controls':0 if positive_only else 87,'controls':receipts,'root_inputs_unchanged':True,'source_copies_restored':True,'root_lock_sha256':codecs.sha(ROOT/'engine/Cargo.lock')}
    (work/'summary.json').write_text(json.dumps(summary,indent=2)+'\n');print(json.dumps(summary,indent=2),flush=True)

def candidate(work,target,rubato,positive_only):
    env,frozen,retained,libraries,resampling=live.prepare(work,target,rubato);package=work/'probe'
    codecs.run(['cargo','metadata','--offline','--format-version','1'],work/'metadata.jsonl',cwd=package,env=env)
    locked=tomllib.loads((ROOT/'engine/Cargo.lock').read_text())['package'];identity=lambda p:(p['name'],p['version'],p.get('source'),p.get('checksum'));known={p['name']for p in locked};pins=set(map(identity,locked))
    assert all(p['name']not in known or identity(p)in pins for p in tomllib.loads((package/'Cargo.lock').read_text())['package'])
    names=['timed_m4a.rs','resampled_streaming.rs','indexed_seek.rs','streaming_transport.rs','producer_control.rs','isomp4_source.rs','inner_audio.rs','stream_control.rs','off_worker.rs']
    originals={n:(work/'source'/n).read_text()for n in names};rows=candidate_rows(originals);assert len(rows)==104 and len({r[0]for r in rows})==104
    codecs.run(['cargo','check','--offline','--locked','--lib'],work/'normal-library-check.log',cwd=package,env=env)
    positives=phase.positive_profiles(work,env,[('cyclic','timed_m4a::loop_tests::',9),('cyclic-clock','resampled_streaming::loop_clock_tests::',2),('public-source','public_control_contracts::',3),('cutover-clock','streaming::cutover_tests::',6),('output-cutover','timed_m4a::cutover_tests::',5),('public-loop','timed_m4a::live_loop_tests::',8)])
    assert positives=={'release':291,'debug':288},positives
    receipts=public.faults(work,package,env,originals,[]if positive_only else rows,lambda name:phase.file.dsp.source.build(work,env,name,release=name=='paused-player-drains-past-prefetch',clean=False),work/'source')
    for row in receipts:row['profile']='release'if row['name']=='paused-player-drains-past-prefetch'else'debug'
    assert all(codecs.sha(ROOT/p)==h and codecs.sha(work/'source-inputs'/p)==h for p,h in frozen.items())
    assert all(codecs.sha(work/p)==h for p,h in retained.items())
    provenance=json.loads((work/'library-source-provenance.json').read_text())
    for row in provenance:assert phase.file.dsp.source.index_gate.timeline.source_tree(libraries[row['crate']])==row['source_files']
    for name,row in resampling.items():assert all(codecs.sha(work/'resampling-vendor'/f"{name}-{row['version']}"/p)==h for p,h in row['source_files'].items())
    summary={'positive_tests_per_profile':positives,'new_tests':4,'compiled_controls':len(receipts),'new_controls':0 if positive_only else 14,'retained_controls':0 if positive_only else 90,'controls':receipts,'root_inputs_unchanged':True,'source_copies_restored':True,'root_lock_sha256':codecs.sha(ROOT/'engine/Cargo.lock'),'libraries':len(provenance),'patches':sum(len(r['patches'])for r in provenance),'resampling_source_files_unchanged':True,'new_file_http_sources_per_profile':30,'new_public_seeks_per_profile':150,'new_repeated_restarts_per_profile':60,'scope':'Canonical public cyclic seek with lastqueued mode,source-bound traversal ACK,exact disabled terminal and terminal-to-continuous restart.Existing CPU/source/index/DSP reused.Productcodec/quota renewal/otherloading/rate/device/performance open.'}
    (work/'summary.json').write_text(json.dumps(summary,indent=2)+'\n');print(json.dumps(summary,indent=2),flush=True)

if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--scope',choices=['native','candidate'],required=True);p.add_argument('--output',type=Path,required=True);p.add_argument('--target-dir',type=Path,required=True);p.add_argument('--rubato-cargo-home',type=Path,required=True);p.add_argument('--positive-only',action='store_true');a=p.parse_args()
    if a.scope=='native':native(a.output.resolve(),a.target_dir.resolve(),a.positive_only)
    else:candidate(a.output.resolve(),a.target_dir.resolve(),a.rubato_cargo_home.resolve(),a.positive_only)
