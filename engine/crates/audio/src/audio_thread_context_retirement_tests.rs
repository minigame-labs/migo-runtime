//! Registry releases and whole-context teardown used to free PCM/graph storage
//! on the audio command caller despite the node-only retirement handoff.
use super::*;
use crate::limits::{AudioAggregateLedger, PcmBudget, PcmUsage};
use std::sync::{
    Condvar, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

#[derive(Default)]
pub(super) struct Dropped {
    pub(super) count: AtomicUsize,
    pub(super) wrong: AtomicBool,
}
pub(super) fn witness(state: &Arc<Dropped>) -> Arc<dyn Fn() + Send + Sync> {
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
pub(super) fn wait(mut ready: impl FnMut() -> bool) {
    let start = Instant::now();
    while !ready() {
        assert!(
            start.elapsed() < Duration::from_secs(15),
            "context resource retirement did not finish"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}
pub(super) struct Gate(Arc<(Mutex<bool>, Condvar)>);
impl Gate {
    pub(super) fn open(&self) {
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
        let mut opened = self.gate.0.lock().unwrap();
        while !*opened {
            opened = self.gate.1.wait(opened).unwrap();
        }
    }
}
pub(super) fn block(cpu: &migo_io::audio_cpu::AudioCpu) -> Gate {
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
pub(super) fn context(
    cpu: &migo_io::audio_cpu::AudioCpu,
    ledger: &Arc<AudioAggregateLedger>,
    usage: &Arc<PcmUsage>,
    process: &Arc<PcmUsage>,
    channels: u32,
) -> AudioContext {
    let budget = PcmBudget::with_aggregate(usage.clone(), process.clone(), ledger.clone());
    let mut ctx = AudioContext::new_with_pcm_budget(1, 48000, channels, budget);
    ctx.prepare_node_retirement(cpu.clone(), ledger.clone())
        .unwrap();
    ctx
}

pub(super) fn registry_and_cow() {
    let host = migo_io::scheduler::IoScheduler::new(1);
    let cpu = host.audio_cpu();
    let ledger = Arc::new(AudioAggregateLedger::new(32 << 20));
    let usage = Arc::new(PcmUsage::new(16 << 20, 16));
    let process = Arc::new(PcmUsage::new(16 << 20, 16));
    let ctx = context(&cpu, &ledger, &usage, &process, 1);
    let mut contexts = HashMap::from([(1, ctx)]);
    let mut index = NodeContextIndex::new();
    let context_credit = ledger.used_bytes();
    let bytes = 4 << 20;
    for cow in [false, true] {
        let ctx = contexts.get_mut(&1).unwrap();
        let id = ctx.create_empty_buffer(1, bytes / 4, 48000).unwrap();
        let state = Arc::new(Dropped::default());
        assert!(ctx.observe_buffer_retirement(id, witness(&state)));
        let mut original = Some(ctx.get_buffer(id).unwrap());
        let weak = Arc::downgrade(original.as_ref().unwrap());
        if !cow {
            drop(original.take());
        }
        let gate = block(&cpu);
        let before = migo_alloc_probe::thread_counts();
        if cow {
            assert!(
                contexts
                    .get_mut(&1)
                    .unwrap()
                    .copy_to_channel(id, &[0.75], 0, 0)
                    .unwrap()
            );
            assert_eq!(original.as_ref().unwrap().samples[0], 0.0);
            drop(original.take());
        } else {
            release_buffer_scoped(&mut contexts, 77, id);
            release_buffer_scoped(&mut contexts, 1, id);
            release_buffer_scoped(&mut contexts, 1, id);
        }
        let freed = migo_alloc_probe::thread_counts().bytes_freed - before.bytes_freed;
        let held = weak.upgrade().is_some();
        let held_pcm = usage.snapshot().bytes;
        gate.open();
        assert!(
            freed < (bytes as u64) / 2,
            "registry resource escaped CPU retirement"
        );
        assert!(held, "registry resource escaped CPU retirement");
        assert_eq!(
            held_pcm,
            if cow {
                bytes as usize * 2
            } else {
                bytes as usize
            },
            "retained PCM credit returned before CPU retirement"
        );
        wait(|| state.count.load(Ordering::Acquire) == 1 && weak.upgrade().is_none());
        assert!(
            !state.wrong.load(Ordering::Acquire),
            "registry resource escaped CPU retirement"
        );
        if cow {
            let copy = contexts[&1].get_buffer(id).unwrap();
            assert_eq!(copy.samples[0], 0.75);
            assert!(copy.samples[1..].iter().all(|v| *v == 0.0));
            drop(copy);
            release_buffer_scoped(&mut contexts, 1, id);
        } else {
            assert!(contexts[&1].get_buffer(id).is_none());
        }
        wait(|| {
            usage.snapshot().bytes == 0
                && process.snapshot().bytes == 0
                && ledger.used_bytes() == context_credit
        });
    }
    // Four PCM bytes fit, but the reserved retirement metadata does not. The
    // rejected creation must preserve the ID and both resource ledgers.
    let baseline = ledger.used_bytes();
    let filler = ledger
        .try_reserve(
            (32 << 20) - baseline - 4,
            "test full retained-resource budget",
        )
        .unwrap();
    let result = contexts
        .get_mut(&1)
        .unwrap()
        .create_empty_buffer(1, 1, 48000);
    let denied = result
        .as_ref()
        .err()
        .is_some_and(|e| e.code == ErrorCode::InputSaturated);
    drop(filler);
    let next_id_preserved = contexts
        .get_mut(&1)
        .unwrap()
        .create_empty_buffer(1, 1, 48000)
        .unwrap()
        == 3;
    assert!(release_context_scoped(&mut contexts, &mut index, 1));
    wait(|| ledger.used_bytes() == 0);
    assert!(
        denied && next_id_preserved,
        "retained resource admission bypassed physical ledger"
    );
}

pub(super) fn close_and_drop() {
    for mode in ["close", "drop", "release"] {
        let host = migo_io::scheduler::IoScheduler::new(2);
        let cpu = host.audio_cpu();
        let ledger = Arc::new(AudioAggregateLedger::new(32 << 20));
        let usage = Arc::new(PcmUsage::new(8 << 20, 16));
        let process = Arc::new(PcmUsage::new(8 << 20, 16));
        let mut ctx = context(&cpu, &ledger, &usage, &process, 32);
        let id = ctx.create_empty_buffer(1, 1 << 20, 48000).unwrap();
        let buffer_state = Arc::new(Dropped::default());
        let storage_state = Arc::new(Dropped::default());
        assert!(ctx.observe_buffer_retirement(id, witness(&buffer_state)));
        ctx.observe_context_retirement(witness(&storage_state));
        let weak = Arc::downgrade(&ctx.get_buffer(id).unwrap());
        let epoch = ctx.device_epoch();
        let mut index = NodeContextIndex::new();
        assert!(ctx.create_buffer_source(5));
        assert!(ctx.set_buffer(5, Some(id)));
        assert!(ctx.start_source(5, 0.0, 0.0, None));
        ctx.connect(5, crate::context::DESTINATION_NODE_ID);
        ctx.watch_source_ended(5, true);
        index.register(5, 1);
        assert!(ctx.has_active_sources());
        for node in 10..138 {
            assert!(ctx.create_gain(node));
            index.register(node, 1);
        }
        let mut contexts = HashMap::from([(1, ctx)]);
        let gate = block(&cpu);
        let old_credit = ledger.used_bytes();
        let before = migo_alloc_probe::thread_counts();
        match mode {
            "close" => {
                contexts.get_mut(&1).unwrap().close();
                contexts.get_mut(&1).unwrap().close();
            }
            "drop" => {
                contexts.clear();
                index.clear_all();
            }
            _ => {
                assert!(release_context_scoped(&mut contexts, &mut index, 1));
                assert!(!release_context_scoped(&mut contexts, &mut index, 1));
            }
        }
        let freed = migo_alloc_probe::thread_counts().bytes_freed - before.bytes_freed;
        let held = weak.upgrade().is_some();
        let held_credit = ledger.used_bytes();
        let held_pcm = usage.snapshot().bytes;
        let mut rejects_closed = true;
        if mode == "close" {
            let ctx = contexts.get_mut(&1).unwrap();
            assert_eq!(ctx.state(), AudioContextState::Closed);
            assert!(ctx.get_buffer(id).is_none());
            assert!(
                ctx.drain_ended().next().is_none(),
                "context close emitted source ended"
            );
            rejects_closed = !ctx.create_gain(200) && ctx.create_empty_buffer(1, 1, 48000).is_err();
        }
        host.close();
        drop(host);
        gate.open();
        assert!(freed < 1 << 20, "context storage freed on audio caller");
        assert!(
            held && held_pcm == 4 << 20 && held_credit == old_credit,
            "context resource credit escaped CPU retirement"
        );
        assert!(!epoch.is_live(), "closed context retained device authority");
        assert!(
            rejects_closed,
            "closed context accepted new graph resources"
        );
        wait(|| weak.upgrade().is_none() && ledger.used_bytes() == 0);
        assert_eq!(buffer_state.count.load(Ordering::Acquire), 1);
        assert_eq!(storage_state.count.load(Ordering::Acquire), 1);
        assert!(
            !buffer_state.wrong.load(Ordering::Acquire)
                && !storage_state.wrong.load(Ordering::Acquire),
            "context storage freed outside CPU workers"
        );
        if mode == "close" {
            assert!(release_context_scoped(&mut contexts, &mut index, 1));
        }
        assert!(
            index.get_context(5).is_none()
                && (10..138).all(|node| index.get_context(node).is_none()),
            "closed context retained node index"
        );
        assert_eq!(process.snapshot().bytes, 0);
    }
}
