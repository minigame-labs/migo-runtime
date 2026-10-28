#!/usr/bin/env python3
"""Compile real Core probe and borrowed Fs/Pack IO regressions and runtime faults."""
from __future__ import annotations
import argparse, importlib.util, json, re, shutil, tomllib
from pathlib import Path
ROOT = Path(__file__).resolve().parents[2]
def module(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    value = importlib.util.module_from_spec(spec); spec.loader.exec_module(value); return value
writers = module('probe_primitive_writers', ROOT/'scripts/testing/check_visible_file_writers.py')
codecs = module('probe_primitive_codecs', ROOT/'scripts/bench/audio-codecs.py')
g = writers.g

def io_controls():
    file_test = 'read_at_file::tests::borrowed_controls_preserve_extent_stamp_and_allocate_no_payload'
    pack_test = 'read_at_package::tests::borrowed_controls_reuse_scratch_with_no_payload_allocation_or_extent_bypass'
    return [
        ('borrowed-file-skips-extent', 'read_at_file.rs', 'self.extent(offset, output.len(), self.max_control_bytes)?;', '', file_test),
        ('borrowed-package-skips-extent', 'read_at_package.rs', 'check_extent(self.len, offset, output.len(), self.max_control_bytes)?;', '', pack_test),
        ('borrowed-file-skips-stamp', 'read_at_file.rs', '        self.fill(offset, output)', '        read_exact_at(&self.file, output, offset)', file_test),
        ('borrowed-file-allocates', 'read_at_file.rs', '        self.fill(offset, output)', '        let data = self.read_exact_at(offset, output.len())?; output.copy_from_slice(&data); Ok(())', file_test),
        ('borrowed-package-allocates', 'read_at_package.rs', 'check_extent(self.len, offset, output.len(), self.max_control_bytes)?;', 'let data = self.read_exact_at(offset, output.len())?; output.copy_from_slice(&data);', pack_test),
    ]

def library_controls():
    return [
        ('identify-eagerly-constructs-container', '        self.find_format(hint, mss, FormatOptions::default(), meta_opts)', '        self.probe(hint, mss, FormatOptions::default(), meta_opts).and_then(|_| crate::errors::unsupported_error("eager container"))', 1, 'formats::probe::identification_tests::identify_defers_factory_and_into_reader_preserves_policy'),
        ('identify-discards-metadata-policy', '        fmt_opts.metadata_options = meta_opts;', '', 2, 'formats::probe::identification_tests::identify_defers_factory_and_into_reader_preserves_policy'),
    ]

def core_build(work, env, name):
    assert g.run(['cargo', 'clean', '--offline', '--locked', '-p', 'symphonia-core'], work/f'{name}-core-clean.log', work/'core', env) == 0
    assert g.run(['cargo', 'test', '--offline', '--locked', '-p', 'symphonia-core', '--lib', '--no-run', '--message-format=json'], work/f'{name}-core-build.jsonl', work/'core', env) == 0
    rows = [json.loads(l) for l in (work/f'{name}-core-build.jsonl').read_text().splitlines() if l.startswith('{')]
    row = next(r for r in rows if r.get('reason') == 'compiler-artifact' and r.get('executable') and r['target']['name'] == 'symphonia_core')
    assert row['fresh'] is False
    binary = work/f'{name}-core-tests'; shutil.copy2(row['executable'], binary); return binary

def main(work, target, positive_only):
    for name, file, before, _, _ in io_controls():
        assert (ROOT/'engine/crates/io/src'/file).read_text().count(before) == 1, (name, 'anchor drift')
    env, frozen, copies = writers.helper.prepare(work, target)
    extra = [Path(__file__).resolve(), ROOT/'scripts/testing/check_visible_file_writers.py', ROOT/'scripts/bench/audio-codecs.py', ROOT/'scripts/testing/check_gitignore_contract.py']
    for path in extra:
        rel = str(path.relative_to(ROOT)); frozen[rel] = codecs.sha(path)
        dest = work/'source-inputs'/rel; dest.parent.mkdir(parents=True, exist_ok=True); shutil.copy2(path, dest)
    core = work/'core'; core.mkdir()
    libraries = codecs.library_patches(ROOT, core)
    for row in json.loads((core/'library-source-provenance.json').read_text()):
        for patch in row['patches']:
            # The provenance entry records the repository-relative patch path.
            path = ROOT/patch['file']
            rel = str(path.relative_to(ROOT)); frozen[rel] = codecs.sha(path)
            dest = work/'source-inputs'/rel; dest.parent.mkdir(parents=True, exist_ok=True); shutil.copy2(path, dest)
    (work/'source-inputs.json').write_text(json.dumps(frozen, indent=2)+'\n')
    (core/'Cargo.toml').write_text('[workspace]\nresolver="2"\nmembers=["vendor/symphonia-core-0.6.1"]\n[patch.crates-io]\n'+''.join(f'{name}={{path={json.dumps(str(path))}}}\n' for name, path in libraries.items()))
    shutil.copy2(ROOT/'engine/Cargo.lock', core/'Cargo.lock')
    locked = {}
    for row in tomllib.loads((ROOT/'engine/Cargo.lock').read_text())['package']: locked.setdefault(row['name'], set()).add(row['version'])
    for label, package in [('io', work/'engine/crates/io'), ('core', core)]:
        log = work/f'{label}-metadata.jsonl'; assert g.run(['cargo', 'metadata', '--offline', '--format-version', '1'], log, package, env) == 0
        selected = json.loads(next(l for l in log.read_text().splitlines() if l.startswith('{"packages"')))['packages']
        assert all(r['name'] not in locked or r['version'] in locked[r['name']] for r in selected), 'existing version drift'
    probe = libraries['symphonia-core']/'src/formats/probe.rs'
    for name, before, _, count, _ in library_controls(): assert probe.read_text().count(before) == count, (name, 'anchor drift')
    suites = {}
    for name, minimal in [('io-positive', False), ('io-minimal-positive', True)]:
        binary = writers.build(work, env, name, 'io', minimal)
        log = work/f'{name}.log'; assert g.run([str(binary), '--nocapture', '--test-threads=1'], log, work/'engine/crates/io', env) == 0
        match = re.search(r'test result: ok\. (\d+) passed; 0 failed; (\d+) ignored;', log.read_text()); assert match
        suites[name] = {'passed': int(match[1]), 'ignored': int(match[2]), 'binary_sha256': codecs.sha(binary)}
    binary = core_build(work, env, 'positive')
    assert g.run([str(binary), '--nocapture', '--test-threads=1'], work/'positive-core.log', core, env) == 0
    assert '72 passed; 0 failed' in (work/'positive-core.log').read_text()
    count = 0
    if not positive_only:
        for name, file, before, after, test in io_controls():
            path = work/'engine/crates/io/src'/file; original = path.read_text(); assert original.count(before) == 1
            path.write_text(original.replace(before, after))
            try:
                mutant = writers.build(work, env, name, 'io'); log = work/f'{name}-test.log'
                status = g.run([str(mutant), '--exact', test, '--nocapture'], log, work/'engine/crates/io', env, timeout=60)
                observed = log.read_text(); assert status == 101 and 'panicked at' in observed and '1 failed' in observed, (name, 'require executed assertion', status)
            finally: path.write_text(original)
            count += 1; print('actual borrowed-IO fault', name, ': executed assertion rejected', flush=True)
        for name, before, after, _, test in library_controls():
            original = probe.read_text(); probe.write_text(original.replace(before, after))
            try:
                mutant = core_build(work, env, name); log = work/f'{name}-test.log'
                status = g.run([str(mutant), '--exact', test, '--nocapture'], log, core, env, timeout=60)
                observed = log.read_text(); assert status == 101 and 'panicked at' in observed and '1 failed' in observed, (name, 'require executed assertion', status)
            finally: probe.write_text(original)
            count += 1; print('actual library-probe fault', name, ': executed assertion rejected', flush=True)
    assert all(codecs.sha(ROOT/p) == h for p, h in frozen.items()) and all(codecs.sha(work/p) == h for p, h in copies.items())
    provenance = json.loads((core/'library-source-provenance.json').read_text())
    timeline = module('probe_primitive_timeline', ROOT/'scripts/testing/check_audio_codec_timeline.py')
    for row in provenance: assert timeline.source_tree(libraries[row['crate']]) == row['source_files']
    summary = {'suites': suites, 'core_tests': 72, 'new_root_tests': 2, 'new_library_tests': 3, 'compiled_controls': count, 'positive_only': positive_only, 'root_inputs_unchanged': True, 'source_copies_restored': True, 'existing_dependency_versions_preserved': True, 'root_lock_sha256': codecs.sha(ROOT/'engine/Cargo.lock'), 'scope': 'Bounded caller-buffer positional IO and shared library container identification; not product/device/maximum-performance acceptance.'}
    (work/'summary.json').write_text(json.dumps(summary, indent=2)+'\n'); print(json.dumps(summary, indent=2), flush=True)

if __name__ == '__main__':
    p = argparse.ArgumentParser(description=__doc__); p.add_argument('--output', type=Path, required=True); p.add_argument('--target-dir', type=Path, required=True); p.add_argument('--positive-only', action='store_true'); a = p.parse_args()
    main(a.output.resolve(), a.target_dir.resolve(), a.positive_only)
