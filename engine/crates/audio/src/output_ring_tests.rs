//! Real shared ring allocation, final callback reference and complete PCM.
use super::*;
use crate::limits::AudioAggregateLedger;
use crate::output::{AudioSync, OutputCallback, RING_BUFFER_FRAMES, SilentOutput};
use ringbuf::traits::{Consumer as _, Producer as _, Split};
use ringbuf::wrap::Wrap;
use std::sync::{
    Barrier, Condvar, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use std::time::{Duration, Instant};

#[path = "output_ring_reference.rs"]
mod before;
const LIMIT: usize = 64 << 20;

fn wait(mut ready: impl FnMut() -> bool) {
    let at = Instant::now();
    while !ready() {
        assert!(
            at.elapsed() < Duration::from_secs(15),
            "output ring CPU retirement did not complete"
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
#[derive(Default)]
struct Seen {
    count: AtomicUsize,
    wrong: AtomicBool,
}
fn hook(seen: &Arc<Seen>) -> Arc<dyn Fn() + Send + Sync> {
    let seen = seen.clone();
    Arc::new(move || {
        if !std::thread::current()
            .name()
            .unwrap_or("")
            .starts_with("Migo-IO-")
        {
            seen.wrong.store(true, Ordering::Release);
        }
        seen.count.fetch_add(1, Ordering::Release);
    })
}
struct Gate(Arc<(Mutex<bool>, Condvar)>);
impl Gate {
    fn open(&self) {
        *self.0.0.lock().unwrap() = true;
        self.0.1.notify_all();
    }
}
impl Drop for Gate {
    fn drop(&mut self) {
        self.open();
    }
}
struct Blocked {
    gate: Arc<(Mutex<bool>, Condvar)>,
    entered: Arc<AtomicBool>,
}
impl Drop for Blocked {
    fn drop(&mut self) {
        self.entered.store(true, Ordering::Release);
        let mut open = self.gate.0.lock().unwrap();
        while !*open {
            open = self.gate.1.wait(open).unwrap();
        }
    }
}
fn block(cpu: &migo_io::audio_cpu::AudioCpu) -> Gate {
    let gate = Gate(Arc::new((Mutex::new(false), Condvar::new())));
    let entered = Arc::new(AtomicBool::new(false));
    let value = Blocked {
        gate: gate.0.clone(),
        entered: entered.clone(),
    };
    let retirement = cpu.retirement();
    crate::streaming::get_stream_runtime().spawn(async move {
        retirement.retire(value).await;
    });
    wait(|| entered.load(Ordering::Acquire));
    gate
}
fn quote(host: &Host, ledger: &Arc<AudioAggregateLedger>, samples: usize) -> usize {
    let initial = ledger.used_bytes();
    let reserved = host
        .reserve_value_with_bytes::<HeapRb<f32>>(samples * 4 + cell_bytes())
        .unwrap();
    let bytes = ledger.used_bytes() - initial;
    drop(reserved);
    bytes
}

#[test]
fn actual_boxed_capacity_and_arc_cell_are_fully_charged() {
    let (_scheduler, host, ledger) = setup();
    let data = Data {
        ring: Reservation::unmanaged().finish(HeapRb::new(1)),
        host: None,
    };
    let before = migo_alloc_probe::thread_counts();
    let actual = Arc::new(data);
    let after = migo_alloc_probe::thread_counts();
    assert_eq!(
        (after.bytes_allocated - before.bytes_allocated) as usize,
        cell_bytes(),
        "output ring Arc layout differs from real allocation"
    );
    drop(actual);
    for channels in [1, 2, 32, 64] {
        let samples = RING_BUFFER_FRAMES * channels;
        let charge = quote(&host, &ledger, samples);
        let start = ledger.used_bytes();
        let (producer, consumer) = new(samples, Some(&host)).unwrap();
        assert_eq!(producer.capacity().get(), samples);
        assert_eq!(
            ledger.used_bytes() - start,
            charge,
            "output ring actual capacity omitted physical credit"
        );
        assert_eq!(
            producer.rb_ref().as_ref() as *const _,
            consumer.rb_ref().as_ref() as *const _,
            "output ring endpoints copied the backing"
        );
        assert!(
            super::host(&producer).is_some_and(|owner| owner.shares_physical_ledger(&host)),
            "output ring lost its host authority"
        );
        let before = ledger.used_bytes();
        let layout = host.for_channels(channels as u32);
        assert_eq!(
            layout.render_buffer_samples(),
            128 * channels,
            "output host lost channel layout"
        );
        assert_eq!(ledger.used_bytes(), before);
        drop((producer, consumer));
        wait(|| ledger.used_bytes() == start);
    }
    drop(host);
    wait(|| ledger.used_bytes() == 0);
}

#[test]
fn refusal_precedes_data_allocation_and_preserves_live_ring() {
    let (_scheduler, host, ledger) = setup();
    let samples = RING_BUFFER_FRAMES * 2;
    let (mut producer, mut consumer) = new(samples, Some(&host)).unwrap();
    let source = vec![0.3125; samples];
    assert_eq!(producer.push_slice(&source), samples);
    let address = producer.rb_ref().as_ref() as *const _ as usize;
    let filler = ledger
        .try_reserve(LIMIT - ledger.used_bytes(), "ring refusal peer")
        .unwrap();
    let at = migo_alloc_probe::thread_counts();
    let result = new(RING_BUFFER_FRAMES * 32, Some(&host));
    let allocated = migo_alloc_probe::thread_counts().bytes_allocated - at.bytes_allocated;
    assert!(
        result.is_err(),
        "output ring allocation bypassed physical admission"
    );
    assert!(
        allocated < (RING_BUFFER_FRAMES * 32 * 4) as u64,
        "output ring backing allocated before admission"
    );
    assert_eq!(
        ledger.used_bytes(),
        LIMIT,
        "output ring refusal changed old credit"
    );
    assert_eq!(producer.rb_ref().as_ref() as *const _ as usize, address);
    let mut output = vec![0.0; samples];
    assert_eq!(consumer.pop_slice(&mut output), samples);
    assert_eq!(output, source, "output ring refusal changed original PCM");
    drop((filler, producer, consumer));
    wait(|| ledger.used_bytes() == crate::node_retirement::HANDOFF_BYTES);
    drop(host);
    wait(|| ledger.used_bytes() == 0);
}

#[test]
fn last_callback_owner_retains_credit_until_original_cpu_drop_after_close() {
    let (scheduler, host, ledger) = setup();
    let samples = RING_BUFFER_FRAMES * 2;
    let seen = Arc::new(Seen::default());
    let (mut producer, consumer) = observe(hook(&seen), || new(samples, Some(&host))).unwrap();
    assert_eq!(producer.push_slice(&vec![0.75; samples]), samples);
    let sync = AudioSync::new();
    let mut callback = OutputCallback::new(consumer, sync, 0);
    let used = ledger.used_bytes();
    let gate = block(&scheduler.audio_cpu());
    scheduler.close();
    drop(producer);
    assert_eq!(
        ledger.used_bytes(),
        used,
        "output ring producer drop returned shared credit"
    );
    let mut output = vec![0.0; samples];
    callback.render_native(&mut output);
    assert!(output.iter().all(|v| *v == 0.75));
    let stats = std::thread::Builder::new()
        .name("emulated-device-callback".into())
        .spawn(move || {
            let before = migo_alloc_probe::thread_counts();
            drop(callback);
            let after = migo_alloc_probe::thread_counts();
            (
                after.allocations - before.allocations,
                after.reallocations - before.reallocations,
                after.bytes_freed - before.bytes_freed,
            )
        })
        .unwrap()
        .join()
        .unwrap();
    assert_eq!(
        (stats.0, stats.1),
        (0, 0),
        "output ring final callback allocated a late handoff"
    );
    assert!(
        stats.2 < (samples * 4) as u64,
        "output ring backing freed on callback caller"
    );
    assert_eq!(
        ledger.used_bytes(),
        used,
        "output ring credit returned before actual CPU destruction"
    );
    assert_eq!(seen.count.load(Ordering::Acquire), 0);
    gate.open();
    wait(|| {
        seen.count.load(Ordering::Acquire) == 1
            && ledger.used_bytes() == crate::node_retirement::HANDOFF_BYTES
    });
    assert!(!seen.wrong.load(Ordering::Acquire));
    drop(host);
    wait(|| ledger.used_bytes() == 0);
}

#[test]
fn observer_and_concurrent_final_endpoints_keep_one_original_owner() {
    for observer in [false, true] {
        let (scheduler, host, ledger) = setup();
        let seen = Arc::new(Seen::default());
        let (producer, consumer) = observe(hook(&seen), || new(16384, Some(&host))).unwrap();
        let extra = observer.then(|| producer.observe());
        let used = ledger.used_bytes();
        let gate = block(&scheduler.audio_cpu());
        let barrier = Arc::new(Barrier::new(3));
        let a = barrier.clone();
        let b = barrier.clone();
        let first = std::thread::spawn(move || {
            a.wait();
            drop(producer);
        });
        let second = std::thread::spawn(move || {
            b.wait();
            drop(consumer);
        });
        barrier.wait();
        first.join().unwrap();
        second.join().unwrap();
        assert_eq!(ledger.used_bytes(), used);
        assert_eq!(seen.count.load(Ordering::Acquire), 0);
        drop(extra);
        assert_eq!(
            ledger.used_bytes(),
            used,
            "output ring observer drop returned credit before CPU"
        );
        gate.open();
        wait(|| {
            seen.count.load(Ordering::Acquire) == 1
                && ledger.used_bytes() == crate::node_retirement::HANDOFF_BYTES
        });
        assert!(!seen.wrong.load(Ordering::Acquire));
        drop(host);
        wait(|| ledger.used_bytes() == 0);
    }
}

#[test]
fn replacement_overlap_and_silent_drop_keep_original_ring_tickets() {
    let (scheduler, host, ledger) = setup();
    let old_seen = Arc::new(Seen::default());
    let new_seen = Arc::new(Seen::default());
    let old = observe(hook(&old_seen), || SilentOutput::with_host(Some(&host))).unwrap();
    let old_sync_bytes = old.sync().storage_bytes();
    let first = ledger.used_bytes();
    let next = observe(hook(&new_seen), || SilentOutput::with_host(Some(&host))).unwrap();
    let next_sync_bytes = next.sync().storage_bytes();
    let both = ledger.used_bytes();
    assert!(both > first, "silent output ring omitted physical credit");
    let gate = block(&scheduler.audio_cpu());
    let mut active = old;
    let old = std::mem::replace(&mut active, next);
    drop(old);
    assert_eq!(
        ledger.used_bytes(),
        both - old_sync_bytes,
        "replacement returned old output ring credit early"
    );
    assert_eq!(old_seen.count.load(Ordering::Acquire), 0);
    drop(active);
    assert_eq!(ledger.used_bytes(), both - old_sync_bytes - next_sync_bytes);
    assert_eq!(new_seen.count.load(Ordering::Acquire), 0);
    gate.open();
    wait(|| {
        old_seen.count.load(Ordering::Acquire) == 1
            && new_seen.count.load(Ordering::Acquire) == 1
            && ledger.used_bytes() == crate::node_retirement::HANDOFF_BYTES
    });
    assert!(!old_seen.wrong.load(Ordering::Acquire) && !new_seen.wrong.load(Ordering::Acquire));
    drop(host);
    wait(|| ledger.used_bytes() == 0);
}

#[test]
fn retry_refusal_is_lazy_and_preserves_existing_output_until_budget_recovers() {
    let (_scheduler, host, ledger) = setup();
    let mut output = SilentOutput::with_host(Some(&host)).unwrap();
    output.write(&[0.5; 128]);
    let level = output.buffered();
    let filler = ledger
        .try_reserve(LIMIT - ledger.used_bytes(), "output retry peer")
        .unwrap();
    let mut retry = Retry::default();
    let at = Instant::now();
    let calls = std::cell::Cell::new(0);
    assert!(
        retry
            .attempt(at, || {
                calls.set(calls.get() + 1);
                SilentOutput::with_host(Some(&host)).map(Some)
            })
            .is_err()
    );
    let start = migo_alloc_probe::thread_counts();
    for millis in [1, 17, 99] {
        assert!(
            retry
                .attempt(at + Duration::from_millis(millis), || {
                    calls.set(calls.get() + 1);
                    SilentOutput::with_host(Some(&host)).map(Some)
                })
                .expect("output retry repeated denied factory before deadline")
                .is_none()
        );
    }
    let end = migo_alloc_probe::thread_counts();
    assert_eq!(
        (
            end.allocations - start.allocations,
            end.reallocations - start.reallocations
        ),
        (0, 0),
        "output retry allocated before its deadline"
    );
    assert_eq!(
        calls.get(),
        1,
        "output retry repeated denied factory before deadline"
    );
    assert_eq!(output.buffered(), level);
    assert_eq!(ledger.used_bytes(), LIMIT);
    drop(filler);
    let next = retry
        .attempt(at + Duration::from_millis(100), || {
            calls.set(calls.get() + 1);
            SilentOutput::with_host(Some(&host)).map(Some)
        })
        .expect("output retry failed after budget recovered")
        .expect("output retry did not run at deadline");
    assert_eq!(calls.get(), 2);
    drop((output, next));
    wait(|| ledger.used_bytes() == crate::node_retirement::HANDOFF_BYTES);
    drop(host);
    wait(|| ledger.used_bytes() == 0);
}

#[test]
fn closed_hosts_zero_and_overflow_refuse_before_any_backing_exists() {
    let (scheduler, host, ledger) = setup();
    let start = ledger.used_bytes();
    assert!(new(0, Some(&host)).is_err());
    assert!(new(usize::MAX, Some(&host)).is_err());
    assert_eq!(ledger.used_bytes(), start);
    scheduler.close();
    assert!(
        new(16384, Some(&host)).is_err(),
        "closed output host admitted new ring"
    );
    assert_eq!(ledger.used_bytes(), start);
    drop(host);
    wait(|| ledger.used_bytes() == 0);
}

fn state(sync: &AudioSync) -> [u64; 6] {
    [
        sync.buffer_level() as u64,
        sync.max_callback() as u64,
        sync.software_underrun_count() as u64,
        sync.device_high_watermark() as u64,
        sync.low_watermark(0) as u64,
        sync.check_and_clear() as u64,
    ]
}
#[test]
fn complete_native_and_integer_callbacks_match_original_kernel_and_clock_hints() {
    let (_scheduler, host, ledger) = setup();
    for channels in [1, 2, 32, 64] {
        for mode in [0, 1, 2] {
            let cap = RING_BUFFER_FRAMES * channels;
            let (mut producer, consumer) = new(cap, Some(&host)).unwrap();
            let (mut old_prod, old_cons) = HeapRb::<f32>::new(cap).split();
            let sync = AudioSync::new();
            let old_sync = AudioSync::new();
            sync.configure_device(channels, 192);
            old_sync.configure_device(channels, 192);
            sync.set_callback_wakeup_enabled(true);
            old_sync.set_callback_wakeup_enabled(true);
            let mut callback = OutputCallback::new(consumer, sync.clone(), 0);
            let mut original = before::OutputCallback::new(old_cons, old_sync.clone(), 0);
            for (round, frames) in [0, 1, 17, 127, 128, 129, 511, 512, 513, 257, 4097, 8193]
                .into_iter()
                .enumerate()
            {
                let input: Vec<_> = (0..(cap / 3 + round * 17))
                    .map(|i| ((i + round) % 4093) as f32 / 1024.0 - 2.0)
                    .collect();
                assert_eq!(producer.push_slice(&input), old_prod.push_slice(&input));
                let len = frames * channels + channels - 1;
                let bits = match mode {
                    0 => {
                        let mut actual = vec![0.875; len];
                        let mut expected = actual.clone();
                        let start = migo_alloc_probe::thread_counts();
                        callback.render_native(&mut actual);
                        let end = migo_alloc_probe::thread_counts();
                        assert_eq!(
                            (
                                end.allocations - start.allocations,
                                end.reallocations - start.reallocations
                            ),
                            (0, 0),
                            "managed output callback reached heap"
                        );
                        original.render_native(&mut expected);
                        let bits: Vec<_> = actual.iter().map(|v| v.to_bits()).collect();
                        assert_eq!(
                            bits,
                            expected.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
                            "managed output callback complete PCM changed"
                        );
                        bits.into_iter().map(u64::from).collect::<Vec<_>>()
                    }
                    1 => {
                        let mut actual = vec![197i16; len];
                        let mut expected = actual.clone();
                        let start = migo_alloc_probe::thread_counts();
                        callback.render_converted(&mut actual);
                        let end = migo_alloc_probe::thread_counts();
                        assert_eq!(
                            (
                                end.allocations - start.allocations,
                                end.reallocations - start.reallocations
                            ),
                            (0, 0),
                            "managed output callback reached heap"
                        );
                        original.render_converted(&mut expected);
                        assert_eq!(
                            actual, expected,
                            "managed output callback complete PCM changed"
                        );
                        actual.into_iter().map(|v| v as u16 as u64).collect()
                    }
                    _ => {
                        let mut actual = vec![197u16; len];
                        let mut expected = actual.clone();
                        let start = migo_alloc_probe::thread_counts();
                        callback.render_converted(&mut actual);
                        let end = migo_alloc_probe::thread_counts();
                        assert_eq!(
                            (
                                end.allocations - start.allocations,
                                end.reallocations - start.reallocations
                            ),
                            (0, 0),
                            "managed output callback reached heap"
                        );
                        original.render_converted(&mut expected);
                        assert_eq!(
                            actual, expected,
                            "managed output callback complete PCM changed"
                        );
                        actual.into_iter().map(u64::from).collect()
                    }
                };
                sync.refresh_device_policy();
                old_sync.refresh_device_policy();
                let hints = state(&sync);
                assert_eq!(
                    hints,
                    state(&old_sync),
                    "managed output callback refill hints changed"
                );
                assert_eq!(producer.occupied_len(), old_prod.occupied_len());
                println!(
                    "MIGO_OUTPUT_RING_PCM {channels}:{mode}:{round} {:?} {:?}",
                    bits, hints
                );
            }
            drop((producer, callback));
            wait(|| ledger.used_bytes() == crate::node_retirement::HANDOFF_BYTES);
        }
    }
    drop(host);
    wait(|| ledger.used_bytes() == 0);
}
