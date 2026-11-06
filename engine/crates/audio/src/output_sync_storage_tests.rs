//! Actual Arc layout, quota-before-factory and the final callback's real free.
use super::*;
use crate::limits::AudioAggregateLedger;
use crate::node_retirement::HANDOFF_BYTES;
use crate::output::{
    AudioSync, AudioSyncState, OutputCallback, RING_BUFFER_FRAMES, SilentOutput, ring,
};
use ringbuf::traits::{Observer, Producer};
use shared::channel::ThreadWakeup;
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
            "AudioSync disposal did not complete"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}
fn setup() -> (
    migo_io::scheduler::IoScheduler,
    Host,
    Arc<AudioAggregateLedger>,
) {
    let s = migo_io::scheduler::IoScheduler::new(1);
    let l = Arc::new(AudioAggregateLedger::new(LIMIT));
    let h = Host::new(s.audio_cpu(), l.clone(), 2).unwrap();
    (s, h, l)
}
fn initial() -> AudioSyncState {
    AudioSyncState {
        needs_data: AtomicBool::new(false),
        buffer_level: AtomicUsize::new(0),
        max_callback: AtomicUsize::new(0),
        callback_wakeup_enabled: AtomicBool::new(false),
        notifier: std::sync::OnceLock::new(),
        policy: std::sync::OnceLock::new(),
        software_underrun: AtomicBool::new(false),
        software_underruns: AtomicUsize::new(0),
    }
}
fn state(s: &AudioSync) -> [u64; 8] {
    [
        s.inner.needs_data.load(Ordering::Acquire) as u64,
        s.buffer_level() as u64,
        s.max_callback() as u64,
        s.inner.callback_wakeup_enabled.load(Ordering::Acquire) as u64,
        s.inner.software_underrun.load(Ordering::Acquire) as u64,
        s.software_underrun_count() as u64,
        s.inner.policy.get().map_or(0, |p| p.low()) as u64,
        s.inner.policy.get().map_or(0, |p| p.high()) as u64,
    ]
}
#[test]
fn actual_arc_cell_layout_and_one_physical_charge_match_allocator() {
    let bytes = Reference::bytes();
    assert!(bytes > std::mem::size_of::<AudioSyncState>() && bytes < 4096);
    let begin = migo_alloc_probe::thread_counts();
    let raw = Arc::new(Data {
        state: initial(),
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
        "AudioSync Arc model differs from allocation"
    );
    assert_eq!(end.bytes_freed - made.bytes_freed, bytes as u64);
    let (_s, h, l) = setup();
    let before = migo_alloc_probe::thread_counts();
    let sync = AudioSync::with_host(Some(&h)).unwrap();
    let after = migo_alloc_probe::thread_counts();
    assert_eq!(
        l.used_bytes(),
        HANDOFF_BYTES + bytes,
        "AudioSync cell omitted physical credit"
    );
    assert_eq!(
        after.bytes_allocated - before.bytes_allocated,
        bytes as u64,
        "AudioSync copied or overallocated state"
    );
    assert_eq!(state(&sync), [0; 8]);
    drop(sync);
    assert_eq!(l.used_bytes(), HANDOFF_BYTES);
    drop(h);
    wait(|| l.used_bytes() == 0);
}
#[test]
fn refusal_precedes_state_factory_and_arc_allocation() {
    let (_s, h, l) = setup();
    let peer = l.try_reserve(LIMIT - l.used_bytes(), "sync peer").unwrap();
    let calls = Cell::new(0usize);
    let before = migo_alloc_probe::thread_counts();
    let control = h.reserve_inline(Reference::bytes());
    let end = migo_alloc_probe::thread_counts();
    assert!(control.is_err());
    let expected = end.allocations - before.allocations;
    let before = migo_alloc_probe::thread_counts();
    let rejected = Reference::new(Some(&h), || {
        calls.set(calls.get() + 1);
        initial()
    });
    let end = migo_alloc_probe::thread_counts();
    assert!(
        rejected.is_err(),
        "AudioSync metadata bypassed physical admission"
    );
    assert_eq!(
        calls.get(),
        0,
        "AudioSync state factory ran before admission"
    );
    assert_eq!(
        end.allocations - before.allocations,
        expected,
        "AudioSync Arc allocated before admission"
    );
    assert_eq!(l.used_bytes(), LIMIT);
    drop((peer, control, rejected, h));
    wait(|| l.used_bytes() == 0);
}
#[test]
fn rejected_new_sync_preserves_old_shared_state_and_original_credit() {
    let (_s, h, l) = setup();
    let old = AudioSync::with_host(Some(&h)).unwrap();
    old.configure_device(2, 192);
    old.observe_callback(960);
    old.update_level(177);
    old.signal_need_data();
    let before = state(&old);
    let pointer = &*old.inner as *const AudioSyncState as usize;
    let peer = l
        .try_reserve(LIMIT - l.used_bytes(), "sync replacement peer")
        .unwrap();
    let candidate = AudioSync::with_host(Some(&h));
    assert!(
        candidate.is_err(),
        "AudioSync replacement ignored physical refusal"
    );
    assert_eq!(state(&old), before, "AudioSync refusal changed old state");
    assert_eq!(&*old.inner as *const _ as usize, pointer);
    assert_eq!(l.used_bytes(), LIMIT);
    drop((candidate, peer, old, h));
    wait(|| l.used_bytes() == 0);
}
#[test]
fn clones_share_state_without_heap_or_new_credit_after_host_close() {
    let (s, h, l) = setup();
    let sync = AudioSync::with_host(Some(&h)).unwrap();
    sync.configure_device(2, 128);
    let peer = l
        .try_reserve(LIMIT - l.used_bytes(), "sync clone peer")
        .unwrap();
    s.close();
    let before = migo_alloc_probe::thread_counts();
    for n in 0..512 {
        let other = sync.clone();
        other.update_level(n);
        other.observe_callback(n);
        other.signal_need_data();
        assert_eq!(sync.buffer_level(), n, "AudioSync clone lost shared state");
        assert!(sync.check_and_clear());
    }
    let end = migo_alloc_probe::thread_counts();
    assert_eq!(
        (
            end.allocations - before.allocations,
            end.reallocations - before.reallocations
        ),
        (0, 0),
        "AudioSync clone allocated new state"
    );
    assert_eq!(
        l.used_bytes(),
        LIMIT,
        "AudioSync clone changed physical credit"
    );
    assert_eq!(sync.max_callback(), 511, "AudioSync lost callback maximum");
    drop((peer, sync, h));
    wait(|| l.used_bytes() == 0);
}
#[test]
fn last_foreign_callback_frees_real_cell_before_credit_refund_without_allocation() {
    let (s, h, l) = setup();
    let bytes = Reference::bytes();
    let mut sync = AudioSync::with_host(Some(&h)).unwrap();
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
    sync.observe_storage(before, after);
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
            "AudioSync final callback allocated cleanup"
        );
    })
    .join()
    .unwrap();
    assert_eq!(
        seen.load(Ordering::Acquire),
        2,
        "AudioSync final state was not actually destroyed"
    );
    assert!(
        valid.load(Ordering::Acquire),
        "AudioSync refund preceded actual Arc/state destruction"
    );
    assert_eq!(l.used_bytes(), HANDOFF_BYTES);
    drop(h);
    wait(|| l.used_bytes() == 0);
}
#[test]
fn concurrent_final_handles_drop_one_cell_and_keep_credit_through_state_drop() {
    let (s, h, l) = setup();
    let mut sync = AudioSync::with_host(Some(&h)).unwrap();
    let before = Arc::new(AtomicUsize::new(0));
    let after = Arc::new(AtomicUsize::new(0));
    let valid = Arc::new(AtomicBool::new(true));
    let (a, b, c) = (before.clone(), valid.clone(), l.clone());
    let (d, e, f) = (after.clone(), valid.clone(), l.clone());
    let bytes = Reference::bytes();
    sync.observe_storage(
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
        "AudioSync concurrent final refund lost physical owner"
    );
    drop(h);
    wait(|| l.used_bytes() == 0);
}
#[test]
fn silent_output_uses_admitted_sync_and_a_retained_sync_clone_keeps_its_credit() {
    let (s, h, l) = setup();
    let output = SilentOutput::with_host(Some(&h)).unwrap();
    let sync = output.sync().clone();
    let used = l.used_bytes();
    assert_eq!(
        sync.storage_bytes(),
        Reference::bytes(),
        "silent output omitted managed AudioSync"
    );
    drop(output);
    wait(|| l.used_bytes() == HANDOFF_BYTES + Reference::bytes());
    assert!(used > l.used_bytes());
    assert_eq!(sync.buffer_level(), 0);
    s.close();
    drop(sync);
    assert_eq!(l.used_bytes(), HANDOFF_BYTES);
    drop(h);
    wait(|| l.used_bytes() == 0);
}
#[test]
fn callback_policy_and_bound_notifier_work_under_full_budget_without_heap() {
    let (s, h, l) = setup();
    let wake = ThreadWakeup::new_audio();
    wake.bind_audio_thread();
    let sync = AudioSync::with_host(Some(&h)).unwrap();
    sync.configure_device(2, 192);
    sync.bind_wakeup(&wake);
    sync.set_callback_wakeup_enabled(true);
    let (mut producer, consumer) = ring::new(4096, None).unwrap();
    let mut callback = OutputCallback::new(consumer, sync.clone(), 0);
    let peer = l
        .try_reserve(LIMIT - l.used_bytes(), "sync signal peer")
        .unwrap();
    s.close();
    let input = [0.25; 256];
    let mut out = [0.0; 256];
    let before = migo_alloc_probe::thread_counts();
    for _ in 0..64 {
        assert_eq!(producer.push_slice(&input), 256);
        callback.render_native(&mut out);
        assert!(out.iter().all(|v| *v == 0.25));
        assert!(
            sync.check_and_clear(),
            "AudioSync callback lost refill signal"
        );
        assert!(
            wake.wait_timeout(Duration::ZERO),
            "AudioSync callback lost bound notifier"
        );
        sync.refresh_device_policy();
    }
    let end = migo_alloc_probe::thread_counts();
    assert_eq!(
        (
            end.allocations - before.allocations,
            end.reallocations - before.reallocations
        ),
        (0, 0),
        "managed AudioSync callback allocated"
    );
    assert_eq!(sync.software_underrun_count(), 0);
    assert_eq!(l.used_bytes(), LIMIT);
    drop((peer, callback, producer, sync, h));
    wait(|| l.used_bytes() == 0);
}
#[test]
fn closed_host_refuses_new_cell_but_existing_sync_still_signals() {
    let (s, h, l) = setup();
    let sync = AudioSync::with_host(Some(&h)).unwrap();
    let used = l.used_bytes();
    s.close();
    assert!(
        AudioSync::with_host(Some(&h)).is_err(),
        "AudioSync admitted new state after host close"
    );
    assert_eq!(l.used_bytes(), used);
    sync.signal_need_data();
    assert!(sync.check_and_clear());
    drop((sync, h));
    wait(|| l.used_bytes() == 0);
}
#[test]
fn complete_managed_callback_pcm_and_refill_state_trace() {
    let (_s, h, l) = setup();
    for channels in [1usize, 2, 32, 64] {
        for mode in 0..3 {
            let (mut producer, consumer) = ring::new(RING_BUFFER_FRAMES * channels, None).unwrap();
            let sync = AudioSync::with_host(Some(&h)).unwrap();
            sync.configure_device(channels, 192);
            let mut callback = OutputCallback::new(consumer, sync.clone(), 0);
            for (round, frames) in [0, 1, 17, 127, 128, 129, 511, 512, 513, 8193]
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
                sync.refresh_device_policy();
                println!(
                    "MIGO_OUTPUT_SYNC_PCM {channels}:{mode}:{round} {:?} {:?}",
                    bits,
                    state(&sync)
                );
            }
            drop((producer, callback, sync));
            assert_eq!(l.used_bytes(), HANDOFF_BYTES);
        }
    }
    drop(h);
    wait(|| l.used_bytes() == 0);
}
