#!/usr/bin/env python3
"""Reject using a decoded PCM prefix or rounded output length as track duration.

An indexed track can know its full duration while the player's buffer is short.
Keep the two cursors separate, preserve rational phase, use library-owned trims,
and rebuild actual faults in metadata publication, seeks, EOF and precision.
The optional candidate metadata does not implement global DSP seek phase.
"""
from __future__ import annotations
import argparse
import json
from pathlib import Path
import re
import shutil
import subprocess
import tomllib

import check_audio_stream_revisions as revisions
import check_audio_forward_seek as forward
import check_audio_mp4_file as file
ROOT=file.ROOT
codecs=file.codecs


def native_controls(originals):
    rows=[];prefix='inner_audio::stream_timeline_tests::'
    def scoped(name,path,signature,before,after,test):
        s=originals[path];a=s.index(signature);b=s.index('\n    }',a)+len('\n    }');body=s[a:b]
        assert body.count(before)==1,(name,'anchor drift')
        rows.append((name,path,body,body.replace(before,after),prefix+test))
    math='rational_positions_keep_phase_and_large_frame_precision'
    valid='timeline_validates_formats_overflow_and_source_immutability'
    clock='streaming.rs';player='inner_audio.rs'
    new='    pub(crate) fn new(\n        frames: u64,'
    position='    pub(crate) fn position('
    scoped('timeline-rounds-duration-down',clock,new,'.div_ceil(u128::from(input_rate))',' / u128::from(input_rate)',math)
    scoped('timeline-drops-rational-phase',clock,position,'phase: (scaled % u128::from(self.input_rate)) as u32','phase: 0',math)
    scoped('timeline-uses-float-for-large-position',clock,position,'frame: (scaled / u128::from(self.input_rate)) as u64','frame: (frame as f64 * self.output_rate as f64 / self.input_rate as f64) as u64',math)
    scoped('timeline-admits-empty-source',clock,new,'if frames == 0\n            || ','if ',valid)
    scoped('timeline-admits-out-of-range-position',clock,position,'if frame > self.frames {','if false {',math)
    scoped('timeline-allows-changing-source',clock,'    pub(crate) fn bind_timeline(','|| *self.timeline.get_or_init(|| timeline) != timeline','|| { let _ = self.timeline.get_or_init(|| timeline); false }',valid)
    scoped('timeline-allows-binding-retired-source',clock,'    pub(crate) fn bind_timeline(','self.is_cancelled() ||','false ||',valid)
    duration='exact_duration_is_available_before_pcm_and_survives_output_rounding'
    scoped('duration-replaced-with-buffer-end',player,'    fn update_stream_duration(','self.shared.set_source_duration(timeline);','self.shared.set_duration_frames(end);',duration)
    scoped('source-format-not-checked',player,'    fn update_stream_duration(','timeline.format() != (self.shared.sample_rate(), self.shared.channels())','false','ready_format_is_checked_after_revision_filtering')
    scoped('excess-pcm-admitted',player,'    fn update_stream_duration(','end > timeline.output_frames()','false','declared_duration_rejects_short_eof_and_excess_pcm')
    scoped('short-eof-admitted',player,'    pub fn poll_stream(','self.stream_decoded_end() != t.output_frames()','false','declared_duration_rejects_short_eof_and_excess_pcm')
    scoped('accepted-revision-forgets-total-duration',player,'    pub(crate) fn accept_stream_revision(','self.shared.set_source_duration(timeline);','self.shared.set_duration_frames(target_frame);','accepted_revision_keeps_total_duration_and_waits_for_its_tail')
    scoped('accepted-target-escapes-total-duration',player,'    pub(crate) fn accept_stream_revision(','|| target_frame > t.output_frames()','|| false','accepted_revision_keeps_total_duration_and_waits_for_its_tail')
    scoped('new-source-keeps-old-exact-duration',player,'    pub fn set_duration_frames(','        self.source_duration_bits.store(0, Ordering::Relaxed);','','new_source_retires_exact_duration_and_old_source_cannot_rebind')
    scoped('duration-uses-rounded-output-time',player,'    pub fn duration(','if exact != 0 {','if false {',duration)
    scoped('ended-time-exceeds-source-duration',player,'    pub fn current_time(','if exact != 0 {','if false {',duration)
    seeking='known_duration_does_not_finish_forward_seek_before_fresh_pcm'
    scoped('forward-seek-clamped-to-short-buffer',player,'    pub fn seek(','target_frame >= self.stream_decoded_end()','target_frame >= self.shared.duration_frames()',seeking)
    scoped('seeked-uses-total-duration-as-buffer-end',player,'    pub fn poll_stream(','target < self.stream_decoded_end()','target < self.shared.duration_frames()',seeking)
    scoped('discard-cursor-uses-total-duration',player,"    fn seek_stream_prefix<'a>(",'let base = self.stream_decoded_end();','let base = self.shared.duration_frames();',seeking)
    scoped('seek-duration-stops-before-final-output-frame',player,'    pub fn seek(','if time >= t.seconds() {','if false {',duration)
    cache_test='cache_handoff_and_reload_keep_the_original_media_duration'
    scoped('clock-not-retained-through-transport-eof',player,'    fn update_stream_duration(',
           '            self.stream_timeline = Some(timeline);','',cache_test)
    scoped('cached-load-forgets-media-clock',player,'    pub(crate) fn complete_cached_load(',
           'if let Some(timeline) = self.stream_timeline {','if let Some(timeline) = None::<crate::streaming::StreamTimeline> {',cache_test)
    scoped('cached-backing-forgets-media-clock',player,'    pub fn attach_cached_backing(',
           'if let Some(timeline) = self.stream_timeline {','if let Some(timeline) = None::<crate::streaming::StreamTimeline> {',cache_test)
    scoped('cache-drops-source-timeline','cache.rs','    fn new(',
           '            source_timeline,','            source_timeline: None,',cache_test)
    scoped('cache-admits-incoherent-timeline','cache.rs','    pub(crate) fn insert_with_source(',
           'if timeline.is_some_and','if false && timeline.is_some_and','cache_rejects_clock_that_does_not_describe_owned_pcm')
    source=originals['audio_thread.rs'];a=source.index('fn service_players(');b=source.index('\n}',a)+2;body=source[a:b]
    before='audio_cache.insert_with_source(url, audio, permit, timeline)';assert body.count(before)==1
    rows.append(('audio-loop-loses-cache-timeline','audio_thread.rs',body,body.replace(before,'audio_cache.insert_with_source(url, audio, permit, None)'),
                 'audio_thread::service_players_caches_and_attaches_the_source_timeline'))
    return rows


def native(work,target,positive_only):
    clock=revisions.clock
    package,env,frozen=clock.prepare(work,target,[Path(__file__).resolve(),ROOT/'scripts/testing/check_audio_forward_seek.py',ROOT/'scripts/testing/check_audio_stream_revisions.py',ROOT/'scripts/testing/check_gitignore_contract.py'])
    originals={name:(package/'src'/name).read_text()for name in ['streaming.rs','inner_audio.rs','cache.rs','audio_thread.rs']}
    rows=native_controls(originals)+revisions.controls(originals)
    rows += [(name,'inner_audio.rs',before,after,test)for name,before,after,test in forward.controls(originals['inner_audio.rs'])]
    binary=clock.build(package,work,env,'positive');log=work/'positive-tests.log'
    assert clock.common.run([str(binary)],log,package,env)==0
    match=re.search(r'test result: ok\. (\d+) passed; 0 failed; (\d+) ignored;',log.read_text());assert match
    assert int(match[1])==295,(match[1],'count drift')
    available=set(re.findall(r'^test (\S+) \.\.\.',log.read_text(),re.M));assert all(test in available for *_,test in rows)
    receipts=[]
    for name,path,before,after,test in ([]if positive_only else rows):
        source=package/'src'/path;assert originals[path].count(before)==1,(name,'ambiguous mutation')
        try:
            source.write_text(originals[path].replace(before,after));mutant=clock.build(package,work,env,name);out=work/f'{name}-test.log'
            status=clock.common.run([str(mutant),'--exact',test,'--nocapture'],out,package,env,timeout=30);text=out.read_text()
            assert status==101 and 'panicked at'in text and '1 failed'in text,(name,'requires executed assertion')
            receipts.append({'name':name,'test':test,'exit':status,'binary_sha256':clock.common.sha(mutant)})
        finally:source.write_text(originals[path])
        print('compiled native timeline control',name,': assertion rejected',flush=True)
    assert all(clock.common.sha(ROOT/p)==h for p,h in frozen.items()),'Root changed'
    audio=ROOT/'engine/crates/audio/src';assert all(p.read_bytes()==(package/'src'/p.relative_to(audio)).read_bytes()for p in audio.rglob('*.rs')),'copy not restored'
    summary={'positive_tests':int(match[1]),'ignored_tests':int(match[2]),'new_timeline_tests':11,'new_controls':len(native_controls(originals))if not positive_only else 0,'compiled_controls':len(receipts),'controls':receipts,'root_inputs_unchanged':True,'source_copies_restored':True,'root_lock_sha256':clock.common.sha(ROOT/'engine/Cargo.lock'),'scope':'Authoritative playable duration,independent decoded end,rational frame phase,exact terminal seconds,source metadata lifetime;global DSP seek phase/product cutover/device remain open.'}
    (work/'summary.json').write_text(json.dumps(summary,indent=2)+'\n');print(json.dumps(summary,indent=2),flush=True)


def candidate_controls(originals):
    rows=[];prefix='timed_m4a::tests::'
    def whole(name,path,before,after,test):
        assert originals[path].count(before)==1,(name,'anchor drift');rows.append((name,path,before,after,prefix+test))
    first='real_file_and_http_publish_authoritative_clock_before_first_ready'
    whole('decoder-timeline-not-published','streaming_transport.rs','                        cancel.bind_timeline(timeline)?;','','real_file_and_http_publish_authoritative_clock_before_first_ready')
    whole('decoder-timeline-only-published-at-eof','streaming_transport.rs','if let Some(timeline) = decoder.timeline()? {','if let Some(timeline) = decoder.timeline()?.filter(|_| eof) {',first)
    whole('m4a-trim-applied-twice','timed_m4a.rs','StreamTimeline::new(frames, rate.get(), output_rate, channels)','StreamTimeline::new(frames.saturating_sub(u64::from(track.delay.unwrap_or(0))), rate.get(), output_rate, channels)','library_presentation_frames_include_trims_and_scaled_media_clocks_once')
    whole('m4a-media-timebase-not-validated','timed_m4a.rs','if track.time_base != Some(TimeBase::from_recip(rate))','if false && track.time_base != Some(TimeBase::from_recip(rate))','missing_or_incoherent_library_clocks_are_rejected')
    return rows


def candidate(work,target,rubato,positive_only):
    env,frozen,_,libraries,resampling=file.prepare(work,target,rubato)
    env.update(CC='/usr/bin/clang',CXX='/usr/bin/clang++');env.pop('MIGO_AUDIO_CHECKS_BLESS',None)
    lib=work/'probe/src/lib.rs';fixtures=ROOT/'scripts/testing/fixtures/audio-codec-probe'
    for name in ['timed_m4a','decoder_seek_contracts']:
        path=work/'source'/(name+'.rs');shutil.copy2(fixtures/path.name,path)
        prefix='#[cfg(test)] 'if name.endswith('contracts')else''
        lib.write_text(lib.read_text()+f'\n{prefix}#[path={json.dumps(str(path))}] mod {name};\n')
    shutil.copy2(fixtures/'timeline_contracts.rs',work/'source/timeline_contracts.rs')
    extras=list((ROOT/'scripts/testing').glob('check_audio_*.py'))+[ROOT/'scripts/testing/check_gitignore_contract.py',ROOT/'scripts/testing/check_http_range_source.py']
    for path in extras:
        rel=str(path.relative_to(ROOT));frozen[rel]=codecs.sha(path);saved=work/'source-inputs'/rel;saved.parent.mkdir(parents=True,exist_ok=True);shutil.copy2(path,saved)
    (work/'source-inputs.json').write_text(json.dumps(frozen,indent=2)+'\n')
    retained={str(p.relative_to(work)):codecs.sha(p)for tree in [work/'source',work/'probe/src']for p in tree.rglob('*.rs')}
    (work/'retained-sources.json').write_text(json.dumps(retained,indent=2)+'\n')
    codecs.run(['cargo','metadata','--offline','--format-version','1'],work/'metadata.jsonl',cwd=work/'probe',env=env)
    identity=lambda p:(p['name'],p['version'],p.get('source'),p.get('checksum'))
    pinned=tomllib.loads((ROOT/'engine/Cargo.lock').read_text())['package'];known={p['name']for p in pinned};identities={identity(p)for p in pinned}
    assert all(p['name']not in known or identity(p)in identities for p in tomllib.loads((work/'probe/Cargo.lock').read_text())['package']),'dependency identity drift'
    codecs.run(['cargo','check','--offline','--locked','--lib'],work/'normal-library-check.log',cwd=work/'probe',env=env)
    originals={name:(work/'source'/name).read_text()for name in ['streaming_transport.rs','timed_m4a.rs']};rows=candidate_controls(originals)
    positives={}
    for release in [True,False]:
        profile='release'if release else'debug';binary=file.dsp.source.build(work,env,'positive-'+profile,release=release)
        for prefix,count,label in [('timed_m4a::tests::',4,'timeline'),('inner_audio::stream_timeline_tests::',10,'player')]:
            log=work/f'positive-{profile}-{label}.log';codecs.run([str(binary),prefix,'--nocapture','--test-threads=1'],log,cwd=work/'probe',env=env);assert f'{count} passed; 0 failed'in log.read_text()
        positives[profile]=14
    receipts=[]
    for name,path,before,after,test in ([]if positive_only else rows):
        source=work/'source'/path
        try:
            source.write_text(originals[path].replace(before,after));binary=file.dsp.source.build(work,env,name,release=False,clean=False);log=work/f'{name}-test.log'
            with log.open('w')as out:r=subprocess.run([str(binary),'--exact',test,'--nocapture'],cwd=work/'probe',env=env,stdout=out,stderr=subprocess.STDOUT,timeout=30)
            text=log.read_text();assert r.returncode==101 and 'panicked at'in text and '1 failed'in text,(name,'requires executed assertion')
            receipts.append({'name':name,'test':test,'binary_sha256':codecs.sha(binary),'exit':r.returncode})
        finally:source.write_text(originals[path])
        print('compiled candidate timeline control',name,': assertion rejected',flush=True)
    assert all(codecs.sha(ROOT/p)==h for p,h in frozen.items()),'Root changed'
    assert all(codecs.sha(work/p)==h for p,h in retained.items()),'copy not restored'
    provenance=json.loads((work/'library-source-provenance.json').read_text())
    for row in provenance:assert file.dsp.source.index_gate.timeline.source_tree(libraries[row['crate']])==row['source_files']
    for name,row in resampling.items():assert all(codecs.sha(work/'resampling-vendor'/f"{name}-{row['version']}"/p)==h for p,h in row['source_files'].items())
    summary={'positive_tests_per_profile':positives,'new_tests':4,'library_track_fixtures':14,'exact_clock_cases':28,'actual_file_http_pcm_comparisons':16,'compiled_controls':len(receipts),'controls':receipts,'libraries':len(provenance),'patches':sum(len(r['patches'])for r in provenance),'root_inputs_unchanged':True,'source_copies_restored':True,'root_lock_sha256':codecs.sha(ROOT/'engine/Cargo.lock'),'scope':'Opt-in pinned AAC playable timeline before Ready,scaled/edited clocks,exact player duration and forward-seek wait;no global DSP seek phase or product cutover.'}
    (work/'summary.json').write_text(json.dumps(summary,indent=2)+'\n');print(json.dumps(summary,indent=2),flush=True)


if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--scope',choices=['native','candidate'],required=True);p.add_argument('--output',type=Path,required=True);p.add_argument('--target-dir',type=Path,required=True);p.add_argument('--rubato-cargo-home',type=Path);p.add_argument('--positive-only',action='store_true');a=p.parse_args()
    if a.scope=='native':native(a.output.resolve(),a.target_dir.resolve(),a.positive_only)
    else:
        assert a.rubato_cargo_home is not None;candidate(a.output.resolve(),a.target_dir.resolve(),a.rubato_cargo_home.resolve(),a.positive_only)
