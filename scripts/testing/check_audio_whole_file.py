#!/usr/bin/env python3
"""Complete WAV/Vorbis buffers must retain bounded codec, PCM and sinc ownership.

Compose the existing candidate ports and the product PCM collector. Observe
actual input/copy/allocation bounds, complete nonzero PCM and retirement. Root
codec selection and locks stay unchanged; every fault must rebuild and execute
one Rust assertion, never fail to compile, time out or run zero tests.
"""
from __future__ import annotations

import argparse
import importlib.util
import json
from pathlib import Path
import re
import shutil
import tomllib

import check_audio_io_renewal as support
import check_audio_pcm_steps as pcm
import check_audio_ogg_streaming as ogg

ROOT = support.ROOT
FIXTURES = ROOT / 'scripts/testing/fixtures/audio-codec-probe'
SPEC = importlib.util.spec_from_file_location('whole_transport', ROOT / 'scripts/testing/check_audio_streaming_transport.py')
transport = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(transport)


def extra_inputs():
    paths = list((ROOT / 'scripts/testing').glob('check_audio_*.py'))
    paths += list(FIXTURES.glob('*.rs')) + list(pcm.mp3.FIXTURES.iterdir())
    paths += [ROOT / 'scripts/testing/check_gitignore_contract.py', ROOT / 'scripts/bench/audio-codecs.py',
              ROOT / 'scripts/testing/generate_mp3_step_fixtures.py',
              ROOT / 'scripts/bench/audio-mp3-product-performance.py',
              ROOT / 'scripts/bench/audio-wave-product-performance.py',
              ROOT / 'scripts/bench/audio-src-product-performance.py',
              ROOT / 'scripts/bench/audio-ogg-product-performance.py']
    for path in (ROOT / 'engine/third_party').glob('symphonia-*-patches'):
        paths += [p for p in path.iterdir() if p.is_file()]
    return paths


def native_declarations(originals):
    path = 'decoder/pcm.rs'
    test = 'decoder::pcm::tests::normalized_float_collection_preserves_every_sample_bit_and_copy_budget'
    before = '.map(convert)'
    assert originals[path].count(before) == 1
    return [
        ('pcm-generic-converter-discarded', path, before, '.map(|_sample| 0.0)', test),
        ('pcm-generic-converter-clamps-nonfinite-values', path, before,
         '.map(convert).map(|sample| sample.clamp(-1.0, 1.0))', test),
    ]


def declarations(original):
    rows = []
    tests = 'whole_file_candidate::tests::'
    wave = tests + 'wave_jobs_match_independent_numeric_pcm_and_the_existing_sinc_tail'
    cancel = tests + 'cancellation_at_all_owned_phases_releases_storage_and_stays_terminal'
    quota = tests + 'quota_codec_and_work_errors_retire_input_without_publishing_partial_pcm'
    bounds = tests + 'metadata_and_final_copy_yield_without_changing_nonzero_samples'

    def change(name, before, after, test):
        assert original.count(before) == 1, (name, 'mutation anchor drift')
        rows.append((name, 'whole_file.rs', before, after, test))

    change('whole-cancellation-ignored', 'cancel.load(Ordering::Acquire)', 'false', cancel)
    change('whole-cancellation-retains-owned-state', 'self.phase = Phase::Failed(ErrorCode::Cancelled);',
           'return Err(EngineError::from_detail(ErrorCode::Cancelled, "injected"));', cancel)
    change('whole-terminal-error-code-lost', 'self.phase = Phase::Failed(error.code);',
           'self.phase = Phase::Failed(ErrorCode::Internal);', quota)
    change('whole-work-budget-disabled', '.checked_sub(1)', '.saturating_sub(1).checked_add(0)', quota)
    change('whole-input-step-expanded', 'const INPUT_BYTES: usize = 8192;',
           'const INPUT_BYTES: usize = 65536;', bounds)
    change('whole-last-offered-fragment-becomes-eof', 'let eof = owner.offset == data.len();',
           'let eof = end == data.len();', wave)
    change('whole-pcm-copy-step-expanded', 'const COPY_SAMPLES: usize = 8192;',
           'const COPY_SAMPLES: usize = 16384;', bounds)
    change('whole-copy-discards-pending-pcm-suffix', 'owner.head = end;', 'owner.head = owner.pcm.len();', wave)
    before = original[original.index('    fn advance('):original.index('    pub fn finish(')]
    assert before.count('.filter(|samples| *samples <= self.limit)') == 1
    assert before.count('expected > self.limit') == 1
    after = before.replace('.filter(|samples| *samples <= self.limit)', '.filter(|_| true)')
    after = after.replace('expected > self.limit', 'false')
    change('whole-caller-output-limit-disabled', before, after, quota)
    change('whole-input-ceiling-disabled', 'input.as_ref().len() > MAX_INPUT_BYTES', 'false',
           tests + 'constructor_rejects_oversized_input_and_invalid_rates_before_codec_storage')
    change('whole-wave-uses-fixed-target-rate', 'WaveResampled::new(target_rate)', 'WaveResampled::new(44_100)', wave)
    change('whole-declared-timeline-adds-one-sample', '.map(Some)', '.map(|samples| Some(samples + 1))', wave)
    change('whole-final-buffer-cloned', 'samples: collector.finish()?,', 'samples: collector.finish()?.clone(),', bounds)
    change('whole-final-copy-drained-without-yield', 'if collector.step()? {',
           'if { while !collector.step()? {} true } {', bounds)
    change('whole-finish-publishes-decoder-scratch', 'Phase::Complete(audio) => Ok(audio),',
           'Phase::Decode(owner) => Ok(DecodedAudio { samples: owner.pcm, sample_rate: self.target_rate, channels: 1 }),\n            Phase::Complete(audio) => Ok(audio),',
           tests + 'only_complete_jobs_can_publish_pcm_and_empty_streams_keep_the_codec_refusal')
    change('whole-eof-leaks-input-codec-and-scratch', 'drop((input, codec, pcm));',
           'std::mem::forget((input, codec, pcm));', cancel)
    change('whole-resume-clears-uncollected-pcm', 'if owner.head < owner.pcm.len() {', 'if false {', wave)
    assert len(rows) == 17
    return rows


def prepare_candidate(work, target, rubato_home):
    package, env, frozen = support.clock.prepare(work, target, extra_inputs())
    libraries = transport.codecs.library_patches(ROOT, work)
    manifest = (package / 'Cargo.toml').read_text()
    # The product now uses these exact crates. Replace its dependency declarations
    # with this probe's feature selection, rather than creating duplicate TOML keys.
    for line in (ROOT / 'engine/crates/audio/Cargo.toml').read_text().splitlines():
        if any(line.startswith(name + ' =') for name in
               ['symphonia-core', 'symphonia-format-ogg', 'symphonia-codec-vorbis',
                'symphonia-format-riff', 'symphonia-codec-pcm', 'rubato']):
            manifest = manifest.replace(line + '\n', '')
    dependencies = '''symphonia={version="=0.6.1",default-features=false,features=["mp3","wav","pcm","ogg","vorbis","id3v2","opt-simd"]}
symphonia-core={version="=0.6.1"}
symphonia-bundle-mp3={version="=0.6.1",default-features=false,features=["streaming"]}
symphonia-format-riff={version="=0.6.1",default-features=false,features=["wav","streaming"]}
symphonia-codec-pcm={version="=0.6.1"}
symphonia-format-ogg={version="=0.6.1",default-features=false,features=["streaming"]}
symphonia-codec-vorbis={version="=0.6.1"}
rubato={version="=5.0.1",default-features=false}
'''
    manifest = manifest.replace('[dependencies]\n', '[dependencies]\n' + dependencies, 1)
    assert '[features]' not in manifest
    manifest += '\n[features]\ndefault=["wave-streaming-probe","vorbis-streaming-probe","metadata-budgets"]\n'
    manifest += ''.join(name + '=[]\n' for name in ['wave-streaming-probe', 'vorbis-streaming-probe',
                        'adts-streaming-probe', 'm4a-streaming-probe', 'phase-probe', 'metadata-budgets'])
    patches = ''.join(f'{name}={{path={json.dumps(str(path))}}}\n' for name, path in libraries.items())
    known = {p['name'] for p in tomllib.loads((ROOT / 'engine/Cargo.lock').read_text())['package']}
    resampling = {}
    for folder in sorted((rubato_home / 'registry/src').glob('*/*')):
        if not (folder / 'Cargo.toml').exists():
            continue
        row = tomllib.loads((folder / 'Cargo.toml').read_text())['package']
        name = row['name']
        if name in known:
            continue
        proof = transport.published_source(folder, rubato_home, row)
        destination = work / 'resampling-vendor' / folder.name
        shutil.copytree(folder, destination)
        patches += f'{name}={{path={json.dumps(str(destination))}}}\n'
        resampling[name] = {'version': row['version'], 'checksum': proof['package'], 'source_files': proof['files']}
    adopted = json.loads((ROOT / 'engine/third_party/rubato-sources.json').read_text())
    assert adopted['rubato']['version'] == '5.0.1'
    (work / 'resampling-paths.json').write_text(json.dumps(resampling, indent=2) + '\n')
    manifest = support.codecs.replace_patch_paths(manifest, libraries)
    # Resampler-only names do not overlap the product patch table.
    for name in libraries:
        patches = patches.replace(f'{name}={{path={json.dumps(str(libraries[name]))}}}\n', '')
    manifest = manifest.replace('[patch.crates-io]\n', '[patch.crates-io]\n' + patches, 1)
    (package / 'Cargo.toml').write_text(manifest)
    source = work / 'source'
    source.mkdir()
    modules = {'push_decoder': 'streaming', 'bandlimited': 'bandlimited',
               'resampled_streaming': 'resampled_streaming', 'candidate': 'candidate', 'whole_file_candidate': 'whole_file'}
    names = ['streaming', 'incremental_decoder', 'wave_streaming', 'vorbis_streaming', 'adts_streaming',
             'bandlimited', 'resampled_streaming', 'candidate', 'whole_file', 'whole_file_contracts',
             'whole_file_known_contracts', 'whole_file_duration', 'whole_file_duration_contracts', 'wave_pcm_contracts']
    for name in names:
        (source / (name + '.rs')).write_bytes((FIXTURES / (name + '.rs')).read_bytes())
    lib = package / 'src/lib.rs'
    addition = ''.join(f'\n#[path={json.dumps(str(source / (file + ".rs")))}] pub mod {name};\n' for name, file in modules.items())
    lib.write_text(lib.read_text() + addition)
    (work / 'lib-addition.txt').write_text(addition)
    # The plain-audio preparation prunes unused pins; seed the candidate solve
    # from the complete application lock so existing identities cannot roll.
    (package / 'Cargo.lock').write_bytes((ROOT / 'engine/Cargo.lock').read_bytes())
    support.codecs.run(['cargo', 'metadata', '--offline', '--format-version', '1'], work / 'candidate-metadata.jsonl', cwd=package, env=env)
    locked = tomllib.loads((ROOT / 'engine/Cargo.lock').read_text())['package']
    selected = tomllib.loads((package / 'Cargo.lock').read_text())['package']
    identity = lambda row: (row['name'], row['version'], row.get('source'), row.get('checksum'))
    pinned = set(map(identity, locked))
    assert all(row['name'] not in known or identity(row) in pinned for row in selected)
    support.codecs.run(['cargo', 'check', '--offline', '--locked', '--lib'], work / 'candidate-library-check.log', cwd=package, env=env)
    inputs = work / 'vorbis'
    inputs.mkdir()
    ogg.fixtures(inputs)
    env['W09_OGG_FIXTURES'] = str(inputs / 'fixtures')
    (work / 'fixture-sha256.json').write_text(json.dumps({str(p.relative_to(inputs)): support.codecs.sha(p)
        for p in inputs.rglob('*') if p.is_file()}, indent=2) + '\n')
    (work / 'retained-sources.json').write_text(json.dumps({str(p.relative_to(work)): support.codecs.sha(p)
        for tree in [package / 'src', source] for p in tree.rglob('*') if p.is_file()}, indent=2) + '\n')
    return package, env, frozen, libraries, resampling


def build_candidate(package, work, env, name, release=False):
    log = work / (name + '-build.jsonl')
    args = ['cargo', 'test', '--lib', '--offline', '--locked', '--no-run', '--message-format=json']
    if release:
        args += ['--release']
    assert support.clock.common.run(args, log, package, env, timeout=300) == 0, ('compile failed', log)
    artifacts = []
    for line in log.read_text().splitlines():
        try:
            row = json.loads(line)
        except ValueError:
            continue
        if row.get('reason') == 'compiler-artifact' and row.get('executable') and row.get('target', {}).get('name') == 'audio':
            assert row['fresh'] is False
            artifacts.append(Path(row['executable']))
    assert len(artifacts) == 1
    binary = work / (name + '-tests')
    shutil.copy2(artifacts[0], binary)
    return binary


def verify_inputs(work, frozen):
    assert all(support.codecs.sha(ROOT / p) == h and support.codecs.sha(work / 'source-inputs' / p) == h
               for p, h in frozen.items()), 'Root/frozen input changed'


def native(work, target, positive_only):
    package, env, frozen = support.clock.prepare(work, target, extra_inputs())
    names = ['decoder/pcm.rs', 'decoder/mp3/job.rs', 'decoder/mod.rs', 'decoder/mp3.rs', 'decoder/mp3/file.rs', 'audio_thread.rs']
    originals = {p: (package / 'src' / p).read_text() for p in names}
    rows = native_declarations(originals) + pcm.declarations(originals) + pcm.mp3.declarations(originals) + pcm.rate.declarations(originals)
    assert len(rows) == 54
    binary = support.public.build_native(package, work, env, 'positive')
    log = work / 'positive-tests.log'
    assert support.clock.common.run([str(binary)], log, package, env) == 0
    assert re.search(r'test result: ok\. 397 passed; 0 failed; 2 ignored;', log.read_text())
    support.program(work, 'positive', 'native')
    receipts = support.faults(work, package, env, originals, [] if positive_only else rows,
        lambda name: support.public.build_native(package, work, env, name), package / 'src', 'native')
    verify_inputs(work, frozen)
    source = ROOT / 'engine/crates/audio/src'
    assert all(p.read_bytes() == (package / 'src' / p.relative_to(source)).read_bytes()
               for p in source.rglob('*') if p.is_file())
    summary = {'positive_tests': 397, 'ignored_tests': 2, 'new_tests': 2, 'compiled_controls': len(receipts),
               'new_controls': 0 if positive_only else 2, 'retained_controls': 0 if positive_only else 52,
               'controls': receipts, 'root_inputs_unchanged': True, 'source_copies_restored': True}
    (work / 'summary.json').write_text(json.dumps(summary, indent=2) + '\n')
    print('native complete', summary['positive_tests'], len(receipts), flush=True)


def candidate(work, target, rubato, positive_only):
    package, env, frozen, libraries, resampling = prepare_candidate(work, target, rubato)
    originals = {'whole_file.rs': (work / 'source/whole_file.rs').read_text()}
    rows = declarations(originals['whole_file.rs'])
    for profile in ['release', 'debug']:
        name = 'positive-' + profile
        binary = build_candidate(package, work, env, name, release=profile == 'release')
        log = work / (name + '-tests.log')
        assert support.clock.common.run([str(binary)], log, package, env, timeout=300) == 0
        assert re.search(r'test result: ok\. 413 passed; 0 failed; 2 ignored;', log.read_text())
        support.program(work, name, 'native')
    receipts = support.faults(work, package, env, originals, [] if positive_only else rows,
        lambda name: build_candidate(package, work, env, name), work / 'source', 'native')
    verify_inputs(work, frozen)
    assert all(support.codecs.sha(work / p) == h for p, h in json.loads((work / 'retained-sources.json').read_text()).items())
    for row in json.loads((work / 'library-source-provenance.json').read_text()):
        assert all(support.codecs.sha(libraries[row['crate']] / p) == h for p, h in row['source_files'].items())
    for name, row in resampling.items():
        assert all(support.codecs.sha(work / 'resampling-vendor' / f"{name}-{row['version']}" / p) == h
                   for p, h in row['source_files'].items())
    summary = {'positive_tests_per_profile': {'release': 413, 'debug': 413}, 'ignored_tests': 2,
               'new_job_tests': 8, 'retained_pcm_library_tests': 4, 'wave_job_comparisons': 80,
               'vorbis_job_comparisons': 12, 'retained_wave_pcm_comparisons': 300,
               'compiled_controls': len(receipts), 'controls': receipts,
               'root_inputs_unchanged': True, 'source_copies_restored': True,
               'libraries': len(libraries), 'resampling_libraries': len(resampling),
               'scope': 'Candidate whole-file WAV/Vorbis owners over existing bounded codec/Rubato; '
                        'shared product collector, complete-only PCM, background handoff; no product codec switch.'}
    (work / 'summary.json').write_text(json.dumps(summary, indent=2) + '\n')
    print('candidate complete', summary['positive_tests_per_profile'], len(receipts), flush=True)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--scope', choices=['native', 'candidate'], required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--target-dir', type=Path, required=True)
    parser.add_argument('--rubato-cargo-home', type=Path)
    parser.add_argument('--positive-only', action='store_true')
    args = parser.parse_args()
    if args.scope == 'native':
        native(args.output.resolve(), args.target_dir.resolve(), args.positive_only)
    else:
        assert args.rubato_cargo_home is not None, '--rubato-cargo-home is required for candidate'
        candidate(args.output.resolve(), args.target_dir.resolve(), args.rubato_cargo_home.resolve(), args.positive_only)
