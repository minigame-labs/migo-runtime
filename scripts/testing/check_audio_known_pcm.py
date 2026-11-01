#!/usr/bin/env python3
"""Declared PCM lengths must reserve once, yield, fill in steps and move output.

The old complete owner retained pages plus a second final buffer even when the
library had already declared the exact output. Reuse the same collector and
conversion kernel; unknown or late lengths retain bounded pages. Execute real
allocator/PCM/cancellation checks and rebuilt Rust assertion controls.
"""
from __future__ import annotations
import argparse
import importlib.util
import json
from pathlib import Path
import re

import check_audio_whole_file as whole

ROOT = whole.ROOT
SPEC = importlib.util.spec_from_file_location('known_pcm_benchmark', ROOT / 'scripts/bench/audio-whole-file-performance.py')
bench = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(bench)


def native_declarations(originals):
    rows = whole.native_declarations(originals) + whole.pcm.declarations(originals)
    path = 'decoder/pcm.rs'
    tests = 'decoder::pcm::tests::'
    bits = tests + 'declared_partitions_write_once_without_reallocation_and_preserve_every_bit'
    invalid = tests + 'declared_short_excess_and_unprepared_protocol_never_publishes_partial_pcm'
    def change(name, before, after, test):
        assert originals[path].count(before) == 1, (name, 'mutation anchor drift')
        rows.append((name, path, before, after, test))
    change('declared-reserve-hidden-in-copy',
        'return Err(internal("PCM reservation requires its own step"));', 'self.step()?;', bits)
    change('declared-reserve-ceiling-disabled', '!crate::limits::pcm_samples_within_budget(*expected)', 'false',
        tests + 'declared_reservation_rejects_oversized_lengths_before_any_heap_request')
    change('declared-reserve-initializes-complete-output', 'pages.push_back(output);',
        'output.resize(*expected, 0.0); pages.push_back(output);', bits)
    change('declared-reserve-repeats-next-step', '*reserved = true;', '*reserved = false;', bits)
    change('declared-output-budget-disabled', 'expected.is_some_and(|limit| needed.is_none_or(|count| count > limit))', 'false', invalid)
    change('declared-short-output-published', '!reserved || samples != expected', '!reserved', invalid)
    change('declared-result-cloned', 'self.phase = Phase::Complete(output);', 'self.phase = Phase::Complete(output.clone());', bits)
    change('declared-copy-budget-expanded', 'const STEP_SAMPLES: usize = 8192;', 'const STEP_SAMPLES: usize = 16384;', invalid)
    change('declared-short-failure-retains-storage',
        'if !reserved || samples != expected {', 'if !reserved || samples != expected { std::mem::forget(pages);', invalid)
    assert len(rows) == 34
    return rows


def candidate_declarations(original):
    rows = whole.declarations(original)
    tests = 'whole_file_candidate::known_tests::'
    exact = tests + 'known_wave_mp3_and_m4a_reserve_once_then_copy_complete_pcm_without_allocation'
    def change(name, before, after, test):
        assert original.count(before) == 1, (name, 'mutation anchor drift')
        rows.append((name, 'whole_file.rs', before, after, test))
    change('whole-declared-selection-disabled', 'PcmCollector::with_expected_samples(Some(expected))', 'PcmCollector::new()', exact)
    change('whole-declared-reserve-combines-next-operation', 'owner.collector.step()?;',
        'owner.collector.step()?; self.phase = Phase::Decode(owner); return self.advance(cancel);', exact)
    change('whole-late-declaration-discards-prefix', 'owner.expected_samples.is_none() && owner.samples == 0',
        'owner.expected_samples.is_none()', tests + 'unknown_and_late_lengths_keep_collected_pages_and_bounded_finalization')
    change('whole-cancellation-skips-pending-reservation', 'if cancel.load(Ordering::Acquire) {',
        'if cancel.load(Ordering::Acquire) && !matches!(&self.phase, Phase::Decode(owner) if owner.collector.needs_reservation()) {',
        tests + 'reservation_cancellation_before_after_and_during_copy_retires_the_owned_job')
    assert len(rows) == 21
    return rows


def native(work, target, preflight):
    extras = whole.extra_inputs() + [ROOT / ('scripts/bench/' + name) for name in
        ['audio-whole-file-performance.py', 'audio-codec-performance.py', 'audio-codec-startup.py']]
    package, env, frozen = whole.support.clock.prepare(work, target, extras)
    names = ['decoder/pcm.rs', 'decoder/mp3/job.rs', 'decoder/mod.rs']
    originals = {name: (package / 'src' / name).read_text() for name in names}
    rows = native_declarations(originals)
    binary = whole.support.public.build_native(package, work, env, 'positive')
    log = work / 'positive-tests.log'
    assert whole.support.clock.common.run([str(binary)], log, package, env, timeout=300) == 0, log
    assert re.search(r'test result: ok\. 397 passed; 0 failed; 2 ignored;', log.read_text())
    whole.support.program(work, 'positive', 'native')
    receipts = whole.support.faults(work, package, env, originals, [] if preflight else rows,
        lambda name: whole.support.public.build_native(package, work, env, name), package / 'src', 'native')
    whole.verify_inputs(work, frozen)
    assert all((package / 'src' / name).read_text() == text for name, text in originals.items())
    summary = dict(passed=397,ignored=2,compiled_controls=len(receipts),controls=receipts,preflight=preflight,
        root_inputs_unchanged=True,source_copies_restored=True)
    (work / 'summary.json').write_text(json.dumps(summary, indent=2) + '\n')
    print('native known-PCM complete',summary['passed'],len(receipts),flush=True)


def candidate(work, target, rubato, preflight):
    package, env, frozen, libraries, resampling = bench.prepare(work, target, rubato)
    originals = {'whole_file.rs': (work / 'source/whole_file.rs').read_text()}
    rows = candidate_declarations(originals['whole_file.rs'])
    profiles = {}
    for profile in (['debug'] if preflight else ['release','debug']):
        name = 'positive-' + profile
        binary = whole.build_candidate(package, work, env, name, release=profile == 'release')
        log = work / (name + '-tests.log')
        assert whole.support.clock.common.run([str(binary)], log, package, env, timeout=300) == 0, log
        assert re.search(r'test result: ok\. 430 passed; 0 failed; 2 ignored;', log.read_text())
        profiles[profile] = dict(passed=430,ignored=2)
        whole.support.program(work, name, 'native')
    receipts = whole.support.faults(work, package, env, originals, [] if preflight else rows,
        lambda name: whole.build_candidate(package, work, env, name), work / 'source', 'native')
    whole.verify_inputs(work, frozen)
    assert all(whole.support.codecs.sha(work / p) == h for p,h in json.loads((work / 'retained-sources.json').read_text()).items())
    for row in json.loads((work / 'library-source-provenance.json').read_text()):
        assert all(whole.support.codecs.sha(libraries[row['crate']] / p) == h for p,h in row['source_files'].items())
    for name,row in resampling.items():
        assert all(whole.support.codecs.sha(work / 'resampling-vendor' / f"{name}-{row['version']}" / p) == h for p,h in row['source_files'].items())
    summary = dict(profiles=profiles,compiled_controls=len(receipts),controls=receipts,preflight=preflight,
        root_inputs_unchanged=True,source_copies_restored=True,product_codec_switched=False)
    (work / 'summary.json').write_text(json.dumps(summary, indent=2) + '\n')
    print('candidate known-PCM complete',profiles,len(receipts),flush=True)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--scope',choices=['native','candidate'],required=True)
    parser.add_argument('--output',type=Path,required=True)
    parser.add_argument('--target-dir',type=Path,required=True)
    parser.add_argument('--rubato-cargo-home',type=Path)
    parser.add_argument('--preflight',action='store_true')
    args = parser.parse_args()
    if args.scope == 'native':native(args.output.resolve(),args.target_dir.resolve(),args.preflight)
    else:
        assert args.rubato_cargo_home is not None
        candidate(args.output.resolve(),args.target_dir.resolve(),args.rubato_cargo_home.resolve(),args.preflight)
