#!/usr/bin/env python3
"""Separate a repeated stream's extent from its one-track media clock.

An aggregate duration hides loops; rounding each cycle drifts on short assets.
Validate the real shared publisher/player and pinned codec/source chain, then
rebuild faults in the rational phase, extent binding and EOF protocol.
"""
from __future__ import annotations
import argparse
import json
from pathlib import Path
import re
import shutil
import subprocess
import tomllib
import check_audio_public_controls as public

ROOT = public.ROOT
codecs = public.codecs
clock = public.clock
phase = public.phase


def prepare(work, target, rubato):
    return public.prepare(work, target, rubato)


def native_declarations(originals):
    rows = []
    prefix = 'inner_audio::cyclic_clock_tests::'
    def change(name, path, signature, before, after, test):
        source = originals[path]; start = source.index(signature)
        end = source.index('\n    }', start)+len('\n    }'); body = source[start:end]
        assert body.count(before) == 1, (name, 'mutation anchor drift')
        rows.append((name, path, body, body.replace(before, after), prefix+test))
    clock_test = 'rational_loop_time_does_not_accumulate_rounded_cycles_or_float_modulo'
    finite = 'finite_cycles_keep_one_duration_and_emit_one_ended_without_local_recycling'
    continuous = 'continuous_media_time_wraps_while_raw_pcm_progress_remains_monotonic'
    eof = 'continuous_done_and_finite_short_or_excess_pcm_are_rejected'
    rebind = 'rejected_mode_rebinding_preserves_the_unbounded_extent_and_source_identity'
    replacement = 'replacing_a_cyclic_source_clears_its_atomic_media_time_and_cache_handoff'
    extent = 'cyclic_extent_constructors_reject_zero_overflow_and_invalid_start'
    stream = 'streaming.rs'; player = 'inner_audio.rs'
    change('cyclic-phase-rounds-through-float', stream, '    pub(crate) fn seconds_at(',
           '(numerator % period) as f64', '((frame as f64 * self.timeline.input_rate as f64) % period as f64)', clock_test)
    change('cyclic-extent-rounds-each-cycle', stream, '    pub(crate) fn repeated(',
           '.map(|t| t.output_frames())', '.map(|_| timeline.output_frames().checked_mul(cycles).unwrap())', clock_test)
    change('cyclic-input-count-saturates', stream, '    pub(crate) fn repeated(',
           'timeline.frames.checked_mul(cycles)', 'timeline.frames.checked_mul(cycles).or(Some(u64::MAX))', extent)
    change('finite-cyclic-end-wraps-instead-of-ending', stream, '    pub(crate) fn seconds_at(',
           'self.output_end.is_some_and(|end| frame >= end)', 'false', clock_test)
    change('continuous-publisher-accepts-done', stream, '    pub(crate) fn validate_finish(',
           'self.presentation.is_some_and(|p| p.output_end().is_none())', 'false', eof)
    change('publisher-forgets-presentation-before-rebinding', stream,
           '    pub(crate) fn bind_presentation(\n        &mut self,',
           '        if self.presentation.is_some_and(|old| old != presentation) || self.finished {',
           '        self.presentation = None; self.expected_samples = None;\n        if self.presentation.is_some_and(|old| old != presentation) || self.finished {', rebind)
    change('source-accepts-rebound-presentation', stream,
           '    pub(crate) fn bind_presentation(&self,',
           '*self.presentation.get_or_init(|| presentation) != presentation', 'false', rebind)
    change('player-binds-single-cycle-pcm-ceiling', player, '    fn update_stream_duration(',
           '.map_or(Some(timeline.output_frames()), |p| p.output_end())',
           '.map_or(Some(timeline.output_frames()), |_| Some(timeline.output_frames()))', finite)
    change('continuous-player-accepts-done', player, '    pub fn poll_stream(',
           '.map_or(Some(t.output_frames()), |p| p.output_end())',
           '.map_or(Some(t.output_frames()), |p| p.output_end().or(Some(self.stream_decoded_end())))', eof)
    change('player-publishes-unwrapped-loop-time', player, '    fn publish_position(',
           'presentation.seconds_at(frame).to_bits()', '(frame as f64 / self.shared.sample_rate() as f64).to_bits()', continuous)
    change('shared-time-ignores-cyclic-publication', player, '    pub fn current_time(',
           'presentation != NO_PRESENTATION_TIME', 'false', continuous)
    change('source-change-keeps-old-media-time', player, '    fn publish_position(',
           '            self.shared\n                .presentation_time_bits\n                .store(NO_PRESENTATION_TIME, Ordering::Relaxed);', '', replacement)
    change('finite-source-recycles-its-pcm-locally', player, '    pub fn process(',
           'self.shared.loop_enabled() && !self.stream_presentation.is_some_and(|p| p.is_cyclic())',
           'self.shared.loop_enabled()', finite)
    change('cache-takes-repeated-pcm-with-single-clock', player, '    pub(crate) fn take_streamed_audio_with_source(',
           '            || self.stream_presentation.is_some_and(|p| p.is_cyclic())', '', replacement)
    change('cyclic-time-publication-allocates', player, '    fn publish_position(',
           '        if let Some(presentation)',
           '        std::hint::black_box(vec![0_u8;64]);\n        if let Some(presentation)',
           'continuous_clock_publication_and_mix_have_no_steady_heap_activity')
    return rows


def candidate_declarations(originals):
    rows = []
    test = 'timed_m4a::loop_tests::real_file_http_loops_keep_exact_pcm_and_emit_one_ended'
    def whole(name, path, before, after, exact=test):
        assert originals[path].count(before) == 1, (name, 'mutation anchor drift')
        rows.append((name, path, before, after, exact))
    whole('driver-forgets-cyclic-presentation', 'streaming_transport.rs',
          'if initial_revision {\n                            cancel.bind_presentation(presentation)?;\n                        }',
          'if false { cancel.bind_presentation(presentation)?; }')
    whole('driver-binds-one-cycle-publication-length', 'streaming_transport.rs',
          'publication.bind_presentation(\n                active_presentation\n                    .unwrap_or_else(|| crate::streaming::StreamPresentation::single(timeline)),\n                0,\n            )?;',
          'publication.bind_timeline(timeline, 0)?;')
    whole('candidate-repeats-aggregate-clock', 'timed_m4a.rs',
          'let timeline = from_track(track, self.output_rate)?;',
          'let timeline = self.timeline()?.unwrap_or(from_track(track, self.output_rate)?);')
    whole('candidate-finite-loop-claims-single-pass', 'timed_m4a.rs',
          'StreamPresentation::repeated(timeline, Some(cycles))', 'Ok(StreamPresentation::single(timeline))')
    whole('candidate-continuous-loop-has-finite-end', 'timed_m4a.rs',
          'Some(Repetitions::Continuous) => StreamPresentation::repeated(timeline, None)',
          'Some(Repetitions::Continuous) => Ok(StreamPresentation::single(timeline))',
          'timed_m4a::loop_tests::continuous_file_http_players_wrap_time_without_ending_or_rounding_each_cycle')
    whole('uncached-loop-forgets-consumption-budget', 'isomp4_source.rs',
          '        self.resumes.charge(state)?;', '',
          'timed_m4a::loop_tests::tiny_file_http_cycles_keep_single_duration_and_exact_rational_clock')
    return rows


def native(work, target, positive_only):
    package, env, frozen = clock.prepare(work, target, [Path(__file__).resolve(), ROOT/'scripts/testing/check_gitignore_contract.py',
        ROOT/'scripts/testing/check_audio_public_controls.py', ROOT/'scripts/testing/check_audio_stream_revisions.py'])
    originals = {n: (package/'src'/n).read_text() for n in ['streaming.rs','inner_audio.rs','stream_control.rs']}
    rows = native_declarations(originals)+public.native_declarations(originals)+public.revisions.controls(originals)+phase.publication_declarations(originals['streaming.rs'])
    binary = public.build_native(package, work, env, 'positive')
    log = work/'positive-tests.log'; assert clock.common.run([str(binary)], log, package, env) == 0
    assert re.search(r'test result: ok\. 312 passed; 0 failed; 2 ignored;', log.read_text())
    receipts = public.faults(work, package, env, originals, [] if positive_only else rows,
        lambda name: public.build_native(package, work, env, name), package/'src')
    assert all(codecs.sha(ROOT/p)==h and codecs.sha(work/'source-inputs'/p)==h for p,h in frozen.items())
    audio=ROOT/'engine/crates/audio/src'
    assert all(p.read_bytes()==(package/'src'/p.relative_to(audio)).read_bytes() for p in audio.rglob('*.rs'))
    summary={'positive_tests':312,'ignored_tests':2,'new_tests':8,'compiled_controls':len(receipts),
             'new_controls':0 if positive_only else len(native_declarations(originals)),'retained_public_revision_publication_controls':0 if positive_only else 49,
             'controls':receipts,'root_inputs_unchanged':True,'source_copies_restored':True,'root_lock_sha256':codecs.sha(ROOT/'engine/Cargo.lock')}
    (work/'summary.json').write_text(json.dumps(summary,indent=2)+'\n');print(json.dumps(summary,indent=2),flush=True)


def candidate(work,target,rubato,positive_only):
    env,frozen,retained,libraries,resampling=prepare(work,target,rubato);package=work/'probe'
    codecs.run(['cargo','metadata','--offline','--format-version','1'],work/'metadata.jsonl',cwd=package,env=env)
    locked=tomllib.loads((ROOT/'engine/Cargo.lock').read_text())['package'];identity=lambda p:(p['name'],p['version'],p.get('source'),p.get('checksum'))
    known={p['name']for p in locked};pins=set(map(identity,locked))
    assert all(p['name']not in known or identity(p)in pins for p in tomllib.loads((package/'Cargo.lock').read_text())['package'])
    originals={n:(work/'source'/n).read_text()for n in ['streaming_transport.rs','timed_m4a.rs','producer_control.rs','resampled_streaming.rs','isomp4_source.rs']}
    rows=candidate_declarations(originals)+public.candidate_declarations(originals)+[row for row in public.cyclic.declarations(originals)if row[1]=='timed_m4a.rs']
    codecs.run(['cargo','check','--offline','--locked','--lib'],work/'normal-library-check.log',cwd=package,env=env)
    positives=phase.positive_profiles(work,env,[('cyclic','timed_m4a::loop_tests::',9),('cyclic-clock','resampled_streaming::loop_clock_tests::',2),('public-source','public_control_contracts::',3)])
    assert positives=={'release':256,'debug':253},positives
    receipts=public.faults(work,package,env,originals,[]if positive_only else rows,
        lambda name:phase.file.dsp.source.build(work,env,name,release=False,clean=False),work/'source')
    assert all(codecs.sha(ROOT/p)==h and codecs.sha(work/'source-inputs'/p)==h for p,h in frozen.items())
    assert all(codecs.sha(work/p)==h for p,h in retained.items()),'copy not restored'
    provenance=json.loads((work/'library-source-provenance.json').read_text())
    for row in provenance:assert phase.file.dsp.source.index_gate.timeline.source_tree(libraries[row['crate']])==row['source_files']
    for name,row in resampling.items():assert all(codecs.sha(work/'resampling-vendor'/f"{name}-{row['version']}"/p)==h for p,h in row['source_files'].items())
    summary={'positive_tests_per_profile':positives,'new_tests':2,'compiled_controls':len(receipts),'new_controls':0 if positive_only else len(candidate_declarations(originals)),
        'retained_public_terminal_loop_controls':0 if positive_only else 13,'controls':receipts,'root_inputs_unchanged':True,'source_copies_restored':True,
        'root_lock_sha256':codecs.sha(ROOT/'engine/Cargo.lock'),'libraries':len(provenance),'patches':sum(len(r['patches'])for r in provenance),
        'resampling_source_files_unchanged':True,'tiny_player_sources_per_profile':12,'cached257_cycle_players_per_profile':6,'uncached7_cycle_players_per_profile':6,'uncached257_cycle_quota_rejections_per_profile':2,'continuous_player_sources_per_profile':2,
        'continuous_frames_per_source':22050,'scope':'Immutable cyclic source presentation separates track duration from PCM extent;integer rational media clock and strict finite/continuous EOF;real File/HTTP players.No live loop cutover,quota renewal,loading intents or shippingcodec adoption.'}
    (work/'summary.json').write_text(json.dumps(summary,indent=2)+'\n');print(json.dumps(summary,indent=2),flush=True)


if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--scope',choices=['native','candidate'],required=True);p.add_argument('--output',type=Path,required=True)
    p.add_argument('--target-dir',type=Path,required=True);p.add_argument('--rubato-cargo-home',type=Path,required=True);p.add_argument('--positive-only',action='store_true');a=p.parse_args()
    if a.scope=='native':native(a.output.resolve(),a.target_dir.resolve(),a.positive_only)
    else:candidate(a.output.resolve(),a.target_dir.resolve(),a.rubato_cargo_home.resolve(),a.positive_only)
