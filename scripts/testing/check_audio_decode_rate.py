#!/usr/bin/env python3
"""Bind whole-file decode rates to submission, independently of worker lifetime.

Exercise real WAV jobs through the production admission and worker paths. Run
the complete native suite and rebuilt assertion faults in an isolated copy;
compile failures, timeouts and zero executed tests never count as rejection.
"""
from __future__ import annotations

import argparse
import json
from pathlib import Path
import re

import check_audio_io_renewal as support

ROOT = support.ROOT


def declarations(originals):
    path = 'audio_thread.rs'
    source = originals[path]
    prefix = 'audio_thread::tests::'
    running = prefix + 'prepared_decodes_follow_route_changes_after_the_pool_has_started'
    queued = prefix + 'queued_decodes_keep_submission_rates_and_the_audio_context_rate'
    rows = []

    def change(name, before, after, test):
        assert source.count(before) == 1, (name, 'mutation anchor drift')
        rows.append((name, path, before, after, test))

    change('submission-uses-fixed-device-rate', 'target_rate: self.sample_rate,',
           'target_rate: 48_000,', running)
    change('route-change-restarts-pool', '        self.submit(DecodeJob::InnerAudio {',
           '        self.pool = None;\n        self.submit(DecodeJob::InnerAudio {', running)
    change('inner-worker-ignores-job-rate',
           'crate::decoder::decode_for_rate(&data, target_rate)',
           'crate::decoder::decode_for_rate(&data, 48_000)', queued)
    change('context-worker-uses-device-rate',
           'run_decode_with_panic_boundary(|| decode_for_rate(&data, target_rate));',
           'run_decode_with_panic_boundary(|| decode_for_rate(&data, 48_000));', queued)
    change('inner-worker-only-relabels-pcm',
           'crate::decoder::decode_for_rate(&data, target_rate)',
           'crate::decoder::decode(&data).map(|mut decoded| { decoded.sample_rate = target_rate; decoded })', queued)
    return rows


def main(work: Path, target: Path, positive_only: bool):
    extra = list((ROOT / 'scripts/testing').glob('check_audio_*.py'))
    extra.append(ROOT / 'scripts/testing/check_gitignore_contract.py')
    package, env, frozen = support.clock.prepare(work, target, extra)
    originals = {'audio_thread.rs': (package / 'src/audio_thread.rs').read_text()}
    rows = declarations(originals)
    binary = support.public.build_native(package, work, env, 'positive')
    log = work / 'positive-tests.log'
    assert support.clock.common.run([str(binary)], log, package, env) == 0
    assert re.search(r'test result: ok\. 397 passed; 0 failed; 2 ignored;', log.read_text())
    support.program(work, 'positive', 'native')
    receipts = support.faults(
        work, package, env, originals, [] if positive_only else rows,
        lambda name: support.public.build_native(package, work, env, name),
        package / 'src', 'native')
    assert all(support.codecs.sha(ROOT / p) == h
               and support.codecs.sha(work / 'source-inputs' / p) == h
               for p, h in frozen.items()), 'frozen input changed'
    audio = ROOT / 'engine/crates/audio/src'
    assert all(p.read_bytes() == (package / 'src' / p.relative_to(audio)).read_bytes()
               for p in audio.rglob('*.rs')), 'source copy was not restored'
    summary = {
        'positive_tests': 397, 'ignored_tests': 2, 'new_tests': 2,
        'compiled_controls': len(receipts), 'controls': receipts,
        'root_inputs_unchanged': True, 'source_copies_restored': True,
        'root_lock_sha256': support.codecs.sha(ROOT / 'engine/Cargo.lock'),
        'scope': 'Whole-file job submission rates, persistent workers and context rate; '
                 'no yielding decode or shared-executor migration yet.',
    }
    (work / 'summary.json').write_text(json.dumps(summary, indent=2) + '\n')
    print('native complete', summary['positive_tests'], len(receipts), flush=True)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--target-dir', type=Path, required=True)
    parser.add_argument('--positive-only', action='store_true')
    args = parser.parse_args()
    main(args.output.resolve(), args.target_dir.resolve(), args.positive_only)
