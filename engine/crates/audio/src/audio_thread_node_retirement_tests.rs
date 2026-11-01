//! Logical collection must not destroy real node/PCM owners in the mix caller.
use super::*;
use crate::{
    limits::{AudioAggregateLedger, PcmBudget, PcmUsage, RetainedAudio},
    nodes::AudioNodeProcessor,
};
use std::sync::{
    Condvar, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

#[derive(Default)]
struct State {
    entered: AtomicUsize,
    finished: AtomicUsize,
    wrong_thread: AtomicBool,
}
struct Gate(Arc<(Mutex<bool>, Condvar)>);
impl Gate {
    fn new() -> Self {
        Self(Arc::new((Mutex::new(false), Condvar::new())))
    }
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
struct Record(Arc<State>);
impl Drop for Record {
    fn drop(&mut self) {
        self.0.finished.fetch_add(1, Ordering::Release);
    }
}
struct Observed {
    node: Box<dyn AudioNodeProcessor>,
    record: Record,
    gate: Option<Arc<(Mutex<bool>, Condvar)>>,
}
impl Drop for Observed {
    fn drop(&mut self) {
        let thread = std::thread::current();
        if !thread.name().unwrap_or("").starts_with("Migo-IO-") {
            self.record.0.wrong_thread.store(true, Ordering::Release);
        }
        self.record.0.entered.fetch_add(1, Ordering::Release);
        if let Some(gate) = &self.gate {
            let mut opened = gate.0.lock().unwrap();
            while !*opened {
                opened = gate.1.wait(opened).unwrap();
            }
        }
    }
}
impl AudioNodeProcessor for Observed {
    fn id(&self) -> u32 {
        self.node.id()
    }
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
    fn is_finished(&self) -> bool {
        self.node.is_finished()
    }
    fn is_producing(&self) -> bool {
        self.node.is_producing()
    }
    fn output_channels(&self) -> u32 {
        self.node.output_channels()
    }
    fn process(
        &mut self,
        inputs: &[f32],
        output: &mut [f32],
        rate: u32,
        channels: u32,
        time: f64,
    ) -> usize {
        self.node.process(inputs, output, rate, channels, time)
    }
}
fn wait(mut ready: impl FnMut() -> bool) {
    let start = Instant::now();
    while !ready() {
        assert!(
            start.elapsed() < Duration::from_secs(15),
            "graph retirement did not complete"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}
fn observed(
    node: crate::nodes::BufferSourceNode,
    state: &Arc<State>,
    gate: Option<&Gate>,
) -> Box<dyn AudioNodeProcessor> {
    Box::new(Observed {
        node: Box::new(node),
        record: Record(state.clone()),
        gate: gate.map(|g| g.0.clone()),
    })
}

pub(super) fn production_drop_and_overflow() {
    for channels in [1, 2, 32] {
        let host = migo_io::scheduler::IoScheduler::new(1);
        let cpu = host.audio_cpu();
        let mut contexts = HashMap::new();
        assert!(create_context_scoped(
            &mut contexts,
            1,
            48000,
            channels,
            Some(&cpu)
        ));
        let ctx = contexts.get_mut(&1).unwrap();
        let state = Arc::new(State::default());
        let context_usage = Arc::new(PcmUsage::new(1 << 20, 8));
        let process_usage = Arc::new(PcmUsage::new(1 << 20, 8));
        let buffer = Arc::new(
            RetainedAudio::try_new(
                DecodedAudio {
                    samples: (0..128).map(|i| 0.25 + i as f32 / 1024.0).collect(),
                    sample_rate: 48000,
                    channels: 1,
                },
                &PcmBudget::new(context_usage.clone(), process_usage.clone()),
            )
            .unwrap(),
        );
        let weak = Arc::downgrade(&buffer);
        let mut index = NodeContextIndex::new();
        for voice in 0..32 {
            let id = 10 + voice * 2;
            let mut source = crate::nodes::BufferSourceNode::new(id, 48000);
            source.set_buffer(Some(buffer.clone()));
            source.start(0.0, 0.0, None, 0.0);
            assert!(ctx.add_node(observed(source, &state, None)));
            assert!(ctx.create_gain(id + 1));
            ctx.set_gain(id + 1, 1.0 / 32.0);
            ctx.connect(id, id + 1);
            ctx.connect(id + 1, crate::context::DESTINATION_NODE_ID);
            ctx.watch_source_ended(id, true);
            ctx.release_node(id + 1);
            index.register(id, 1);
            index.register(id + 1, 1);
        }
        drop(buffer);
        let mut mixer = crate::device_prepare::DeviceMixer::new(cpu);
        let mut output = vec![0.125; 128 * channels as usize];
        let mut ended = Vec::with_capacity(32);
        let before = migo_alloc_probe::thread_counts();
        mix_contexts(
            &mut contexts,
            &mut index,
            &mut mixer,
            &mut output,
            48000,
            channels,
            |id| ended.push(id),
        );
        let after = migo_alloc_probe::thread_counts();
        assert_eq!(
            (after.allocations, after.reallocations),
            (before.allocations, before.reallocations),
            "managed graph retirement allocated in mix"
        );
        if channels == 32 {
            assert!(
                after.bytes_freed - before.bytes_freed < 8 * 128 * 32 * 4,
                "excess render buffers were freed in mix"
            );
        }
        for (frame, values) in output.chunks(channels as usize).enumerate() {
            assert!(
                values
                    .iter()
                    .all(|v| v.to_bits() == (0.125 + 0.25 + frame as f32 / 1024.0).to_bits()),
                "retired source final PCM changed"
            );
        }
        ended.sort_unstable();
        assert_eq!(
            ended,
            (0..32).map(|v| 10 + v * 2).collect::<Vec<_>>(),
            "retired source ended set changed"
        );
        assert!(
            (0..32).all(|v| index.get_context(10 + v * 2).is_none()
                && index.get_context(11 + v * 2).is_none()),
            "retired source index survived logical collection"
        );
        output.fill(0.125);
        mix_contexts(
            &mut contexts,
            &mut index,
            &mut mixer,
            &mut output,
            48000,
            channels,
            |_| panic!("retired source emitted ended twice"),
        );
        assert!(output.iter().all(|v| *v == 0.125));
        wait(|| state.finished.load(Ordering::Acquire) == 32 && weak.upgrade().is_none());
        assert!(
            !state.wrong_thread.load(Ordering::Acquire),
            "node or PCM owner was destroyed outside CPU workers"
        );
        assert_eq!(context_usage.snapshot().bytes, 0);
        assert_eq!(process_usage.snapshot().bytes, 0);
    }
}

pub(super) fn held_credit_and_closed_host() {
    let host = migo_io::scheduler::IoScheduler::new(2);
    let cpu = host.audio_cpu();
    let gate = Gate::new();
    let state = Arc::new(State::default());
    let pcm_bytes = 4 * 1024 * 1024;
    let limit = pcm_bytes + 32 * 1024;
    let ledger = Arc::new(AudioAggregateLedger::new(limit));
    let context_usage = Arc::new(PcmUsage::new(pcm_bytes, 8));
    let process_usage = Arc::new(PcmUsage::new(pcm_bytes, 8));
    let buffer = Arc::new(
        RetainedAudio::try_new(
            DecodedAudio {
                samples: vec![0.25; pcm_bytes / 4],
                sample_rate: 48000,
                channels: 1,
            },
            &PcmBudget::with_aggregate(
                context_usage.clone(),
                process_usage.clone(),
                ledger.clone(),
            ),
        )
        .unwrap(),
    );
    let weak = Arc::downgrade(&buffer);
    let mut ctx = AudioContext::new(1, 48000, 1);
    ctx.prepare_node_retirement(cpu.clone(), ledger.clone())
        .unwrap();
    let mut source = crate::nodes::BufferSourceNode::new(10, 48000);
    source.set_buffer(Some(buffer));
    source.start(0.0, 0.0, None, 0.0);
    source.stop(128.0 / 48000.0);
    assert!(ctx.add_node(observed(source, &state, Some(&gate))));
    ctx.connect(10, crate::context::DESTINATION_NODE_ID);
    ctx.watch_source_ended(10, true);
    let filler = ledger
        .try_reserve(limit - ledger.used_bytes(), "test occupied audio storage")
        .unwrap();
    let mut contexts = HashMap::from([(1, ctx)]);
    let mut index = NodeContextIndex::new();
    index.register(10, 1);
    let mut mixer = crate::device_prepare::DeviceMixer::new(cpu.clone());
    let mut ended = Vec::with_capacity(1);
    let mut output = [0.125; 128];
    mix_contexts(
        &mut contexts,
        &mut index,
        &mut mixer,
        &mut output,
        48000,
        1,
        |id| ended.push(id),
    );
    wait(|| state.entered.load(Ordering::Acquire) == 1);
    let held_bytes = ledger.used_bytes();
    let held_pcm = context_usage.snapshot().bytes;
    let held_owner = weak.upgrade().is_some();
    let denied = !contexts.get_mut(&1).unwrap().create_gain(99);
    host.close();
    drop(host);
    drop(filler);
    let closed_denied = !contexts.get_mut(&1).unwrap().create_gain(100);
    gate.open();
    wait(|| state.finished.load(Ordering::Acquire) == 1 && weak.upgrade().is_none());
    contexts.clear();
    mixer.restart();
    wait(|| ledger.used_bytes() == 0);
    assert_eq!(
        held_bytes, limit,
        "node retirement returned credit before actual destructor"
    );
    assert!(
        held_owner && held_pcm == pcm_bytes,
        "PCM credit escaped a running node destructor"
    );
    assert!(denied, "retirement backlog bypassed physical admission");
    assert!(
        closed_denied && cpu.is_closed(),
        "closed host admitted new graph owner"
    );
    assert_eq!(ended, [10]);
    assert!(output.iter().all(|v| *v == 0.375));
    assert!(
        !state.wrong_thread.load(Ordering::Acquire),
        "closed host node destructor left CPU workers"
    );
    assert_eq!(process_usage.snapshot().bytes, 0);
}
