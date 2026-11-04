//! Active curves and FIR histories retain physical credit through CPU drop.
use super::context_retirement_tests::{Dropped, block, context, wait, witness};
use super::*;
use crate::limits::{AudioAggregateLedger, PcmUsage};
use crate::nodes::AudioNodeProcessor;
use std::sync::atomic::Ordering;

use crate::nodes::wave_shaper_before as before;
const LIMIT: usize = 32 << 20;
const BULK: usize = 4 << 20;

fn graph(
    cpu: &migo_io::audio_cpu::AudioCpu,
    ledger: &Arc<AudioAggregateLedger>,
    channels: u32,
) -> AudioContext {
    context(
        cpu,
        ledger,
        &Arc::new(PcmUsage::new(LIMIT, 32)),
        &Arc::new(PcmUsage::new(LIMIT, 32)),
        channels,
    )
}
fn curve(capacity: usize, value: f32) -> Vec<f32> {
    let mut data = Vec::with_capacity(capacity);
    data.resize(capacity.min(2), value);
    data
}
fn same(actual: &[f32], expected: &[f32]) {
    assert!(
        actual.len() == expected.len()
            && actual
                .iter()
                .zip(expected)
                .all(|(a, b)| a.to_bits() == b.to_bits()),
        "owned waveshaper changed complete PCM or history"
    );
}
fn render(
    node: &mut WaveShaperNode,
    old: &mut before::WaveShaperNode,
    input: &[f32],
    output: &mut [f32],
    rate: u32,
    channels: u32,
) {
    let mut expected = output.to_vec();
    let start = migo_alloc_probe::thread_counts();
    let frames = node.process(input, output, rate, channels, 0.0);
    let end = migo_alloc_probe::thread_counts();
    assert_eq!(
        (
            end.allocations - start.allocations,
            end.reallocations - start.reallocations
        ),
        (0, 0),
        "owned waveshaper first/steady render allocated"
    );
    assert_eq!(
        frames,
        old.process(input, &mut expected, rate, channels, 0.0)
    );
    same(output, &expected);
    assert_eq!(node.has_tail_audio(), old.has_tail_audio());
    assert_eq!(node.is_producing(), old.is_producing());
}

#[test]
fn history_admission_precedes_allocation_and_mode_change() {
    let host = migo_io::scheduler::IoScheduler::new(1);
    let cpu = host.audio_cpu();
    let ledger = Arc::new(AudioAggregateLedger::new(LIMIT));
    let mut ctx = graph(&cpu, &ledger, 32);
    assert!(ctx.add_node(Box::new(WaveShaperNode::new(10))));
    let original = ctx
        .with_node_typed::<WaveShaperNode, _>(10, |n| n.history_identity())
        .unwrap();
    let credit = ledger.used_bytes();
    let filler = ledger.try_reserve(LIMIT - credit, "history peer").unwrap();
    let start = migo_alloc_probe::thread_counts();
    assert!(
        !ctx.set_wave_shaper_oversample(10, OversampleType::TwoX),
        "FIR history admission bypassed physical ledger"
    );
    let allocated = migo_alloc_probe::thread_counts().bytes_allocated - start.bytes_allocated;
    let bytes = 2 * 32 * (32 * 4 + std::mem::size_of::<usize>());
    assert!(
        allocated < bytes as u64,
        "FIR history allocated before admission"
    );
    assert_eq!(
        ctx.with_node_typed::<WaveShaperNode, _>(10, |n| n.history_identity())
            .unwrap(),
        original,
        "FIR history refusal changed mode or storage"
    );
    assert_eq!(ledger.used_bytes(), LIMIT);
    drop(filler);
    assert!(ctx.set_wave_shaper_oversample(10, OversampleType::TwoX));
    assert!(
        ledger.used_bytes() >= credit + bytes + 4096,
        "FIR history capacity omitted from ledger"
    );
    let ready = ctx
        .with_node_typed::<WaveShaperNode, _>(10, |n| n.history_identity())
        .unwrap();
    assert_eq!(ready.3, bytes);
    let filler = ledger
        .try_reserve(LIMIT - ledger.used_bytes(), "history reuse peer")
        .unwrap();
    for mode in [
        OversampleType::TwoX,
        OversampleType::FourX,
        OversampleType::None,
        OversampleType::TwoX,
    ] {
        assert!(
            ctx.set_wave_shaper_oversample(10, mode),
            "prepared FIR history acquired duplicate admission"
        );
        let state = ctx
            .with_node_typed::<WaveShaperNode, _>(10, |n| n.history_identity())
            .unwrap();
        assert_eq!(
            (state.1, state.2, state.3),
            (ready.1, ready.2, ready.3),
            "FIR history reuse changed capacity owner"
        );
        assert_eq!(ledger.used_bytes(), LIMIT);
    }
    drop(filler);
    assert!(ctx.add_node(Box::new(WaveShaperNode::new(20))));
    let untouched = ctx
        .with_node_typed::<WaveShaperNode, _>(20, |n| n.history_identity())
        .unwrap();
    host.close();
    assert!(
        !ctx.set_wave_shaper_oversample(20, OversampleType::TwoX),
        "closed host admitted fresh FIR history"
    );
    assert_eq!(
        ctx.with_node_typed::<WaveShaperNode, _>(20, |n| n.history_identity())
            .unwrap(),
        untouched,
        "closed host changed rejected FIR history"
    );
    ctx.close();
    wait(|| ledger.used_bytes() == 0);
}

#[test]
fn adoption_counts_reserved_capacity_without_copying() {
    let host = migo_io::scheduler::IoScheduler::new(1);
    let cpu = host.audio_cpu();
    let ledger = Arc::new(AudioAggregateLedger::new(LIMIT));
    let mut ctx = graph(&cpu, &ledger, 2);
    let mut node = WaveShaperNode::new(10);
    node.set_oversample(OversampleType::TwoX);
    node.prepare_oversampling(64);
    let data = curve(BULK / 4, 0.25);
    let ptr = data.as_ptr() as usize;
    node.set_curve(Some(data));
    let identity = node.history_identity();
    let base = ledger.used_bytes();
    assert!(ctx.add_node(Box::new(node)));
    let bytes = 2 * 64 * (32 * 4 + std::mem::size_of::<usize>());
    ctx.with_node_typed::<WaveShaperNode, _>(10, |n| {
        assert_eq!(
            n.history_identity(),
            identity,
            "history adoption copied or changed reserved data"
        );
        assert_eq!(
            n.curve_identity(),
            Some((ptr, BULK)),
            "curve adoption copied original allocation"
        );
    });
    assert!(
        ledger.used_bytes() >= base + bytes + BULK + 3 * 4096,
        "waveshaper adoption omitted actual capacity"
    );
    ctx.close();
    wait(|| ledger.used_bytes() == 0);
}

#[test]
fn existing_context_adopts_preexisting_waveshaper_storage() {
    let host = migo_io::scheduler::IoScheduler::new(1);
    let cpu = host.audio_cpu();
    let ledger = Arc::new(AudioAggregateLedger::new(LIMIT));
    let mut ctx = AudioContext::new(1, 48000, 32);
    let mut node = WaveShaperNode::new(10);
    node.set_oversample(OversampleType::FourX);
    node.prepare_oversampling(64);
    node.set_curve(Some(curve(BULK / 4, 0.25)));
    let identity = node.history_identity();
    let original_curve = node.curve_identity();
    assert!(ctx.add_node(Box::new(node)));
    ctx.prepare_node_retirement(cpu, ledger.clone()).unwrap();
    ctx.with_node_typed::<WaveShaperNode, _>(10, |n| {
        assert_eq!(n.history_identity(), identity);
        assert_eq!(n.curve_identity(), original_curve);
    });
    assert!(
        ledger.used_bytes() >= BULK + identity.3 + 3 * 4096,
        "existing context omitted waveshaper capacity"
    );
    ctx.close();
    wait(|| ledger.used_bytes() == 0);
}

#[test]
fn history_growth_refusal_preserves_original_pcm_and_owner() {
    let host = migo_io::scheduler::IoScheduler::new(1);
    let cpu = host.audio_cpu();
    let ledger = Arc::new(AudioAggregateLedger::new(LIMIT));
    let owner = crate::node_retirement::Host::new(cpu, ledger.clone(), 2).unwrap();
    let mut node = WaveShaperNode::new(10);
    let mut old = before::WaveShaperNode::new(10);
    node.set_curve(Some(vec![-0.5, 0.25, 1.0]));
    old.set_curve(Some(vec![-0.5, 0.25, 1.0]));
    node.prepare_storage(&owner, 2).unwrap();
    node.try_set_oversample(OversampleType::TwoX, 2).unwrap();
    old.set_oversample(before::OversampleType::TwoX);
    let input: Vec<_> = (0..514).map(|i| (i % 37) as f32 / 37.0 - 0.5).collect();
    let mut output = vec![0.375; input.len()];
    render(&mut node, &mut old, &input, &mut output, 48000, 2);
    let identity = node.history_identity();
    let filler = ledger
        .try_reserve(LIMIT - ledger.used_bytes(), "growth peer")
        .unwrap();
    assert!(
        node.try_set_oversample(OversampleType::FourX, 32).is_err(),
        "FIR history growth bypassed admission"
    );
    assert_eq!(
        node.history_identity(),
        identity,
        "FIR history refusal changed mode or storage"
    );
    render(&mut node, &mut old, &input, &mut output, 48000, 2);
    drop(filler);
    let state = Arc::new(Dropped::default());
    node.observe_history(witness(&state));
    let gate = block(&host.audio_cpu());
    node.prepare_storage(&owner, 32).unwrap();
    assert_eq!(
        state.count.load(Ordering::Acquire),
        0,
        "old FIR history escaped CPU retirement"
    );
    let input: Vec<_> = (0..129 * 32)
        .map(|i| (i % 31) as f32 / 31.0 - 0.5)
        .collect();
    let mut output = vec![0.375; input.len()];
    render(&mut node, &mut old, &input, &mut output, 48000, 32);
    gate.open();
    wait(|| state.count.load(Ordering::Acquire) == 1);
    assert!(
        !state.wrong.load(Ordering::Acquire),
        "old FIR history escaped CPU retirement"
    );
    drop(node);
    drop(owner);
    wait(|| ledger.used_bytes() == 0);
}

#[test]
fn curve_admission_preserves_input_and_clear_needs_no_new_credit() {
    let host = migo_io::scheduler::IoScheduler::new(1);
    let cpu = host.audio_cpu();
    let ledger = Arc::new(AudioAggregateLedger::new(LIMIT));
    let mut ctx = graph(&cpu, &ledger, 1);
    assert!(ctx.add_node(Box::new(WaveShaperNode::new(10))));
    let base = ledger.used_bytes();
    let data = curve(BULK / 4, 0.25);
    let pointer = data.as_ptr() as usize;
    assert!(ctx.set_wave_shaper_curve(10, Some(data)));
    assert!(
        ledger.used_bytes() >= base + BULK + 4096,
        "live curve capacity omitted from ledger"
    );
    let filler = ledger
        .try_reserve(LIMIT - ledger.used_bytes(), "curve peer")
        .unwrap();
    let mut incoming = Some(curve(BULK / 4, 0.75));
    let incoming_pointer = incoming.as_ref().unwrap().as_ptr() as usize;
    assert!(
        !ctx.try_set_wave_shaper_curve(10, &mut incoming),
        "curve admission bypassed physical ledger"
    );
    assert_eq!(
        incoming.as_ref().map(|v| v.as_ptr() as usize),
        Some(incoming_pointer),
        "curve refusal took original input"
    );
    assert_eq!(
        ctx.with_node_typed::<WaveShaperNode, _>(10, |n| n.curve_identity())
            .unwrap(),
        Some((pointer, BULK)),
        "curve refusal changed old storage"
    );
    let state = Arc::new(Dropped::default());
    ctx.with_node_typed::<WaveShaperNode, _>(10, |n| n.observe_curve(witness(&state)));
    let gate = block(&cpu);
    let start = migo_alloc_probe::thread_counts();
    assert!(
        ctx.set_wave_shaper_curve(10, None),
        "curve clear acquired duplicate admission"
    );
    assert!(
        migo_alloc_probe::thread_counts().bytes_freed - start.bytes_freed < BULK as u64,
        "curve data freed on caller"
    );
    assert_eq!(
        ledger.used_bytes(),
        LIMIT,
        "curve credit returned before actual CPU drop"
    );
    assert_eq!(
        state.count.load(Ordering::Acquire),
        0,
        "curve data escaped CPU retirement"
    );
    gate.open();
    wait(|| state.count.load(Ordering::Acquire) == 1);
    assert!(!state.wrong.load(Ordering::Acquire));
    drop(filler);
    drop(incoming);
    ctx.close();
    wait(|| ledger.used_bytes() == 0);
}

#[test]
fn curve_capacity_threshold_keeps_small_values_inline() {
    for bytes in [0, 4, 4092, 4096, BULK] {
        let host = migo_io::scheduler::IoScheduler::new(1);
        let cpu = host.audio_cpu();
        let ledger = Arc::new(AudioAggregateLedger::new(LIMIT));
        let mut ctx = graph(&cpu, &ledger, 1);
        assert!(ctx.add_node(Box::new(WaveShaperNode::new(10))));
        let base = ledger.used_bytes();
        let data = curve(bytes / 4, 0.25);
        let pointer = data.as_ptr() as usize;
        let gate = block(&cpu);
        assert!(ctx.set_wave_shaper_curve(10, Some(data)));
        assert_eq!(
            ctx.with_node_typed::<WaveShaperNode, _>(10, |n| n.curve_identity())
                .unwrap(),
            Some((pointer, bytes)),
            "curve owner copied input or used length"
        );
        let held = ledger.used_bytes();
        if bytes < 4096 {
            assert_eq!(
                held,
                base + bytes,
                "small curve acquired artificial handoff credit"
            );
        } else {
            assert!(
                held >= base + bytes + 4096,
                "live curve capacity omitted from ledger"
            );
        }
        let state = Arc::new(Dropped::default());
        ctx.with_node_typed::<WaveShaperNode, _>(10, |n| n.observe_curve(witness(&state)));
        assert!(ctx.set_wave_shaper_curve(10, None));
        assert_eq!(
            ledger.used_bytes(),
            if bytes < 4096 { base } else { held },
            "curve credit returned before actual CPU drop"
        );
        assert_eq!(state.count.load(Ordering::Acquire), 0);
        gate.open();
        if bytes >= 4096 {
            wait(|| state.count.load(Ordering::Acquire) == 1);
            assert!(!state.wrong.load(Ordering::Acquire));
        }
        ctx.close();
        wait(|| ledger.used_bytes() == 0);
    }
}

#[test]
fn refused_nodes_and_closed_hosts_keep_original_cpu_owners() {
    for mode in ["foreign", "full", "closed"] {
        let host = migo_io::scheduler::IoScheduler::new(1);
        let cpu = host.audio_cpu();
        let ledger = Arc::new(AudioAggregateLedger::new(LIMIT));
        let other = Arc::new(AudioAggregateLedger::new(LIMIT));
        let mut ctx = graph(&cpu, &ledger, 32);
        let mut peer = graph(&cpu, &other, 32);
        let mut node = WaveShaperNode::new(10);
        node.set_oversample(OversampleType::FourX);
        node.prepare_oversampling(32);
        node.set_curve(Some(curve(BULK / 4, 0.25)));
        node.prepare_storage(ctx.response_retirement_host().unwrap(), 32)
            .unwrap();
        let state = Arc::new(Dropped::default());
        node.observe_history(witness(&state));
        node.observe_curve(witness(&state));
        let filler = (mode == "full").then(|| {
            ledger
                .try_reserve(LIMIT - ledger.used_bytes(), "node peer")
                .unwrap()
        });
        let gate = block(&cpu);
        if mode == "closed" {
            host.close();
        }
        let credit = ledger.used_bytes();
        let start = migo_alloc_probe::thread_counts();
        let accepted = if mode == "foreign" {
            peer.add_node(Box::new(node))
        } else {
            ctx.add_node(Box::new(node))
        };
        assert!(
            !accepted,
            "waveshaper accepted foreign or refused node admission"
        );
        assert!(
            migo_alloc_probe::thread_counts().bytes_freed - start.bytes_freed < BULK as u64,
            "refused waveshaper data freed on caller"
        );
        assert_eq!(
            ledger.used_bytes(),
            credit,
            "refused node returned live waveshaper credit"
        );
        assert_eq!(
            state.count.load(Ordering::Acquire),
            0,
            "refused waveshaper escaped CPU retirement"
        );
        gate.open();
        wait(|| state.count.load(Ordering::Acquire) == 2);
        assert!(
            !state.wrong.load(Ordering::Acquire),
            "refused waveshaper escaped CPU retirement"
        );
        drop(filler);
        ctx.close();
        peer.close();
        wait(|| ledger.used_bytes() == 0 && other.used_bytes() == 0);
    }
}

#[test]
fn context_and_host_close_hold_live_curve_and_history_until_cpu_drop() {
    let host = migo_io::scheduler::IoScheduler::new(1);
    let cpu = host.audio_cpu();
    let ledger = Arc::new(AudioAggregateLedger::new(LIMIT));
    let mut ctx = graph(&cpu, &ledger, 32);
    assert!(ctx.add_node(Box::new(WaveShaperNode::new(10))));
    assert!(ctx.set_wave_shaper_curve(10, Some(curve(BULK / 4, 0.25))));
    assert!(ctx.set_wave_shaper_oversample(10, OversampleType::FourX));
    let state = Arc::new(Dropped::default());
    ctx.with_node_typed::<WaveShaperNode, _>(10, |n| {
        n.observe_history(witness(&state));
        n.observe_curve(witness(&state));
    });
    let gate = block(&cpu);
    host.close();
    let start = migo_alloc_probe::thread_counts();
    ctx.close();
    assert!(
        migo_alloc_probe::thread_counts().bytes_freed - start.bytes_freed < BULK as u64,
        "closed waveshaper data freed on caller"
    );
    assert!(
        ledger.used_bytes() >= BULK + 2 * 32 * (32 * 4 + std::mem::size_of::<usize>()),
        "closed waveshaper returned live physical credit"
    );
    assert_eq!(
        state.count.load(Ordering::Acquire),
        0,
        "closed waveshaper escaped CPU retirement"
    );
    gate.open();
    wait(|| state.count.load(Ordering::Acquire) == 2 && ledger.used_bytes() == 0);
    assert!(
        !state.wrong.load(Ordering::Acquire),
        "closed waveshaper escaped CPU retirement"
    );
}

#[test]
fn managed_modes_curves_tails_and_long_blocks_match_frozen_kernel() {
    for rate in [44100, 48000] {
        for channels in [1, 2, 32] {
            for data in [
                None,
                Some(vec![]),
                Some(vec![0.25]),
                Some(vec![-0.5, 0.25, 1.0]),
            ] {
                let host = migo_io::scheduler::IoScheduler::new(1);
                let cpu = host.audio_cpu();
                let ledger = Arc::new(AudioAggregateLedger::new(LIMIT));
                let mut ctx = graph(&cpu, &ledger, channels);
                assert!(ctx.add_node(Box::new(WaveShaperNode::new(10))));
                assert!(ctx.set_wave_shaper_curve(10, data.clone()));
                ctx.connect(10, crate::context::DESTINATION_NODE_ID);
                let mut old = before::WaveShaperNode::new(10);
                old.set_curve(data);
                old.set_output_connected(true);
                for (mode, frames) in [
                    ("2x", 1),
                    ("2x", 129),
                    ("4x", 257),
                    ("none", 129),
                    ("2x", 4097),
                    ("4x", 257),
                ] {
                    assert!(ctx.set_wave_shaper_oversample(10, OversampleType::from_str(mode)));
                    old.set_oversample(before::OversampleType::from_str(mode));
                    let input: Vec<_> = (0..frames * channels + channels - 1)
                        .map(|i| (i % 37) as f32 / 37.0 * 3.0 - 1.5)
                        .collect();
                    let mut output = vec![0.375; input.len()];
                    ctx.with_node_typed::<WaveShaperNode, _>(10, |n| {
                        if mode != "none" {
                            let factor = if mode == "2x" { 2.0 } else { 4.0 };
                            let (up, down) = n.coefficients();
                            same(up, &before::coefficients(0.5 / factor, factor));
                            same(down, &before::coefficients(0.5 / factor, 1.0));
                        }
                        render(n, &mut old, &input, &mut output, rate, channels)
                    });
                }
                let mut output = vec![0.375; 257 * channels as usize];
                ctx.with_node_typed::<WaveShaperNode, _>(10, |n| {
                    render(n, &mut old, &[], &mut output, rate, channels)
                });
                ctx.close();
                wait(|| ledger.used_bytes() == 0);
            }
        }
    }
}
