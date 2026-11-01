#!/usr/bin/env python3
"""Verify recoverable media waits without reviving cancelled work or real errors.

A seek can interrupt media IO before deciding whether a target is valid. Reusing
a boolean file activity flag allowed old jobs to borrow a later operation's
permission; HTTP recovery could lose an already validated body or erase a hard
failure. Compile the actual readers and adapters, retain their source identities,
and require each injected fault to fail an executed Rust assertion.
"""
from __future__ import annotations

import argparse
import json
from pathlib import Path
import subprocess
import tomllib

import check_audio_mp4_file as file

ROOT = file.ROOT
codecs = file.codecs


def controls(originals):
    rows = []

    def change(name, path, signature, before, after, test):
        original = originals[path]
        start = original.index(signature)
        end = original.index('\n    }', start) + len('\n    }')
        body = original[start:end]
        assert body.count(before) == 1, (name, 'mutation anchor changed')
        rows.append((name, path, body, body.replace(before, after), test))

    local = 'source/local_source.rs'
    resume = '    pub fn resume_after_interruption('
    prefix = 'local_source::interruption_tests::'
    change('file-recovery-revives-hard-error', local, resume,
           'if self.media_interrupted && self.read_active.load(Ordering::Acquire) == 0 {',
           'if true {', prefix + 'failed_or_exhausted_file_operations_stay_terminal_without_submitting_io')
    change('file-recovery-keeps-poisoned-latch', local, resume,
           '            self.failed = false;', '',
           prefix + 'interrupted_jobs_keep_their_retired_epoch_before_and_after_disk_io')
    change('file-recovery-loses-cursor', local, resume,
           '            self.media_interrupted = false;',
           '            self.position = 0;\n            self.media_interrupted = false;',
           prefix + 'interruption_after_a_delivered_prefix_preserves_cursor_and_accounting')
    change('file-recovery-allocates', local, resume,
           '        self.active()', '        std::hint::black_box(vec![0_u8; 64]);\n        self.active()',
           prefix + 'real_interruption_recovery_has_no_caller_heap_activity')
    change('file-drop-keeps-retired-epoch', local, '    fn drop(&mut self) {',
           'let _ = self\n            .active\n            .compare_exchange(self.epoch, 0, Ordering::AcqRel, Ordering::Acquire);',
           'let _ = self.active.load(Ordering::Acquire);',
           prefix + 'interrupted_jobs_keep_their_retired_epoch_before_and_after_disk_io')
    # Both guards matter: a queued job must not enter disk IO, and an already
    # running job must not return a successful late completion under a new key.
    signature = '    async fn next_chunk('
    original = originals[local]
    start = original.index(signature)
    end = original.index('\n    }', start) + len('\n    }')
    body = original[start:end]
    guard = 'read_active.load(Ordering::Acquire) != epoch'
    assert body.count(guard) == 2
    for ordinal, label in [(0, 'before-disk'), (1, 'after-disk')]:
        at = body.index(guard) if ordinal == 0 else body.rindex(guard)
        mutant = body[:at] + body[at:].replace(guard, 'read_active.load(Ordering::Acquire) == 0', 1)
        rows.append(('file-old-epoch-' + label, local, body, mutant,
                     prefix + 'interrupted_jobs_keep_their_retired_epoch_before_and_after_disk_io'))

    media = 'probe/src/network/http/range/stream.rs'
    prefix = 'network::http::range::interruption_tests::'
    change('http-wait-not-recoverable', media, '    pub async fn chunk(',
           '        self.interrupted = true;', '        self.interrupted = false;',
           prefix + 'interrupted_body_keeps_its_exact_segment_without_another_get')
    change('http-recovery-erases-hard-error', media, resume,
           'if self.interrupted {', 'if true {',
           prefix + 'returned_transport_identity_extent_and_timeout_errors_stay_terminal')
    change('http-recovery-drops-validated-body', media, resume,
           '            self.interrupted = false;',
           '            self.segment = None;\n            self.interrupted = false;',
           prefix + 'interrupted_body_keeps_its_exact_segment_without_another_get')
    change('http-recovery-resets-budget', media, resume,
           '            self.interrupted = false;',
           '            self.requests = 0;\n            self.interrupted = false;',
           prefix + 'interrupted_headers_retry_from_the_same_position_with_the_same_budget')
    change('http-recovery-loses-position', media, resume,
           '            self.interrupted = false;',
           '            self.position = 0;\n            self.interrupted = false;',
           prefix + 'interrupted_body_keeps_its_exact_segment_without_another_get')

    adapter = 'source/isomp4_transport.rs'
    prefix = 'isomp4_transport::interruption_tests::'
    change('adapter-reinterprets-error-as-eof', adapter, '    async fn read(\n',
           'if self.failed {', 'if false {',
           prefix + 'returned_initial_body_error_cannot_be_resumed_or_reinterpreted_as_eof')
    change('adapter-recovery-erases-hard-error', adapter, '    fn resume_after_read_interruption(',
           'if self.failed && !self.interrupted {', 'if false {',
           prefix + 'returned_initial_body_error_cannot_be_resumed_or_reinterpreted_as_eof')
    change('adapter-recovery-ignores-failed-reader', adapter, '    fn resume_after_read_interruption(',
           'if let Some(source) = &mut self.source {', 'if let Some(source) = None::<&mut IndexSource> {',
           prefix + 'prepared_failed_control_reader_cannot_be_hidden_by_the_initial_response')
    change('adapter-recovery-keeps-poisoned-latch', adapter, '    fn resume_after_read_interruption(',
           '        self.failed = false;', '',
           prefix + 'initial_body_interruption_keeps_exact_bytes_without_reopening_http')
    return rows


def main(work, target, rubato, positive_only):
    env, frozen, retained, libraries, resampling = file.prepare(
        work, target, rubato, [Path(__file__).resolve()])
    env.update(CC='/usr/bin/clang', CXX='/usr/bin/clang++')
    package = work / 'probe'
    codecs.run(['cargo', 'metadata', '--offline', '--format-version', '1'],
               work / 'candidate-metadata.jsonl', cwd=package, env=env)
    identity = lambda row: (row['name'], row['version'], row.get('source'), row.get('checksum'))
    root_lock = tomllib.loads((ROOT / 'engine/Cargo.lock').read_text())['package']
    known = {row['name'] for row in root_lock}
    pinned = {identity(row) for row in root_lock}
    candidate_lock = tomllib.loads((package / 'Cargo.lock').read_text())['package']
    assert all(row['name'] not in known or identity(row) in pinned for row in candidate_lock), 'dependency identity drift'
    paths = ['source/local_source.rs', 'source/isomp4_transport.rs',
             'probe/src/network/http/range/stream.rs']
    originals = {path: (work / path).read_text() for path in paths}
    declarations = controls(originals)
    codecs.run(['cargo', 'check', '--offline', '--locked', '--lib'],
               work / 'normal-library-check.log', cwd=package, env=env)
    binary = file.dsp.source.build(work, env, 'positive')
    checks = [
        ('range-interruption', 'network::http::range::interruption_tests::', 6),
        ('file-interruption', 'local_source::interruption_tests::', 7),
        ('adapter-interruption', 'isomp4_transport::interruption_tests::', 3),
        ('range', 'network::http::range::tests::', 18),
        ('file', 'isomp4_source_contracts::transport::local::', 11),
        ('package', 'isomp4_source_contracts::transport::local::package::', 8),
        ('directory', 'isomp4_source_contracts::transport::local::directory::', 5),
        ('setup', 'local_source::tests::', 1),
        ('transport', 'isomp4_source_contracts::transport::', 11),
        ('source', 'isomp4_source_contracts::', 25),
        ('common', 'streaming_transport::tests::', 3),
        ('player', 'inner_audio::', 51),
    ]
    for label, prefix, count in checks:
        args = [str(binary), prefix, '--nocapture', '--test-threads=1']
        if label == 'file':
            args += ['--skip', 'isomp4_source_contracts::transport::local::package::',
                     '--skip', 'isomp4_source_contracts::transport::local::directory::']
        if label == 'transport':
            args += ['--skip', 'isomp4_source_contracts::transport::local::']
        if label == 'source':
            args += ['--skip', 'isomp4_source_contracts::transport::']
        log = work / f'positive-{label}.log'
        codecs.run(args, log, cwd=package, env=env)
        assert f'{count} passed; 0 failed' in log.read_text(), label
        print(label, 'PASS', count, flush=True)
    receipts = []
    for name, path, before, after, test in ([] if positive_only else declarations):
        source = work / path
        assert originals[path].count(before) == 1, (name, 'ambiguous mutation')
        try:
            source.write_text(originals[path].replace(before, after))
            mutant = file.dsp.source.build(work, env, name)
            log = work / f'{name}-test.log'
            with log.open('w') as out:
                result = subprocess.run([str(mutant), '--exact', test, '--nocapture'],
                                        cwd=package, env=env, stdout=out,
                                        stderr=subprocess.STDOUT, timeout=45)
            observed = log.read_text()
            assert result.returncode == 101 and 'panicked at' in observed and '1 failed' in observed, (name, 'requires an executed assertion')
            receipts.append({'name': name, 'test': test, 'exit': result.returncode,
                             'binary_sha256': codecs.sha(mutant)})
        finally:
            source.write_text(originals[path])
        print('compiled read interruption control', name, ': assertion rejected', flush=True)
    assert all(codecs.sha(ROOT / path) == sha for path, sha in frozen.items()), 'Root inputs changed'
    assert all(codecs.sha(work / path) == sha for path, sha in retained.items()), 'test sources not restored'
    provenance = json.loads((work / 'library-source-provenance.json').read_text())
    for row in provenance:
        assert file.dsp.source.index_gate.timeline.source_tree(libraries[row['crate']]) == row['source_files']
    for name, row in resampling.items():
        directory = work / 'resampling-vendor' / f"{name}-{row['version']}"
        assert all(codecs.sha(directory / path) == sha for path, sha in row['source_files'].items())
    summary = {
        'positive_tests': sum(count for _, _, count in checks),
        'read_interruption_tests': 16, 'compiled_controls': len(receipts), 'controls': receipts,
        'positive_only': positive_only, 'binary_sha256': codecs.sha(binary),
        'libraries': len(provenance), 'patches': sum(len(row['patches']) for row in provenance),
        'root_inputs': len(frozen), 'root_inputs_unchanged': True, 'source_copies_restored': True,
        'existing_dependency_identities_preserved': True,
        'root_lock_sha256': codecs.sha(ROOT / 'engine/Cargo.lock'),
        'scope': 'Explicit recovery of cancelled media waits; pinned file epochs, HTTP body/cursor/budgets and sticky hard failures. Persistent actor, product codec cutover, native/device and peak performance remain open.',
    }
    (work / 'retained-sources.json').write_text(json.dumps(retained, indent=2) + '\n')
    (work / 'summary.json').write_text(json.dumps(summary, indent=2) + '\n')
    print(json.dumps(summary, indent=2), flush=True)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--target-dir', type=Path, required=True)
    parser.add_argument('--rubato-cargo-home', type=Path, required=True)
    parser.add_argument('--positive-only', action='store_true')
    args = parser.parse_args()
    main(args.output.resolve(), args.target_dir.resolve(), args.rubato_cargo_home.resolve(), args.positive_only)
