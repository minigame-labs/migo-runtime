//! Physical Box accounting and the actual Rust render/release handoff.
use super::super::{Callbacks, into_callbacks, observe_callback, prepare};
use super::*;
use crate::limits::AudioAggregateLedger;
use crate::node_retirement::HANDOFF_BYTES;
use crate::output::{AudioSync, RING_BUFFER_FRAMES, ring};
use ringbuf::{
    traits::{Observer, Producer},
    wrap::Wrap,
};
use std::{
    cell::Cell,
    collections::VecDeque,
    sync::{
        Arc, Condvar, Mutex,
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
            "Oboe callback disposal did not complete"
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
fn dummy() -> (ring::Producer, AudioSync, OutputCallback) {
    let (p, c) = ring::new(64, None).unwrap();
    let s = AudioSync::new();
    let v = OutputCallback::new(c, s.clone(), 0);
    (p, s, v)
}
fn base(host: &Host, ledger: &Arc<AudioAggregateLedger>) -> usize {
    let initial = ledger.used_bytes();
    let s = AudioSync::with_host(Some(host)).unwrap();
    let (p, c) = ring::new(RING_BUFFER_FRAMES * 2, Some(host)).unwrap();
    let bytes = ledger.used_bytes() - initial;
    drop((p, c, s));
    wait(|| ledger.used_bytes() == initial);
    bytes
}
struct Context {
    pointer: Option<usize>,
    render: unsafe extern "C" fn(*mut std::ffi::c_void, *mut f32, usize),
    release: unsafe extern "C" fn(*mut std::ffi::c_void),
}
impl Context {
    fn new(c: Callbacks) -> Self {
        Self {
            pointer: Some(c.context as usize),
            render: c.render,
            release: c.release,
        }
    }
    fn render(&mut self, out: &mut [f32]) {
        // SAFETY: unique live context; calls are serialized and output is exclusive.
        unsafe { (self.render)(self.pointer.unwrap() as *mut _, out.as_mut_ptr(), out.len()) };
    }
}
impl Drop for Context {
    fn drop(&mut self) {
        if let Some(p) = self.pointer.take() {
            // SAFETY: this fixture is the last owner and all data calls have ended.
            unsafe { (self.release)(p as *mut _) };
        }
    }
}
#[test]
fn actual_box_layout_and_production_charge_match_real_allocation() {
    let (p, s, value) = dummy();
    let before = migo_alloc_probe::thread_counts();
    let raw = Box::new(Data {
        before_drop: Witness::default(),
        callback: value,
        _permit: None,
        after_refund: Witness::default(),
    });
    let made = migo_alloc_probe::thread_counts();
    drop(raw);
    let after = migo_alloc_probe::thread_counts();
    assert_eq!(
        made.bytes_allocated - before.bytes_allocated,
        Admission::bytes() as u64,
        "Oboe callback Box allocation layout mismatch"
    );
    assert_eq!(
        after.bytes_freed - made.bytes_freed,
        Admission::bytes() as u64
    );
    drop((p, s));
    let (_s, h, l) = setup();
    let baseline = base(&h, &l);
    let ready = prepare(&h).unwrap();
    assert_eq!(
        l.used_bytes(),
        HANDOFF_BYTES + baseline + Admission::bytes(),
        "Oboe callback Box omitted physical credit"
    );
    drop(ready);
    wait(|| l.used_bytes() == HANDOFF_BYTES);
    drop(h);
    wait(|| l.used_bytes() == 0);
}
#[test]
fn physical_refusal_preserves_old_prepared_output_and_original_pcm() {
    let (_s, h, l) = setup();
    let baseline = base(&h, &l);
    let mut old = prepare(&h).unwrap();
    old.sync.configure_device(2, 192);
    assert_eq!(old.producer.push_slice(&[0.25; 7]), 7);
    let pointer = old.producer.rb_ref().as_ref() as *const _ as usize;
    let occupied = old.producer.occupied_len();
    let peer = l
        .try_reserve(LIMIT - l.used_bytes() - baseline, "Oboe replacement peer")
        .unwrap();
    let used = l.used_bytes();
    let candidate = prepare(&h);
    assert!(
        candidate.is_err(),
        "Oboe callback capacity bypassed physical refusal"
    );
    assert_eq!(old.producer.rb_ref().as_ref() as *const _ as usize, pointer);
    assert_eq!(old.producer.occupied_len(), occupied);
    assert_eq!(
        l.used_bytes(),
        used,
        "Oboe refusal changed old physical credit"
    );
    let mut context = Context::new(into_callbacks(old.callback));
    let mut out = [0.0; 7];
    context.render(&mut out);
    assert_eq!(out, [0.25; 7], "Oboe refusal changed old PCM");
    drop((context, old.producer, old.sync, candidate, peer, h));
    wait(|| l.used_bytes() == 0);
}
#[test]
fn admission_precedes_factory_and_all_production_preparation_allocations() {
    let (_s, h, l) = setup();
    let start = l.used_bytes();
    let sync = AudioSync::with_host(Some(&h)).unwrap();
    let sync_bytes = l.used_bytes() - start;
    drop(sync);
    let free = Admission::bytes().max(sync_bytes);
    let peer = l
        .try_reserve(LIMIT - l.used_bytes() - free, "Oboe preparation peer")
        .unwrap();
    let admission = Admission::new(&h).unwrap();
    let before = migo_alloc_probe::thread_counts();
    let control = h.reserve_inline(sync_bytes);
    let after = migo_alloc_probe::thread_counts();
    assert!(control.is_err());
    let expected = after.allocations - before.allocations;
    drop((admission, control));
    let before = migo_alloc_probe::thread_counts();
    let rejected = prepare(&h);
    let after = migo_alloc_probe::thread_counts();
    assert!(rejected.is_err());
    assert_eq!(
        after.allocations - before.allocations,
        expected,
        "Oboe preparation allocated before callback Box admission"
    );
    drop(rejected);
    let rest = l.try_reserve(free, "Oboe full peer").unwrap();
    let calls = Cell::new(0);
    let before = migo_alloc_probe::thread_counts();
    let rejected = Admission::new(&h).map(|admission| {
        calls.set(calls.get() + 1);
        let (_p, _s, value) = dummy();
        admission.finish(value)
    });
    let after = migo_alloc_probe::thread_counts();
    assert!(rejected.is_err(), "Oboe callback Box bypassed admission");
    assert_eq!(calls.get(), 0, "Oboe callback factory ran before admission");
    assert_eq!(
        after.allocations - before.allocations,
        expected,
        "Oboe callback Box allocated before admission"
    );
    drop((rejected, rest, peer, h));
    wait(|| l.used_bytes() == 0);
}
#[test]
fn unused_admission_refunds_without_allocating_and_closed_host_refuses() {
    let (s, h, l) = setup();
    let before = migo_alloc_probe::thread_counts();
    let admission = Admission::new(&h).unwrap();
    let made = migo_alloc_probe::thread_counts();
    assert_eq!(made.allocations - before.allocations, 0);
    assert_eq!(
        l.used_bytes(),
        HANDOFF_BYTES + Admission::bytes(),
        "Oboe unused callback admission omitted credit"
    );
    drop(admission);
    assert_eq!(l.used_bytes(), HANDOFF_BYTES);
    s.close();
    assert!(
        Admission::new(&h).is_err(),
        "Oboe callback admitted after host close"
    );
    assert!(prepare(&h).is_err());
    drop(h);
    wait(|| l.used_bytes() == 0);
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
    let g = Gate(Arc::new((Mutex::new(false), Condvar::new())));
    let entered = Arc::new(AtomicBool::new(false));
    let value = Blocked {
        gate: g.0.clone(),
        entered: entered.clone(),
    };
    let r = cpu.retirement();
    crate::streaming::get_stream_runtime().spawn(async move {
        r.retire(value).await;
    });
    wait(|| entered.load(Ordering::Acquire));
    g
}
#[test]
fn final_native_release_frees_box_before_refund_and_bulk_on_original_cpu() {
    let (s, h, l) = setup();
    let disposed = Arc::new(AtomicUsize::new(0));
    let correct = Arc::new(AtomicBool::new(true));
    let watched = Arc::new(std::sync::OnceLock::<migo_alloc_probe::DeallocationWatch>::new());
    let (a, b, c) = (disposed.clone(), correct.clone(), watched.clone());
    let hook = Arc::new(move || {
        b.fetch_and(
            std::thread::current()
                .name()
                .is_some_and(|n| n.starts_with("Migo-IO-"))
                && c.get().unwrap().freed_bytes() == Some(RING_BUFFER_FRAMES * 2 * 4),
            Ordering::AcqRel,
        );
        a.fetch_add(1, Ordering::AcqRel);
    });
    let mut ready = ring::observe(hook, || prepare(&h)).unwrap();
    let pointer = ready.producer.vacant_slices().0.as_ptr().cast::<u8>();
    let before_watch = migo_alloc_probe::thread_counts();
    let watch = migo_alloc_probe::DeallocationWatch::new(pointer).unwrap();
    let after_watch = migo_alloc_probe::thread_counts();
    assert_eq!(
        after_watch.allocation_events(),
        before_watch.allocation_events()
    );
    assert!(watched.set(watch).is_ok());
    ready.sync.configure_device(2, 192);
    let full = l.used_bytes();
    let sync_bytes = ready.sync.storage_bytes();
    let after_expected = full - sync_bytes - Admission::bytes();
    let seen = Arc::new(AtomicUsize::new(0));
    let valid = Arc::new(AtomicBool::new(true));
    let base = Arc::new(AtomicU64::new(0));
    let (a, b, c, d) = (seen.clone(), valid.clone(), base.clone(), l.clone());
    let (e, f, g, j) = (seen.clone(), valid.clone(), base.clone(), l.clone());
    observe_callback(
        &mut ready.callback,
        Arc::new(move || {
            b.fetch_and(
                d.used_bytes() == full
                    && migo_alloc_probe::thread_counts().bytes_freed - c.load(Ordering::Acquire)
                        >= Admission::bytes() as u64,
                Ordering::AcqRel,
            );
            a.fetch_add(1, Ordering::AcqRel);
        }),
        Arc::new(move || {
            f.fetch_and(
                j.used_bytes() == after_expected
                    && migo_alloc_probe::thread_counts().bytes_freed - g.load(Ordering::Acquire)
                        >= Admission::bytes() as u64,
                Ordering::AcqRel,
            );
            e.fetch_add(1, Ordering::AcqRel);
        }),
    );
    let context = Context::new(into_callbacks(ready.callback));
    assert_eq!(
        l.used_bytes(),
        full,
        "Oboe handoff returned callback Box credit early"
    );
    let gate = block(&s.audio_cpu());
    s.close();
    drop((ready.producer, ready.sync));
    assert_eq!(
        l.used_bytes(),
        full,
        "Oboe owner drop released live callback credit"
    );
    let stats = std::thread::spawn(move || {
        base.store(
            migo_alloc_probe::thread_counts().bytes_freed,
            Ordering::Release,
        );
        let before = migo_alloc_probe::thread_counts();
        drop(context);
        let after = migo_alloc_probe::thread_counts();
        (
            after.allocations - before.allocations,
            after.reallocations - before.reallocations,
            after.bytes_freed - before.bytes_freed,
        )
    })
    .join()
    .unwrap();
    assert_eq!(
        (stats.0, stats.1),
        (0, 0),
        "Oboe native release allocated cleanup"
    );
    assert!(
        stats.2 < (RING_BUFFER_FRAMES * 2 * 4) as u64,
        "Oboe bulk backing freed on callback caller"
    );
    assert_eq!(
        seen.load(Ordering::Acquire),
        2,
        "Oboe final native context was not actually released"
    );
    assert!(
        valid.load(Ordering::Acquire),
        "Oboe callback refund preceded actual Box free or payload drop"
    );
    assert_eq!(
        l.used_bytes(),
        after_expected,
        "Oboe bulk credit returned before original CPU drop"
    );
    assert_eq!(disposed.load(Ordering::Acquire), 0);
    assert_eq!(
        watched.get().unwrap().freed_bytes(),
        None,
        "Oboe original backing freed before original CPU release"
    );
    gate.open();
    wait(|| disposed.load(Ordering::Acquire) == 1 && l.used_bytes() == HANDOFF_BYTES);
    assert!(
        correct.load(Ordering::Acquire),
        "Oboe backing was not really freed on original CPU"
    );
    drop(h);
    wait(|| l.used_bytes() == 0);
}
#[test]
fn cancelling_before_handoff_destroys_payload_once_after_box_free_without_heap() {
    let (_s, h, l) = setup();
    let (p, c) = ring::new(64, None).unwrap();
    let mut sync = AudioSync::new();
    let seen = Arc::new(AtomicUsize::new(0));
    let a = seen.clone();
    let b = seen.clone();
    sync.observe_storage(
        Arc::new(move || {
            a.fetch_add(1, Ordering::AcqRel);
        }),
        Arc::new(move || {
            b.fetch_add(1, Ordering::AcqRel);
        }),
    );
    let callback = Admission::new(&h)
        .unwrap()
        .finish(OutputCallback::new(c, sync, 0));
    let before = migo_alloc_probe::thread_counts();
    drop(callback);
    let after = migo_alloc_probe::thread_counts();
    assert_eq!(
        seen.load(Ordering::Acquire),
        2,
        "Oboe callback payload was not destroyed"
    );
    assert_eq!(
        (
            after.allocations - before.allocations,
            after.reallocations - before.reallocations
        ),
        (0, 0),
        "Oboe callback cancellation allocated cleanup"
    );
    assert_eq!(
        l.used_bytes(),
        HANDOFF_BYTES,
        "Oboe cancellation did not refund physical Box credit"
    );
    drop((p, h));
    wait(|| l.used_bytes() == 0);
}
#[test]
fn admitted_callback_handoff_and_first_steady_render_work_at_full_budget_after_close() {
    let (s, h, l) = setup();
    let mut ready = prepare(&h).unwrap();
    ready.sync.configure_device(2, 192);
    let peer = l
        .try_reserve(LIMIT - l.used_bytes(), "Oboe render peer")
        .unwrap();
    s.close();
    let before = migo_alloc_probe::thread_counts();
    let mut context = Context::new(into_callbacks(ready.callback));
    let after = migo_alloc_probe::thread_counts();
    assert_eq!(
        (
            after.allocations - before.allocations,
            after.reallocations - before.reallocations
        ),
        (0, 0),
        "Oboe native handoff allocated"
    );
    assert_eq!(l.used_bytes(), LIMIT, "Oboe handoff lost live Box credit");
    let input = [0.25f32; 256];
    let mut out = [0.0f32; 256];
    let before = migo_alloc_probe::thread_counts();
    for _ in 0..64 {
        assert_eq!(ready.producer.push_slice(&input), 256);
        context.render(&mut out);
        assert_eq!(out, input, "Oboe native callback PCM changed");
    }
    let after = migo_alloc_probe::thread_counts();
    assert_eq!(
        (
            after.allocations - before.allocations,
            after.reallocations - before.reallocations
        ),
        (0, 0),
        "Oboe first or steady render allocated"
    );
    drop((context, peer, ready.producer, ready.sync, h));
    wait(|| l.used_bytes() == 0);
}
#[test]
fn abandoned_prepared_owner_refunds_all_resources_without_a_native_handle() {
    let (_s, h, l) = setup();
    let ready = prepare(&h).unwrap();
    assert!(l.used_bytes() > HANDOFF_BYTES + Admission::bytes());
    drop(ready);
    wait(|| l.used_bytes() == HANDOFF_BYTES);
    let again = prepare(&h).unwrap();
    drop((again, h));
    wait(|| l.used_bytes() == 0);
}
#[test]
fn raw_handoff_keeps_original_ring_and_sync_addresses() {
    assert_eq!(
        std::mem::size_of::<Callbacks>(),
        3 * std::mem::size_of::<usize>()
    );
    assert_eq!(
        std::mem::size_of::<super::super::Config>(),
        6 * std::mem::size_of::<i32>()
    );
    let (_s, h, l) = setup();
    let (p, s, value) = dummy();
    let ring_pointer = p.rb_ref().as_ref() as *const _ as usize;
    let sync_pointer = &*s.inner as *const _ as usize;
    let owner = Admission::new(&h).unwrap().finish(value);
    let before = migo_alloc_probe::thread_counts();
    let raw = owner.into_raw();
    let after = migo_alloc_probe::thread_counts();
    assert_eq!(
        after.allocations - before.allocations,
        0,
        "Oboe raw Box handoff allocated"
    );
    // SAFETY: this fixture holds the unique live Box; no native code has it yet.
    let data = unsafe { &*raw };
    assert_eq!(
        data.callback.consumer.rb_ref().as_ref() as *const _ as usize,
        ring_pointer,
        "Oboe callback Box copied its ring"
    );
    assert_eq!(
        &*data.callback.sync.inner as *const _ as usize, sync_pointer,
        "Oboe callback Box copied its sync"
    );
    // SAFETY: no callback accesses remain and this is the unique release.
    unsafe { release(raw) };
    assert_eq!(l.used_bytes(), HANDOFF_BYTES);
    drop((p, s, h));
    wait(|| l.used_bytes() == 0);
}
#[test]
fn complete_native_handoff_pcm_and_refill_trace() {
    let (_s, h, l) = setup();
    for hint in [0usize, 128, 192, 513] {
        let mut ready = prepare(&h).unwrap();
        ready.sync.configure_device(2, hint);
        let mut context = Context::new(into_callbacks(ready.callback));
        let mut oracle = VecDeque::new();
        for (round, frames) in [0usize, 1, 17, 127, 128, 129, 511, 512, 513, 257, 4097, 8193]
            .into_iter()
            .enumerate()
        {
            let len = frames * 2 + 1;
            ready.sync.set_callback_wakeup_enabled(round % 2 == 1);
            let input: Vec<_> = (0..(len / 2 + 17).min(ready.producer.vacant_len()))
                .map(|n| match n % 7 {
                    0 => -2.0,
                    1 => -0.0,
                    2 => 2.0,
                    _ => (n % 97) as f32 / 64.0 - 0.75,
                })
                .collect();
            assert_eq!(ready.producer.push_slice(&input), input.len());
            oracle.extend(input);
            let mut out = vec![0.375f32; len];
            context.render(&mut out);
            let bits: Vec<_> = out.iter().map(|v| v.to_bits()).collect();
            let expected: Vec<_> = (0..len)
                .map(|_| oracle.pop_front().unwrap_or(0.0f32).to_bits())
                .collect();
            assert_eq!(bits, expected, "Oboe complete callback Box PCM changed");
            ready.sync.refresh_device_policy();
            let state = [
                ready.sync.buffer_level() as u64,
                ready.sync.max_callback() as u64,
                ready.sync.check_and_clear() as u64,
                ready.sync.software_underrun_count() as u64,
                ready.sync.inner.policy.get().unwrap().low() as u64,
                ready.sync.inner.policy.get().unwrap().high() as u64,
                ready.producer.occupied_len() as u64,
            ];
            println!("MIGO_OBOE_BOX_PCM {hint}:{round} {:?} {:?}", bits, state);
        }
        drop((context, ready.producer, ready.sync));
        wait(|| l.used_bytes() == HANDOFF_BYTES);
    }
    drop(h);
    wait(|| l.used_bytes() == 0);
}
