//! Actual OHAudio flag cell, lifecycle and real Rust callback projections.
use super::super::{CallbackState, on_error, on_write_data, shared_stream_error};
use super::*;
use crate::limits::AudioAggregateLedger;
use crate::node_retirement::HANDOFF_BYTES;
use crate::output::{AudioSync, OutputCallback, RING_BUFFER_FRAMES, ring};
use ringbuf::traits::{Observer, Producer};
use std::{
    cell::Cell,
    collections::VecDeque,
    ptr,
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
            "OHAudio flag disposal did not complete"
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
#[test]
fn actual_arc_cell_layout_and_one_physical_charge_match_allocator() {
    let bytes = Reference::bytes();
    assert!(bytes > std::mem::size_of::<AtomicBool>() && bytes < 4096);
    let before = migo_alloc_probe::thread_counts();
    let raw = Arc::new(Data {
        state: AtomicBool::new(false),
        before_refund: Witness::default(),
        _permit: None,
        after_refund: Witness::default(),
    });
    let made = migo_alloc_probe::thread_counts();
    drop(raw);
    let after = migo_alloc_probe::thread_counts();
    assert_eq!(
        made.bytes_allocated - before.bytes_allocated,
        bytes as u64,
        "OHAudio flag Arc layout differs from allocation"
    );
    assert_eq!(after.bytes_freed - made.bytes_freed, bytes as u64);
    let (_s, h, l) = setup();
    let before = migo_alloc_probe::thread_counts();
    let flag = shared_stream_error(&h).unwrap();
    let after = migo_alloc_probe::thread_counts();
    assert_eq!(
        l.used_bytes(),
        HANDOFF_BYTES + bytes,
        "OHAudio flag cell omitted physical credit"
    );
    assert_eq!(
        after.bytes_allocated - before.bytes_allocated,
        bytes as u64,
        "OHAudio flag state copied or overallocated"
    );
    assert!(!flag.load(Ordering::Acquire));
    drop(flag);
    assert_eq!(l.used_bytes(), HANDOFF_BYTES);
    drop(h);
    wait(|| l.used_bytes() == 0);
}
#[test]
fn refusal_precedes_flag_factory_and_arc_allocation() {
    let (_s, h, l) = setup();
    let peer = l
        .try_reserve(LIMIT - l.used_bytes(), "OHAudio flag peer")
        .unwrap();
    let calls = Cell::new(0);
    let before = migo_alloc_probe::thread_counts();
    let control = h.reserve_inline(Reference::bytes());
    let after = migo_alloc_probe::thread_counts();
    assert!(control.is_err());
    let expected = after.allocations - before.allocations;
    let before = migo_alloc_probe::thread_counts();
    let rejected = Reference::new(Some(&h), || {
        calls.set(calls.get() + 1);
        AtomicBool::new(false)
    });
    let after = migo_alloc_probe::thread_counts();
    assert!(
        rejected.is_err(),
        "OHAudio flag bypassed physical admission"
    );
    assert_eq!(calls.get(), 0, "OHAudio flag factory ran before admission");
    assert_eq!(
        after.allocations - before.allocations,
        expected,
        "OHAudio flag Arc allocated before admission"
    );
    assert_eq!(l.used_bytes(), LIMIT);
    drop((peer, control, rejected, h));
    wait(|| l.used_bytes() == 0);
}
#[test]
fn refused_flag_replacement_preserves_old_state_pointer_and_credit() {
    let (_s, h, l) = setup();
    let old = shared_stream_error(&h).unwrap();
    old.store(true, Ordering::Release);
    let pointer = &*old as *const AtomicBool as usize;
    let peer = l
        .try_reserve(LIMIT - l.used_bytes(), "OHAudio replacement peer")
        .unwrap();
    let candidate = shared_stream_error(&h);
    assert!(
        candidate.is_err(),
        "OHAudio replacement ignored physical refusal"
    );
    assert!(
        old.load(Ordering::Acquire),
        "OHAudio refusal changed old error flag"
    );
    assert_eq!(&*old as *const AtomicBool as usize, pointer);
    assert_eq!(l.used_bytes(), LIMIT);
    drop((candidate, peer, old, h));
    wait(|| l.used_bytes() == 0);
}
#[test]
fn full_budget_closed_cpu_clones_share_flag_without_heap() {
    let (s, h, l) = setup();
    let flag = shared_stream_error(&h).unwrap();
    let peer = l
        .try_reserve(LIMIT - l.used_bytes(), "OHAudio clone peer")
        .unwrap();
    s.close();
    let before = migo_alloc_probe::thread_counts();
    for n in 0..512 {
        let other = flag.clone();
        other.store(n % 2 == 1, Ordering::Release);
        assert_eq!(
            flag.load(Ordering::Acquire),
            n % 2 == 1,
            "OHAudio flag clone lost shared state"
        );
    }
    let after = migo_alloc_probe::thread_counts();
    assert_eq!(
        (
            after.allocations - before.allocations,
            after.reallocations - before.reallocations
        ),
        (0, 0),
        "OHAudio flag clone allocated state"
    );
    assert_eq!(
        l.used_bytes(),
        LIMIT,
        "OHAudio flag clone changed physical credit"
    );
    drop((peer, flag, h));
    wait(|| l.used_bytes() == 0);
}
#[test]
fn last_foreign_callback_frees_real_cell_before_credit_refund_without_allocation() {
    let (s, h, l) = setup();
    let bytes = Reference::bytes();
    let mut sync = Reference::new(Some(&h), || AtomicBool::new(false)).unwrap();
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
            "OHAudio flag final callback allocated cleanup"
        );
    })
    .join()
    .unwrap();
    assert_eq!(
        seen.load(Ordering::Acquire),
        2,
        "OHAudio flag final state was not actually destroyed"
    );
    assert!(
        valid.load(Ordering::Acquire),
        "OHAudio flag refund preceded actual Arc/state destruction"
    );
    assert_eq!(l.used_bytes(), HANDOFF_BYTES);
    drop(h);
    wait(|| l.used_bytes() == 0);
}
#[test]
fn concurrent_final_handles_drop_one_cell_and_keep_credit_through_state_drop() {
    let (s, h, l) = setup();
    let mut sync = Reference::new(Some(&h), || AtomicBool::new(false)).unwrap();
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
        "OHAudio flag concurrent final refund lost physical owner"
    );
    drop(h);
    wait(|| l.used_bytes() == 0);
}
#[test]
fn retained_callback_state_keeps_flag_credit_after_owner_and_host_drop() {
    let (_s, h, l) = setup();
    let flag = shared_stream_error(&h).unwrap();
    let (_producer, consumer) = ring::new(512, None).unwrap();
    let state = Box::into_raw(Box::new(CallbackState {
        render: OutputCallback::new(consumer, AudioSync::new(), 0),
        stream_error: flag.clone(),
    }));
    drop((flag, h));
    wait(|| l.used_bytes() == Reference::bytes());
    // SAFETY: this fixture retains the entire state as the release-failure path does.
    unsafe { on_error(ptr::null_mut(), state.cast(), -7) };
    assert!(
        unsafe { &(*state).stream_error }.load(Ordering::Acquire),
        "OHAudio native error callback lost the flag"
    );
    assert_eq!(l.used_bytes(), Reference::bytes());
    // SAFETY: all fixture callback invocations ended; release the retained state once.
    drop(unsafe { Box::from_raw(state) });
    wait(|| l.used_bytes() == 0);
}
#[test]
fn closed_host_refuses_new_flag_but_existing_flag_remains_live() {
    let (s, h, l) = setup();
    let flag = shared_stream_error(&h).unwrap();
    let used = l.used_bytes();
    s.close();
    assert!(
        shared_stream_error(&h).is_err(),
        "OHAudio flag admitted after host close"
    );
    flag.store(true, Ordering::Release);
    assert!(flag.load(Ordering::Acquire));
    assert_eq!(l.used_bytes(), used);
    drop((flag, h));
    wait(|| l.used_bytes() == 0);
}
#[test]
fn actual_callbacks_project_disjoint_fields_under_full_budget_and_close() {
    let (s, h, l) = setup();
    let flag = shared_stream_error(&h).unwrap();
    let (mut producer, consumer) = ring::new(1024, None).unwrap();
    let sync = AudioSync::new();
    sync.configure_device(2, 128);
    let mut state = Box::new(CallbackState {
        render: OutputCallback::new(consumer, sync, 0),
        stream_error: flag.clone(),
    });
    let address = (&mut *state as *mut CallbackState) as usize;
    let peer = l
        .try_reserve(LIMIT - l.used_bytes(), "OHAudio callback peer")
        .unwrap();
    s.close();
    assert_eq!(producer.push_slice(&[0.125; 3]), 3);
    let mut partial = [0.75f32; 3];
    // SAFETY: valid retained state/buffer; byte count 9 is deliberately not whole f32s.
    unsafe {
        on_write_data(
            ptr::null_mut(),
            address as *mut _,
            partial.as_mut_ptr().cast(),
            9,
        )
    };
    assert_eq!(
        partial, [0.75; 3],
        "OHAudio partial sample byte length was accepted"
    );
    assert_eq!(producer.occupied_len(), 3);
    let before = migo_alloc_probe::thread_counts();
    unsafe {
        on_write_data(
            ptr::null_mut(),
            address as *mut _,
            partial.as_mut_ptr().cast(),
            12,
        )
    };
    let after = migo_alloc_probe::thread_counts();
    assert_eq!(partial, [0.125; 3]);
    assert_eq!(
        (
            after.allocations - before.allocations,
            after.reallocations - before.reallocations
        ),
        (0, 0),
        "OHAudio first native callback allocated"
    );
    let gate = Arc::new(Barrier::new(3));
    let a = gate.clone();
    let b = gate.clone();
    let writer = std::thread::spawn(move || {
        a.wait();
        let input = [0.25f32; 256];
        let mut out = [0.0f32; 256];
        let before = migo_alloc_probe::thread_counts();
        for _ in 0..128 {
            assert_eq!(producer.push_slice(&input), 256);
            unsafe {
                on_write_data(
                    ptr::null_mut(),
                    address as *mut _,
                    out.as_mut_ptr().cast(),
                    1024,
                )
            };
            assert_eq!(out, input, "OHAudio concurrent callback PCM changed");
        }
        let after = migo_alloc_probe::thread_counts();
        assert_eq!(
            (
                after.allocations - before.allocations,
                after.reallocations - before.reallocations
            ),
            (0, 0),
            "OHAudio steady native callback allocated"
        );
    });
    let errors = std::thread::spawn(move || {
        b.wait();
        for _ in 0..32 {
            unsafe { on_error(ptr::null_mut(), address as *mut _, -9) };
        }
    });
    gate.wait();
    writer.join().unwrap();
    errors.join().unwrap();
    assert!(
        flag.load(Ordering::Acquire),
        "OHAudio native error callback lost the flag"
    );
    assert_eq!(l.used_bytes(), LIMIT);
    drop((peer, state, flag, h));
    wait(|| l.used_bytes() == 0);
}
#[test]
fn complete_native_callback_pcm_and_error_state_trace() {
    let (_s, h, l) = setup();
    for channels in [1usize, 2, 32, 64] {
        let (mut producer, consumer) = ring::new(RING_BUFFER_FRAMES * channels, None).unwrap();
        let sync = AudioSync::with_host(Some(&h)).unwrap();
        sync.configure_device(channels, 192);
        let flag = shared_stream_error(&h).unwrap();
        let mut state = Box::new(CallbackState {
            render: OutputCallback::new(consumer, sync.clone(), 0),
            stream_error: flag.clone(),
        });
        let raw = &mut *state as *mut CallbackState;
        let mut oracle = VecDeque::new();
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
            oracle.extend(input);
            let mut out = vec![0.375f32; len];
            // SAFETY: live state and a whole-f32 initialized buffer; one serialized data call.
            unsafe {
                on_write_data(
                    ptr::null_mut(),
                    raw.cast(),
                    out.as_mut_ptr().cast(),
                    (len * 4) as i32,
                )
            };
            let expected: Vec<_> = (0..len)
                .map(|_| oracle.pop_front().unwrap_or(0.0f32).to_bits())
                .collect();
            let bits: Vec<_> = out.iter().map(|v| v.to_bits()).collect();
            assert_eq!(
                bits, expected,
                "OHAudio complete native callback PCM changed"
            );
            if round % 3 == 2 {
                unsafe { on_error(ptr::null_mut(), raw.cast(), -7) };
            }
            assert_eq!(
                flag.load(Ordering::Acquire),
                round >= 2,
                "OHAudio complete native error state changed"
            );
            sync.refresh_device_policy();
            let values = [
                sync.buffer_level() as u64,
                sync.max_callback() as u64,
                sync.check_and_clear() as u64,
                sync.software_underrun_count() as u64,
                sync.inner.policy.get().unwrap().low() as u64,
                sync.inner.policy.get().unwrap().high() as u64,
                flag.load(Ordering::Acquire) as u64,
                producer.occupied_len() as u64,
            ];
            println!(
                "MIGO_OHAUDIO_FLAG_PCM {channels}:{round} {:?} {:?}",
                bits, values
            );
        }
        drop((producer, state, sync, flag));
        assert_eq!(l.used_bytes(), HANDOFF_BYTES);
    }
    drop(h);
    wait(|| l.used_bytes() == 0);
}
