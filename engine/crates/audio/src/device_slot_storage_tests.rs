//! Slot admission must precede request side effects and keep cancellation immediate.
use super::*;
use crate::node_retirement::{HANDOFF_BYTES, Host as StorageHost};
use std::{
    collections::HashMap,
    sync::{
        Condvar, Mutex,
        atomic::{AtomicBool, AtomicUsize},
    },
    time::{Duration, Instant},
};
const LIMIT: usize = 64 << 20;
#[derive(Default)]
struct Dropped {
    count: AtomicUsize,
    wrong: AtomicBool,
}
fn witness(state: &Arc<Dropped>) -> Arc<dyn Fn() + Send + Sync> {
    let state = state.clone();
    Arc::new(move || {
        if !std::thread::current()
            .name()
            .unwrap_or("")
            .starts_with("Migo-IO-")
        {
            state.wrong.store(true, Ordering::Release);
        }
        state.count.fetch_add(1, Ordering::Release);
    })
}
fn wait(mut ready: impl FnMut() -> bool) {
    let start = Instant::now();
    while !ready() {
        assert!(
            start.elapsed() < Duration::from_secs(15),
            "slot retirement or preparation did not finish"
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
    Controller,
    Arc<AudioAggregateLedger>,
) {
    let scheduler = migo_io::scheduler::IoScheduler::new(1);
    let ledger = Arc::new(AudioAggregateLedger::new(LIMIT));
    let storage = StorageHost::new(scheduler.audio_cpu(), ledger.clone(), 2).unwrap();
    let mut controller = Controller::managed(scheduler.audio_cpu(), storage.clone());
    controller.host.aggregate = ledger.clone();
    controller.host.usage = Usage::new(8);
    controller.host.process = Usage::new(16);
    (scheduler, storage, controller, ledger)
}
fn table_bytes(entries: usize) -> usize {
    let before = migo_alloc_probe::thread_counts();
    let native = HashMap::<u32, Slot>::with_capacity(entries);
    let bytes = migo_alloc_probe::thread_counts().bytes_allocated - before.bytes_allocated;
    let layout =
        crate::graph_table_layout::Layout::for_entries::<(u32, Slot)>(native.capacity()).unwrap();
    assert_eq!(bytes, layout.bytes as u64);
    layout.bytes
}
fn fee(storage: &StorageHost, ledger: &Arc<AudioAggregateLedger>, bytes: usize) -> usize {
    if bytes < 4096 {
        return bytes;
    }
    let used = ledger.used_bytes();
    let quote = storage
        .reserve_value_with_bytes::<Option<HashMap<u32, Slot>>>(bytes)
        .unwrap();
    let result = ledger.used_bytes() - used;
    drop(quote);
    result
}
fn ensure(c: &mut Controller, ctx: &AudioContext, target: u32) {
    c.ensure(ctx, target, ThreadWakeup::new(), None).unwrap();
}
fn fill(c: &mut Controller, count: usize) -> Vec<AudioContext> {
    let mut contexts = Vec::with_capacity(count);
    for id in 1..=count as u32 {
        let ctx = AudioContext::new(id, 48000, 2);
        ensure(c, &ctx, 48000);
        contexts.push(ctx);
    }
    contexts
}
fn bindings(c: &Controller) -> Vec<Binding> {
    c.slots
        .values()
        .map(|slot| Binding {
            epoch: slot.epoch.clone(),
            operation: slot.operation.clone(),
        })
        .collect()
}
#[test]
fn empty_is_lazy_and_first_insert_charges_real_native_layout() {
    let (_scheduler, storage, mut c, ledger) = setup();
    assert_eq!(c.slots.capacity(), 0);
    assert_eq!(c.slots.table_bytes(), 0);
    assert_eq!(
        ledger.used_bytes(),
        HANDOFF_BYTES,
        "empty slot table allocated or charged buckets"
    );
    let expected = fee(&storage, &ledger, table_bytes(1));
    let ctx = AudioContext::new(1, 48000, 2);
    ensure(&mut c, &ctx, 48000);
    assert_eq!(
        ledger.used_bytes(),
        HANDOFF_BYTES + expected,
        "slot first table omitted physical charge"
    );
    assert_eq!(
        c.slots.table_bytes(),
        table_bytes(1),
        "slot table layout differs from real allocation"
    );
    assert!(c.slots[&1].pending.is_none());
    assert_eq!(c.host.usage.owners.load(Ordering::Acquire), 0);
    drop((c, storage));
    wait(|| ledger.used_bytes() == 0);
}
#[test]
fn first_refusal_precedes_buckets_operations_and_wakeup() {
    let (_scheduler, storage, mut c, ledger) = setup();
    let ctx = AudioContext::new(1, 48000, 2);
    let wakeup = ThreadWakeup::new();
    let bytes = table_bytes(1);
    let filler = ledger
        .try_reserve(LIMIT - ledger.used_bytes(), "slot first peer")
        .unwrap();
    let before = migo_alloc_probe::thread_counts();
    let result = c.ensure(&ctx, 48000, wakeup.clone(), None);
    let allocated = migo_alloc_probe::thread_counts().bytes_allocated - before.bytes_allocated;
    assert!(result.is_err(), "slot first insertion bypassed admission");
    assert!(
        !wakeup.wait_timeout(Duration::ZERO),
        "slot request woke before first table admission"
    );
    assert!(c.slots.is_empty());
    assert!(
        allocated < bytes as u64,
        "slot first buckets allocated before admission"
    );
    drop((filler, c, storage));
    wait(|| ledger.used_bytes() == 0);
}
#[test]
fn full_table_refusal_preserves_all_slots_before_request_side_effects() {
    let (_scheduler, storage, mut c, ledger) = setup();
    let _contexts = fill(&mut c, 28);
    let expected = HANDOFF_BYTES + fee(&storage, &ledger, table_bytes(28));
    // The raw before table has no charge; reach the same growth assertion.
    // Independent exact bulk-table charging is checked by the rehash test.
    wait(|| ledger.used_bytes() <= expected);
    assert_eq!(c.slots.len(), c.slots.capacity());
    let original = c.slots.identity();
    let live = bindings(&c);
    let target = table_bytes(c.slots.len() + 1);
    let ctx = AudioContext::new(10000, 48000, 2);
    let wakeup = ThreadWakeup::new();
    let filler = ledger
        .try_reserve(LIMIT - ledger.used_bytes(), "slot growth peer")
        .unwrap();
    let before = migo_alloc_probe::thread_counts();
    let result = c.ensure(&ctx, 48000, wakeup.clone(), None);
    let allocated = migo_alloc_probe::thread_counts().bytes_allocated - before.bytes_allocated;
    assert!(result.is_err(), "slot growth bypassed admission");
    assert!(
        !wakeup.wait_timeout(Duration::ZERO),
        "slot factory request ran before table admission"
    );
    assert_eq!(
        c.slots.identity(),
        original,
        "slot refusal moved or replaced live entries"
    );
    assert!(
        live.iter().all(Binding::is_live),
        "slot refusal revoked live bindings"
    );
    assert!(
        allocated < target as u64,
        "slot refusal allocated target buckets"
    );
    drop((filler, c, storage));
    wait(|| ledger.used_bytes() == 0);
}
#[test]
fn duplicate_ensure_is_allocation_free_under_full_budget() {
    let (scheduler, storage, mut c, ledger) = setup();
    let contexts = fill(&mut c, 28);
    let expected = HANDOFF_BYTES + fee(&storage, &ledger, table_bytes(28));
    wait(|| ledger.used_bytes() == expected);
    let original = c.slots.identity();
    let wakeup = ThreadWakeup::new();
    let filler = ledger
        .try_reserve(LIMIT - ledger.used_bytes(), "slot duplicate peer")
        .unwrap();
    let before = migo_alloc_probe::thread_counts();
    let result = c.ensure(&contexts[0], 48000, wakeup.clone(), None);
    let end = migo_alloc_probe::thread_counts();
    assert!(
        result.is_ok(),
        "duplicate slot ensure attempted new admission"
    );
    assert_eq!(
        (
            end.allocations - before.allocations,
            end.reallocations - before.reallocations
        ),
        (0, 0),
        "duplicate slot ensure allocated"
    );
    assert!(
        !wakeup.wait_timeout(Duration::ZERO),
        "duplicate slot ensure restarted request"
    );
    assert_eq!(c.slots.identity(), original);
    scheduler.close();
    assert!(
        c.ensure(&contexts[0], 48000, wakeup, None).is_err(),
        "closed slot host lost original early refusal"
    );
    drop((filler, c, storage));
    wait(|| ledger.used_bytes() == 0);
}
#[test]
fn failed_factory_keeps_full_table_and_refunds_reserved_growth() {
    let (_scheduler, storage, mut c, ledger) = setup();
    let _contexts = fill(&mut c, 28);
    let expected = HANDOFF_BYTES + fee(&storage, &ledger, table_bytes(28));
    wait(|| ledger.used_bytes() == expected);
    let original = c.slots.identity();
    let target = table_bytes(29);
    let before = migo_alloc_probe::thread_counts();
    let result = c
        .slots
        .create_with(10000, || Err(EngineError::new(ErrorCode::InvalidArgument)));
    let allocated = migo_alloc_probe::thread_counts().bytes_allocated - before.bytes_allocated;
    assert!(result.is_err());
    assert_eq!(
        c.slots.identity(),
        original,
        "failed slot factory installed or published table growth"
    );
    assert!(
        allocated < target as u64,
        "failed slot factory allocated target buckets"
    );
    assert_eq!(
        ledger.used_bytes(),
        expected,
        "failed slot factory leaked reservation"
    );
    drop((c, storage));
    wait(|| ledger.used_bytes() == 0);
}
#[test]
fn full_key_replacement_preserves_capacity_and_revokes_only_old_operation() {
    let (_scheduler, storage, mut c, ledger) = setup();
    let contexts = fill(&mut c, 28);
    let expected = HANDOFF_BYTES + fee(&storage, &ledger, table_bytes(28));
    wait(|| ledger.used_bytes() == expected);
    let old = c.slots[&1].operation.clone();
    let other = c.slots[&2].operation.clone();
    let next = AudioContext::new(1, 48000, 2);
    let identity = c.slots.identity();
    let filler = ledger
        .try_reserve(LIMIT - ledger.used_bytes(), "slot replacement peer")
        .unwrap();
    ensure(&mut c, &next, 48000);
    assert!(
        !old.live.is_live(),
        "slot replacement did not cancel old operation"
    );
    assert!(other.live.is_live() && contexts[1].device_epoch().is_live());
    assert!(c.slots[&1].epoch.same(&next.device_epoch()));
    assert_eq!(
        c.slots.capacity(),
        identity.1,
        "slot existing-key replacement grew full table"
    );
    assert_eq!(
        c.slots.identity().2[0].1,
        identity.2[0].1,
        "slot replacement moved full table allocation"
    );
    assert_eq!(ledger.used_bytes(), LIMIT);
    c.remove(&contexts[0]);
    assert!(
        c.slots.contains_key(&1),
        "late old-epoch remove erased replacement"
    );
    drop((filler, c, storage));
    wait(|| ledger.used_bytes() == 0);
}
#[test]
fn bulk_rehash_keeps_operations_live_and_both_bucket_allocations_charged() {
    let (scheduler, storage, mut c, ledger) = setup();
    let _contexts = fill(&mut c, 28);
    let bytes = table_bytes(28);
    assert!(bytes >= 4096);
    let expected = HANDOFF_BYTES + fee(&storage, &ledger, bytes);
    wait(|| ledger.used_bytes() == expected);
    let current = c.slots.identity();
    let live = bindings(&c);
    let next = AudioContext::new(10000, 48000, 2);
    let target_fee = fee(&storage, &ledger, table_bytes(29));
    let dropped = Arc::new(Dropped::default());
    c.slots.observe(witness(&dropped));
    let gate = block(&scheduler.audio_cpu());
    let before = migo_alloc_probe::thread_counts();
    ensure(&mut c, &next, 48000);
    let end = migo_alloc_probe::thread_counts();
    assert!(
        c.slots.contains_key(&10000),
        "slot creation did not publish new entry"
    );
    assert!(
        current.2.iter().all(|(id, _, _)| c.slots.contains_key(id)),
        "slot rehash lost existing operations"
    );
    for (id, _, operation) in current.2 {
        assert_eq!(
            Arc::as_ptr(&c.slots[&id].operation) as usize,
            operation,
            "slot rehash replaced operation identity"
        );
    }
    assert!(
        live.iter().all(Binding::is_live),
        "slot rehash revoked operation binding"
    );
    assert!(
        end.bytes_freed - before.bytes_freed < bytes as u64,
        "slot rehash freed old buckets on caller"
    );
    assert_eq!(
        ledger.used_bytes(),
        expected + target_fee,
        "slot rehash returned overlapping bucket credit"
    );
    assert_eq!(dropped.count.load(Ordering::Acquire), 0);
    gate.open();
    wait(|| {
        dropped.count.load(Ordering::Acquire) == 1
            && ledger.used_bytes() == HANDOFF_BYTES + target_fee
    });
    assert!(!dropped.wrong.load(Ordering::Acquire));
    drop((c, storage));
    wait(|| ledger.used_bytes() == 0);
}
#[test]
fn remove_and_restart_cancel_immediately_without_returning_capacity_credit() {
    let (_scheduler, storage, mut c, ledger) = setup();
    let contexts = fill(&mut c, 28);
    let expected = HANDOFF_BYTES + fee(&storage, &ledger, table_bytes(28));
    wait(|| ledger.used_bytes() == expected);
    let capacity = c.slots.capacity();
    let first = c.slots[&1].operation.clone();
    c.remove(&contexts[0]);
    assert!(
        !first.live.is_live(),
        "slot remove did not revoke current operation"
    );
    assert!(!c.slots.contains_key(&1));
    let live = bindings(&c);
    c.restart();
    assert!(
        live.iter().all(|binding| !binding.is_live()),
        "slot restart did not revoke remaining operations"
    );
    assert!(c.slots.is_empty());
    assert_eq!(c.slots.capacity(), capacity);
    assert_eq!(
        ledger.used_bytes(),
        expected,
        "slot removal returned retained bucket capacity credit"
    );
    drop((c, storage));
    wait(|| ledger.used_bytes() == 0);
}
#[test]
fn closed_cpu_drop_revokes_operations_before_empty_buckets_really_die() {
    let (scheduler, storage, mut c, ledger) = setup();
    let contexts = fill(&mut c, 28);
    let expected = HANDOFF_BYTES + fee(&storage, &ledger, table_bytes(28));
    wait(|| ledger.used_bytes() == expected);
    let live = bindings(&c);
    let bytes = c.slots.table_bytes();
    let dropped = Arc::new(Dropped::default());
    c.slots.observe(witness(&dropped));
    let gate = block(&scheduler.audio_cpu());
    scheduler.close();
    let before = migo_alloc_probe::thread_counts();
    drop(c);
    let end = migo_alloc_probe::thread_counts();
    assert!(
        live.iter().all(|binding| !binding.is_live()),
        "slot shutdown postponed operation revocation"
    );
    assert!(
        contexts.iter().all(|ctx| ctx.device_epoch().is_live()),
        "slot shutdown revoked external context authority"
    );
    assert_eq!(
        (
            end.allocations - before.allocations,
            end.reallocations - before.reallocations
        ),
        (0, 0),
        "slot shutdown allocated late handoff"
    );
    assert!(
        end.bytes_freed - before.bytes_freed < bytes as u64,
        "slot shutdown freed bulk buckets on caller"
    );
    assert_eq!(
        ledger.used_bytes(),
        expected,
        "slot shutdown returned bucket credit before CPU drop"
    );
    assert_eq!(dropped.count.load(Ordering::Acquire), 0);
    gate.open();
    wait(|| dropped.count.load(Ordering::Acquire) == 1 && ledger.used_bytes() == HANDOFF_BYTES);
    assert!(!dropped.wrong.load(Ordering::Acquire));
    drop(storage);
    wait(|| ledger.used_bytes() == 0);
}
struct Release(Option<std::sync::mpsc::Sender<()>>);
impl Drop for Release {
    fn drop(&mut self) {
        self.0.take();
    }
}
#[test]
fn pending_abort_and_id_reuse_keep_running_filter_credit_until_original_cpu_drop() {
    let (_scheduler, storage, mut c, ledger) = setup();
    let ctx = AudioContext::new(7, 768000, 2);
    let (entered, started) = std::sync::mpsc::channel();
    let (unlock, blocked) = std::sync::mpsc::channel::<()>();
    let release = Release(Some(unlock));
    let blocked = Mutex::new(Some(blocked));
    let hook: Hook = Arc::new(move |ready| {
        if ready.stream.as_ref().unwrap().prepared_layers() == 1
            && let Some(wait) = blocked.lock().unwrap().take()
        {
            entered.send(()).unwrap();
            let _ = wait.recv_timeout(Duration::from_secs(15));
        }
    });
    c.ensure(&ctx, 3000, ThreadWakeup::new(), Some(hook))
        .unwrap();
    started.recv_timeout(Duration::from_secs(15)).unwrap();
    let old = Binding {
        epoch: ctx.device_epoch(),
        operation: c.slots[&7].operation.clone(),
    };
    let metadata = HANDOFF_BYTES + fee(&storage, &ledger, table_bytes(1));
    assert_eq!(ledger.used_bytes(), metadata + OWNER_BYTES);
    c.restart();
    assert!(!old.is_live(), "slot restart left pending operation live");
    assert_eq!(
        ledger.used_bytes(),
        metadata + OWNER_BYTES,
        "slot abort returned running filter credit before CPU destruction"
    );
    let next = AudioContext::new(7, 48000, 2);
    ensure(&mut c, &next, 48000);
    let current = c.slots[&7].operation.clone();
    drop(release);
    wait(|| c.host.usage.owners.load(Ordering::Acquire) == 0 && ledger.used_bytes() == metadata);
    assert!(c.take(&next).is_none());
    assert!(Arc::ptr_eq(&current, &c.slots[&7].operation));
    assert!(current.live.is_live() && !old.is_live());
    drop((c, storage));
    wait(|| ledger.used_bytes() == 0);
}
#[test]
fn complete_managed_pending_slot_pcm_clock_and_rehash_trace() {
    let (_scheduler, storage, mut c, ledger) = setup();
    for (rate, target) in [(44100, 48000), (48000, 44100)] {
        for channels in [1, 2, 32] {
            let mut ctx = AudioContext::new(7, rate, channels);
            let samples = (0..8192 * channels as usize)
                .map(|i| (i % 127) as f32 / 256.0 - 0.25)
                .collect();
            let buffer = ctx
                .add_buffer(crate::decoder::DecodedAudio {
                    samples,
                    sample_rate: rate,
                    channels,
                })
                .unwrap();
            assert!(ctx.create_buffer_source(10));
            assert!(ctx.set_buffer(10, Some(buffer)));
            ctx.connect(10, crate::context::DESTINATION_NODE_ID);
            ctx.watch_source_ended(10, true);
            ctx.start_source(10, 0.0, 0.0, None);
            ensure(&mut c, &ctx, target);
            let mut result = None;
            wait(|| {
                result = c.take(&ctx);
                result.is_some()
            });
            let mut device = result.unwrap().unwrap();
            let operation = c.slots[&7].operation.clone();
            let mut keep = Vec::new();
            let mut all = Vec::new();
            let mut rendered = Vec::new();
            for frames in [1, 127, 128, 129, 257, 4097] {
                let mut output = vec![0.375; frames * channels as usize + channels as usize - 1];
                let mut done = 0;
                let mut collected = Vec::new();
                let mut ended = Vec::new();
                for _ in 0..200000 {
                    let turn = device
                        .step_context(
                            &mut ctx,
                            &mut output[done * channels as usize..frames * channels as usize],
                            |id| collected.push(id),
                        )
                        .unwrap();
                    if let SessionTurn::Work(Turn::Rendered(n)) = turn {
                        let buffers = device.session().unwrap().pull.buffers.as_ref().unwrap();
                        rendered.extend_from_slice(&buffers.input[..n * channels as usize]);
                    }
                    if let SessionTurn::Work(Turn::Published(n)) = turn {
                        done += n;
                    }
                    while let Some(id) = device.pop_ended_context(&ctx).unwrap() {
                        ended.push(id);
                    }
                    if done == frames {
                        break;
                    }
                }
                assert_eq!(
                    done, frames,
                    "slot PCM trace failed to publish complete block"
                );
                assert!(
                    output[frames * channels as usize..]
                        .iter()
                        .all(|value| *value == 0.375)
                );
                all.extend_from_slice(&output[..frames * channels as usize]);
                collected.sort_unstable();
                ended.sort_unstable();
                let clocks = device.session().unwrap().pull.clocks();
                let bits: Vec<_> = output.iter().map(|value| value.to_bits()).collect();
                println!(
                    "MIGO_SLOT_PCM {rate}:{target}:{channels}:{frames} {:?} {:?} {:?} {:?}",
                    bits,
                    collected,
                    ended,
                    [
                        ctx.current_time().to_bits(),
                        clocks.rendered_source,
                        clocks.published_source,
                        clocks.published_output
                    ]
                );
                if frames == 127 {
                    for id in 100..132 {
                        let other = AudioContext::new(id, 48000, 2);
                        ensure(&mut c, &other, 48000);
                        keep.push(other);
                    }
                }
                if frames == 257 {
                    ensure(&mut c, &ctx, target);
                    assert!(Arc::ptr_eq(&operation, &c.slots[&7].operation));
                }
            }
            rendered.resize(rendered.len() + 4096 * channels as usize, 0.0);
            let reference = crate::resample_job::tests::reference(
                &crate::decoder::DecodedAudio {
                    samples: rendered,
                    sample_rate: rate,
                    channels,
                },
                target,
            );
            assert_eq!(
                all.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
                reference[..all.len()]
                    .iter()
                    .map(|v| (0.375 + *v).to_bits())
                    .collect::<Vec<_>>(),
                "slot table path diverged from complete mature SRC prefix"
            );
            drop(device);
            c.restart();
            drop(keep);
        }
    }
    drop((c, storage));
    wait(|| ledger.used_bytes() == 0);
}
