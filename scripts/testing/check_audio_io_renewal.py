#!/usr/bin/env python3
"""Renew bounded indexed media work from real, source-bound output consumption.

A lifetime 64-reposition cap eventually stops valid uncached loops. Resetting
it on seek, decoded PCM or elapsed time instead lets an idle source amplify IO.
Retain the burst and all transport/index limits; prove real mix accounting,
pause backpressure, bounded credit and fail-closed ownership with fresh faults.
"""
from __future__ import annotations
import argparse
import json
from pathlib import Path
import re
import tomllib
import check_audio_cyclic_seek as seek

ROOT = seek.ROOT
codecs = seek.codecs
public = seek.public
phase = seek.phase
clock = seek.clock


def native_declarations(originals):
    rows = []
    prefix = 'inner_audio::live_loop_tests::cyclic_seek::'
    mixed = prefix+'source_credit_counts_only_real_mixed_output_frames'
    stalled = prefix+'source_credit_excludes_stalled_tail_and_retired_source'
    counter = 'streaming::rendered_progress_tests::consumed_credit_saturates_and_retirement_cannot_mint_more'
    def whole(name, path, before, after, test):
        assert originals[path].count(before) == 1, (name, 'mutation anchor drift')
        rows.append((name, path, before, after, test))
    whole('io-credit-omits-real-mix', 'inner_audio.rs',
          '            source.record_rendered_frames(frames_rendered);', '', mixed)
    whole('io-credit-counts-samples-instead-of-frames', 'inner_audio.rs',
          'source.record_rendered_frames(frames_rendered)',
          'source.record_rendered_frames(frames_rendered * self.output_channels as usize)', mixed)
    whole('io-credit-counts-unrendered-stall-tail', 'inner_audio.rs',
          'self.accumulate_time_update(frame_idx);',
          'self.accumulate_time_update(output_frames);', stalled)
    whole('io-credit-mints-after-retirement', 'streaming.rs',
          'if frames != 0 && !self.is_cancelled() {', 'if frames != 0 {', counter)
    whole('io-credit-wraps-into-new-epoch', 'streaming.rs',
          'old.saturating_add(frames as u64)', 'old.wrapping_add(frames as u64)', counter)
    return rows


def candidate_declarations(originals):
    rows = []
    prefix = 'isomp4_source::quota_tests::'
    bound = prefix+'consumed_frames_renew_one_request_each_and_surplus_is_not_banked'
    identity = prefix+'first_observation_zero_limit_cancellation_and_owner_are_fail_closed'
    heap = prefix+'resumed_work_remains_bounded_across_many_observations_without_heap_activity'
    def whole(name, before, after, test):
        assert originals['isomp4_source.rs'].count(before) == 1, (name, 'mutation anchor drift')
        rows.append((name, 'isomp4_source.rs', before, after, test))
    whole('io-quota-never-renews-consumed-credit',
          '.saturating_add(rendered.saturating_sub(self.observed))', '', bound)
    whole('io-quota-banks-unbounded-surplus', '.min(self.burst)', '', bound)
    whole('io-quota-reuses-old-consumption', '        self.observed = rendered;', '', heap)
    whole('io-quota-trusts-foreign-consumer',
          'if owner.as_ptr() != Arc::as_ptr(state) {', 'if false {', identity)
    whole('io-quota-revives-cancelled-owner', 'if state.is_cancelled() {', 'if false {', identity)
    whole('io-quota-revives-exhausted-owner', 'if self.failed {', 'if false {', bound)
    whole('io-quota-accepts-zero-burst',
          'self\n            .remaining\n            .checked_sub(1)',
          'Some(self.remaining.saturating_sub(1))', identity)
    whole('io-quota-allocates-per-observation',
          '    fn charge(&mut self, state: &Arc<StreamingState>) -> Result<(), Error> {',
          '    fn charge(&mut self, state: &Arc<StreamingState>) -> Result<(), Error> {\n        std::hint::black_box(vec![0_u8;64]);', heap)
    whole('io-quota-counter-wraps',
          'self\n            .attempts\n            .checked_add(1)',
          'Some(self.attempts.wrapping_add(1))', identity)
    before = '            source.record_rendered_frames(frames_rendered);'
    assert originals['inner_audio.rs'].count(before) == 1
    rows.append(('io-renewal-disconnected-from-real-player', 'inner_audio.rs', before, '',
                 'timed_m4a::loop_tests::consumed_uncached_file_http_cycles_renew_burst_without_changing_pcm'))
    return rows


def program(work, name, scope, prune=False):
    """Record the executed program before recycling its generated copy."""
    binary = work/(name+'-tests')
    assert binary.parent == work and not binary.is_symlink()
    artifacts = []
    for line in (work/(name+'-build.jsonl')).read_text().splitlines():
        try:
            row = json.loads(line)
        except ValueError:
            continue
        if (row.get('reason') == 'compiler-artifact' and row.get('executable')
                and row['profile']['test']
                and row['target']['name'] == ('audio' if scope == 'native' else 'migo_services')):
            artifacts.append(row)
    assert len(artifacts) == 1 and artifacts[0]['fresh'] is False, (name, 'must rebuild')
    result = {'path': str(binary), 'name': name, 'scope': scope, 'bytes': binary.stat().st_size,
              'sha256': codecs.sha(binary), 'fresh': False, 'pruned_after_execution': prune}
    ledger = work/'executed-programs.json'
    records = json.loads(ledger.read_text()) if ledger.exists() else []
    assert all(r['name'] != name for r in records)
    records.append(result)
    ledger.write_text(json.dumps(records, indent=2)+'\n')
    if prune:
        binary.unlink()
    return result


def faults(work, package, env, originals, rows, build, source_dir, scope):
    receipts = []
    for row in rows:
        receipt, = public.faults(work, package, env, originals, [row], build, source_dir)
        record = program(work, row[0], scope, prune=True)
        assert record['sha256'] == receipt['binary_sha256']
        receipts.append(receipt)
        (work/'completed-controls.json').write_text(json.dumps(receipts, indent=2)+'\n')
    return receipts


def native(work, target, positive_only):
    package, env, frozen = clock.prepare(work, target,
        list((ROOT/'scripts/testing').glob('check_audio_*.py'))+[ROOT/'scripts/testing/check_gitignore_contract.py'])
    originals = {n: (package/'src'/n).read_text() for n in ['streaming.rs', 'inner_audio.rs', 'stream_control.rs']}
    rows = native_declarations(originals)+seek.native_rows(originals)
    assert len(rows) == 103 and len({r[0] for r in rows}) == 103
    binary = public.build_native(package, work, env, 'positive')
    log = work/'positive-tests.log'
    assert clock.common.run([str(binary)], log, package, env) == 0
    assert re.search(r'test result: ok\. 337 passed; 0 failed; 2 ignored;', log.read_text())
    program(work, 'positive', 'native')
    receipts = faults(work, package, env, originals, [] if positive_only else rows,
        lambda name: public.build_native(package, work, env, name), package/'src', 'native')
    assert all(codecs.sha(ROOT/p) == h and codecs.sha(work/'source-inputs'/p) == h for p,h in frozen.items())
    audio = ROOT/'engine/crates/audio/src'
    assert all(p.read_bytes() == (package/'src'/p.relative_to(audio)).read_bytes() for p in audio.rglob('*.rs'))
    summary = {'positive_tests':337, 'ignored_tests':2, 'compiled_controls':len(receipts),
        'new_controls':0 if positive_only else 5, 'retained_controls':0 if positive_only else 98,
        'controls':receipts, 'root_inputs_unchanged':True, 'source_copies_restored':True,
        'root_lock_sha256':codecs.sha(ROOT/'engine/Cargo.lock')}
    (work/'summary.json').write_text(json.dumps(summary, indent=2)+'\n')
    print('native completed', summary['positive_tests'], len(receipts), flush=True)


def candidate(work, target, rubato, positive_only):
    env, frozen, retained, libraries, resampling = seek.live.prepare(work, target, rubato)
    package = work/'probe'
    codecs.run(['cargo','metadata','--offline','--format-version','1'], work/'metadata.jsonl', cwd=package, env=env)
    identity = lambda p:(p['name'],p['version'],p.get('source'),p.get('checksum'))
    locked = tomllib.loads((ROOT/'engine/Cargo.lock').read_text())['package']
    known = {p['name'] for p in locked}; pins = set(map(identity,locked))
    assert all(p['name'] not in known or identity(p) in pins for p in tomllib.loads((package/'Cargo.lock').read_text())['package'])
    names = ['timed_m4a.rs','resampled_streaming.rs','indexed_seek.rs','streaming_transport.rs',
             'producer_control.rs','isomp4_source.rs','inner_audio.rs','stream_control.rs','off_worker.rs']
    originals = {n:(work/'source'/n).read_text() for n in names}
    rows = candidate_declarations(originals)+seek.candidate_rows(originals)
    assert len(rows) == 114 and len({r[0] for r in rows}) == 114
    codecs.run(['cargo','check','--offline','--locked','--lib'],work/'normal-library-check.log',cwd=package,env=env)
    positives = phase.positive_profiles(work,env,[('cyclic','timed_m4a::loop_tests::',11),
        ('cyclic-clock','resampled_streaming::loop_clock_tests::',2),('public-source','public_control_contracts::',3),
        ('cutover-clock','streaming::cutover_tests::',6),('output-cutover','timed_m4a::cutover_tests::',5),
        ('public-loop','timed_m4a::live_loop_tests::',8),('quota','isomp4_source::quota_tests::',3),
        ('consumption','streaming::rendered_progress_tests::',1)])
    assert positives == {'release':299,'debug':296}, positives
    for profile in ['release','debug']: program(work,'positive-'+profile,'candidate')
    receipts = faults(work,package,env,originals,[] if positive_only else rows,
        lambda name:phase.file.dsp.source.build(work,env,name,release=name=='paused-player-drains-past-prefetch',clean=False),work/'source','candidate')
    for row in receipts: row['profile'] = 'release' if row['name']=='paused-player-drains-past-prefetch' else 'debug'
    assert all(codecs.sha(ROOT/p)==h and codecs.sha(work/'source-inputs'/p)==h for p,h in frozen.items())
    assert all(codecs.sha(work/p)==h for p,h in retained.items())
    provenance=json.loads((work/'library-source-provenance.json').read_text())
    for row in provenance:
        assert phase.file.dsp.source.index_gate.timeline.source_tree(libraries[row['crate']])==row['source_files']
    for name,row in resampling.items():
        assert all(codecs.sha(work/'resampling-vendor'/f"{name}-{row['version']}"/p)==h for p,h in row['source_files'].items())
    summary={'positive_tests_per_profile':positives,'compiled_controls':len(receipts),
        'new_controls':0 if positive_only else 10,'retained_controls':0 if positive_only else 104,
        'controls':receipts,'root_inputs_unchanged':True,'source_copies_restored':True,
        'root_lock_sha256':codecs.sha(ROOT/'engine/Cargo.lock'),'libraries':len(provenance),
        'patches':sum(len(r['patches']) for r in provenance),'resampling_source_files_unchanged':True,
        'new_file_http_finite_players_per_profile':6,'new_continuous_paused_players_per_profile':2,
        'scope':'Source-bound real mixed output renews one request per frame,burst64/no banking. Tiny preroll within startup burst remains required; original index/transport/deadline/retirement budgets retained. Candidate codec only.'}
    (work/'summary.json').write_text(json.dumps(summary,indent=2)+'\n')
    print('candidate completed',positives,len(receipts),flush=True)


if __name__ == '__main__':
    p=argparse.ArgumentParser(description=__doc__)
    p.add_argument('--scope',choices=['native','candidate'],required=True)
    p.add_argument('--output',type=Path,required=True);p.add_argument('--target-dir',type=Path,required=True)
    p.add_argument('--rubato-cargo-home',type=Path,required=True);p.add_argument('--positive-only',action='store_true')
    a=p.parse_args()
    if a.scope=='native':native(a.output.resolve(),a.target_dir.resolve(),a.positive_only)
    else:candidate(a.output.resolve(),a.target_dir.resolve(),a.rubato_cargo_home.resolve(),a.positive_only)
