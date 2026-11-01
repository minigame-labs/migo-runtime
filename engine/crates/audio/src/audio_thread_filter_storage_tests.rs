use super::context_retirement_tests::{Dropped, block, wait, witness};
use super::*;
use crate::limits::{AudioAggregateLedger, PcmUsage};
use crate::node_storage::{CapacityBytes, FixedStorage};
use crate::nodes::AudioNodeProcessor;
use std::sync::atomic::Ordering;
#[path = "nodes/filter_storage_biquad_filter_reference.rs"]
mod biquad_before;
#[path = "nodes/filter_storage_iir_filter_reference.rs"]
mod iir_before;
const LIMIT: usize = 16 << 20;

#[test]
fn biquad_factory_binds_parameter_range_to_context_rate() {
    let host = migo_io::scheduler::IoScheduler::new(1);
    let cpu = host.audio_cpu();
    let ledger = Arc::new(AudioAggregateLedger::new(LIMIT));
    let mut ctx = graph_with_rate(&cpu, &ledger, 2, 44100);
    assert!(ctx.create_biquad_filter(20));
    let mut expected = biquad_before::BiquadFilterNode::new(20, 2, 44100);
    expected
        .get_param_mut("frequency")
        .unwrap()
        .set_value(23000.0);
    ctx.with_node_typed::<BiquadFilterNode, _>(20, |n| {
        n.get_param_mut("frequency").unwrap().set_value(23000.0);
        let a = n.get_frequency_response(44100.0, &[1000.0, 22050.0, 23000.0]);
        let b = expected.get_frequency_response(44100.0, &[1000.0, 22050.0, 23000.0]);
        assert!(
            a.0.iter()
                .zip(&b.0)
                .chain(a.1.iter().zip(&b.1))
                .all(|(a, b)| a.to_bits() == b.to_bits()),
            "biquad factory used device rate instead of context rate"
        );
    });
    ctx.close();
    wait(|| ledger.used_bytes() == 0);
}

#[test]
fn biquad_rate_alone_invalidates_cached_coefficients() {
    let mut current = BiquadFilterNode::new(20, 2, 48000);
    let mut expected = biquad_before::BiquadFilterNode::new(20, 2, 48000);
    let input: Vec<_> = (0..256).map(|i| (i % 37) as f32 / 37.0 - 0.5).collect();
    let mut actual = vec![0.0; input.len()];
    let mut reference = actual.clone();
    for rate in [44100, 32000, 48000, 44100] {
        // Force only the old reference to calculate for the current rate,
        // preserving its histories and every parameter value.
        expected.set_type(biquad_before::BiquadFilterType::Lowpass);
        let a = current.get_frequency_response(rate as f64, &[350.0, 1000.0, 12000.0]);
        let b = expected.get_frequency_response(rate as f64, &[350.0, 1000.0, 12000.0]);
        assert!(
            a.0.iter()
                .zip(&b.0)
                .chain(a.1.iter().zip(&b.1))
                .all(|(a, b)| a.to_bits() == b.to_bits()),
            "biquad reused coefficients from another sample rate"
        );
        current.process(&input, &mut actual, rate, 2, 0.0);
        expected.process(&input, &mut reference, rate, 2, 0.0);
        same(&actual, &reference);
    }
}
fn graph(
    cpu: &migo_io::audio_cpu::AudioCpu,
    ledger: &Arc<AudioAggregateLedger>,
    channels: u32,
) -> AudioContext {
    graph_with_rate(cpu, ledger, channels, 48000)
}
fn graph_with_rate(
    cpu: &migo_io::audio_cpu::AudioCpu,
    ledger: &Arc<AudioAggregateLedger>,
    channels: u32,
    rate: u32,
) -> AudioContext {
    let budget = crate::limits::PcmBudget::with_aggregate(
        Arc::new(PcmUsage::new(LIMIT, 32)),
        Arc::new(PcmUsage::new(LIMIT, 32)),
        ledger.clone(),
    );
    let mut ctx = AudioContext::new_with_pcm_budget(1, rate, channels, budget);
    ctx.prepare_node_retirement(cpu.clone(), ledger.clone())
        .unwrap();
    ctx
}
fn same(a: &[f32], b: &[f32]) {
    assert!(
        a.len() == b.len() && a.iter().zip(b).all(|(a, b)| a.to_bits() == b.to_bits()),
        "filter retained storage changed complete PCM or response"
    );
}
fn iir_bytes(ff: usize, fb: usize, ch: usize) -> usize {
    8 * (ff + fb) + 8 * ch * (ff.max(1) + fb.max(1)) + 2 * ch * std::mem::size_of::<Vec<f64>>()
}
#[test]
fn factories_charge_nested_history_and_refuse_before_allocation() {
    let host = migo_io::scheduler::IoScheduler::new(1);
    let cpu = host.audio_cpu();
    let ledger = Arc::new(AudioAggregateLedger::new(LIMIT));
    let mut ctx = graph(&cpu, &ledger, 32);
    let base = ledger.used_bytes();
    let ff = vec![0.125; 1024];
    let fb = vec![1.0; 1024];
    let bytes = iir_bytes(ff.len(), fb.len(), 32);
    let pressure = ledger
        .try_reserve(LIMIT - base - bytes + 1, "peer")
        .unwrap();
    let before = migo_alloc_probe::thread_counts();
    assert!(
        !ctx.create_iir_filter(20, &ff, &fb),
        "IIR factory bypassed storage admission"
    );
    assert!(
        migo_alloc_probe::thread_counts().bytes_allocated - before.bytes_allocated < bytes as u64,
        "IIR factory allocated before admission"
    );
    drop(pressure);
    assert!(ctx.create_iir_filter(20, &ff, &fb));
    ctx.with_node_typed::<IIRFilterNode, _>(20, |n| assert_eq!(n.storage_bytes(), bytes));
    assert!(
        ledger.used_bytes() >= base + bytes + 8192,
        "nested IIR history or outer metadata omitted from ledger"
    );
    let before_biquad = ledger.used_bytes();
    assert!(ctx.create_biquad_filter(30));
    ctx.with_node_typed::<BiquadFilterNode, _>(30, |n| assert_eq!(n.storage_bytes(), 32 * 32));
    assert!(
        ledger.used_bytes() >= before_biquad + 32 * 32 + 8192,
        "biquad states omitted from ledger"
    );
    ctx.close();
    wait(|| ledger.used_bytes() == 0);
}
#[test]
fn actual_filter_storage_retires_with_credit_on_cpu_after_context_close() {
    let host = migo_io::scheduler::IoScheduler::new(1);
    let cpu = host.audio_cpu();
    let ledger = Arc::new(AudioAggregateLedger::new(LIMIT));
    let mut ctx = graph(&cpu, &ledger, 32);
    let ff = vec![0.125; 1024];
    let fb = vec![1.0; 1024];
    assert!(ctx.create_iir_filter(20, &ff, &fb));
    assert!(ctx.create_biquad_filter(30));
    let state = Arc::new(Dropped::default());
    ctx.with_node_typed::<IIRFilterNode, _>(20, |n| n.observe_storage(witness(&state)));
    ctx.with_node_typed::<BiquadFilterNode, _>(30, |n| n.observe_storage(witness(&state)));
    let bytes = iir_bytes(1024, 1024, 32) + 32 * 32;
    let gate = block(&cpu);
    let before = migo_alloc_probe::thread_counts();
    ctx.close();
    assert_eq!(
        state.count.load(Ordering::Acquire),
        0,
        "filter data escaped CPU retirement"
    );
    assert!(
        migo_alloc_probe::thread_counts().bytes_freed - before.bytes_freed < bytes as u64,
        "filter arrays freed on caller"
    );
    assert!(
        ledger.used_bytes() >= bytes + 8192,
        "filter credit returned before actual CPU drop"
    );
    gate.open();
    wait(|| state.count.load(Ordering::Acquire) == 2 && ledger.used_bytes() == 0);
    assert!(
        !state.wrong.load(Ordering::Acquire),
        "filter arrays destroyed outside CPU workers"
    );
}
#[test]
fn iir_pcm_and_response_match_frozen_before_for_all_history_shapes() {
    for channels in [1, 2, 32] {
        for (ff, fb) in [
            (vec![0.25, 0.5, 0.25], vec![1.0, -0.4, 0.1]),
            (vec![0.6, 0.25, -0.1], vec![2.0, -0.3, 0.15]),
            (vec![0.4, -0.1], vec![-2.0, 0.3]),
            (vec![], vec![1.0]),
            (vec![1.0], vec![]),
            (vec![], vec![]),
            (vec![0.05; 20], vec![1.0]),
        ] {
            let host = migo_io::scheduler::IoScheduler::new(1);
            let cpu = host.audio_cpu();
            let ledger = Arc::new(AudioAggregateLedger::new(LIMIT));
            let mut ctx = graph(&cpu, &ledger, channels);
            assert!(ctx.create_iir_filter(20, &ff, &fb));
            let mut reference =
                iir_before::IIRFilterNode::new(20, ff.clone(), fb.clone(), channels);
            let input: Vec<_> = (0..128 * channels)
                .map(|i| (i % 37) as f32 / 37.0 - 0.5)
                .collect();
            let mut actual = vec![0.0; input.len()];
            let mut expected = actual.clone();
            ctx.with_node_typed::<IIRFilterNode, _>(20, |n| {
                assert_eq!(
                    n.storage_bytes(),
                    iir_bytes(ff.len(), fb.len(), channels as usize)
                );
                for block in 0..5 {
                    let data = if block < 3 { input.as_slice() } else { &[] };
                    let before = migo_alloc_probe::thread_counts();
                    n.process(data, &mut actual, 48000, channels, 0.0);
                    let after = migo_alloc_probe::thread_counts();
                    assert_eq!(
                        (
                            after.allocations - before.allocations,
                            after.reallocations - before.reallocations
                        ),
                        (0, 0),
                        "IIR first/steady render allocated"
                    );
                    reference.process(data, &mut expected, 48000, channels, 0.0);
                    same(&actual, &expected);
                    assert_eq!(n.has_tail_audio(), reference.has_tail_audio());
                }
                let freq = [-0.0, 0.0, 1000.0, 12000.0, 24000.0];
                let a = n.get_frequency_response(48000.0, &freq);
                let b = reference.get_frequency_response(48000.0, &freq);
                same(&a.0, &b.0);
                same(&a.1, &b.1);
            });
            ctx.close();
            wait(|| ledger.used_bytes() == 0);
        }
    }
}
#[test]
fn biquad_pcm_and_response_match_frozen_before_for_every_filter_type() {
    for rate in [44100, 48000] {
        for channels in [1, 2, 32] {
            for kind in [
                "lowpass",
                "highpass",
                "bandpass",
                "lowshelf",
                "highshelf",
                "peaking",
                "notch",
                "allpass",
            ] {
                let host = migo_io::scheduler::IoScheduler::new(1);
                let cpu = host.audio_cpu();
                let ledger = Arc::new(AudioAggregateLedger::new(LIMIT));
                let mut ctx = graph_with_rate(&cpu, &ledger, channels, rate);
                assert!(ctx.create_biquad_filter(20));
                let mut reference = biquad_before::BiquadFilterNode::new(20, channels, rate);
                reference.set_type(biquad_before::BiquadFilterType::from_str(kind));
                for (key, value) in [
                    ("frequency", 1333.0),
                    ("detune", 300.0),
                    ("Q", 0.707),
                    ("gain", 6.0),
                ] {
                    reference.get_param_mut(key).unwrap().set_value(value);
                }
                let input: Vec<_> = (0..128 * channels)
                    .map(|i| (i % 37) as f32 / 37.0 - 0.5)
                    .collect();
                let mut actual = vec![0.0; input.len()];
                let mut expected = actual.clone();
                ctx.with_node_typed::<BiquadFilterNode, _>(20, |n| {
                    n.set_type(crate::nodes::BiquadFilterType::from_str(kind));
                    for (key, value) in [
                        ("frequency", 1333.0),
                        ("detune", 300.0),
                        ("Q", 0.707),
                        ("gain", 6.0),
                    ] {
                        n.get_param_mut(key).unwrap().set_value(value);
                    }
                    for block in 0..5 {
                        let data = if block < 3 { input.as_slice() } else { &[] };
                        let before = migo_alloc_probe::thread_counts();
                        n.process(data, &mut actual, rate, channels, 0.0);
                        let after = migo_alloc_probe::thread_counts();
                        assert_eq!(
                            (
                                after.allocations - before.allocations,
                                after.reallocations - before.reallocations
                            ),
                            (0, 0),
                            "biquad first/steady render allocated"
                        );
                        reference.process(data, &mut expected, rate, channels, 0.0);
                        same(&actual, &expected);
                        assert_eq!(n.has_tail_audio(), reference.has_tail_audio());
                    }
                    let freq = [-0.0, 0.0, 1000.0, 12000.0, 24000.0];
                    let a = n.get_frequency_response(rate as f64, &freq);
                    let b = reference.get_frequency_response(rate as f64, &freq);
                    same(&a.0, &b.0);
                    same(&a.1, &b.1);
                });
                ctx.close();
                wait(|| ledger.used_bytes() == 0);
            }
        }
    }
}
#[test]
fn fixed_storage_adoption_keeps_pointer_and_refuses_foreign_ledger() {
    let host = migo_io::scheduler::IoScheduler::new(1);
    let cpu = host.audio_cpu();
    let a = Arc::new(AudioAggregateLedger::new(LIMIT));
    let b = Arc::new(AudioAggregateLedger::new(LIMIT));
    let owner = crate::node_retirement::Host::new(cpu.clone(), a.clone(), 2).unwrap();
    let foreign = crate::node_retirement::Host::new(cpu.clone(), b.clone(), 2).unwrap();
    let data = vec![7_u8; 65536];
    let pointer = data.as_ptr();
    let mut storage = FixedStorage::new(None, data.capacity_bytes(), || data).unwrap();
    let base = a.used_bytes();
    storage.prepare(&owner).unwrap();
    assert_eq!(
        storage.as_ptr(),
        pointer,
        "fixed storage adoption copied data"
    );
    assert!(
        a.used_bytes() >= base + 65536 + 4096,
        "fixed storage adoption omitted actual capacity"
    );
    assert!(
        storage.prepare(&foreign).is_err(),
        "fixed storage reused foreign physical ledger"
    );
    let state = Arc::new(Dropped::default());
    storage.observe(witness(&state));
    let gate = block(&cpu);
    drop(storage);
    assert_eq!(state.count.load(Ordering::Acquire), 0);
    assert!(a.used_bytes() >= base + 65536);
    gate.open();
    wait(|| state.count.load(Ordering::Acquire) == 1 && a.used_bytes() == base);
    drop((owner, foreign));
    wait(|| a.used_bytes() == 0 && b.used_bytes() == 0);
}
#[test]
fn generic_node_and_closed_host_refusals_preserve_owned_filter_data() {
    let host = migo_io::scheduler::IoScheduler::new(1);
    let cpu = host.audio_cpu();
    let ledger = Arc::new(AudioAggregateLedger::new(LIMIT));
    let mut ctx = graph(&cpu, &ledger, 32);
    let base = ledger.used_bytes();
    let ff = vec![0.125; 1024];
    let fb = vec![1.0; 1024];
    let mut node =
        IIRFilterNode::new_with_retirement(20, &ff, &fb, 32, ctx.response_retirement_host())
            .unwrap();
    let state = Arc::new(Dropped::default());
    node.observe_storage(witness(&state));
    let cost = ledger.used_bytes() - base;
    let pressure = ledger
        .try_reserve(LIMIT - ledger.used_bytes(), "peer")
        .unwrap();
    let gate = block(&cpu);
    assert!(!ctx.add_node(Box::new(node)));
    drop(pressure);
    assert_eq!(
        ledger.used_bytes(),
        base + cost,
        "refused node returned live filter credit"
    );
    assert_eq!(state.count.load(Ordering::Acquire), 0);
    gate.open();
    wait(|| state.count.load(Ordering::Acquire) == 1 && ledger.used_bytes() == base);
    host.close();
    let before = migo_alloc_probe::thread_counts();
    assert!(!ctx.create_iir_filter(30, &ff, &fb));
    assert!(!ctx.create_biquad_filter(40));
    assert!(migo_alloc_probe::thread_counts().bytes_allocated - before.bytes_allocated < 65536);
    ctx.close();
    wait(|| ledger.used_bytes() == 0);
}
