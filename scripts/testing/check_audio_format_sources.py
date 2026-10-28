#!/usr/bin/env python3
"""Replay library-native bounded local format selection and exact PCM dispatch.

Container scoring and metadata parsing belong to Symphonia. This gate must
observe executed assertion failures, not compile errors, for deliberate faults.
It does not claim product codec cutover, HTTP fallback or device performance.
"""
from __future__ import annotations
import argparse, importlib.util, json, shutil, subprocess, tomllib
from pathlib import Path
ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location('format_local', ROOT/'scripts/testing/check_audio_local_sources.py')
local = importlib.util.module_from_spec(spec); spec.loader.exec_module(local)
codecs = local.codecs

def declarations():
    limits = 'format_source_contracts::read_seek_and_deadline_limits_fail_sticky_without_codec_output'
    return [
        ('probe-byte-cap', 'source/format_source.rs', '.filter(|n| *n <= self.limits.max_read_bytes)', '.filter(|_| true)', limits, None),
        ('probe-request-cap', 'source/format_source.rs', '.filter(|n| *n <= self.limits.max_reads)', '.filter(|_| true)', limits, None),
        ('probe-seek-cap', 'source/format_source.rs', '.filter(|n| *n <= self.limits.max_seek_bytes)', '.filter(|_| true)', limits, None),
        ('probe-failure-not-sticky', 'source/format_source.rs', 'reader.failed = true;', 'reader.failed = false;', limits, None),
        ('probe-wrong-codec', 'source/format_source.rs', 'FORMAT_ID_ADTS => Format::Adts,', 'FORMAT_ID_ADTS => Format::Mp3,', 'format_source_contracts::metadata_skipping_reuses_library_seek_and_never_guesses_id3_means_mp3', None),
        ('probe-memory-unreserved', 'source/format_source.rs', '.reserve_bytes(PROBE_MEMORY_BYTES)', '.reserve_bytes(1)', 'format_source_contracts::probe_memory_credit_is_reserved_before_work_and_returned_after_it', None),
        ('probe-package-runs-on-fs', 'source/format_source.rs', '.submit_async(\n            reader.file.kind(),', '.submit_async(\n            migo_io::task::PoolKind::Fs,', 'format_source_contracts::queued_probe_obeys_pack_cancellation_and_deadline_without_repolling', None),
    ]

def validate(base):
    for name, path, before, _, _, _ in declarations():
        actual = base/path
        if not actual.is_file():
            actual = ROOT/'scripts/testing/fixtures/audio-codec-probe'/Path(path).name
        assert actual.read_text().count(before) == 1, (name, 'anchor drift')

def main(work, target, rubato, positive_only):
    validate(ROOT)
    files = ROOT/'scripts/testing/fixtures/audio-codec-probe'
    extra = [Path(__file__).resolve(), ROOT/'scripts/testing/check_audio_auto_sources.py', ROOT/'scripts/testing/check_gitignore_contract.py', files/'format_source.rs', files/'format_source_contracts.rs', files/'auto_source_contracts.rs']
    env, frozen, libraries, new = local.prepare(work, target, rubato, extra)
    env.pop('MIGO_AUDIO_CHECKS_BLESS', None)
    runtime = work/'runtime-tmp'; runtime.mkdir()
    env.update(TMPDIR=str(runtime), TMP=str(runtime), TEMP=str(runtime), CC='/usr/bin/clang', CXX='/usr/bin/clang++')
    fixture = Path('platforms/apple/WebContent/PerformancePlus/test/fixtures/audio-check-answers.json')
    dest = work/fixture; dest.parent.mkdir(parents=True, exist_ok=True); shutil.copy2(ROOT/fixture, dest)
    frozen[str(fixture)] = codecs.sha(ROOT/fixture)
    dest = work/'source-inputs'/fixture; dest.parent.mkdir(parents=True, exist_ok=True); shutil.copy2(ROOT/fixture, dest)
    (work/'source-inputs.json').write_text(json.dumps(frozen, indent=2)+'\n')
    package = work/'probe'; lib = package/'src/lib.rs'
    for name, testing in [('format_source', False), ('format_source_contracts', True), ('auto_source_contracts', True)]:
        source = work/'source'/f'{name}.rs'; shutil.copy2(files/f'{name}.rs', source)
        lib.write_text(lib.read_text()+f'\n'+('#[cfg(test)] ' if testing else '')+f'#[path={json.dumps(str(source))}] mod {name};\n')
    retained = {str(p.relative_to(work)): codecs.sha(p) for tree in [package/'src', work/'source'] for p in tree.rglob('*.rs')}
    validate(work)
    codecs.run(['cargo', 'metadata', '--offline', '--format-version', '1'], work/'candidate-metadata.jsonl', cwd=package, env=env)
    selected = json.loads(next(l for l in (work/'candidate-metadata.jsonl').read_text().splitlines() if l.startswith('{"packages"')))['packages']
    locked = {}
    for row in tomllib.loads((ROOT/'engine/Cargo.lock').read_text())['package']: locked.setdefault(row['name'], set()).add(row['version'])
    assert all(r['name'] not in locked or r['version'] in locked[r['name']] for r in selected), 'existing dependency drift'
    codecs.run(['cargo', 'check', '--offline', '--locked', '--lib'], work/'normal-library-check.log', cwd=package, env=env)
    binary = local.file.dsp.source.build(work, env, 'positive')
    checks = [('format', 'format_source_contracts::', 7), ('automatic', 'auto_source_contracts::', 7), ('sequential', 'local_source_contracts::', 5), ('local', 'isomp4_source_contracts::transport::local::', 11), ('package', 'isomp4_source_contracts::transport::local::package::', 8), ('directory', 'isomp4_source_contracts::transport::local::directory::', 5), ('setup', 'local_source::tests::', 1), ('http', 'isomp4_source_contracts::transport::', 11), ('common', 'streaming_transport::tests::', 3)]
    for label, prefix, count in checks:
        command = [str(binary), prefix, '--nocapture', '--test-threads=1']
        if label == 'local': command += ['--skip', 'isomp4_source_contracts::transport::local::package::', '--skip', 'isomp4_source_contracts::transport::local::directory::']
        if label == 'http': command += ['--skip', 'isomp4_source_contracts::transport::local::']
        log = work/f'positive-{label}.log'; codecs.run(command, log, cwd=package, env=env)
        assert f'{count} passed; 0 failed' in log.read_text(), label
    assert 'W09_FORMAT_AUTO_PCM 144 exact comparisons' in (work/'positive-format.log').read_text()
    count = 0
    if not positive_only:
        for name, relative, before, after, test, library in declarations():
            path = work/relative; original = path.read_text(); assert original.count(before) == 1
            path.write_text(original.replace(before, after))
            try:
                mutant = local.file.dsp.source.build(work, env, name)
                log = work/f'{name}-test.log'
                with log.open('w') as output:
                    r = subprocess.run([str(mutant), '--exact', test, '--nocapture'], cwd=package, env=env, stdout=output, stderr=subprocess.STDOUT, timeout=120)
                observed = log.read_text()
                assert r.returncode == 101 and 'panicked at' in observed and '1 failed' in observed, (name, 'require executed assertion', r.returncode)
            finally: path.write_text(original)
            count += 1; print('actual format-source fault', name, ': executed assertion rejected', flush=True)
    assert all(codecs.sha(ROOT/p) == h for p, h in frozen.items()) and all(codecs.sha(work/p) == h for p, h in retained.items())
    rows = json.loads((work/'library-source-provenance.json').read_text()); assert len(rows) == 9 and sum(len(r['patches']) for r in rows) == 27
    for row in rows: assert local.file.dsp.source.index_gate.timeline.source_tree(libraries[row['crate']]) == row['source_files']
    for name, row in new.items(): assert all(codecs.sha(work/'resampling-vendor'/f"{name}-{row['version']}"/p) == h for p, h in row['source_files'].items())
    summary = {'format_tests': 7, 'format_pcm_comparisons': 144, 'compiled_controls': count, 'positive_only': positive_only, 'binary_sha256': codecs.sha(binary), 'libraries': 9, 'patches': 27, 'root_inputs_unchanged': True, 'source_copies_restored': True, 'existing_dependency_versions_preserved': True, 'root_lock_sha256': codecs.sha(ROOT/'engine/Cargo.lock'), 'scope': 'Library-native bounded automatic local format selection and shared codec/CPU/DSP dispatch. Not HTTP fallback, product codec cutover, device or maximum-performance acceptance.'}
    (work/'summary.json').write_text(json.dumps(summary, indent=2)+'\n'); print(json.dumps(summary, indent=2), flush=True)

if __name__ == '__main__':
    p = argparse.ArgumentParser(description=__doc__); p.add_argument('--output', type=Path, required=True); p.add_argument('--target-dir', type=Path, required=True); p.add_argument('--rubato-cargo-home', type=Path, required=True); p.add_argument('--positive-only', action='store_true'); a = p.parse_args()
    main(a.output.resolve(), a.target_dir.resolve(), a.rubato_cargo_home.resolve(), a.positive_only)
