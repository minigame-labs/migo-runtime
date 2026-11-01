#!/usr/bin/env python3
"""Verify actual whole-file dispatch on the existing runtime and CPU lane.

Mutations stay in an isolated source copy. Only a fresh executable's exact
Rust test failure counts as a rejected fault; build errors never count.
"""
from __future__ import annotations
import argparse
import json
from pathlib import Path

import check_audio_whole_decode as product

TEST = 'audio_thread::decode_pool::tests::'
PARENT = 'audio_thread::tests::'


def declarations():
    return [
        ('production-refill-window-bypassed', 'decode_pool.rs', 'let limit = self.refill.step_limit(generation, now);',
         'let limit = MAX_STEPS_PER_TURN;', TEST + 'refill_burst_uses_short_steps_then_restores_long_turns'),
        ('refill-generation-not-observed', 'decode_pool.rs', 'if generation != self.generation {',
         'if false {', TEST + 'refill_window_observes_short_requests_and_expires_at_the_boundary'),
        ('refill-burst-budget-raised', 'decode_pool.rs', 'const REFILL_STEPS_PER_TURN: usize = 32;',
         'const REFILL_STEPS_PER_TURN: usize = 128;', TEST + 'refill_burst_uses_short_steps_then_restores_long_turns'),
        ('refill-grace-boundary-inclusive', 'decode_pool.rs', 'now.duration_since(recent) < REFILL_GRACE',
         'now.duration_since(recent) <= REFILL_GRACE', TEST + 'refill_window_observes_short_requests_and_expires_at_the_boundary'),
        ('adaptive-step-limit-discarded', 'decode_pool.rs', '0..step_limit.clamp(1, MAX_STEPS_PER_TURN)',
         '0..MAX_STEPS_PER_TURN', TEST + 'refill_burst_uses_short_steps_then_restores_long_turns'),
        ('queued-refill-not-observed', 'decode_pool.rs', 'if refill_queued() || start.elapsed() >= max_elapsed {',
         'if start.elapsed() >= max_elapsed {', TEST + 'queued_refill_interrupts_a_turn_after_one_more_bounded_step'),
        ('cancel-starts-codec', 'decode_pool.rs', 'if self.cancel.load(Ordering::Acquire) {',
         'if false {', TEST + 'pre_cancelled_turn_never_constructs_the_codec'),
        ('turn-step-cap-raised', 'decode_pool.rs', 'const MAX_STEPS_PER_TURN: usize = 128;',
         'const MAX_STEPS_PER_TURN: usize = 256;', TEST + 'cpu_turn_has_a_hard_logical_step_cap_even_without_clock_pressure'),
        ('turn-elapsed-budget-ignored', 'decode_pool.rs', 'if refill_queued() || start.elapsed() >= max_elapsed {',
         'if false {', TEST + 'cpu_turn_checks_elapsed_budget_after_each_bounded_library_step'),
        ('waiting-cancel-disabled', 'decode_pool.rs', '_ = reply.cancelled() =>',
         '_ = std::future::pending::<()>() =>', TEST + 'waiting_reply_cancel_returns_encoded_credit_without_an_active_slot'),
        ('source-change-not-observed', 'decode_pool.rs', 'let _ = load.watch().while_current(resp.closed()).await;',
         'let _ = load; resp.closed().await;', TEST + 'queued_source_change_wakes_without_waiting_for_cpu_or_local_admission'),
        ('running-cancel-flag-lost', 'decode_pool.rs', 'self.0.store(true, Ordering::Release);',
         'self.0.store(false, Ordering::Release);', TEST + 'running_cancel_keeps_memory_and_slot_until_the_cpu_owner_actually_retires'),
        ('local-slot-returned-early', 'decode_pool.rs', 'resources.slot = Some(slot);',
         'drop(slot);', TEST + 'running_cancel_keeps_memory_and_slot_until_the_cpu_owner_actually_retires'),
        ('host-close-not-watched', 'decode_pool.rs', '_ = cpu.closed() =>',
         '_ = std::future::pending::<()>() =>', TEST + 'host_close_releases_waiting_input_without_a_local_slot'),
        ('late-retirement-not-awaited', 'decode_pool.rs', 'retired.wait().await;',
         'let _ = &retired;', TEST + 'host_close_waits_for_late_native_retirement_before_publishing_error'),
        ('job-bytes-not-counted', 'decode_pool.rs', 'self.pending.try_reserve(job.queued_bytes())',
         'self.pending.try_reserve(0)', PARENT + 'decode_job_queue_rejects_limit_plus_one_encoded_byte'),
        ('job-count-limit-raised', 'decode_pool.rs', 'max_items: DECODE_JOB_QUEUE_CAPACITY,',
         'max_items: DECODE_JOB_QUEUE_CAPACITY + 1,', PARENT + 'decode_job_and_result_transports_are_bounded_and_nonblocking'),
        ('memory-admission-bypassed', 'decode_pool.rs', 'budget.try_reserve().ok_or_else(|| {',
         'Arc::new(DecodeInFlightUsage::new(2, 1)).try_reserve().ok_or_else(|| {', PARENT + 'every_decode_reserves_the_shared_in_flight_peak_budget_before_decoding'),
        ('submitted-rate-discarded', 'decode_pool.rs', 'Some(self.target)',
         'Some(44100)', TEST + 'scheduled_formats_preserve_every_pcm_sample_while_refill_and_async_work_progress'),
        ('finished-pcm-silenced', 'decode_pool.rs', 'let audio = self.job.take().unwrap().finish()?;',
         'let mut audio = self.job.take().unwrap().finish()?; audio.samples.fill(0.);', TEST + 'scheduled_formats_preserve_every_pcm_sample_while_refill_and_async_work_progress'),
    ]


def main(work: Path, target: Path, controls_only: bool):
    whole = product.whole
    package, env, frozen = whole.support.clock.prepare(work, target, whole.extra_inputs())
    files = {name: package / 'src' / name for name in ['decode_pool.rs', 'audio_thread.rs']}
    originals = {name: p.read_text() for name, p in files.items()}
    controls, programs, profiles = [], [], {}
    for profile in [] if controls_only else ['release', 'debug']:
        label = 'positive-' + profile
        binary = product.src.build(package, work, env, label, profile == 'release')
        log = work / (label + '-tests.log')
        assert whole.support.clock.common.run([str(binary)], log, package, env, timeout=900) == 0, log
        assert 'test result: ok. 567 passed; 0 failed; 2 ignored;' in log.read_text(), log
        profiles[profile] = dict(passed=567, ignored=2)
        programs.append(dict(name=label, sha256=whole.support.codecs.sha(binary), fresh_rust=True, pruned=False))
    for name, path, before, after, test in declarations():
        assert originals[path].count(before) == 1, (name, 'anchor drift', before)
        try:
            files[path].write_text(originals[path].replace(before, after))
            binary = product.src.build(package, work, env, name, True)
            log = work / (name + '-test.log')
            status = whole.support.clock.common.run([str(binary), '--exact', test, '--nocapture'], log, package, env, timeout=30)
            assert status == 101 and all(s in log.read_text() for s in ['running 1 test', 'panicked at', '1 failed']), (name, status, log)
            digest = whole.support.codecs.sha(binary)
            controls.append(dict(name=name, test=test, exit=status, binary_sha256=digest))
            programs.append(dict(name=name, sha256=digest, fresh_rust=True, pruned=True))
            binary.unlink()
            (work / 'completed-controls.json').write_text(json.dumps(controls, indent=2) + '\n')
            print('executed assertion rejected:', name, flush=True)
        finally:
            files[path].write_text(originals[path])
    whole.verify_inputs(work, frozen)
    root_src = whole.ROOT / 'engine/crates/audio/src'
    assert all(p.read_bytes() == (package / 'src' / p.relative_to(root_src)).read_bytes() for p in root_src.rglob('*') if p.is_file())
    result = dict(profiles=profiles, controls=controls, compiled_controls=len(controls), programs=programs,
                  root_inputs_unchanged=True, source_copies_restored=True, production_pool_migrated=True,
                  no_new_executor=True, device_or_shipping_acceptance=False)
    (work / 'summary.json').write_text(json.dumps(result, indent=2) + '\n')
    print('shared decode dispatch verified', profiles, len(controls), flush=True)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--target-dir', type=Path, required=True)
    parser.add_argument('--controls-only', action='store_true')
    args = parser.parse_args()
    main(args.output, args.target_dir, args.controls_only)
