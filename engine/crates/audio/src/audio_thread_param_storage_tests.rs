//! Admission, physical destruction, and unchanged complete automation output.
use super::context_retirement_tests::{Dropped, block, context, wait, witness};
use super::*;
use crate::limits::{AudioAggregateLedger, PcmUsage};
use crate::nodes::{AudioNodeProcessor, BufferSourceNode, GainNode};
use crate::param::AudioParamTimeline;
use std::sync::atomic::Ordering;
#[path = "param_storage_reference.rs"]
mod before;

const LIMIT: usize = 32 << 20;
// Measure the independently frozen enum, including Rust layout padding.
const EVENT_BYTES: usize = before::EVENT_BYTES;
fn graph(cpu: &migo_io::audio_cpu::AudioCpu, ledger: &Arc<AudioAggregateLedger>) -> AudioContext {
    context(
        cpu,
        ledger,
        &Arc::new(PcmUsage::new(LIMIT, 32)),
        &Arc::new(PcmUsage::new(LIMIT, 32)),
        2,
    )
}
fn timeline(capacity: usize, count: usize) -> AudioParamTimeline {
    let mut param = AudioParamTimeline::new(0.5, -10.0, 10.0);
    param.reserve_native_events(capacity);
    for i in 0..count {
        param.set_value_at_time(i as f32 * 0.001, i as f64 + 1.0);
    }
    param
}
fn unchanged(param: &AudioParamTimeline, identity: (usize, usize, usize), debug: &str) {
    assert_eq!(
        param.event_identity(),
        identity,
        "event refusal changed capacity or pointer"
    );
    assert_eq!(
        format!("{param:?}"),
        debug,
        "event refusal changed automation or static value"
    );
}

#[test]
fn refused_growth_is_atomic_for_every_scheduling_operation() {
    let host = migo_io::scheduler::IoScheduler::new(1);
    let cpu = host.audio_cpu();
    let ledger = Arc::new(AudioAggregateLedger::new(LIMIT));
    let mut ctx = graph(&cpu, &ledger);
    let owner = ctx.response_retirement_host().unwrap().clone();
    let mut param = timeline(256, 256);
    param.prepare_storage(&owner).unwrap();
    let identity = param.event_identity();
    let state = format!("{param:?}");
    let filler = ledger
        .try_reserve(LIMIT - ledger.used_bytes(), "event peer")
        .unwrap();
    for operation in 0..5 {
        let start = migo_alloc_probe::thread_counts();
        let result = match operation {
            0 => param.try_set_value_at_time(0.75, 300.0),
            1 => param.try_linear_ramp_to_value_at_time(0.75, 300.0),
            2 => param.try_exponential_ramp_to_value_at_time(0.75, 300.0),
            3 => param.try_set_target_at_time(0.75, 300.0, 0.1),
            _ => {
                // All events are future; replacing the first with a ramp does
                // not need extra capacity until the immediate anchor is added.
                param.cancel_scheduled_values(256.0);
                assert!(param.try_linear_ramp_to_value_at_time(0.75, 300.0).is_ok());
                param.try_set_value_now(0.125, 0.0)
            }
        };
        assert!(result.is_err(), "event growth bypassed physical admission");
        assert!(
            migo_alloc_probe::thread_counts().bytes_allocated - start.bytes_allocated
                < (identity.1 * EVENT_BYTES) as u64,
            "event storage allocated before admission"
        );
        if operation != 4 {
            unchanged(&param, identity, &state);
        }
        assert_eq!(
            ledger.used_bytes(),
            LIMIT,
            "refused event returned peer credit"
        );
    }
    // Refusal with past events must also preserve those past anchors.
    drop(filler);
    let mut now = timeline(4, 3);
    now.linear_ramp_to_value_at_time(0.875, 10.0);
    now.prepare_storage(&owner).unwrap();
    let before = now.event_identity();
    let debug = format!("{now:?}");
    let filler = ledger
        .try_reserve(LIMIT - ledger.used_bytes(), "anchor peer")
        .unwrap();
    // No discarded anchor can make room: all four events are still in future.
    assert!(
        now.try_set_value_now(0.125, 0.5).is_err(),
        "immediate anchor bypassed physical admission"
    );
    unchanged(&now, before, &debug);
    drop(filler);
    drop(now);
    drop(param);
    drop(owner);
    ctx.close();
    wait(|| ledger.used_bytes() == 0);
}

#[test]
fn event_capacity_growth_keeps_prefix_and_old_cpu_credit() {
    let host = migo_io::scheduler::IoScheduler::new(1);
    let cpu = host.audio_cpu();
    let ledger = Arc::new(AudioAggregateLedger::new(LIMIT));
    let mut ctx = graph(&cpu, &ledger);
    let owner = ctx.response_retirement_host().unwrap().clone();
    let mut param = timeline(256, 256);
    param.prepare_storage(&owner).unwrap();
    let original = param.clone();
    let old_bytes = param.event_identity().1 * EVENT_BYTES;
    let credit = ledger.used_bytes();
    let state = Arc::new(Dropped::default());
    param.observe_events(witness(&state));
    let gate = block(&cpu);
    assert!(param.try_set_value_at_time(0.75, 1000.0).is_ok());
    let next_bytes = param.event_identity().1 * EVENT_BYTES;
    assert!(next_bytes >= old_bytes * 2);
    assert!(
        ledger.used_bytes() >= credit + next_bytes + 4096,
        "event growth returned old physical credit early"
    );
    for t in [0.0, 1.0, 63.5, 128.0, 255.0, 500.0] {
        assert_eq!(
            param.compute_value(t).to_bits(),
            original.compute_value(t).to_bits(),
            "event growth lost scheduled prefix"
        );
    }
    assert_eq!(
        state.count.load(Ordering::Acquire),
        0,
        "event storage destroyed while original CPU blocked"
    );
    gate.open();
    wait(|| state.count.load(Ordering::Acquire) == 1);
    wait(|| ledger.used_bytes() < credit + next_bytes + 4096);
    assert!(
        !state.wrong.load(Ordering::Acquire),
        "event storage destroyed off original CPU"
    );
    drop(param);
    drop(owner);
    ctx.close();
    wait(|| ledger.used_bytes() == 0);
}

#[test]
fn cancellation_gc_and_closed_host_reuse_keep_capacity_charge() {
    let host = migo_io::scheduler::IoScheduler::new(1);
    let cpu = host.audio_cpu();
    let ledger = Arc::new(AudioAggregateLedger::new(LIMIT));
    let mut ctx = graph(&cpu, &ledger);
    let owner = ctx.response_retirement_host().unwrap().clone();
    let mut param = timeline(256, 200);
    param.prepare_storage(&owner).unwrap();
    let credit = ledger.used_bytes();
    let identity = param.event_identity();
    param.gc_events(150.5);
    assert!(param.event_identity().0 < identity.0);
    param.cancel_scheduled_values(0.0);
    assert_eq!(param.event_identity().0, 0);
    assert_eq!(
        param.event_identity().1,
        identity.1,
        "event GC shrank physical capacity"
    );
    assert_eq!(
        param.event_identity().2,
        identity.2,
        "event GC replaced physical allocation"
    );
    assert_eq!(
        ledger.used_bytes(),
        credit,
        "event cancellation returned live capacity credit"
    );
    host.close();
    for i in 0..identity.1 {
        assert!(
            param
                .try_linear_ramp_to_value_at_time(0.875, i as f64 + 1.0)
                .is_ok(),
            "event capacity reuse required fresh admission"
        );
    }
    let full = format!("{param:?}");
    assert!(
        param.try_set_value_at_time(0.1, 1000.0).is_err(),
        "closed host admitted fresh event capacity"
    );
    let full_identity = (identity.1, identity.1, identity.2);
    unchanged(&param, full_identity, &full);
    // Existing past anchors supply room without growth, even when closed.
    assert!(
        param.try_set_value_now(0.25, 128.5).is_ok(),
        "immediate anchor did not reuse discarded capacity"
    );
    assert_eq!(ledger.used_bytes(), credit);
    drop(param);
    drop(owner);
    ctx.close();
    wait(|| ledger.used_bytes() == 0);
}

#[test]
fn all_twenty_two_parameters_adopt_existing_capacity() {
    let host = migo_io::scheduler::IoScheduler::new(1);
    let cpu = host.audio_cpu();
    let ledger = Arc::new(AudioAggregateLedger::new(LIMIT));
    let mut ctx = graph(&cpu, &ledger);
    let owner = ctx.response_retirement_host().unwrap().clone();
    let rows: Vec<(Box<dyn AudioNodeProcessor>, &[&str])> = vec![
        (Box::new(GainNode::new(10)), &["gain"]),
        (Box::new(DelayNode::new(11, 0.01, 48000, 2)), &["delayTime"]),
        (Box::new(ConstantSourceNode::new(12)), &["offset"]),
        (
            Box::new(OscillatorNode::new(13, 48000)),
            &["frequency", "detune"],
        ),
        (
            Box::new(BufferSourceNode::new(14, 48000)),
            &["playbackRate", "detune"],
        ),
        (
            Box::new(BiquadFilterNode::new(15, 2, 48000)),
            &["frequency", "detune", "Q", "gain"],
        ),
        (
            Box::new(DynamicsCompressorNode::new(16, 2)),
            &["threshold", "knee", "ratio", "attack", "release"],
        ),
        (
            Box::new(PannerNode::new(17)),
            &[
                "positionX",
                "positionY",
                "positionZ",
                "orientationX",
                "orientationY",
                "orientationZ",
            ],
        ),
    ];
    let mut count = 0;
    for (mut node, names) in rows {
        let mut identities = Vec::new();
        for name in names {
            let param = node.get_param_mut(name).unwrap();
            param.reserve_native_events(257);
            param.set_value_at_time(0.5, 1.0);
            identities.push(param.event_identity());
        }
        let credit = ledger.used_bytes();
        node.prepare_params(&owner).unwrap();
        assert!(
            ledger.used_bytes() >= credit + names.len() * (257 * EVENT_BYTES + 4096),
            "node omitted AudioParam capacity adoption"
        );
        let adopted = ledger.used_bytes();
        node.prepare_params(&owner).unwrap();
        assert_eq!(ledger.used_bytes(), adopted, "AudioParam adopted twice");
        for (name, identity) in names.iter().zip(identities) {
            let param = node.get_param_mut(name).unwrap();
            assert_eq!(
                param.event_identity(),
                identity,
                "AudioParam adoption copied original storage"
            );
            count += 1;
        }
        assert!(ctx.add_node(node));
    }
    assert_eq!(count, 22);
    drop(owner);
    ctx.close();
    wait(|| ledger.used_bytes() == 0);
}

#[test]
fn managed_context_and_generic_node_insertion_adopt_timelines() {
    let host = migo_io::scheduler::IoScheduler::new(1);
    let cpu = host.audio_cpu();
    let ledger = Arc::new(AudioAggregateLedger::new(LIMIT));
    let mut empty = AudioContext::new(2, 48000, 2);
    assert!(empty.create_gain(10));
    empty
        .prepare_node_retirement(cpu.clone(), ledger.clone())
        .unwrap();
    let empty_credit = ledger.used_bytes();
    let mut ctx = AudioContext::new(1, 48000, 2);
    let mut node = GainNode::new(10);
    let p = node.get_param_mut("gain").unwrap();
    p.reserve_native_events(257);
    p.set_value_at_time(0.5, 1.0);
    let identity = p.event_identity();
    assert!(ctx.add_node(Box::new(node)));
    ctx.prepare_node_retirement(cpu.clone(), ledger.clone())
        .unwrap();
    let owner = ctx.response_retirement_host().unwrap().clone();
    let adopted = ledger.used_bytes();
    assert!(
        adopted - empty_credit >= empty_credit + 257 * EVENT_BYTES + 4096,
        "existing context omitted AudioParam capacity"
    );
    assert_eq!(
        ctx.with_node_typed::<GainNode, _>(10, |n| n
            .get_param_mut("gain")
            .unwrap()
            .event_identity())
            .unwrap(),
        identity
    );
    let mut extra = GainNode::new(11);
    extra
        .get_param_mut("gain")
        .unwrap()
        .reserve_native_events(257);
    assert!(ctx.add_node(Box::new(extra)));
    assert!(
        ledger.used_bytes() >= adopted + 257 * EVENT_BYTES + 4096,
        "generic insertion omitted AudioParam capacity"
    );
    // An initially empty timeline must also remember its host.
    assert!(ctx.create_gain(12));
    let filler = ledger
        .try_reserve(LIMIT - ledger.used_bytes(), "empty param peer")
        .unwrap();
    assert!(
        !ctx.param_set_value_at_time(12, "gain", 0.5, 1.0),
        "empty managed parameter lost physical host"
    );
    drop(filler);
    drop(owner);
    ctx.close();
    empty.close();
    wait(|| ledger.used_bytes() == 0);
}

#[test]
fn context_setter_refusals_preserve_timeline_and_report_false() {
    let host = migo_io::scheduler::IoScheduler::new(1);
    let cpu = host.audio_cpu();
    let ledger = Arc::new(AudioAggregateLedger::new(LIMIT));
    let mut ctx = graph(&cpu, &ledger);
    let mut node = GainNode::new(10);
    let p = node.get_param_mut("gain").unwrap();
    p.reserve_native_events(4);
    for i in 0..4 {
        p.linear_ramp_to_value_at_time(0.25 + i as f32 * 0.1, i as f64 + 1.0);
    }
    let identity = p.event_identity();
    let state = format!("{p:?}");
    assert!(ctx.add_node(Box::new(node)));
    let filler = ledger
        .try_reserve(LIMIT - ledger.used_bytes(), "context event peer")
        .unwrap();
    for operation in 0..5 {
        let accepted = match operation {
            0 => ctx.param_set_value_at_time(10, "gain", 0.5, 9.0),
            1 => ctx.param_linear_ramp(10, "gain", 0.5, 9.0),
            2 => ctx.param_exponential_ramp(10, "gain", 0.5, 9.0),
            3 => ctx.param_set_target(10, "gain", 0.5, 9.0, 0.1),
            _ => ctx.set_node_param(10, "gain", 0.5),
        };
        assert!(!accepted, "context reported rejected event as accepted");
        ctx.with_node_typed::<GainNode, _>(10, |n| {
            unchanged(n.get_param_mut("gain").unwrap(), identity, &state)
        })
        .unwrap();
    }
    drop(filler);
    ctx.close();
    wait(|| ledger.used_bytes() == 0);
}

#[test]
fn small_large_boundary_and_closed_teardown_follow_original_cpu() {
    let host = migo_io::scheduler::IoScheduler::new(1);
    let cpu = host.audio_cpu();
    let ledger = Arc::new(AudioAggregateLedger::new(LIMIT));
    let mut ctx = graph(&cpu, &ledger);
    let owner = ctx.response_retirement_host().unwrap().clone();
    let small_capacity = 4095 / EVENT_BYTES;
    let bulk_capacity = 4096_usize.div_ceil(EVENT_BYTES);
    let mut small = timeline(small_capacity, 1);
    let credit = ledger.used_bytes();
    small.prepare_storage(&owner).unwrap();
    assert_eq!(
        ledger.used_bytes() - credit,
        small_capacity * EVENT_BYTES,
        "small events acquired bulk handoff overhead"
    );
    drop(small);
    assert_eq!(ledger.used_bytes(), credit);
    let mut bulk = timeline(bulk_capacity, 1);
    bulk.prepare_storage(&owner).unwrap();
    assert!(
        ledger.used_bytes() >= credit + bulk_capacity * EVENT_BYTES + 4096,
        "bulk events lost CPU handoff charge"
    );
    let state = Arc::new(Dropped::default());
    bulk.observe_events(witness(&state));
    let gate = block(&cpu);
    host.close();
    let held = ledger.used_bytes();
    let start = migo_alloc_probe::thread_counts();
    drop(bulk);
    let freed = migo_alloc_probe::thread_counts().bytes_freed - start.bytes_freed;
    assert!(
        freed < (bulk_capacity * EVENT_BYTES) as u64,
        "bulk event storage freed on command caller"
    );
    assert_eq!(
        ledger.used_bytes(),
        held,
        "event teardown returned credit before CPU destruction"
    );
    assert_eq!(state.count.load(Ordering::Acquire), 0);
    gate.open();
    wait(|| state.count.load(Ordering::Acquire) == 1);
    assert!(
        !state.wrong.load(Ordering::Acquire),
        "event storage destroyed off original CPU"
    );
    drop(owner);
    ctx.close();
    wait(|| ledger.used_bytes() == 0);
}

#[test]
fn foreign_adoption_refusal_and_detached_clone_preserve_owners() {
    let first = migo_io::scheduler::IoScheduler::new(1);
    let second = migo_io::scheduler::IoScheduler::new(1);
    let cpu = first.audio_cpu();
    let ledger = Arc::new(AudioAggregateLedger::new(LIMIT));
    let mut ctx = graph(&cpu, &ledger);
    let owner = ctx.response_retirement_host().unwrap().clone();
    let other_ledger = Arc::new(AudioAggregateLedger::new(LIMIT));
    let mut other = graph(&second.audio_cpu(), &other_ledger);
    let foreign = other.response_retirement_host().unwrap().clone();
    let mut param = timeline(257, 200);
    param.prepare_storage(&owner).unwrap();
    let identity = param.event_identity();
    let credit = ledger.used_bytes();
    assert!(
        param.prepare_storage(&foreign).is_err(),
        "AudioParam accepted a foreign physical ledger"
    );
    assert_eq!(param.event_identity(), identity);
    assert_eq!(ledger.used_bytes(), credit);
    let mut clone = param.clone();
    assert_eq!(ledger.used_bytes(), credit);
    assert_ne!(clone.event_identity().2, identity.2);
    let before = other_ledger.used_bytes();
    clone.prepare_storage(&foreign).unwrap();
    assert!(
        other_ledger.used_bytes() >= before + clone.event_identity().1 * EVENT_BYTES + 4096,
        "detached clone omitted capacity adoption"
    );
    clone.set_value_at_time(1.0, 0.0);
    assert_eq!(param.event_identity(), identity);
    let filler = ledger
        .try_reserve(LIMIT - ledger.used_bytes(), "adoption peer")
        .unwrap();
    let mut refused = timeline(257, 1);
    let original = refused.event_identity();
    assert!(
        refused.prepare_storage(&owner).is_err(),
        "native event adoption bypassed physical admission"
    );
    assert_eq!(
        refused.event_identity(),
        original,
        "refused adoption moved native storage"
    );
    drop(filler);
    drop(refused);
    drop(param);
    drop(clone);
    drop(owner);
    drop(foreign);
    ctx.close();
    other.close();
    wait(|| ledger.used_bytes() == 0 && other_ledger.used_bytes() == 0);
}

#[test]
fn owned_timeline_matches_complete_frozen_scalar_and_block_output() {
    let host = migo_io::scheduler::IoScheduler::new(1);
    let cpu = host.audio_cpu();
    let ledger = Arc::new(AudioAggregateLedger::new(LIMIT));
    let mut ctx = graph(&cpu, &ledger);
    let owner = ctx.response_retirement_host().unwrap().clone();
    for rate in [44100, 48000] {
        let mut actual = AudioParamTimeline::new(0.25, 0.0001, 4.0);
        let mut expected = before::AudioParamTimeline::new(0.25, 0.0001, 4.0);
        actual.prepare_storage(&owner).unwrap();
        macro_rules! both {($method:ident($($arg:expr),*))=>{{actual.$method($($arg),*);expected.$method($($arg),*);}};}
        both!(set_value_at_time(0.25, 0.0));
        for i in 0..600 {
            let t = i as f64 * 0.002;
            match i % 4 {
                0 => both!(set_value_at_time(0.75, t)),
                1 => both!(linear_ramp_to_value_at_time(1.25, t)),
                2 => both!(exponential_ramp_to_value_at_time(0.5, t)),
                _ => both!(set_target_at_time(1.5, t, 0.0003)),
            }
            if i % 37 == 0 {
                both!(set_value_at_time(0.625, t));
            }
        }
        for stage in 0..4 {
            match stage {
                1 => both!(set_value_now(0.875, 0.123456789)),
                2 => both!(gc_events(0.4)),
                3 => both!(cancel_scheduled_values(0.95)),
                _ => {}
            }
            for frames in [1, 127, 128, 129, 257, 4097] {
                let mut output = vec![0.0; frames];
                let mut reference = output.clone();
                let mut block = actual.values_block(0.123456789, rate);
                let start = migo_alloc_probe::thread_counts();
                for (chunk, buffer) in output.chunks_mut(128).enumerate() {
                    actual.compute_values_from(&mut block, chunk * 128, buffer);
                }
                let end = migo_alloc_probe::thread_counts();
                assert_eq!(
                    (
                        end.allocations - start.allocations,
                        end.reallocations - start.reallocations,
                        end.bytes_freed - start.bytes_freed
                    ),
                    (0, 0, 0),
                    "owned automation rendered through allocator"
                );
                expected.compute_values(0.123456789, &mut reference, rate);
                assert!(
                    output
                        .iter()
                        .zip(&reference)
                        .all(|(a, b)| a.to_bits() == b.to_bits()),
                    "owned automation changed complete frozen output"
                );
                for (i, value) in output.iter().enumerate() {
                    let t = 0.123456789 + i as f64 / rate as f64;
                    assert_eq!(
                        value.to_bits(),
                        expected.compute_value(t).to_bits(),
                        "owned automation changed frozen scalar value"
                    );
                }
            }
        }
        assert_eq!(actual.default_value(), expected.default_value());
        assert_eq!(actual.min_value(), expected.min_value());
        assert_eq!(actual.max_value(), expected.max_value());
        assert_eq!(actual.has_automation(), expected.has_automation());
        expected.set_value(0.5);
        actual.set_value(0.5);
        assert_eq!(actual.value(), expected.value());
    }
    drop(owner);
    ctx.close();
    wait(|| ledger.used_bytes() == 0);
}
