#!/usr/bin/env python3
"""Execute actual bounded mutable-file snapshots and compiled failure controls.

An open File and restored metadata do not stabilize writable bytes. Cooperating
epochs must span the whole private copy, and memory/disk credit must follow its
real owners. Only freshly rebuilt, executed assertion failures count as controls.
"""
from __future__ import annotations

import argparse
import importlib.util
import json
from pathlib import Path
import re
import shutil
import tomllib

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location(
    'snapshot_writer_helpers', ROOT / 'scripts/testing/check_visible_file_writers.py')
writers = importlib.util.module_from_spec(spec)
spec.loader.exec_module(writers)
g = writers.g


def declarations():
    source = 'source_snapshot.rs'
    return [
        ('epoch-taken-after-metadata', source,
         '        let epoch = mutations.read_epoch()?;\n        after_epoch();\n        let stamp = FileStamp::of(source.metadata()?)?;',
         '        after_epoch();\n        let stamp = FileStamp::of(source.metadata()?)?;\n        let epoch = mutations.read_epoch()?;',
         'epoch_is_captured_before_metadata_and_verified_before_disk_admission'),
        ('epoch-tracks-another-file', source, '        let epoch = mutations.read_epoch()?;',
         '        let epoch = {drop(mutations); let other = tempfile::tempfile()?; FileMutations::for_file(&other)?.unwrap().read_epoch()?};',
         'an_active_writer_is_rejected_before_reading_length_or_creating_a_spool'),
        ('setup-epoch-not-verified', source,
         '        let stamp = FileStamp::of(source.metadata()?)?;\n        epoch.verify()?;',
         '        let stamp = FileStamp::of(source.metadata()?)?;',
         'epoch_is_captured_before_metadata_and_verified_before_disk_admission'),
        ('copy-epoch-ignored', source, '        self.epoch.verify()?;', '',
         'restored_metadata_cannot_hide_mutation_between_copy_steps'),
        ('copy-observed-metadata-ignored', source, '        self.stamp.verify(&self.source)',
         '        Ok(())', 'observed_untracked_metadata_changes_are_also_rejected'),
        ('pre-read-verification-missing', source,
         '            check(scheduler, cancelled)?;\n            self.verify()?;\n            if self.copied',
         '            check(scheduler, cancelled)?;\n            if self.copied',
         'restored_metadata_cannot_hide_mutation_between_copy_steps'),
        ('final-read-verification-missing', source,
         '            after_read(self.copied);\n            self.verify()?;',
         '            after_read(self.copied);',
         'mutation_after_the_final_read_never_publishes_a_sealed_snapshot'),
        ('copy-never-yields', source, 'const STEP_CHUNKS: usize = 16;',
         'const STEP_CHUNKS: usize = usize::MAX;',
         'public_snapshot_pins_the_source_yields_between_jobs_and_survives_later_writes'),
        ('copy-restarts-at-zero', source,
         'read_exact_at(&self.source, &mut self.scratch[..len], self.copied)?;',
         'read_exact_at(&self.source, &mut self.scratch[..len], 0)?;',
         'public_snapshot_pins_the_source_yields_between_jobs_and_survives_later_writes'),
        ('copy-clones-every-chunk', source, 'self.writer.append(&self.scratch[..len])?;',
         'self.writer.append(&self.scratch[..len].to_vec())?;',
         'bounded_steps_reuse_scratch_without_steady_allocations_and_transfer_it_to_the_reader'),
        ('only-one-media-slot-admitted', source,
         'const MEMORY_BYTES: u64 = 2 * FILE_CHUNK_BYTES as u64;',
         'const MEMORY_BYTES: u64 = FILE_CHUNK_BYTES as u64;',
         'byte_credit_follows_the_last_chunk_after_the_reader_is_dropped'),
        ('temporary-file-before-memory-credit', source,
         '''        let lease = pools
            .reserve_bytes(MEMORY_BYTES)
            .map_err(io::Error::other)?;
        let writer = SpoolWriter::create(dir, budget, max_bytes, Some(stamp.len))?;''',
         '''        let writer = SpoolWriter::create(dir, budget, max_bytes, Some(stamp.len))?;
        let lease = pools.reserve_bytes(MEMORY_BYTES).map_err(io::Error::other)?;''',
         'memory_and_disk_admission_precede_temporary_files_and_payload'),
        ('seal-drops-byte-credit', 'read_at_file.rs',
         '        Ok(Self::from_parts(buffer, Some(lease)))',
         '        drop(lease);\n        Ok(Self::from_parts(buffer, None))',
         'byte_credit_follows_the_last_chunk_after_the_reader_is_dropped'),
        ('chunk-loses-shared-byte-credit', 'read_at_file.rs', '            pool: self.0.clone(),',
         '            pool: Arc::new(Buffers {state: Mutex::new(BufferState {slots:[None,None],allocated:2}),_lease:None}),',
         'byte_credit_follows_the_last_chunk_after_the_reader_is_dropped'),
        ('seal-allocates-another-buffer', 'read_at_file.rs',
         '        Ok(Self::from_parts(buffer, Some(lease)))',
         '        drop(buffer);\n        Ok(Self::new(Some(lease)))',
         'bounded_steps_reuse_scratch_without_steady_allocations_and_transfer_it_to_the_reader'),
        ('copy-ignores-cancellation', source, '    if cancelled.load(Ordering::Relaxed) {',
         '    if false {',
         'cancellation_and_domain_close_interrupt_an_active_step_at_a_chunk_boundary'),
        ('copy-publishes-after-final-cancellation', source,
         '        check(scheduler, cancelled)?;\n        if self.copied == self.stamp.len {',
         '        if self.copied == self.stamp.len {',
         'cancellation_after_the_final_read_does_not_publish_output'),
        ('copy-ignores-domain-close', source, '    scheduler.ensure_open().map_err(pool_error)',
         '    Ok(())',
         'cancellation_and_domain_close_interrupt_an_active_step_at_a_chunk_boundary'),
        ('future-drop-forgets-cancellation', source,
         '    let _cancel_on_drop = CancelOnDrop(cancelled.clone());',
         '    let _cancel_on_drop = ();',
         'dropping_the_public_future_cancels_a_dispatched_job_before_setup'),
        ('sealed-empty-file-loses-disk-slot', 'spool.rs', '            _ticket: self.ticket,',
         '            _ticket: {drop(self.ticket); SpoolBudget::new(1,1)?.reserve(0)?},',
         'empty_sources_still_hold_one_disk_slot_until_the_reader_drops'),
        ('oversized-reused-buffer-accepted', 'read_at_file.rs',
         '        if buffer.len() != FILE_CHUNK_BYTES || buffer.capacity() != FILE_CHUNK_BYTES {',
         '        if false {',
         'invalid_reused_buffers_cannot_escape_the_memory_credit_bound'),
    ]


def validate_anchors(source):
    for name, file, before, _, _ in declarations():
        assert (source / file).read_text().count(before) == 1, (name, 'anchor drift')


def controls(work, env):
    source = work / 'engine/crates/io/src'
    for name, file, before, after, test in declarations():
        path = source / file
        original = path.read_text()
        assert original.count(before) == 1, (name, 'anchor drift')
        path.write_text(original.replace(before, after))
        try:
            binary = writers.build(work, env, name, 'io')
            log = work / f'{name}-test.log'
            status = g.run([str(binary), '--exact', 'source_snapshot::tests::' + test, '--nocapture'],
                           log, source.parent, env, timeout=40)
            observed = log.read_text()
            assert status == 101 and 'panicked at' in observed and '1 failed' in observed, (
                name, 'require an executed assertion failure', status)
        finally:
            path.write_text(original)
        print('actual source snapshot fault', name, ': executed assertion rejected', flush=True)
    return len(declarations())


def main(work, target, positive_only):
    validate_anchors(ROOT / 'engine/crates/io/src')
    env, frozen, copies = writers.helper.prepare(work, target)
    validate_anchors(work / 'engine/crates/io/src')
    env.pop('MIGO_AUDIO_CHECKS_BLESS', None)
    fixture = Path('platforms/apple/WebContent/PerformancePlus/test/fixtures/audio-check-answers.json')
    (work / fixture).parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(ROOT / fixture, work / fixture)
    for path in [Path(__file__).resolve(), ROOT / fixture,
                 ROOT / 'scripts/testing/check_visible_file_writers.py',
                 ROOT / 'scripts/testing/check_gitignore_contract.py']:
        relative = str(path.relative_to(ROOT))
        frozen[relative] = g.sha(path)
        dest = work / 'source-inputs' / relative
        dest.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(path, dest)
    (work / 'source-inputs.json').write_text(json.dumps(frozen, indent=2) + '\n')
    log = work / 'io-metadata.jsonl'
    assert g.run(['cargo', 'metadata', '--offline', '--format-version', '1'], log,
                 work / 'engine/crates/io', env) == 0
    selected = json.loads(next(line for line in log.read_text().splitlines()
                               if line.startswith('{"packages"')))['packages']
    locked = {}
    for row in tomllib.loads((ROOT / 'engine/Cargo.lock').read_text())['package']:
        locked.setdefault(row['name'], set()).add(row['version'])
    assert all(row['name'] not in locked or row['version'] in locked[row['name']] for row in selected)
    log = work / 'normal-check.jsonl'
    assert g.run(['cargo', 'check', '--offline', '--locked', '--lib', '--message-format=json'],
                 log, work / 'engine/crates/services', env) == 0
    normal = []
    for line in log.read_text().splitlines():
        try:
            row = json.loads(line)
        except ValueError:
            continue
        if row.get('reason') == 'compiler-artifact' and row['target']['name'] == 'migo_io':
            normal.append(row['features'])
    assert len(normal) == 1 and 'admission-probe' not in normal[0]
    suites = {}
    for name, crate, minimal in [('io-positive', 'io', False),
                                  ('io-minimal-positive', 'io', True),
                                  ('services-positive', 'services', False)]:
        binary = writers.build(work, env, name, crate, minimal)
        log = work / f'{name}.log'
        assert g.run([str(binary), '--nocapture', '--test-threads=1'], log,
                     work / 'engine/crates' / crate, env) == 0
        observed = log.read_text()
        match = re.search(r'test result: ok\. (\d+) passed; 0 failed; (\d+) ignored;', observed)
        assert match
        if crate == 'io':
            assert observed.count('test source_snapshot::tests::') == 22
            assert observed.count('test spool::tests::') == 7
            assert observed.count('test read_at_file::tests::') == 8
        suites[name] = {'passed': int(match[1]), 'ignored': int(match[2]),
                        'binary_sha256': g.sha(binary)}
    count = 0 if positive_only else controls(work, env)
    assert all(g.sha(ROOT / p) == h for p, h in frozen.items()), 'Root changed'
    assert all(g.sha(work / p) == h for p, h in copies.items()), 'source copy not restored'
    summary = {
        'suites': suites, 'snapshot_tests': 22, 'spool_regression_tests': 7,
        'positioned_file_regression_tests': 8, 'compiled_controls': count,
        'positive_only': positive_only, 'root_inputs_unchanged': True,
        'source_copies_restored': True, 'existing_dependency_versions_preserved': True,
        'normal_io_features': normal[0], 'probe_feature_absent_from_normal_io': True,
        'root_lock_sha256': g.sha(ROOT / 'engine/Cargo.lock'),
        'scope': 'Cooperating mutable source epochs, whole-copy verification, anonymous private snapshot, bounded/yielding Fs steps and cancellation, persistent Rust payload/disk ledgers, scratch transfer and zero steady Rust copy-step allocation. No arbitrary host-writer atomicity, automatic source dispatch, product codec/DSP cutover, native/device/RSS/maximum-performance acceptance.',
    }
    (work / 'summary.json').write_text(json.dumps(summary, indent=2) + '\n')
    print(json.dumps(summary, indent=2), flush=True)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--target-dir', type=Path, required=True)
    parser.add_argument('--positive-only', action='store_true')
    args = parser.parse_args()
    main(args.output.resolve(), args.target_dir.resolve(), args.positive_only)
