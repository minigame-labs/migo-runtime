//! Real bucket admission, caller handoff and uninterrupted managed SRC playback.
use super::*;
use crate::node_retirement::{HANDOFF_BYTES, Host as StorageHost};
use std::sync::{
    Condvar, Mutex,
    atomic::{AtomicBool, AtomicUsize},
};
const LIMIT: usize = 64 << 20;
fn wait(mut ready: impl FnMut() -> bool) {
    let start = Instant::now();
    while !ready() {
        assert!(
            start.elapsed() < Duration::from_secs(15),
            "route retirement or preparation did not finish"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
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
fn block(cpu: &AudioCpu) -> Gate {
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
fn setup() -> (
    migo_io::scheduler::IoScheduler,
    StorageHost,
    DeviceMixer,
    Arc<AudioAggregateLedger>,
) {
    let scheduler = migo_io::scheduler::IoScheduler::new(1);
    let ledger = Arc::new(AudioAggregateLedger::new(LIMIT));
    let storage = StorageHost::new(scheduler.audio_cpu(), ledger.clone(), 2).unwrap();
    let mut mixer = DeviceMixer::managed(scheduler.audio_cpu(), storage.clone());
    mixer.controller.host.aggregate = ledger.clone();
    mixer.controller.host.usage = Usage::new(8);
    mixer.controller.host.process = Usage::new(16);
    (scheduler, storage, mixer, ledger)
}
fn route(id: u32) -> Route {
    let context = AudioContext::new(id, 44100, 2);
    Route {
        epoch: context.device_epoch(),
        format: Format {
            source: 44100,
            target: 48000,
            channels: 2,
        },
        state: RouteState::Pending,
    }
}
fn fill(routes: &mut Routes, count: u32) {
    for id in 1..=count {
        routes.create_with(id, || Ok(route(id))).unwrap();
    }
}
fn bytes(entries: usize) -> usize {
    let before = migo_alloc_probe::thread_counts();
    let native = HashMap::<u32, Route>::with_capacity(entries);
    let allocated = migo_alloc_probe::thread_counts().bytes_allocated - before.bytes_allocated;
    let layout =
        crate::graph_table_layout::Layout::for_entries::<(u32, Route)>(native.capacity()).unwrap();
    assert_eq!(allocated, layout.bytes as u64);
    layout.bytes
}
fn fee(storage: &StorageHost, ledger: &Arc<AudioAggregateLedger>, size: usize) -> usize {
    if size < 4096 {
        return size;
    }
    let before = ledger.used_bytes();
    let quote = storage
        .reserve_value_with_bytes::<Option<HashMap<u32, Route>>>(size)
        .unwrap();
    let result = ledger.used_bytes() - before;
    drop(quote);
    result
}
fn source(id: u32, rate: u32, channels: u32, frames: usize) -> AudioContext {
    let mut ctx = AudioContext::new(id, rate, channels);
    let samples = (0..frames * channels as usize)
        .map(|i| (i % 127) as f32 / 256.0 - 0.25)
        .collect();
    let b = ctx
        .add_buffer(crate::decoder::DecodedAudio {
            sample_rate: rate,
            channels,
            samples,
        })
        .unwrap();
    assert!(ctx.create_buffer_source(10));
    assert!(ctx.set_buffer(10, Some(b)));
    ctx.watch_source_ended(10, true);
    ctx.connect(10, crate::context::DESTINATION_NODE_ID);
    ctx.start_source(10, 0.0, 0.0, None);
    ctx.resume();
    ctx
}
fn service(m: &mut DeviceMixer, contexts: &mut HashMap<u32, AudioContext>, target: u32) {
    m.service(
        contexts,
        target,
        false,
        &ThreadWakeup::new(),
        |_| {},
        |_| {},
    );
}
fn ready(m: &mut DeviceMixer, contexts: &mut HashMap<u32, AudioContext>, target: u32) {
    wait(|| {
        service(m, contexts, target);
        matches!(
            m.routes.get(&7).map(|r| &r.state),
            Some(RouteState::Active(_))
        )
    });
}
fn device(m: &mut DeviceMixer) -> &mut Device {
    let RouteState::Active(device) = &mut m.routes.get_mut(&7).unwrap().state else {
        panic!("route not active")
    };
    device
}
#[test]
fn empty_is_lazy_and_first_table_charges_real_native_layout() {
    let (_s, storage, mut m, ledger) = setup();
    assert_eq!(m.routes.capacity(), 0);
    assert_eq!(ledger.used_bytes(), HANDOFF_BYTES);
    let expected = fee(&storage, &ledger, bytes(1));
    m.routes.create_with(1, || Ok(route(1))).unwrap();
    assert_eq!(
        ledger.used_bytes(),
        HANDOFF_BYTES + expected,
        "route first table omitted physical charge"
    );
    assert_eq!(m.routes.table_bytes(), bytes(1));
    assert!(m.controller.slots.is_empty());
    drop((m, storage));
    wait(|| ledger.used_bytes() == 0);
}
#[test]
fn first_refusal_precedes_request_and_keeps_idle_retry_without_rendering() {
    let (_s, storage, mut m, ledger) = setup();
    let mut contexts = HashMap::from([(7, source(7, 44100, 2, 1024))]);
    let clock = contexts[&7].current_time();
    let wakeup = ThreadWakeup::new();
    let filler = ledger
        .try_reserve(LIMIT - ledger.used_bytes(), "route first peer")
        .unwrap();
    let before = migo_alloc_probe::thread_counts();
    m.service(&mut contexts, 48000, false, &wakeup, |_| {}, |_| {});
    let allocated = migo_alloc_probe::thread_counts().bytes_allocated - before.bytes_allocated;
    assert!(
        m.routes.is_empty(),
        "route first insertion bypassed admission"
    );
    assert!(
        m.controller.slots.is_empty(),
        "route refusal requested a filter before admission"
    );
    assert!(!wakeup.wait_timeout(Duration::ZERO));
    assert!(
        allocated < bytes(1) as u64,
        "route first buckets allocated before admission"
    );
    assert_eq!(contexts[&7].current_time(), clock);
    let delay = m
        .retry_wait(&contexts)
        .expect("route admission refusal lost idle retry");
    assert!(delay <= ADMISSION_RETRY);
    assert!(
        matches!(crate::audio_thread::device_retry_wait(crate::power_manager::AudioWaitMode::Indefinite,Some(delay)),crate::power_manager::AudioWaitMode::Timed(d) if d==delay)
    );
    drop(filler);
    ready(&mut m, &mut contexts, 48000);
    assert!(
        m.admission_retry.is_none(),
        "route success retained obsolete retry deadline"
    );
    drop((m, contexts, storage));
    wait(|| ledger.used_bytes() == 0);
}
#[test]
fn full_table_refusal_preserves_routes_and_does_not_call_factory() {
    let (_s, storage, mut m, ledger) = setup();
    fill(&mut m.routes, 28);
    let expected = HANDOFF_BYTES + fee(&storage, &ledger, bytes(28));
    wait(|| ledger.used_bytes() <= expected);
    assert_eq!(m.routes.len(), m.routes.capacity());
    let original = m.routes.identity();
    let next = route(10000);
    let target = bytes(29);
    let filler = ledger
        .try_reserve(LIMIT - ledger.used_bytes(), "route growth peer")
        .unwrap();
    let called = AtomicBool::new(false);
    let before = migo_alloc_probe::thread_counts();
    let result = m.routes.create_with(10000, || {
        called.store(true, Ordering::Release);
        Ok(next)
    });
    let allocated = migo_alloc_probe::thread_counts().bytes_allocated - before.bytes_allocated;
    assert!(
        !called.load(Ordering::Acquire),
        "route factory ran before capacity admission"
    );
    assert!(result.is_err(), "route growth bypassed admission");
    assert_eq!(
        m.routes.identity(),
        original,
        "route refusal moved existing entries"
    );
    assert!(
        allocated < target as u64,
        "route refusal allocated target buckets"
    );
    drop((filler, m, storage));
    wait(|| ledger.used_bytes() == 0);
}
#[test]
fn failed_factory_preserves_table_and_refunds_growth_without_target_buckets() {
    let (_s, storage, mut m, ledger) = setup();
    fill(&mut m.routes, 28);
    let expected = HANDOFF_BYTES + fee(&storage, &ledger, bytes(28));
    wait(|| ledger.used_bytes() == expected);
    let original = m.routes.identity();
    let target = bytes(29);
    let before = migo_alloc_probe::thread_counts();
    let result = m
        .routes
        .create_with(10000, || Err(invalid("route factory failure")));
    let allocated = migo_alloc_probe::thread_counts().bytes_allocated - before.bytes_allocated;
    assert!(result.is_err());
    assert_eq!(
        m.routes.identity(),
        original,
        "failed route factory installed or published table growth"
    );
    assert!(
        allocated < target as u64,
        "failed route factory allocated target buckets"
    );
    assert_eq!(
        ledger.used_bytes(),
        expected,
        "failed route factory leaked reserved credit"
    );
    drop((m, storage));
    wait(|| ledger.used_bytes() == 0);
}
#[test]
fn bulk_rehash_preserves_epochs_and_keeps_overlapping_credit_to_cpu_drop() {
    let (s, storage, mut m, ledger) = setup();
    fill(&mut m.routes, 28);
    let old_bytes = bytes(28);
    assert!(old_bytes >= 4096);
    let expected = HANDOFF_BYTES + fee(&storage, &ledger, old_bytes);
    wait(|| ledger.used_bytes() == expected);
    let epochs: Vec<_> = m
        .routes
        .iter()
        .map(|(id, r)| (*id, r.epoch.clone()))
        .collect();
    let new_fee = fee(&storage, &ledger, bytes(29));
    let dropped = Arc::new(AtomicUsize::new(0));
    let wrong = Arc::new(AtomicBool::new(false));
    let count = dropped.clone();
    let thread = wrong.clone();
    m.routes.observe(Arc::new(move || {
        if !std::thread::current()
            .name()
            .unwrap_or("")
            .starts_with("Migo-IO-")
        {
            thread.store(true, Ordering::Release);
        }
        count.fetch_add(1, Ordering::Release);
    }));
    let next = route(10000);
    let gate = block(&s.audio_cpu());
    let before = migo_alloc_probe::thread_counts();
    m.routes.create_with(10000, || Ok(next)).unwrap();
    let freed = migo_alloc_probe::thread_counts().bytes_freed - before.bytes_freed;
    assert!(
        m.routes.contains_key(&10000),
        "route creation did not publish new entry"
    );
    assert!(
        epochs
            .iter()
            .all(|(id, e)| m.routes.get(id).is_some_and(|r| r.epoch.same(e))),
        "route rehash lost existing epochs"
    );
    assert!(
        freed < old_bytes as u64,
        "route rehash freed old buckets on caller"
    );
    assert_eq!(
        ledger.used_bytes(),
        expected + new_fee,
        "route rehash returned overlapping bucket credit"
    );
    assert_eq!(dropped.load(Ordering::Acquire), 0);
    gate.open();
    wait(|| dropped.load(Ordering::Acquire) == 1 && ledger.used_bytes() == HANDOFF_BYTES + new_fee);
    assert!(!wrong.load(Ordering::Acquire));
    drop((m, storage));
    wait(|| ledger.used_bytes() == 0);
}
#[test]
fn remove_retain_and_restart_keep_retained_bucket_credit() {
    let (_s, storage, mut m, ledger) = setup();
    fill(&mut m.routes, 28);
    let expected = HANDOFF_BYTES + fee(&storage, &ledger, bytes(28));
    wait(|| ledger.used_bytes() == expected);
    let cap = m.routes.capacity();
    drop(m.routes.remove(&1));
    m.routes.retain(|id, _| *id == 2);
    assert_eq!(m.routes.len(), 1, "route retain left discarded entries");
    m.admission_retry = Some(Instant::now());
    m.restart();
    assert!(m.routes.is_empty(), "route restart retained entries");
    assert_eq!(m.routes.capacity(), cap);
    assert_eq!(
        ledger.used_bytes(),
        expected,
        "route removal returned retained bucket credit"
    );
    assert!(
        m.admission_retry.is_none(),
        "route restart retained admission retry"
    );
    drop((m, storage));
    wait(|| ledger.used_bytes() == 0);
}
#[test]
fn closed_cpu_drop_releases_route_owners_before_empty_bulk_buckets_die() {
    let (s, storage, mut m, ledger) = setup();
    let mut contexts = HashMap::from([(7, source(7, 44100, 2, 1024))]);
    ready(&mut m, &mut contexts, 48000);
    for id in 100..128 {
        m.routes.create_with(id, || Ok(route(id))).unwrap();
    }
    let route_bytes = bytes(29);
    let route_fee = fee(&storage, &ledger, route_bytes);
    let expected = HANDOFF_BYTES
        + route_fee
        + crate::graph_table_layout::Layout::for_entries::<(u32, Slot)>(1)
            .unwrap()
            .bytes
        + OWNER_BYTES;
    wait(|| ledger.used_bytes() == expected);
    let operation = m.controller.slots[&7].operation.clone();
    let table_drops = Arc::new(AtomicUsize::new(0));
    let count = table_drops.clone();
    m.routes.observe(Arc::new(move || {
        count.fetch_add(1, Ordering::Release);
    }));
    let gate = block(&s.audio_cpu());
    s.close();
    let before = migo_alloc_probe::thread_counts();
    drop(m);
    let end = migo_alloc_probe::thread_counts();
    assert!(
        !operation.live.is_live(),
        "route shutdown postponed operation cancellation"
    );
    assert_eq!(
        Arc::strong_count(&operation),
        1,
        "route shutdown postponed live route destruction"
    );
    assert!(contexts[&7].device_epoch().is_live());
    assert_eq!(
        (
            end.allocations - before.allocations,
            end.reallocations - before.reallocations
        ),
        (0, 0),
        "route shutdown allocated late handoff"
    );
    assert!(
        end.bytes_freed - before.bytes_freed < route_bytes as u64,
        "route shutdown freed bulk buckets on caller"
    );
    assert!(
        ledger.used_bytes() >= HANDOFF_BYTES + route_fee + OWNER_BYTES,
        "route shutdown returned CPU-owned credit early"
    );
    assert_eq!(table_drops.load(Ordering::Acquire), 0);
    gate.open();
    wait(|| table_drops.load(Ordering::Acquire) == 1 && ledger.used_bytes() == HANDOFF_BYTES);
    drop((contexts, storage));
    wait(|| ledger.used_bytes() == 0);
}
#[test]
fn established_active_route_is_allocation_free_under_full_budget() {
    let (_s, storage, mut m, ledger) = setup();
    let mut contexts = HashMap::from([(7, source(7, 44100, 2, 1024))]);
    ready(&mut m, &mut contexts, 48000);
    let identity = m.routes.identity();
    let operation = m.controller.slots[&7].operation.clone();
    let filler = ledger
        .try_reserve(LIMIT - ledger.used_bytes(), "route duplicate peer")
        .unwrap();
    let wakeup = ThreadWakeup::new();
    let before = migo_alloc_probe::thread_counts();
    m.service(&mut contexts, 48000, false, &wakeup, |_| {}, |_| {});
    let end = migo_alloc_probe::thread_counts();
    assert_eq!(
        (
            end.allocations - before.allocations,
            end.reallocations - before.reallocations
        ),
        (0, 0),
        "established route attempted new allocation"
    );
    assert_eq!(m.routes.identity(), identity);
    assert!(Arc::ptr_eq(&operation, &m.controller.slots[&7].operation));
    drop((filler, m, contexts, storage));
    wait(|| ledger.used_bytes() == 0);
}
#[test]
fn control_only_and_paused_or_native_hosts_clear_capacity_retry() {
    let (_s, storage, mut m, ledger) = setup();
    let mut contexts = HashMap::from([(7, source(7, 44100, 2, 1024))]);
    let filler = ledger
        .try_reserve(LIMIT - ledger.used_bytes(), "route controls peer")
        .unwrap();
    service(&mut m, &mut contexts, 48000);
    contexts.get_mut(&7).unwrap().stop_source(10, 0.0);
    let mut collected = Vec::new();
    let mut ended = Vec::new();
    m.service(
        &mut contexts,
        48000,
        true,
        &ThreadWakeup::new(),
        |id| collected.push(id),
        |id| ended.push(id),
    );
    assert_eq!(
        collected,
        [10],
        "route refusal lost explicit stop collection"
    );
    assert!(ended.is_empty());
    assert!(
        m.retry_wait(&contexts).is_none(),
        "paused route retained capacity retry"
    );
    m.service(
        &mut contexts,
        48000,
        false,
        &ThreadWakeup::new(),
        |_| {},
        |id| ended.push(id),
    );
    assert_eq!(ended, [10]);
    service(&mut m, &mut contexts, 44100);
    assert!(m.routes.is_empty() && m.controller.slots.is_empty());
    assert!(m.admission_retry.is_none());
    drop((filler, m, contexts, storage));
    wait(|| ledger.used_bytes() == 0);
}
#[test]
fn complete_active_route_pcm_clock_and_rehash_trace() {
    let (_s, storage, mut m, ledger) = setup();
    for (rate, target) in [(44100, 48000), (48000, 44100)] {
        for channels in [1, 2, 32] {
            let mut contexts = HashMap::from([(7, source(7, rate, channels, 8192))]);
            ready(&mut m, &mut contexts, target);
            let operation = m.controller.slots[&7].operation.clone();
            let session = device(&mut m).session().unwrap() as *mut Session as usize;
            for frames in [1, 127, 128, 129, 257, 4097] {
                let mut output = vec![0.375; frames * channels as usize + channels as usize - 1];
                let mut collected = Vec::new();
                let mut ended = Vec::new();
                for chunk in
                    output[..frames * channels as usize].chunks_mut(QUANTUM * channels as usize)
                {
                    m.mix(
                        contexts.get_mut(&7).unwrap(),
                        chunk,
                        target,
                        channels,
                        |id| collected.push(id),
                        |id| ended.push(id),
                    );
                }
                assert!(
                    output[frames * channels as usize..]
                        .iter()
                        .all(|v| *v == 0.375)
                );
                let clocks = device(&mut m).session().unwrap().pull.clocks();
                let bits: Vec<_> = output.iter().map(|v| v.to_bits()).collect();
                collected.sort_unstable();
                ended.sort_unstable();
                println!(
                    "MIGO_ROUTE_PCM {rate}:{target}:{channels}:{frames} {:?} {:?} {:?} {:?}",
                    bits,
                    collected,
                    ended,
                    [
                        contexts[&7].current_time().to_bits(),
                        clocks.rendered_source,
                        clocks.published_source,
                        clocks.published_output
                    ]
                );
                if frames == 127 {
                    for id in 100..132 {
                        m.routes.create_with(id, || Ok(route(id))).unwrap();
                    }
                }
                if frames == 257 {
                    service(&mut m, &mut contexts, target);
                    assert!(
                        Arc::ptr_eq(&operation, &m.controller.slots[&7].operation),
                        "route rehash replaced active operation"
                    );
                    assert_eq!(
                        device(&mut m).session().unwrap() as *mut Session as usize,
                        session,
                        "route rehash replaced live filter session"
                    );
                }
            }
            m.restart();
            drop(contexts);
            wait(|| m.controller.host.usage.owners.load(Ordering::Acquire) == 0);
        }
    }
    drop((m, storage));
    wait(|| ledger.used_bytes() == 0);
}
