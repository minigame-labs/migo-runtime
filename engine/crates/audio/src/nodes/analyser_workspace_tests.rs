use super::*;
use crate::{
    context::AudioContext,
    limits::{AudioAggregateLedger, PcmBudget, PcmUsage},
};
use std::sync::{
    Arc, Condvar, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use std::time::{Duration, Instant};
#[path = "analyser_workspace_reference.rs"]
mod reference;
const MAX: usize = 16 << 20;
fn wait(mut ready: impl FnMut() -> bool) {
    let start = Instant::now();
    while !ready() {
        assert!(
            start.elapsed() < Duration::from_secs(15),
            "FFT workspace retirement did not finish"
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
struct Block {
    gate: Arc<(Mutex<bool>, Condvar)>,
    entered: Arc<AtomicBool>,
}
impl Drop for Block {
    fn drop(&mut self) {
        self.entered.store(true, Ordering::Release);
        let mut ready = self.gate.0.lock().unwrap();
        while !*ready {
            ready = self.gate.1.wait(ready).unwrap();
        }
    }
}
fn block(cpu: &migo_io::audio_cpu::AudioCpu) -> Gate {
    let gate = Gate(Arc::new((Mutex::new(false), Condvar::new())));
    let entered = Arc::new(AtomicBool::new(false));
    let value = Block {
        gate: gate.0.clone(),
        entered: entered.clone(),
    };
    let lane = cpu.retirement();
    crate::streaming::get_stream_runtime().spawn(async move {
        lane.retire(value).await;
    });
    wait(|| entered.load(Ordering::Acquire));
    gate
}
fn context(
    cpu: &migo_io::audio_cpu::AudioCpu,
    ledger: &Arc<AudioAggregateLedger>,
    channels: u32,
) -> AudioContext {
    let budget = PcmBudget::with_aggregate(
        Arc::new(PcmUsage::new(MAX, 64)),
        Arc::new(PcmUsage::new(MAX, 64)),
        ledger.clone(),
    );
    let mut ctx = AudioContext::new_with_pcm_budget(1, 48000, channels, budget);
    ctx.prepare_node_retirement(cpu.clone(), ledger.clone())
        .unwrap();
    ctx
}
fn same(a: &[f32], b: &[f32]) {
    assert!(
        a.len() == b.len() && a.iter().zip(b).all(|(a, b)| a.to_bits() == b.to_bits()),
        "FFT workspace changed complete samples or history"
    );
}
fn pointers(node: &AnalyserNode) -> [usize; 6] {
    let w = &*node.workspace;
    [
        w.time_domain_buffer.as_ptr() as usize,
        w.fft_re.as_ptr() as usize,
        w.fft_im.as_ptr() as usize,
        w.window.as_ptr() as usize,
        w.twiddles.as_ptr() as usize,
        w.smoothed.as_ptr() as usize,
    ]
}
#[test]
fn factory_admits_all_six_capacities_before_allocating_and_final_drop_uses_cpu() {
    let scheduler = migo_io::scheduler::IoScheduler::new(1);
    let cpu = scheduler.audio_cpu();
    let ledger = Arc::new(AudioAggregateLedger::new(MAX));
    let mut ctx = context(&cpu, &ledger, 2);
    let base = ledger.used_bytes();
    let pressure = ledger
        .try_reserve(MAX - base - 2048 * 40 + 1, "peer")
        .unwrap();
    let before = migo_alloc_probe::thread_counts();
    assert!(
        !ctx.create_analyser(20),
        "analyser factory bypassed workspace admission"
    );
    assert!(
        migo_alloc_probe::thread_counts().bytes_allocated - before.bytes_allocated < 2048 * 40,
        "FFT factory allocated before admission"
    );
    assert!(!ctx.node_is::<AnalyserNode>(20));
    drop(pressure);
    assert!(ctx.create_analyser(20));
    let live = ledger.used_bytes();
    assert!(
        live - base >= 2048 * 40 + 8192,
        "six FFT capacities are not physically charged"
    );
    ctx.with_node_typed::<AnalyserNode, _>(20, |node| {
        assert_eq!(node.workspace.capacity_bytes(), 2048 * 40)
    });
    let gate = block(&cpu);
    let before = migo_alloc_probe::thread_counts();
    ctx.close();
    assert!(
        migo_alloc_probe::thread_counts().bytes_freed - before.bytes_freed < 2048 * 40,
        "FFT workspace freed on caller"
    );
    assert!(ledger.used_bytes() >= 2048 * 40);
    gate.open();
    wait(|| ledger.used_bytes() == 0);
}
#[test]
fn resize_matches_frozen_old_analyser_and_keeps_both_workspaces_until_cpu_drop() {
    for channels in [1, 2, 32] {
        let scheduler = migo_io::scheduler::IoScheduler::new(1);
        let cpu = scheduler.audio_cpu();
        let ledger = Arc::new(AudioAggregateLedger::new(MAX));
        let mut ctx = context(&cpu, &ledger, channels);
        assert!(ctx.create_analyser(20));
        let mut old = reference::AnalyserNode::new(20, channels);
        old.set_min_decibels(-83.5);
        old.set_max_decibels(-19.25);
        old.set_smoothing_time_constant(0.59);
        ctx.with_node_typed::<AnalyserNode, _>(20, |n| {
            n.set_min_decibels(-83.5);
            n.set_max_decibels(-19.25);
            n.set_smoothing_time_constant(0.59);
        });
        let input: Vec<_> = (0..128 * channels)
            .map(|i| (i % 37) as f32 / 37.0 - 0.5)
            .collect();
        let mut out = vec![0.0; input.len()];
        let mut want = out.clone();
        for size in [2048_usize, 32768, 32, 64, 1025, 0, 32000, 32768, 33, 2048] {
            let before_size = old.get_float_time_domain_data().len();
            let size_norm = size.next_power_of_two().clamp(32, 32768);
            let base = ledger.used_bytes();
            let retired = Arc::new(AtomicUsize::new(0));
            let wrong = Arc::new(AtomicBool::new(false));
            ctx.with_node_typed::<AnalyserNode, _>(20, |n| {
                let retired = retired.clone();
                let wrong = wrong.clone();
                n.workspace.observe(Arc::new(move || {
                    if !std::thread::current()
                        .name()
                        .unwrap_or("")
                        .starts_with("Migo-IO-")
                    {
                        wrong.store(true, Ordering::Release);
                    }
                    retired.fetch_add(1, Ordering::Release);
                }));
            });
            let gate = block(&cpu);
            let before = migo_alloc_probe::thread_counts();
            assert!(ctx.set_analyser_fft_size(20, size));
            if size_norm == before_size {
                assert_eq!(
                    migo_alloc_probe::thread_counts().allocations,
                    before.allocations,
                    "unchanged FFT allocated workspace"
                );
                assert_eq!(ledger.used_bytes(), base);
            } else {
                assert!(
                    ledger.used_bytes() >= base + size_norm * 40 + 4096,
                    "overlapping FFT workspaces lost physical credit"
                );
                assert_eq!(
                    retired.load(Ordering::Acquire),
                    0,
                    "FFT workspace escaped CPU retirement"
                );
            }
            gate.open();
            if size_norm != before_size {
                wait(|| {
                    retired.load(Ordering::Acquire) == 1
                        && ledger.used_bytes() == base - before_size * 40 + size_norm * 40
                });
                assert!(!wrong.load(Ordering::Acquire));
            }
            old.set_fft_size(size);
            ctx.with_node_typed::<AnalyserNode, _>(20, |node| {
                assert_eq!(node.workspace.capacity_bytes(), size_norm * 40);
                same(
                    &node.get_float_time_domain_data(),
                    &old.get_float_time_domain_data(),
                );
                for _ in 0..2 {
                    let before = migo_alloc_probe::thread_counts();
                    node.process(&input, &mut out, 48000, channels, 0.0);
                    let after = migo_alloc_probe::thread_counts();
                    assert_eq!(
                        (
                            after.allocations - before.allocations,
                            after.reallocations - before.reallocations
                        ),
                        (0, 0),
                        "FFT first render allocated"
                    );
                    old.process(&input, &mut want, 48000, channels, 0.0);
                    same(&out, &want);
                    same(
                        &node.get_float_time_domain_data(),
                        &old.get_float_time_domain_data(),
                    );
                    assert_eq!(
                        node.get_byte_time_domain_data(),
                        old.get_byte_time_domain_data()
                    );
                    assert_eq!(
                        node.get_byte_frequency_data(),
                        old.get_byte_frequency_data()
                    );
                    same(
                        &node.get_float_frequency_data(),
                        &old.get_float_frequency_data(),
                    );
                }
            });
        }
        ctx.close();
        wait(|| ledger.used_bytes() == 0);
    }
}
#[test]
fn refused_resize_preserves_all_six_pointers_and_smoothing_state() {
    let scheduler = migo_io::scheduler::IoScheduler::new(1);
    let cpu = scheduler.audio_cpu();
    let ledger = Arc::new(AudioAggregateLedger::new(MAX));
    let mut ctx = context(&cpu, &ledger, 2);
    assert!(ctx.create_analyser(20));
    let pointers_before = ctx
        .with_node_typed::<AnalyserNode, _>(20, |n| {
            n.get_float_frequency_data();
            pointers(n)
        })
        .unwrap();
    let base = ledger.used_bytes();
    let pressure = ledger.try_reserve(MAX - base, "peer").unwrap();
    let before = migo_alloc_probe::thread_counts();
    assert!(
        !ctx.set_analyser_fft_size(20, 32768),
        "FFT resize ignored physical refusal"
    );
    assert!(
        migo_alloc_probe::thread_counts().bytes_allocated - before.bytes_allocated < 32768 * 40,
        "FFT resize allocated before admission"
    );
    ctx.with_node_typed::<AnalyserNode, _>(20, |n| {
        assert_eq!(
            pointers(n),
            pointers_before,
            "FFT refusal took live storage"
        );
        assert_eq!(n.fft_size(), 2048);
        assert!(n.workspace.smoothed.iter().all(|x| *x == 0.0));
    });
    assert_eq!(ledger.used_bytes(), MAX);
    drop(pressure);
    assert!(ctx.set_analyser_fft_size(20, 32768));
    ctx.close();
    wait(|| ledger.used_bytes() == 0);
}
#[test]
fn prebuilt_workspace_is_adopted_without_copy_and_foreign_ledger_is_refused() {
    let scheduler = migo_io::scheduler::IoScheduler::new(1);
    let cpu = scheduler.audio_cpu();
    let ledger = Arc::new(AudioAggregateLedger::new(MAX));
    let mut ctx = context(&cpu, &ledger, 2);
    let mut node = AnalyserNode::new(20, 2);
    node.set_fft_size(32768);
    let ptrs = pointers(&node);
    let base = ledger.used_bytes();
    assert!(ctx.add_node(Box::new(node)));
    assert!(
        ledger.used_bytes() >= base + 32768 * 40 + 8192,
        "adopted FFT workspace omitted actual capacity"
    );
    ctx.with_node_typed::<AnalyserNode, _>(20, |n| {
        assert_eq!(pointers(n), ptrs, "FFT adoption copied storage")
    });
    let other = Arc::new(AudioAggregateLedger::new(MAX));
    let host = crate::node_retirement::Host::new(cpu.clone(), other.clone(), 2).unwrap();
    let node = AnalyserNode::new_with_retirement(30, 2, Some(&host)).unwrap();
    assert!(
        !ctx.add_node(Box::new(node)),
        "foreign FFT ledger was silently reused"
    );
    drop(host);
    ctx.close();
    wait(|| ledger.used_bytes() == 0 && other.used_bytes() == 0);
}
#[test]
fn generic_node_refusal_keeps_prepared_workspace_owned_until_cpu_release() {
    let scheduler = migo_io::scheduler::IoScheduler::new(1);
    let cpu = scheduler.audio_cpu();
    let ledger = Arc::new(AudioAggregateLedger::new(MAX));
    let mut ctx = context(&cpu, &ledger, 2);
    let base = ledger.used_bytes();
    let node = AnalyserNode::new_with_retirement(20, 2, ctx.response_retirement_host()).unwrap();
    let cost = ledger.used_bytes() - base;
    let pressure = ledger
        .try_reserve(MAX - ledger.used_bytes(), "peer")
        .unwrap();
    let gate = block(&cpu);
    assert!(!ctx.add_node(Box::new(node)));
    drop(pressure);
    assert_eq!(
        ledger.used_bytes(),
        base + cost,
        "rejected node returned live FFT workspace credit"
    );
    gate.open();
    wait(|| ledger.used_bytes() == base);
    ctx.close();
    wait(|| ledger.used_bytes() == 0);
}
#[test]
fn closed_host_rejects_new_and_replacement_workspaces_without_allocation() {
    let scheduler = migo_io::scheduler::IoScheduler::new(1);
    let cpu = scheduler.audio_cpu();
    let ledger = Arc::new(AudioAggregateLedger::new(MAX));
    let mut ctx = context(&cpu, &ledger, 2);
    assert!(ctx.create_analyser(20));
    scheduler.close();
    let base = ledger.used_bytes();
    let before = migo_alloc_probe::thread_counts();
    assert!(!ctx.create_analyser(30));
    assert!(!ctx.set_analyser_fft_size(20, 32768));
    assert!(migo_alloc_probe::thread_counts().bytes_allocated - before.bytes_allocated < 2048 * 40);
    assert_eq!(ledger.used_bytes(), base);
    ctx.close();
    wait(|| ledger.used_bytes() == 0);
}
