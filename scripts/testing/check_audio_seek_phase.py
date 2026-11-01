#!/usr/bin/env python3
"""Prove clock-aligned AAC/Rubato seek, bounded preroll and source receipts.

The complete library render is the waveform oracle. Candidate-only; no product
codec selection, public control or platform/performance completion is claimed.
"""
from __future__ import annotations
import argparse
import json
from pathlib import Path
import re
import shutil
import subprocess
import tomllib

import check_audio_producer_controls as producer
import check_audio_stream_timeline as timeline

file = producer.file
codecs = file.codecs
ROOT = file.ROOT


def prepare(work, target, rubato):
    env, frozen, _, libraries, resampling = file.prepare(work, target, rubato)
    env.update(CC='/usr/bin/clang', CXX='/usr/bin/clang++')
    env.pop('MIGO_AUDIO_CHECKS_BLESS', None)
    lib = work/'probe/src/lib.rs'
    fixtures = ROOT/'scripts/testing/fixtures/audio-codec-probe'
    for name in ['producer_control', 'decoder_seek_contracts', 'source_seek_contracts', 'timed_m4a']:
        path = work/'source'/(name+'.rs')
        shutil.copy2(fixtures/path.name, path)
        cfg = '#[cfg(test)] ' if name.endswith('contracts') else ''
        lib.write_text(lib.read_text()+f'\n{cfg}#[path={json.dumps(str(path))}] mod {name};\n')
    for name in ['producer_contracts.rs', 'timeline_contracts.rs', 'phase_contracts.rs', 'phase_producer_contracts.rs', 'cyclic_seek_contracts.rs', 'loading_controls_contracts.rs']:
        shutil.copy2(fixtures/name, work/'source'/name)
    # Faults must mutate only the isolated player, never the user's Root tree.
    for name in ['inner_audio.rs', 'loading_control_tests.rs', 'stream_revision_tests.rs', 'seek_tests.rs', 'forward_seek_tests.rs', 'stream_timeline_tests.rs', 'public_control_tests.rs', 'cyclic_clock_tests.rs', 'live_loop_tests.rs', 'cyclic_seek_tests.rs']:
        path = ROOT/'engine/crates/audio/src'/name
        saved = work/'source'/name
        shutil.copy2(path, saved)
        if name == 'inner_audio.rs':
            lib.write_text(lib.read_text().replace(json.dumps(str(path)), json.dumps(str(saved))))
    extras = list((ROOT/'scripts/testing').glob('check_audio_*.py'))
    extras += [ROOT/'scripts/testing/check_gitignore_contract.py', ROOT/'scripts/testing/check_http_range_source.py']
    for path in extras:
        rel = str(path.relative_to(ROOT))
        frozen[rel] = codecs.sha(path)
        saved = work/'source-inputs'/rel
        saved.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(path, saved)
    (work/'source-inputs.json').write_text(json.dumps(frozen, indent=2)+'\n')
    retained = {str(p.relative_to(work)): codecs.sha(p)
                for tree in [work/'source', work/'probe/src'] for p in tree.rglob('*.rs')}
    (work/'retained-sources.json').write_text(json.dumps(retained, indent=2)+'\n')
    return env, frozen, retained, libraries, resampling


def declarations(originals):
    rows = []
    phase = 'timed_m4a::phase_tests::'
    wave = phase+'clocked_seek_matches_whole_stream_output'
    def whole(name, path, before, after, test):
        assert originals[path].count(before) == 1, (name, 'mutation anchor drift')
        rows.append((name, path, before, after, test))
    whole('seek-restarts-at-target-without-history', 'timed_m4a.rs',
          'Timestamp::new(plan.input_origin as i64)', 'target', wave)
    whole('seek-forgets-filter-preroll', 'timed_m4a.rs',
          'target.saturating_sub(history) / period * period', 'target / period * period', wave)
    whole('seek-origin-not-on-rational-grid', 'timed_m4a.rs',
          'target.saturating_sub(history) / period * period', 'target.saturating_sub(history)', wave)
    whole('seek-publishes-floor-frame', 'timed_m4a.rs',
          'position.frame + u64::from(position.phase != 0)', 'position.frame', wave)
    whole('seek-clock-loses-large-integer-precision', 'timed_m4a.rs',
          'position.frame + u64::from(position.phase != 0)',
          '(target as f64 * output as f64 / input as f64).ceil() as u64',
          phase+'integer_seek_plan_bounds_history_and_keeps_the_global_output_boundary')
    whole('seek-receipt-reports-preroll-target', 'timed_m4a.rs',
          '        seeked.required_ts = target;', '', wave)
    whole('same-rate-seek-replays-the-prefix', 'timed_m4a.rs',
          'let input_origin = if input == output {\n        target',
          'let input_origin = if input == output {\n        0', wave)
    raw = phase+'invalid_origins_preserve_the_live_resampler_and_absolute_eof_is_exact'
    whole('resampler-forgets-absolute-input-origin', 'bandlimited.rs',
          '        self.input_frames = input_origin;', '        self.input_frames = 0;', raw)
    whole('resampler-forgets-absolute-output-origin', 'bandlimited.rs',
          '        self.output_frames = origin;', '        self.output_frames = 0;', raw)
    whole('resampler-publishes-preroll', 'bandlimited.rs',
          '        self.discard_until = output_start;', '        self.discard_until = 0;', wave)
    whole('resampler-accepts-fractional-origin', 'bandlimited.rs',
          'if !scaled.is_multiple_of(u128::from(self.input_rate)) {', 'if false {', raw)
    whole('invalid-origin-resets-live-filter', 'bandlimited.rs',
          '        let scaled = u128::from(input_origin) * u128::from(self.output_rate);',
          '        self.reset();\n        let scaled = u128::from(input_origin) * u128::from(self.output_rate);', raw)
    whole('resampler-rounds-large-eof-through-float', 'bandlimited.rs',
          '(u128::from(self.input_frames) * u128::from(self.output_rate))\n            .div_ceil(u128::from(self.input_rate))',
          '(self.input_frames as f64 * self.output_rate as f64 / self.input_rate as f64).ceil() as u128', raw)
    whole('clocked-seek-allocates', 'timed_m4a.rs',
          '        let plan =\n',
          '        std::hint::black_box(vec![0_u8;64]);\n        let plan =\n',
          phase+'cached_clocked_seek_and_preroll_have_no_steady_allocation')
    whole('receipt-uses-floor-output-position', 'producer_control.rs',
          'Ok(position.frame + u64::from(position.phase != 0))', 'Ok(position.frame)',
          'producer_control::clock_tests::clocked_receipt_uses_the_ceil_boundary_instead_of_the_input_frame')
    whole('receipt-accepts-arbitrary-host-mapping', 'producer_control.rs',
          'if self.source.timeline().is_some() && self.mapped_output()? != output_frame {',
          'if false && self.source.timeline().is_some() && self.mapped_output()? != output_frame {',
          'producer_control::clock_tests::mismatched_external_mapping_cannot_mutate_player_or_acknowledge')
    whole('accepted-traversal-forgets-output-origin', 'streaming_transport.rs',
          'publication.bind_timeline(timeline, start)?', 'publication.bind_timeline(timeline, 0)?',
          phase+'producer::file_http_clocked_receipts_keep_player_and_pcm_on_one_output_grid')
    whole('accepted-traversal-loses-its-clock', 'producer_control.rs',
          '                        output_start,', '                        output_start: None,',
          phase+'producer::file_http_clocked_receipts_keep_player_and_pcm_on_one_output_grid')
    whole('paused-publication-drains-past-prefetch', 'inner_audio.rs',
          'if self.stream_prefetch_full() {', 'if false && self.stream_prefetch_full() {',
          'isomp4_source_contracts::transport::local::local_player_keeps_pause_backpressure_pcm_and_exact_ended')
    return rows


def publication_declarations(original):
    rows = []
    prefix = 'streaming::timeline_publication_tests::'
    empty = prefix+'only_a_proven_output_endpoint_can_publish_an_empty_traversal'
    counts = prefix+'clocked_publication_rejects_short_excess_and_changed_format'
    reset = prefix+'a_new_revision_resets_its_length_and_invalid_binding_preserves_the_ledger'
    def scoped(name, signature, before, after, test):
        start = original.index(signature)
        end = original.index('\n    }', start)+len('\n    }')
        body = original[start:end]
        assert body.count(before) == 1, (name, 'mutation anchor drift')
        rows.append((name, 'streaming.rs', body, body.replace(before, after), test))
    finish = '    pub(crate) fn validate_finish('
    bind = '    pub(crate) fn bind_timeline(\n        &mut self,'
    scoped('known-end-still-requires-pcm', finish,
           '.map_or(self.total_samples > 0, |n| n == self.total_samples)',
           '.map_or(self.total_samples > 0, |_| self.total_samples > 0)', empty)
    scoped('unknown-empty-traversal-accepted', finish,
           '.map_or(self.total_samples > 0,', '.map_or(true,', empty)
    scoped('short-clocked-traversal-accepted', finish,
           '|n| n == self.total_samples', '|_| true', counts)
    scoped('excess-clocked-pcm-accepted', '    pub(crate) fn prepare_chunk(',
           '.is_some_and(|expected| total > expected)',
           '.is_some_and(|expected| false && total > expected)', counts)
    scoped('clocked-count-confuses-samples-and-frames', bind,
           '.checked_mul(u64::from(timeline.format().1))', '.checked_mul(1)', counts)
    scoped('revision-keeps-previous-length', '    pub(crate) fn begin(',
           '        self.expected_samples = None;', '', reset)
    scoped('output-end-binding-forgets-start', bind,
           '.checked_sub(output_start)', '.checked_sub(0)', empty)
    scoped('publication-format-rebound', bind,
           'self.format.is_some_and(|old| old != timeline.format())', 'false', counts)
    return rows


def native(work, target, positive_only):
    clock = timeline.revisions.clock
    package, env, frozen = clock.prepare(work, target,
        [Path(__file__).resolve(), ROOT/'scripts/testing/check_gitignore_contract.py',
         ROOT/'scripts/testing/check_audio_stream_revisions.py'])
    originals = {n: (package/'src'/n).read_text() for n in ['streaming.rs', 'inner_audio.rs']}
    rows = publication_declarations(originals['streaming.rs']) + timeline.revisions.controls(originals)
    binary = clock.build(package, work, env, 'positive')
    log = work/'positive-tests.log'
    assert clock.common.run([str(binary)], log, package, env) == 0
    match = re.search(r'test result: ok\. (\d+) passed; 0 failed; (\d+) ignored;', log.read_text())
    assert match and (int(match[1]), int(match[2])) == (295, 2)
    receipts = []
    for name, path, before, after, test in ([] if positive_only else rows):
        source = package/'src'/path
        assert originals[path].count(before) == 1, name
        try:
            source.write_text(originals[path].replace(before, after))
            mutant = clock.build(package, work, env, name)
            out = work/f'{name}-test.log'
            status = clock.common.run([str(mutant), '--exact', test, '--nocapture'], out, package, env, timeout=30)
            observed = out.read_text()
            assert status == 101 and 'panicked at' in observed and '1 failed' in observed, name
            receipts.append({'name': name, 'test': test, 'exit': status,
                             'binary_sha256': clock.common.sha(mutant)})
        finally:
            source.write_text(originals[path])
        print('compiled native publication fault', name, ': executed assertion rejected', flush=True)
    assert all(clock.common.sha(ROOT/p) == h and clock.common.sha(work/'source-inputs'/p) == h
               for p, h in frozen.items())
    audio = ROOT/'engine/crates/audio/src'
    assert all(p.read_bytes() == (package/'src'/p.relative_to(audio)).read_bytes() for p in audio.rglob('*.rs'))
    summary = {'positive_tests': 295, 'ignored_tests': 2, 'new_publication_tests': 3,
               'compiled_controls': len(receipts), 'controls': receipts,
               'root_inputs_unchanged': True, 'source_copies_restored': True,
               'root_lock_sha256': clock.common.sha(ROOT/'engine/Cargo.lock')}
    (work/'summary.json').write_text(json.dumps(summary, indent=2)+'\n')
    print(json.dumps(summary, indent=2), flush=True)


def positive_profiles(work, env, extra=()):
    package = work/'probe'
    checks = [
        ('phase', 'timed_m4a::phase_tests::', 7),
        ('receipt-clock', 'producer_control::clock_tests::', 2),
        ('timeline', 'timed_m4a::tests::', 4),
        ('actor', 'producer_control::tests::', 22),
        ('owner', 'off_worker::tests::', 4),
        ('source-seek', 'source_seek_contracts::', 10),
        ('decoder-seek', 'decoder_seek_contracts::', 9),
        ('file', 'isomp4_source_contracts::transport::local::', 11),
        ('package', 'isomp4_source_contracts::transport::local::package::', 8),
        ('directory', 'isomp4_source_contracts::transport::local::directory::', 5),
        ('setup', 'local_source::tests::', 1),
        ('transport', 'isomp4_source_contracts::transport::', 11),
        ('source', 'isomp4_source_contracts::', 25),
        ('range', 'network::http::range::tests::', 18),
        ('range-interruption', 'network::http::range::interruption_tests::', 6),
        ('file-interruption', 'local_source::interruption_tests::', 7),
        ('adapter-interruption', 'isomp4_transport::interruption_tests::', 3),
        ('common', 'streaming_transport::tests::', 3),
        ('player', 'inner_audio::', 111),
        ('publication', 'streaming::revision_tests::', 5),
        ('publication-clock', 'streaming::timeline_publication_tests::', 3),
    ]
    checks += list(extra)
    debug_skips = {
        'file': 'isomp4_source_contracts::transport::local::local_long_player_exceeds_cache_quota_without_accumulating_pcm',
        'package': 'isomp4_source_contracts::transport::local::package::package_long_player_exceeds_cache_quota_without_accumulating_pcm',
        'transport': 'isomp4_source_contracts::transport::long_m4a_player_exceeds_whole_cache_quota_with_bounded_pcm',
    }
    positives = {}
    for release in [True, False]:
        profile = 'release' if release else 'debug'
        binary = file.dsp.source.build(work, env, 'positive-'+profile, release=release)
        total = 0
        for label, prefix, count in checks:
            args = [str(binary), prefix, '--nocapture', '--test-threads=1']
            if label == 'source':
                args += ['--skip', 'isomp4_source_contracts::transport::']
            elif label == 'file':
                args += ['--skip', 'isomp4_source_contracts::transport::local::package::',
                         '--skip', 'isomp4_source_contracts::transport::local::directory::']
            elif label == 'transport':
                args += ['--skip', 'isomp4_source_contracts::transport::local::']
            if not release and label in debug_skips:
                args += ['--skip', debug_skips[label]]
                count -= 1
            log = work/f'positive-{profile}-{label}.log'
            codecs.run(args, log, cwd=package, env=env)
            match = re.search(r'test result: ok\. (\d+) passed; 0 failed;', log.read_text())
            assert match and int(match[1]) == count, (label, profile, count)
            total += count
        positives[profile] = total
        print(profile, total, 'positive tests passed', flush=True)
    return positives


def main(work, target, rubato, positive_only):
    env, frozen, retained, libraries, resampling = prepare(work, target, rubato)
    package = work/'probe'
    codecs.run(['cargo', 'metadata', '--offline', '--format-version', '1'],
               work/'metadata.jsonl', cwd=package, env=env)
    identity = lambda p: (p['name'], p['version'], p.get('source'), p.get('checksum'))
    root_lock = tomllib.loads((ROOT/'engine/Cargo.lock').read_text())['package']
    known = {p['name'] for p in root_lock}
    pinned = {identity(p) for p in root_lock}
    assert all(p['name'] not in known or identity(p) in pinned
               for p in tomllib.loads((package/'Cargo.lock').read_text())['package'])
    originals = {name: (work/'source'/name).read_text() for name in
                 ['bandlimited.rs', 'timed_m4a.rs', 'producer_control.rs', 'streaming_transport.rs', 'inner_audio.rs']}
    rows = declarations(originals)
    codecs.run(['cargo', 'check', '--offline', '--locked', '--lib'],
               work/'normal-library-check.log', cwd=package, env=env)
    positives = positive_profiles(work, env)
    receipts = []
    for name, path, before, after, test in ([] if positive_only else rows):
        source = work/'source'/path
        assert originals[path].count(before) == 1, name
        try:
            source.write_text(originals[path].replace(before, after))
            binary = file.dsp.source.build(work, env, name, release=False, clean=False)
            log = work/f'{name}-test.log'
            with log.open('w') as out:
                r = subprocess.run([str(binary), '--exact', test, '--nocapture'],
                    cwd=package, env=env, stdout=out, stderr=subprocess.STDOUT, timeout=120)
            observed = log.read_text()
            assert r.returncode == 101 and 'panicked at' in observed and '1 failed' in observed, name
            receipts.append({'name': name, 'test': test, 'exit': r.returncode,
                             'binary_sha256': codecs.sha(binary)})
        finally:
            source.write_text(originals[path])
        print('compiled phase fault', name, ': executed assertion rejected', flush=True)
    assert all(codecs.sha(ROOT/p) == h and codecs.sha(work/'source-inputs'/p) == h
               for p, h in frozen.items()), 'Root/frozen source drift'
    assert all(codecs.sha(work/p) == h for p, h in retained.items()), 'source copy not restored'
    provenance = json.loads((work/'library-source-provenance.json').read_text())
    for row in provenance:
        assert file.dsp.source.index_gate.timeline.source_tree(libraries[row['crate']]) == row['source_files']
    for name, row in resampling.items():
        assert all(codecs.sha(work/'resampling-vendor'/f"{name}-{row['version']}"/p) == h
                   for p, h in row['source_files'].items())
    summary = {'positive_tests_per_profile': positives, 'new_phase_and_receipt_tests': 8,
               'whole_stream_suffixes_per_profile': {'release': 840, 'debug': 840},
               'file_http_receipt_player_suffixes_per_profile': {'release': 240, 'debug': 240},
               'compiled_controls': len(receipts), 'controls': receipts,
               'root_inputs_unchanged': True, 'source_copies_restored': True,
               'libraries': len(provenance), 'patches': sum(len(p['patches']) for p in provenance),
               'resampling_sources_unchanged': True, 'root_lock_sha256': codecs.sha(ROOT/'engine/Cargo.lock'),
               'scope': 'Opt-in source-bound rational seek grid and finite real filter preroll;absolute EOF and receipt mapping.File/HTTP+player host verified;public controls/product cutover/platform/device/performance remain open.'}
    (work/'summary.json').write_text(json.dumps(summary, indent=2)+'\n')
    print(json.dumps(summary, indent=2), flush=True)


if __name__ == '__main__':
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--output', type=Path, required=True)
    p.add_argument('--mode', choices=['native', 'candidate'], default='candidate')
    p.add_argument('--target-dir', type=Path, required=True)
    p.add_argument('--rubato-cargo-home', type=Path)
    p.add_argument('--positive-only', action='store_true')
    a = p.parse_args()
    if a.mode == 'native':
        native(a.output.resolve(), a.target_dir.resolve(), a.positive_only)
    else:
        assert a.rubato_cargo_home, 'candidate mode requires --rubato-cargo-home'
        main(a.output.resolve(), a.target_dir.resolve(), a.rubato_cargo_home.resolve(), a.positive_only)
