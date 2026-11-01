#!/usr/bin/env python3
"""Public loop modes require source-bound per-revision output metadata and ACK.

An initial source clock must not override a later traversal. Validate the
shared audio-thread setter and ordered receipts against real File/HTTP PCM,
then rebuild violations in identity, mode, boundaries and publication order.
"""
from __future__ import annotations
import argparse,json,re,shutil,tomllib
from pathlib import Path
import check_audio_loop_cutover as cutover
import check_audio_producer_controls as producer
ROOT=cutover.ROOT
codecs=cutover.codecs
public=cutover.public
phase=cutover.phase
clock=cutover.clock

def prepare(work,target,rubato):
    env,frozen,_,libraries,resampling=cutover.prepare(work,target,rubato)
    for rel in ['engine/crates/audio/src/live_loop_tests.rs','scripts/testing/fixtures/audio-codec-probe/live_loop_contracts.rs']:
        p=ROOT/rel;shutil.copy2(p,work/'source'/p.name);assert rel in frozen
    retained={str(p.relative_to(work)):codecs.sha(p)for tree in [work/'source',work/'probe/src']for p in tree.rglob('*.rs')}
    (work/'retained-sources.json').write_text(json.dumps(retained,indent=2)+'\n')
    return env,frozen,retained,libraries,resampling

def native_declarations(originals):
    rows=[];p='inner_audio.rs';prefix='inner_audio::live_loop_tests::'
    first=prefix+'loop_waits_for_exact_source_revision_then_keeps_position_without_seek_events'
    queued=prefix+'queued_on_off_and_later_stop_never_restore_an_older_position'
    loading=prefix+'loading_loop_intent_is_submitted_once_after_source_can_play'
    ended=prefix+'disabling_after_many_wraps_uses_new_extent_and_one_exact_ended'
    def change(name,path,signature,before,after,test):cutover.scoped(rows,originals,name,path,signature,before,after,test)
    setter='    pub fn set_loop_enabled(&mut self,'
    change('loop-public-setter-bypasses-source',p,setter,'        self.request_stream_control(0.0, ControlKind::Loop(enabled));','        self.shared.set_loop_enabled(enabled);',first)
    change('loop-receipt-forgets-requested-traversal',p,'    fn poll_stream_controls(',
        'if expected.as_ref().is_err() || expected.ok().flatten() != prepared.traversal {','if false {',prefix+'loop_receipt_must_match_the_requested_output_mode_and_clock')
    change('loop-output-uses-legacy-input-mapping','stream_control.rs','    pub(crate) fn mapped_output(',
        'if let Some(traversal) = self.traversal {','if let Some(traversal) = self.traversal.filter(|_| false) {',first)
    change('loop-duration-prefers-initial-source-extent',p,'    fn update_stream_duration(',
        '''let presentation = self
            .stream_presentation
            .or_else(|| self.stream_state.as_ref().and_then(|s| s.presentation()));''',
        'let presentation = self.stream_state.as_ref().and_then(|s| s.presentation()).or(self.stream_presentation);',ended)
    change('loop-receipt-is-bounded-by-single-track',p,'    fn accept_stream_revision_as(',
        '.map_or(Some(t.output_frames()), |p| p.presentation().output_end())','.map_or(Some(t.output_frames()), |_| Some(t.output_frames()))',ended)
    change('loop-acceptance-loses-output-position',p,'    fn accept_as(',
        '!quiet && (notify_seek || self.traversal.is_some())','!quiet && notify_seek',first)
    change('loop-acceptance-keeps-old-shared-mode',p,'    fn accept_stream_revision_as(',
        '.set_loop_enabled(traversal.presentation().output_end().is_none())','.set_loop_enabled(false)',first)
    change('loop-acceptance-resumes-paused-player',p,'    fn accept_stream_revision_as(',
        '        self.replay_needed = false;','        self.replay_needed = false;\n        self.shared.set_state(PlaybackState::Playing);',prefix+'loop_acceptance_does_not_resume_a_paused_or_stopped_player')
    change('loop-restart-forgets-queued-cutover',p,'    fn request_stream_control(',
        '|| self.stream_controls.has_cutover()','|| false',queued)
    change('loop-restart-uses-first-queued-mode','stream_control.rs','    pub(crate) fn desired_loop(',
        '            .rev()\n','',queued)
    change('loop-quiet-receipt-exposes-old-position',p,'    fn accept_as(',
        '!quiet && (notify_seek || self.traversal.is_some())','(!quiet && notify_seek) || self.traversal.is_some()',queued)
    change('loop-loading-intent-is-discarded',p,setter,'            self.loop_intent = Some(enabled);','            self.loop_intent = None;',loading)
    change('loop-loading-intent-overrides-uncontrolled-source',p,'    pub(crate) fn complete_stream_load(',
        '            .filter(|s| s.control_sender().is_some())\n','', 'inner_audio::tests::streaming_loop_waits_for_complete_input_before_wrapping')
    change('loop-loading-intent-is-submitted-repeatedly',p,'    pub fn poll_stream(',
        'self.loop_intent.take()','self.loop_intent.as_ref().copied()',loading)
    return rows

def candidate_declarations(originals):
    rows=[];prefix='timed_m4a::live_loop_tests::'
    first=prefix+'file_http_public_loop_modes_keep_exact_pcm_clock_and_current_cycle_end'
    malicious=prefix+'unaccepted_decoder_metadata_change_is_fatal_after_loop_ack'
    def change(name,path,signature,before,after,test=first):cutover.scoped(rows,originals,name,path,signature,before,after,test)
    # The producer method is nested in its trait implementation. Use its whole
    # source for unique anchors, so nested async scopes cannot truncate it.
    def whole(name,path,before,after,test=first):
        assert originals[path].count(before)==1,(name,'mutation anchor drift')
        rows.append((name,path,before,after,test))
    p='producer_control.rs';driver='streaming_transport.rs'
    whole('loop-producer-seeks-instead-of-cutover',p,'.cutover(request.output_start, request.enabled)','.seek(Timestamp::ZERO)')
    whole('loop-producer-rounds-output-to-zero',p,'.cutover(request.output_start, request.enabled)','.cutover(0, request.enabled)')
    whole('loop-producer-inverts-requested-mode',p,'.cutover(request.output_start, request.enabled)','.cutover(request.output_start, !request.enabled)')
    whole('loop-prepared-receipt-loses-traversal',p,'                        traversal,\n                        source:','                        traversal: None,\n                        source:')
    whole('loop-applied-result-loses-traversal',p,'                        output_start,\n                        traversal,','                        output_start,\n                        traversal: None,')
    whole('loop-driver-keeps-initial-presentation',driver,'                active_presentation = Some(traversal.presentation());','                active_presentation = state.presentation();')
    whole('loop-driver-rebinds-immutable-source-presentation',driver,'if initial_revision {','if true {')
    whole('loop-producer-publishes-from-zero',p,'Some(traversal.output_start())','Some(0)')
    whole('loop-decoder-changes-unaccepted-metadata',driver,'if expected_presentation.is_some_and(|old| old != presentation) {','if false {',malicious)
    source=originals[p];start=source.index('                    let accepted = while_active(');check=source.index('                    if !matches!(accepted, Some(Ok(Ok(()))))',start);end=source.index('\n                    }',check)+len('\n                    }')
    whole('loop-producer-publishes-before-consumer-ack',p,source[start:end],'                    drop(receipt);')
    whole('loop-timed-trait-refuses-valid-cutover','timed_m4a.rs','        Timed::cutover(self, output_start, enabled)',
        '        Err(symphonia::core::errors::Error::SeekError(symphonia::core::errors::SeekErrorKind::Unseekable))')
    change('loop-command-drops-cutover','stream_control.rs','pub(crate) fn request_as(',
        '        cutover,','        cutover: None,')
    return rows

def native_rows(originals):
    return native_declarations(originals)+cutover.native_declarations(originals)+cutover.cyclic.native_declarations(originals)+public.native_declarations(originals)+public.revisions.controls(originals)+phase.publication_declarations(originals['streaming.rs'])

def candidate_rows(originals):
    return candidate_declarations(originals)+cutover.candidate_declarations(originals)+cutover.cyclic.candidate_declarations(originals)+public.candidate_declarations(originals)+public.cyclic.declarations(originals)+cutover.source_declarations()+producer.declarations(originals)

def native(work,target,positive_only):
    extras=list((ROOT/'scripts/testing').glob('check_audio_*.py'))+[ROOT/'scripts/testing/check_gitignore_contract.py']
    package,env,frozen=clock.prepare(work,target,extras)
    originals={n:(package/'src'/n).read_text()for n in ['streaming.rs','inner_audio.rs','stream_control.rs']}
    rows=native_rows(originals);assert len(rows)==87 and len({r[0]for r in rows})==87
    binary=public.build_native(package,work,env,'positive');log=work/'positive-tests.log'
    assert clock.common.run([str(binary)],log,package,env)==0
    assert re.search(r'test result: ok\. 326 passed; 0 failed; 2 ignored;',log.read_text())
    receipts=public.faults(work,package,env,originals,[]if positive_only else rows,lambda name:public.build_native(package,work,env,name),package/'src')
    assert all(codecs.sha(ROOT/p)==h and codecs.sha(work/'source-inputs'/p)==h for p,h in frozen.items())
    audio=ROOT/'engine/crates/audio/src';assert all(p.read_bytes()==(package/'src'/p.relative_to(audio)).read_bytes()for p in audio.rglob('*.rs'))
    summary={'positive_tests':326,'ignored_tests':2,'new_tests':8,'compiled_controls':len(receipts),'new_controls':0 if positive_only else 14,'retained_controls':0 if positive_only else 73,'controls':receipts,'root_inputs_unchanged':True,'source_copies_restored':True,'root_lock_sha256':codecs.sha(ROOT/'engine/Cargo.lock')}
    (work/'summary.json').write_text(json.dumps(summary,indent=2)+'\n');print(json.dumps(summary,indent=2),flush=True)

def candidate(work,target,rubato,positive_only):
    env,frozen,retained,libraries,resampling=prepare(work,target,rubato);package=work/'probe'
    codecs.run(['cargo','metadata','--offline','--format-version','1'],work/'metadata.jsonl',cwd=package,env=env)
    locked=tomllib.loads((ROOT/'engine/Cargo.lock').read_text())['package'];identity=lambda p:(p['name'],p['version'],p.get('source'),p.get('checksum'));known={p['name']for p in locked};pins=set(map(identity,locked))
    assert all(p['name']not in known or identity(p)in pins for p in tomllib.loads((package/'Cargo.lock').read_text())['package'])
    names=['timed_m4a.rs','resampled_streaming.rs','indexed_seek.rs','streaming_transport.rs','producer_control.rs','isomp4_source.rs','inner_audio.rs','stream_control.rs','off_worker.rs']
    originals={n:(work/'source'/n).read_text()for n in names}
    rows=candidate_rows(originals);assert len(rows)==90 and len({r[0]for r in rows})==90
    codecs.run(['cargo','check','--offline','--locked','--lib'],work/'normal-library-check.log',cwd=package,env=env)
    positives=phase.positive_profiles(work,env,[('cyclic','timed_m4a::loop_tests::',9),('cyclic-clock','resampled_streaming::loop_clock_tests::',2),('public-source','public_control_contracts::',3),('cutover-clock','streaming::cutover_tests::',6),('output-cutover','timed_m4a::cutover_tests::',5),('public-loop','timed_m4a::live_loop_tests::',4)])
    assert positives=={'release':279,'debug':276},positives
    release_faults={r[0]for r in rows if r[0]=='paused-player-drains-past-prefetch'}
    receipts=public.faults(work,package,env,originals,[]if positive_only else rows,lambda name:phase.file.dsp.source.build(work,env,name,release=name in release_faults,clean=False),work/'source')
    for row in receipts:row['profile']='release'if row['name']in release_faults else'debug'
    assert all(codecs.sha(ROOT/p)==h and codecs.sha(work/'source-inputs'/p)==h for p,h in frozen.items())
    assert all(codecs.sha(work/p)==h for p,h in retained.items())
    provenance=json.loads((work/'library-source-provenance.json').read_text())
    for row in provenance:assert phase.file.dsp.source.index_gate.timeline.source_tree(libraries[row['crate']])==row['source_files']
    for name,row in resampling.items():assert all(codecs.sha(work/'resampling-vendor'/f"{name}-{row['version']}"/p)==h for p,h in row['source_files'].items())
    summary={'positive_tests_per_profile':positives,'new_tests':4,'compiled_controls':len(receipts),'new_controls':0 if positive_only else 12,'retained_controls':0 if positive_only else 78,'controls':receipts,'root_inputs_unchanged':True,'source_copies_restored':True,'root_lock_sha256':codecs.sha(ROOT/'engine/Cargo.lock'),'libraries':len(provenance),'patches':sum(len(r['patches'])for r in provenance),'resampling_source_files_unchanged':True,'loop_file_http_transactions_per_profile':30,'queued_on_off_stop_play_sources_per_profile':2,'loading_intent_sources_per_profile':2,'scope':'Public player loop setter and source-bound traversal revisions with consumer ACK;File/HTTP exact PCM and clock,ordered stop/play,immutable source identity.No cyclic public seek,quota renewal,shipping codec or device/performance completion.'}
    (work/'summary.json').write_text(json.dumps(summary,indent=2)+'\n');print(json.dumps(summary,indent=2),flush=True)

if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--scope',choices=['native','candidate'],required=True);p.add_argument('--output',type=Path,required=True);p.add_argument('--target-dir',type=Path,required=True);p.add_argument('--rubato-cargo-home',type=Path,required=True);p.add_argument('--positive-only',action='store_true');a=p.parse_args()
    if a.scope=='native':native(a.output.resolve(),a.target_dir.resolve(),a.positive_only)
    else:candidate(a.output.resolve(),a.target_dir.resolve(),a.rubato_cargo_home.resolve(),a.positive_only)
