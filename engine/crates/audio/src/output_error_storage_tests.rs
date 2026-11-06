//! Physical CPAL metadata, real final-reference free, and unchanged error/PCM flow.
use super::super::shared_stream_errors;
use super::*;
use crate::limits::AudioAggregateLedger;
use crate::node_retirement::HANDOFF_BYTES;
use crate::output::{AudioSync, OutputCallback, RING_BUFFER_FRAMES, ring};
use cpal::ErrorKind as Kind;
use ringbuf::traits::{Observer, Producer};
use shared::error::{EngineError, ErrorCode};
use std::{
    cell::Cell,
    sync::{
        Barrier,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
const LIMIT: usize = 64 << 20;
fn wait(mut ready: impl FnMut() -> bool) {
    let at = Instant::now();
    while !ready() {
        assert!(
            at.elapsed() < Duration::from_secs(15),
            "CPAL error disposal did not complete"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}
fn setup() -> (
    migo_io::scheduler::IoScheduler,
    Host,
    Arc<AudioAggregateLedger>,
) {
    let scheduler = migo_io::scheduler::IoScheduler::new(1);
    let ledger = Arc::new(AudioAggregateLedger::new(LIMIT));
    let host = Host::new(scheduler.audio_cpu(), ledger.clone(), 2).unwrap();
    (scheduler, host, ledger)
}
#[test]
fn actual_arc_cell_layout_and_one_physical_charge_match_allocator() {
    let bytes = Reference::bytes();
    assert!(bytes > std::mem::size_of::<StreamErrors>() && bytes < 4096);
    let begin = migo_alloc_probe::thread_counts();
    let raw = Arc::new(Data {
        state: StreamErrors::new().unwrap(),
        before_refund: Witness::default(),
        _permit: None,
        after_refund: Witness::default(),
    });
    let made = migo_alloc_probe::thread_counts();
    drop(raw);
    let end = migo_alloc_probe::thread_counts();
    assert_eq!(
        made.bytes_allocated - begin.bytes_allocated,
        bytes as u64,
        "CPAL error Arc layout differs from allocation"
    );
    assert_eq!(end.bytes_freed - made.bytes_freed, bytes as u64);
    let (_scheduler, host, ledger) = setup();
    let before = migo_alloc_probe::thread_counts();
    let errors = shared_stream_errors(&host).unwrap();
    let after = migo_alloc_probe::thread_counts();
    assert_eq!(
        ledger.used_bytes(),
        HANDOFF_BYTES + bytes,
        "CPAL error cell omitted physical credit"
    );
    assert_eq!(
        after.bytes_allocated - before.bytes_allocated,
        bytes as u64,
        "CPAL error state was copied or overallocated"
    );
    assert!(!errors.failed.load(Ordering::Acquire));
    assert_eq!(errors.pending.swap(None), None);
    drop(errors);
    assert_eq!(ledger.used_bytes(), HANDOFF_BYTES);
    drop(host);
    wait(|| ledger.used_bytes() == 0);
}
#[test]
fn refusal_precedes_error_factory_and_arc_allocation() {
    let (_scheduler, host, ledger) = setup();
    let peer = ledger
        .try_reserve(LIMIT - ledger.used_bytes(), "CPAL refusal peer")
        .unwrap();
    let calls = Cell::new(0);
    let before = migo_alloc_probe::thread_counts();
    let control = host.reserve_inline(Reference::bytes());
    let after = migo_alloc_probe::thread_counts();
    assert!(control.is_err());
    let expected = after.allocations - before.allocations;
    let before = migo_alloc_probe::thread_counts();
    let rejected = Reference::new(Some(&host), || {
        calls.set(calls.get() + 1);
        StreamErrors::new()
    });
    let after = migo_alloc_probe::thread_counts();
    assert!(
        rejected.is_err(),
        "CPAL error cell bypassed physical admission"
    );
    assert_eq!(calls.get(), 0, "CPAL error factory ran before admission");
    assert_eq!(
        after.allocations - before.allocations,
        expected,
        "CPAL error Arc allocated before admission"
    );
    assert_eq!(ledger.used_bytes(), LIMIT);
    drop((peer, control, rejected, host));
    wait(|| ledger.used_bytes() == 0);
}
#[test]
fn factory_failure_refunds_reserved_credit_without_allocating_an_arc() {
    let (_scheduler, host, ledger) = setup();
    let error = EngineError::from_detail(ErrorCode::Unsupported, "CPAL sentinel factory");
    let expected = format!("{error:?}");
    let calls = Cell::new(0);
    let before = migo_alloc_probe::thread_counts();
    let rejected = Reference::new(Some(&host), || {
        calls.set(calls.get() + 1);
        Err(error)
    });
    let after = migo_alloc_probe::thread_counts();
    assert_eq!(calls.get(), 1);
    assert_eq!(
        ledger.used_bytes(),
        HANDOFF_BYTES,
        "CPAL failed factory leaked physical credit"
    );
    assert_eq!(
        after.allocations - before.allocations,
        0,
        "CPAL failed factory allocated an Arc"
    );
    assert_eq!(format!("{:?}", rejected.err().unwrap()), expected);
    drop(host);
    wait(|| ledger.used_bytes() == 0);
}
#[test]
fn rejected_error_replacement_preserves_old_state_pointer_and_credit() {
    let (_scheduler, host, ledger) = setup();
    let old = shared_stream_errors(&host).unwrap();
    old.record(Kind::DeviceChanged);
    let pointer = &*old as *const StreamErrors as usize;
    let state = (old.failed.load(Ordering::Acquire), old.pending.load());
    let peer = ledger
        .try_reserve(LIMIT - ledger.used_bytes(), "CPAL replacement peer")
        .unwrap();
    let candidate = shared_stream_errors(&host);
    assert!(
        candidate.is_err(),
        "CPAL replacement ignored physical refusal"
    );
    assert_eq!(
        (old.failed.load(Ordering::Acquire), old.pending.load()),
        state,
        "CPAL refusal changed old error state"
    );
    assert_eq!(&*old as *const StreamErrors as usize, pointer);
    assert_eq!(ledger.used_bytes(), LIMIT);
    drop((candidate, peer, old, host));
    wait(|| ledger.used_bytes() == 0);
}
#[test]
fn full_budget_closed_cpu_clones_share_coalesced_error_state_without_heap() {
    let (scheduler, host, ledger) = setup();
    let errors = shared_stream_errors(&host).unwrap();
    let peer = ledger
        .try_reserve(LIMIT - ledger.used_bytes(), "CPAL callback peer")
        .unwrap();
    scheduler.close();
    let before = migo_alloc_probe::thread_counts();
    for n in 0..512 {
        let other = errors.clone();
        let kind = match n % 3 {
            0 => Kind::Xrun,
            1 => Kind::DeviceChanged,
            _ => Kind::RealtimeDenied,
        };
        other.record(kind);
        assert_eq!(
            errors.pending.swap(None),
            Some(kind),
            "CPAL clone lost coalesced pending event"
        );
        assert!(
            !errors.failed.load(Ordering::Acquire),
            "CPAL recoverable event killed the live stream"
        );
    }
    let other = errors.clone();
    other.record(Kind::StreamInvalidated);
    other.record(Kind::Xrun);
    assert!(
        errors.failed.load(Ordering::Acquire),
        "CPAL fatal event did not persist across a later recoverable event"
    );
    assert_eq!(errors.pending.swap(None), Some(Kind::Xrun));
    drop(other);
    let after = migo_alloc_probe::thread_counts();
    assert_eq!(
        (
            after.allocations - before.allocations,
            after.reallocations - before.reallocations
        ),
        (0, 0),
        "CPAL error callback clone allocated state"
    );
    assert_eq!(
        ledger.used_bytes(),
        LIMIT,
        "CPAL error clone changed physical credit"
    );
    drop((peer, errors, host));
    wait(|| ledger.used_bytes() == 0);
}
#[test]
fn last_foreign_callback_frees_real_cell_before_credit_refund_without_allocation() {
    let (s, h, l) = setup();
    let bytes = Reference::bytes();
    let mut sync = Reference::new(Some(&h), StreamErrors::new).unwrap();
    let base = Arc::new(AtomicU64::new(0));
    let seen = Arc::new(AtomicUsize::new(0));
    let valid = Arc::new(AtomicBool::new(true));
    let (a, b, c, d) = (base.clone(), seen.clone(), valid.clone(), l.clone());
    let before = Arc::new(move || {
        c.fetch_and(
            d.used_bytes() == HANDOFF_BYTES + bytes
                && migo_alloc_probe::thread_counts().bytes_freed - a.load(Ordering::Acquire)
                    >= bytes as u64,
            Ordering::AcqRel,
        );
        b.fetch_add(1, Ordering::AcqRel);
    });
    let (a, b, c, d) = (base.clone(), seen.clone(), valid.clone(), l.clone());
    let after = Arc::new(move || {
        c.fetch_and(
            d.used_bytes() == HANDOFF_BYTES
                && migo_alloc_probe::thread_counts().bytes_freed - a.load(Ordering::Acquire)
                    >= bytes as u64,
            Ordering::AcqRel,
        );
        b.fetch_add(1, Ordering::AcqRel);
    });
    sync.observe(before, after);
    let callback = sync.clone();
    drop(sync);
    assert_eq!(l.used_bytes(), HANDOFF_BYTES + bytes);
    s.close();
    std::thread::spawn(move || {
        base.store(
            migo_alloc_probe::thread_counts().bytes_freed,
            Ordering::Release,
        );
        let begin = migo_alloc_probe::thread_counts();
        drop(callback);
        let end = migo_alloc_probe::thread_counts();
        assert_eq!(
            (
                end.allocations - begin.allocations,
                end.reallocations - begin.reallocations
            ),
            (0, 0),
            "CPAL error final callback allocated cleanup"
        );
    })
    .join()
    .unwrap();
    assert_eq!(
        seen.load(Ordering::Acquire),
        2,
        "CPAL error final state was not actually destroyed"
    );
    assert!(
        valid.load(Ordering::Acquire),
        "CPAL error refund preceded actual Arc/state destruction"
    );
    assert_eq!(l.used_bytes(), HANDOFF_BYTES);
    drop(h);
    wait(|| l.used_bytes() == 0);
}
#[test]
fn concurrent_final_handles_drop_one_cell_and_keep_credit_through_state_drop() {
    let (s, h, l) = setup();
    let mut sync = Reference::new(Some(&h), StreamErrors::new).unwrap();
    let before = Arc::new(AtomicUsize::new(0));
    let after = Arc::new(AtomicUsize::new(0));
    let valid = Arc::new(AtomicBool::new(true));
    let (a, b, c) = (before.clone(), valid.clone(), l.clone());
    let (d, e, f) = (after.clone(), valid.clone(), l.clone());
    let bytes = Reference::bytes();
    sync.observe(
        Arc::new(move || {
            b.fetch_and(c.used_bytes() == HANDOFF_BYTES + bytes, Ordering::AcqRel);
            a.fetch_add(1, Ordering::AcqRel);
        }),
        Arc::new(move || {
            e.fetch_and(f.used_bytes() == HANDOFF_BYTES, Ordering::AcqRel);
            d.fetch_add(1, Ordering::AcqRel);
        }),
    );
    let other = sync.clone();
    s.close();
    let gate = Arc::new(Barrier::new(3));
    let a = gate.clone();
    let b = gate.clone();
    let first = std::thread::spawn(move || {
        a.wait();
        drop(sync);
    });
    let last = std::thread::spawn(move || {
        b.wait();
        drop(other);
    });
    assert_eq!(l.used_bytes(), HANDOFF_BYTES + bytes);
    gate.wait();
    first.join().unwrap();
    last.join().unwrap();
    assert_eq!(before.load(Ordering::Acquire), 1);
    assert_eq!(after.load(Ordering::Acquire), 1);
    assert!(
        valid.load(Ordering::Acquire),
        "CPAL error concurrent final refund lost physical owner"
    );
    drop(h);
    wait(|| l.used_bytes() == 0);
}
#[test]
fn callback_keeps_physical_cell_after_producer_and_host_are_destroyed() {
    let (_scheduler, host, ledger) = setup();
    let producer = shared_stream_errors(&host).unwrap();
    let callback = producer.clone();
    drop(producer);
    drop(host);
    wait(|| ledger.used_bytes() == Reference::bytes());
    callback.record(Kind::BackendError);
    assert!(callback.failed.load(Ordering::Acquire));
    assert_eq!(callback.pending.swap(None), Some(Kind::BackendError));
    assert_eq!(ledger.used_bytes(), Reference::bytes());
    drop(callback);
    wait(|| ledger.used_bytes() == 0);
}
#[test]
fn closed_host_refuses_new_errors_and_existing_callback_still_records() {
    let (scheduler, host, ledger) = setup();
    let errors = shared_stream_errors(&host).unwrap();
    let used = ledger.used_bytes();
    scheduler.close();
    assert!(
        shared_stream_errors(&host).is_err(),
        "CPAL error cell admitted after host close"
    );
    errors.record(Kind::StreamInvalidated);
    assert!(errors.failed.load(Ordering::Acquire));
    assert_eq!(errors.pending.swap(None), Some(Kind::StreamInvalidated));
    assert_eq!(ledger.used_bytes(), used);
    drop((errors, host));
    wait(|| ledger.used_bytes() == 0);
}
fn tag(kind: Option<Kind>) -> u64 {
    match kind {
        None => 0,
        Some(Kind::Xrun) => 1,
        Some(Kind::DeviceChanged) => 2,
        Some(Kind::RealtimeDenied) => 3,
        Some(Kind::DeviceNotAvailable) => 4,
        Some(Kind::StreamInvalidated) => 5,
        Some(Kind::BackendError) => 6,
        _ => 99,
    }
}
#[test]
fn complete_error_callback_pcm_and_atomic_state_trace() {
    let (_scheduler, host, ledger) = setup();
    for channels in [1usize, 2, 32, 64] {
        for mode in 0..3 {
            let (mut producer, consumer) = ring::new(RING_BUFFER_FRAMES * channels, None).unwrap();
            let sync = AudioSync::with_host(Some(&host)).unwrap();
            sync.configure_device(channels, 192);
            let mut callback = OutputCallback::new(consumer, sync.clone(), 0);
            let errors = shared_stream_errors(&host).unwrap();
            for (round, frames) in [0usize, 1, 17, 127, 128, 129, 511, 512, 513, 257, 4097, 8193]
                .into_iter()
                .enumerate()
            {
                let len = frames * channels + channels - 1;
                sync.set_callback_wakeup_enabled(round % 2 == 1);
                let input: Vec<_> = (0..(len / 2 + 17).min(producer.vacant_len()))
                    .map(|n| match n % 7 {
                        0 => -2.0,
                        1 => -0.0,
                        2 => 2.0,
                        _ => (n % 97) as f32 / 64.0 - 0.75,
                    })
                    .collect();
                assert_eq!(producer.push_slice(&input), input.len());
                let mut bits = Vec::new();
                match mode {
                    0 => {
                        let mut out = vec![0.375f32; len];
                        callback.render_native(&mut out);
                        bits.extend(out.iter().map(|v| v.to_bits()));
                    }
                    1 => {
                        let mut out = vec![123i16; len];
                        callback.render_converted(&mut out);
                        bits.extend(out.iter().map(|v| u32::from(*v as u16)));
                    }
                    _ => {
                        let mut out = vec![123u16; len];
                        callback.render_converted(&mut out);
                        bits.extend(out.iter().map(|v| u32::from(*v)));
                    }
                }
                let kind = match round % 6 {
                    0 => Kind::Xrun,
                    1 => Kind::DeviceChanged,
                    2 => Kind::RealtimeDenied,
                    3 => Kind::DeviceNotAvailable,
                    4 => Kind::StreamInvalidated,
                    _ => Kind::BackendError,
                };
                errors.record(kind);
                let pending = errors.pending.swap(None);
                assert_eq!(
                    pending,
                    Some(kind),
                    "CPAL error callback lost the complete event"
                );
                assert_eq!(
                    errors.failed.load(Ordering::Acquire),
                    round >= 3,
                    "CPAL error callback changed fatal/recoverable state"
                );
                sync.refresh_device_policy();
                let state = [
                    sync.buffer_level() as u64,
                    sync.max_callback() as u64,
                    sync.check_and_clear() as u64,
                    sync.software_underrun_count() as u64,
                    sync.inner.policy.get().unwrap().low() as u64,
                    sync.inner.policy.get().unwrap().high() as u64,
                    errors.failed.load(Ordering::Acquire) as u64,
                    tag(pending),
                    tag(errors.pending.swap(None)),
                ];
                println!(
                    "MIGO_CPAL_ERROR_PCM {channels}:{mode}:{round} {:?} {:?}",
                    bits, state
                );
            }
            drop((producer, callback, sync, errors));
            assert_eq!(ledger.used_bytes(), HANDOFF_BYTES);
        }
    }
    drop(host);
    wait(|| ledger.used_bytes() == 0);
}
