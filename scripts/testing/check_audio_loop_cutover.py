#!/usr/bin/env python3
"""Verify exact output-position cyclic reconfiguration before public loop wiring.

Decoder lookahead cannot decide the audible loop's end. Rebuild bounded real
history across wraps, retain the global sample grid, and reuse the existing
indexed CPU/source transaction without publishing an unaccepted traversal.
"""
from __future__ import annotations
import argparse
import importlib.util
import json
from pathlib import Path
import re
import shutil
import tomllib
import check_audio_cyclic_clock as cyclic

ROOT=cyclic.ROOT
codecs=cyclic.codecs
public=cyclic.public
phase=cyclic.phase
clock=cyclic.clock

def prepare(work,target,rubato):
    env,frozen,_,libraries,resampling=cyclic.prepare(work,target,rubato)
    for rel in ['engine/crates/audio/src/stream_cutover_tests.rs','scripts/testing/fixtures/audio-codec-probe/loop_cutover_contracts.rs']:
        path=ROOT/rel;shutil.copy2(path,work/'source'/path.name)
        assert rel in frozen
    retained={str(p.relative_to(work)):codecs.sha(p)for tree in [work/'source',work/'probe/src']for p in tree.rglob('*.rs')}
    (work/'retained-sources.json').write_text(json.dumps(retained,indent=2)+'\n')
    return env,frozen,retained,libraries,resampling

def scoped(rows,originals,name,path,signature,before,after,test):
    source=originals[path];start=source.index(signature);end=source.index('\n    }',start)+len('\n    }');body=source[start:end]
    assert body.count(before)==1,(name,'mutation anchor drift')
    rows.append((name,path,body,body.replace(before,after),test))

def native_declarations(originals):
    rows=[];path='streaming.rs';prefix='streaming::cutover_tests::'
    main='impl StreamCutover {\n    pub(crate) fn new('
    boundary=prefix+'disabling_finishes_the_consumed_cycle_on_the_global_grid'
    history=prefix+'cyclic_preroll_crosses_short_tracks_without_rebasing_phase_to_each_track'
    overflow=prefix+'cutover_overflow_is_rejected_without_saturating_or_wrapping'
    def change(name,signature,before,after,test):scoped(rows,originals,name,path,signature,before,after,test)
    change('cutover-clock-uses-float-before-integer-phase',main,
        'u64::try_from(u128::from(output_start) * u128::from(input) / u128::from(output))',
        'Ok::<u64, std::num::TryFromIntError>((output_start as f64 * input as f64 / output as f64) as u64)',prefix+'arbitrary_output_samples_are_not_rounded_to_an_input_frame_boundary')
    change('cutover-cycle-uses-rounded-single-track-output',main,'target / timeline.frames','output_start / timeline.output_frames()',boundary)
    change('cutover-preroll-dropped',main,'target.saturating_sub(history)','target',history)
    change('cutover-preroll-forgets-global-grid',main,'let period = input / a;','let period = 1;',history)
    change('cutover-output-position-rounded-down',main,'            output_start,','            output_start: (u128::from(target)*u128::from(output)/u128::from(input)) as u64,',prefix+'arbitrary_output_samples_are_not_rounded_to_an_input_frame_boundary')
    change('cutover-cycle-overflow-saturates',main,'.checked_add(1)','.checked_add(1).or(Some(1))',overflow)
    change('cutover-input-overflow-saturates',main,
        '''        let target =
            u64::try_from(u128::from(output_start) * u128::from(input) / u128::from(output))
                .map_err(|_| {
                    EngineError::from_detail(ErrorCode::InvalidArgument, "cutover input overflow")
                })?;''',
        '        let target = (u128::from(output_start)*u128::from(input)/u128::from(output)).min(u128::from(u64::MAX)) as u64;',overflow)
    change('cutover-source-origin-forgets-absolute-cycles','    pub(crate) fn source_origin(',
        'self.input_origin / frames','0',history)
    change('cutover-clock-planning-allocates',main,'        let input =',
        '        std::hint::black_box(vec![0_u8;64]);\n        let input =',prefix+'cutover_planning_has_no_steady_heap_activity')
    return rows

def candidate_declarations(originals):
    rows=[];prefix='timed_m4a::cutover_tests::';finite=prefix+'finite_cutovers_match_whole_concatenation_at_arbitrary_output_samples'
    look=prefix+'enabling_then_disabling_uses_consumption_instead_of_decoder_lookahead'
    rebase='    pub(crate) fn cutover_from('
    def change(name,path,signature,before,after,test=finite):scoped(rows,originals,name,path,signature,before,after,test)
    change('loop-cutover-dsp-origin-becomes-track-relative','resampled_streaming.rs',rebase,'Some((plan.input_origin(), plan.output_start()))','Some((local, plan.output_start()))')
    change('loop-cutover-forgets-first-partial-cycle','resampled_streaming.rs',rebase,'frames: local,','frames: 0,')
    change('loop-cutover-forgets-completed-cycles','resampled_streaming.rs',rebase,'            completed,','            completed: 0,')
    change('loop-cutover-starts-with-sealed-boundary','resampled_streaming.rs',rebase,'sealed: false,','sealed: true,')
    change('loop-cutover-dsp-clock-not-rebased','resampled_streaming.rs',rebase,'Some((plan.input_origin(), plan.output_start()))','None')
    change('loop-cutover-uses-decoder-lookahead-end','resampled_streaming.rs',rebase,
        '.map_or(Repetitions::Continuous, Repetitions::Finite)',
        '.map_or(Repetitions::Continuous, |cycles| Repetitions::Finite(self.looping.as_ref().map_or(cycles, |old| old.completed.saturating_add(1))))',look)
    change('loop-cutover-rounds-requested-output-position','timed_m4a.rs','    pub(crate) fn cutover(',
        'StreamCutover::new(clock, output_start, enabled, history)','StreamCutover::new(clock, output_start / 16 * 16, enabled, history)')
    change('loop-cutover-revives-retired-wrapper','resampled_streaming.rs','    pub(crate) fn seek_from(',
        'if self.failed || self.stopped {','if false {',prefix+'rejected_or_retired_cutovers_preserve_or_retire_the_exact_decoder')
    change('loop-cutover-rebase-allocates','resampled_streaming.rs',rebase,'        let (local, completed)',
        '        std::hint::black_box(vec![0_u8;64]);\n        let (local, completed)',prefix+'warmed_cached_cutovers_and_cyclic_preroll_do_not_allocate')
    # Scope to the generic primitive, not the backward-compatible input seek.
    source=originals['indexed_seek.rs'];before='let seeked = match position(slot) {';assert source.count(before)==1
    rows.append(('output-cutover-operation-ignored','indexed_seek.rs',before,'let seeked = match slot.seek(Timestamp::ZERO) {',prefix+'real_file_http_output_cutovers_reuse_cpu_source_deadline_and_strict_pcm'))
    assert originals['indexed_seek.rs'].count('if slot.is_none() {')==1
    rows.append(('output-cutover-mutates-uninitialized-decoder','indexed_seek.rs','if slot.is_none() {','if false {',prefix+'real_file_http_output_cutovers_reuse_cpu_source_deadline_and_strict_pcm'))
    return rows

def source_declarations():
    spec=importlib.util.spec_from_file_location('cutover_source_seek',ROOT/'scripts/testing/check_audio_source_seek.py');m=importlib.util.module_from_spec(spec);spec.loader.exec_module(m)
    return [(name,path,before,after,'source_seek_contracts::'+test)for name,path,before,after,test in m.declarations()]

def native(work,target,positive_only):
    package,env,frozen=clock.prepare(work,target,[Path(__file__).resolve(),ROOT/'scripts/testing/check_audio_cyclic_clock.py',ROOT/'scripts/testing/check_audio_public_controls.py',ROOT/'scripts/testing/check_audio_stream_revisions.py',ROOT/'scripts/testing/check_gitignore_contract.py'])
    originals={n:(package/'src'/n).read_text()for n in ['streaming.rs','inner_audio.rs','stream_control.rs']}
    rows=native_declarations(originals)+cyclic.native_declarations(originals)+public.native_declarations(originals)+public.revisions.controls(originals)+phase.publication_declarations(originals['streaming.rs'])
    assert len(rows)==73
    binary=public.build_native(package,work,env,'positive');log=work/'positive-tests.log'
    assert clock.common.run([str(binary)],log,package,env)==0
    assert re.search(r'test result: ok\. 318 passed; 0 failed; 2 ignored;',log.read_text())
    receipts=public.faults(work,package,env,originals,[]if positive_only else rows,lambda name:public.build_native(package,work,env,name),package/'src')
    assert all(codecs.sha(ROOT/p)==h and codecs.sha(work/'source-inputs'/p)==h for p,h in frozen.items())
    audio=ROOT/'engine/crates/audio/src';assert all(p.read_bytes()==(package/'src'/p.relative_to(audio)).read_bytes()for p in audio.rglob('*.rs'))
    summary={'positive_tests':318,'ignored_tests':2,'new_tests':6,'compiled_controls':len(receipts),'new_controls':0 if positive_only else 9,'retained_controls':0 if positive_only else 64,'controls':receipts,'root_inputs_unchanged':True,'source_copies_restored':True,'root_lock_sha256':codecs.sha(ROOT/'engine/Cargo.lock')}
    (work/'summary.json').write_text(json.dumps(summary,indent=2)+'\n');print(json.dumps(summary,indent=2),flush=True)

def candidate(work,target,rubato,positive_only):
    env,frozen,retained,libraries,resampling=prepare(work,target,rubato);package=work/'probe'
    codecs.run(['cargo','metadata','--offline','--format-version','1'],work/'metadata.jsonl',cwd=package,env=env)
    locked=tomllib.loads((ROOT/'engine/Cargo.lock').read_text())['package'];identity=lambda p:(p['name'],p['version'],p.get('source'),p.get('checksum'));known={p['name']for p in locked};pins=set(map(identity,locked))
    assert all(p['name']not in known or identity(p)in pins for p in tomllib.loads((package/'Cargo.lock').read_text())['package'])
    originals={n:(work/'source'/n).read_text()for n in ['timed_m4a.rs','resampled_streaming.rs','indexed_seek.rs','streaming_transport.rs','producer_control.rs','isomp4_source.rs']}
    rows=candidate_declarations(originals)+cyclic.candidate_declarations(originals)+public.candidate_declarations(originals)+public.cyclic.declarations(originals)+source_declarations()
    assert len(rows)==61
    codecs.run(['cargo','check','--offline','--locked','--lib'],work/'normal-library-check.log',cwd=package,env=env)
    positives=phase.positive_profiles(work,env,[('cyclic','timed_m4a::loop_tests::',9),('cyclic-clock','resampled_streaming::loop_clock_tests::',2),('public-source','public_control_contracts::',3),('cutover-clock','streaming::cutover_tests::',6),('output-cutover','timed_m4a::cutover_tests::',5)])
    assert positives=={'release':267,'debug':264},positives
    receipts=public.faults(work,package,env,originals,[]if positive_only else rows,lambda name:phase.file.dsp.source.build(work,env,name,release=False,clean=False),work/'source')
    assert all(codecs.sha(ROOT/p)==h and codecs.sha(work/'source-inputs'/p)==h for p,h in frozen.items())
    assert all(codecs.sha(work/p)==h for p,h in retained.items())
    provenance=json.loads((work/'library-source-provenance.json').read_text())
    for row in provenance:assert phase.file.dsp.source.index_gate.timeline.source_tree(libraries[row['crate']])==row['source_files']
    for name,row in resampling.items():assert all(codecs.sha(work/'resampling-vendor'/f"{name}-{row['version']}"/p)==h for p,h in row['source_files'].items())
    summary={'positive_tests_per_profile':positives,'new_tests':5,'compiled_controls':len(receipts),'new_controls':0 if positive_only else 11,'retained_controls':0 if positive_only else 50,'controls':receipts,'root_inputs_unchanged':True,'source_copies_restored':True,'root_lock_sha256':codecs.sha(ROOT/'engine/Cargo.lock'),'libraries':len(provenance),'patches':sum(len(r['patches'])for r in provenance),'resampling_source_files_unchanged':True,'new_full_render_comparisons_per_profile':144,'real_source_transactions_per_profile':36,'uninitialized_and_expired_refusals_each_per_profile':36,'scope':'Exact consumed-output cutover math and bounded cyclic codec/sinc reconfiguration;existing CPU/cache/ReadAt/deadline transaction.No public loop/revision wiring,quota renewal,shippingcodec or device/performance completion.'}
    (work/'summary.json').write_text(json.dumps(summary,indent=2)+'\n');print(json.dumps(summary,indent=2),flush=True)

if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--scope',choices=['native','candidate'],required=True);p.add_argument('--output',type=Path,required=True);p.add_argument('--target-dir',type=Path,required=True);p.add_argument('--rubato-cargo-home',type=Path,required=True);p.add_argument('--positive-only',action='store_true');a=p.parse_args()
    if a.scope=='native':native(a.output.resolve(),a.target_dir.resolve(),a.positive_only)
    else:candidate(a.output.resolve(),a.target_dir.resolve(),a.rubato_cargo_home.resolve(),a.positive_only)
