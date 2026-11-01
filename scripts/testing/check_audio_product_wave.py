#!/usr/bin/env python3
"""The synchronous WAVE reader could not yield, and missed canonical valid-bit PCM.

Select the existing mature RIFF/PCM port through an owned bounded product job.
Keep an exact test-only legacy reader and independent numeric oracles. Runtime
fault rejection requires a fresh executable and an actual Rust assertion;
compilation errors, signals, timeouts and empty filters are never accepted.
"""
from __future__ import annotations
import argparse
import json
from pathlib import Path
import re
import tomllib

import check_audio_whole_file as whole
import check_audio_duration_scan as duration

ROOT = whole.ROOT
TESTS = 'decoder::wav::tests::'
ADOPTED = {'symphonia-core', 'symphonia-metadata', 'symphonia-format-ogg',
           'symphonia-codec-vorbis', 'symphonia-format-riff', 'symphonia-codec-pcm'}


def verify_libraries(work, libraries):
    adopted = {}
    for name in ADOPTED:
        materialized = libraries[name]
        product = ROOT / 'engine/third_party' / materialized.name
        inventory = lambda base: {str(p.relative_to(base)): whole.support.codecs.sha(p)
                                  for p in base.rglob('*') if p.is_file()}
        assert inventory(product) == inventory(materialized), name
        adopted[name] = inventory(product)
    (work / 'adopted-library-proof.json').write_text(json.dumps(adopted, indent=2) + '\n')


def declarations(originals):
    wave, library = 'decoder/wav.rs', '@riff/src/wave/chunks.rs'
    rows = []
    def change(name, path, edits, test):
        for before, after in edits:
            assert originals[path].count(before) == 1, (name, 'mutation anchor drift', before)
        rows.append((name, path, edits, TESTS + test))
    def w(name, before, after, test):
        change(name, wave, [(before, after)], test)
    cancel = 'cancellation_at_every_owned_phase_releases_input_and_stays_terminal'
    known = 'known_pcm_configuration_and_reservation_yield_then_transfer_without_allocation'
    quota = 'declared_and_actual_output_limits_reject_before_partial_pcm_can_escape'
    malformed = 'truncated_empty_unsupported_and_oversized_files_never_publish_pcm'
    w('wave-cancellation-ignored', 'if cancel.load(Ordering::Acquire) {', 'if false {', cancel)
    w('wave-cancel-retains-storage', 'self.phase = Phase::Failed(ErrorCode::Cancelled);',
      'return Err(EngineError::from_detail(ErrorCode::Cancelled, "injected"));', cancel)
    w('wave-reservation-does-not-yield',
      '                    owner.collector.step()?;\n                    self.phase = Phase::Decode(owner);\n                    return Ok(false);',
      '                    owner.collector.step()?;', known)
    w('wave-declared-reservation-lost', 'PcmCollector::with_expected_samples(expected)',
      '{ let _ = expected; PcmCollector::new() }', known)
    w('wave-final-pcm-cloned', 'samples: collector.finish()?,', 'samples: collector.finish()?.clone(),', known)
    w('wave-target-fixed', 'ResampleJob::new(audio, rate)?', 'ResampleJob::new(audio, 44100)?',
      'target_rate_dispatch_matches_the_existing_dsp_and_exact_tail')
    w('wave-pcm-discarded', 'append_with(&scratch[..n], |v| v)', 'append_with(&scratch[..n], |_| 0.)',
      'all_integer_widths_and_channel_counts_keep_complete_legacy_pcm')
    w('wave-floats-clamped', 'append_with(&scratch[..n], |v| v)',
      'append_with(&scratch[..n], |v| v.clamp(-1.,1.))',
      'float_payloads_and_zero_valid_bits_preserve_every_sample_bit')
    w('wave-input-step-doubled', 'const INPUT_BYTES: usize = 8192;', 'const INPUT_BYTES: usize = 16384;',
      'metadata_skips_and_pcm_packets_obey_actual_per_step_limits')
    w('wave-total-work-unlimited', 'if owner.steps > MAX_STEPS {', 'if false {', quota)
    change('wave-output-limits-disabled', wave,
           [('.filter(|n| *n <= self.limit)', '.filter(|_| true)'),
            ('if total > self.limit {', 'if false {')], quota)
    w('wave-input-limit-disabled', 'input.as_ref().len() > 16 * 1024 * 1024', 'false', malformed)
    w('wave-finish-publishes-incomplete', 'Phase::Complete(audio) => Ok(audio),',
      'Phase::Decode(_) => Ok(DecodedAudio { samples: Vec::new(), sample_rate:48000, channels:1 }),\n            Phase::Complete(audio) => Ok(audio),', malformed)
    change('wave-padded-storage-shift-lost', library,
           [('(24, 4) => (32, 8)', '(24, 4) => (32, 0)')],
           'padded_24_bit_pcm_uses_mature_storage_shift_without_changing_numeric_values')
    change('wave-discrete-channel-count-lost', library,
           [('.unwrap_or(Channels::Discrete(count))', '.unwrap_or(Channels::Discrete(1))')],
           'all_integer_widths_and_channel_counts_keep_complete_legacy_pcm')
    change('wave-zero-valid-width-lost', library,
           [('if valid_bits_per_sample == 0 { bits_per_sample } else { valid_bits_per_sample }',
             'if valid_bits_per_sample == 0 { 1 } else { valid_bits_per_sample }')],
           'float_payloads_and_zero_valid_bits_preserve_every_sample_bit')
    return rows


def build(package, work, env, label, release=False):
    lib = package / 'src/lib.rs'
    lib.write_text(lib.read_text())
    return whole.build_candidate(package, work, env, label, release)


def main(work, target, candidate, minimal, rubato, preflight):
    if candidate:
        factory = whole.prepare_candidate if minimal else duration.bench.prepare
        package, env, frozen, libraries, resampling = factory(work, target, rubato)
        expected = 434 if minimal else 451
    else:
        package, env, frozen = whole.support.clock.prepare(work, target, whole.extra_inputs())
        libraries = whole.support.codecs.library_patches(ROOT, work)
        manifest = whole.support.codecs.replace_patch_paths((package / 'Cargo.toml').read_text(), libraries)
        (package / 'Cargo.toml').write_text(manifest)
        whole.support.codecs.run(['cargo', 'metadata', '--offline', '--format-version', '1'],
            work / 'private-metadata.jsonl', cwd=package, env=env)
        expected = 418
    verify_libraries(work, libraries)
    files = {'decoder/wav.rs': package / 'src/decoder/wav.rs',
             '@riff/src/wave/chunks.rs': libraries['symphonia-format-riff'] / 'src/wave/chunks.rs'}
    originals = {name: path.read_text() for name, path in files.items()}
    rows = declarations(originals)
    library_sources = {str(p.relative_to(work)): whole.support.codecs.sha(p)
                       for base in libraries.values() for p in base.rglob('*') if p.is_file()}
    (work / 'private-library-inputs.json').write_text(json.dumps(library_sources, indent=2) + '\n')
    programs, rejected, positives = [], [], {}
    def record(label, binary, role, prune=False):
        programs.append(dict(name=label, role=role, sha256=whole.support.codecs.sha(binary),
                             fresh_rust=True, pruned=prune))
        if prune:
            binary.unlink()
    for profile in (['debug'] if preflight else ['release', 'debug']):
        label = 'positive-' + profile
        binary = build(package, work, env, label, profile == 'release')
        log = work / (label + '-tests.log')
        filters = [TESTS, '--nocapture'] if preflight else []
        assert whole.support.clock.common.run([str(binary), *filters], log, package, env, timeout=300) == 0, log
        count = 12 if preflight else expected
        assert f'test result: ok. {count} passed; 0 failed;' in log.read_text(), log
        assert whole.support.clock.common.run([str(binary), '--list'], work / (label + '-inventory.log'), package, env) == 0
        inventory = (work / (label + '-inventory.log')).read_text()
        assert all(test + ': test' in inventory for _, _, _, test in rows)
        record(label, binary, 'full-positive' if not preflight else 'wave-only-preflight')
        positives[profile] = dict(passed=count, ignored=0 if preflight else 2)
    if not candidate and not preflight:
        for name, path, edits, test in rows:
            try:
                changed = originals[path]
                for before, after in edits:
                    changed = changed.replace(before, after)
                files[path].write_text(changed)
                binary = build(package, work, env, name)
                log = work / (name + '-test.log')
                status = whole.support.clock.common.run([str(binary), '--exact', test, '--nocapture'], log, package, env, timeout=120)
                observed = log.read_text()
                assert status == 101 and 'panicked at' in observed and '1 failed' in observed and 'running 1 test' in observed, (name, status, log)
                if path.startswith('@riff/'):
                    artifacts = [json.loads(line) for line in (work / (name + '-build.jsonl')).read_text().splitlines() if line.startswith('{')]
                    assert any(row.get('reason') == 'compiler-artifact' and row.get('target', {}).get('name') == 'symphonia_format_riff' and row['fresh'] is False for row in artifacts)
                record(name, binary, 'runtime-assertion', True)
                rejected.append(dict(name=name, test=test, exit=status, binary_sha256=programs[-1]['sha256']))
                print('actual WAVE assertion rejected:', name, flush=True)
            finally:
                files[path].write_text(originals[path])
    whole.verify_inputs(work, frozen)
    assert all(path.read_text() == originals[name] for name, path in files.items())
    assert all(whole.support.codecs.sha(work / p) == value for p, value in library_sources.items())
    expected_lib = (ROOT / 'engine/crates/audio/src/lib.rs').read_bytes()
    if candidate:
        expected_lib += (work / 'lib-addition.txt').read_bytes()
    assert (package / 'src/lib.rs').read_bytes() == expected_lib
    assert all((package / 'src' / p.relative_to(ROOT / 'engine/crates/audio/src')).read_bytes() == p.read_bytes()
               for p in (ROOT / 'engine/crates/audio/src').rglob('*') if p.is_file() and p.name != 'lib.rs')
    if candidate:
        assert all(whole.support.codecs.sha(work / p) == value for p, value in json.loads((work / 'retained-sources.json').read_text()).items())
    assert whole.support.clock.common.run(['cargo', 'tree', '--offline', '--locked', '-e', 'normal', '-p', 'migo-audio'],
        work / 'product-runtime-tree.log', package, env) == 0
    assert 'hound v' not in (work / 'product-runtime-tree.log').read_text()
    summary = dict(profiles=positives, compiled_controls=len(rejected), controls=rejected, programs=programs,
                   root_inputs_unchanged=True, source_copies_restored=True, library_sources_restored=True,
                   product_wave_selected=True, hound_test_only=True, candidate=candidate, minimal=minimal,
                   preflight=preflight, production_pool_migrated=False)
    (work / 'summary.json').write_text(json.dumps(summary, indent=2) + '\n')
    print('product WAVE complete', positives, len(rejected), flush=True)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--target-dir', type=Path, required=True)
    parser.add_argument('--candidate', action='store_true')
    parser.add_argument('--minimal', action='store_true')
    parser.add_argument('--rubato-home', type=Path, default=Path('/tmp/migo-w09-rubato-cargo'))
    parser.add_argument('--preflight', action='store_true')
    args = parser.parse_args()
    assert not args.minimal or args.candidate
    main(args.output.resolve(), args.target_dir.resolve(), args.candidate, args.minimal, args.rubato_home.resolve(), args.preflight)
