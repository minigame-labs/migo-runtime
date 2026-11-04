//! Large String inputs must die on CPU after the original ordered effects.
use super::super::context_retirement_tests::{Dropped, block, context, wait, witness};
use super::*;
use crate::limits::{AudioAggregateLedger, PcmUsage};
use shared::audio_channel::{
    AUDIO_COMMAND_CAPACITY, AudioCommandReserveError, MAX_AUDIO_COMMAND_QUEUED_BYTES,
};
use std::sync::atomic::Ordering;
const BYTES: usize = 4 << 20;
const RATE: f64 = 48000.0;
fn wide(value: &str) -> String {
    let mut s = String::with_capacity(BYTES);
    s.push_str(value);
    assert_eq!(s.capacity(), BYTES);
    s
}
fn graph(ctx: &mut AudioContext) {
    assert!(ctx.create_gain(60));
    assert!(ctx.add_node(Box::new(OscillatorNode::new(10, 48000))));
    assert!(ctx.add_node(Box::new(BiquadFilterNode::new(20, 2, 48000))));
    assert!(ctx.add_node(Box::new(WaveShaperNode::new(30))));
    assert!(ctx.add_node(Box::new(PannerNode::new(40))));
    assert!(ctx.add_node(Box::new(AnalyserNode::new(50, 2))));
    assert!(ctx.set_wave_shaper_curve(30, Some(vec![-1.0, -0.8, 0.0, 0.8, 1.0])));
    ctx.set_node_param(40, "positionX", 3.0);
    ctx.set_node_param(40, "positionZ", 4.0);
    ctx.with_node_typed::<PannerNode, _>(40, |n| n.set_rolloff_factor(2.0));
    ctx.param_set_value_at_time(60, "gain", 0.95, 768.0 / RATE);
    for (a, b) in [
        (10, 60),
        (60, 20),
        (20, 30),
        (30, 40),
        (40, 50),
        (50, crate::context::DESTINATION_NODE_ID),
    ] {
        ctx.connect(a, b);
    }
    ctx.with_node_typed::<OscillatorNode, _>(10, |n| n.start(0.0));
}
fn reference(ctx: &mut AudioContext) {
    assert!(ctx.set_node_param(60, "gain", 0.5));
    assert!(ctx.param_set_value_at_time(60, "gain", 0.75, 0.0));
    assert!(ctx.param_linear_ramp(60, "gain", 0.4, 128.0 / RATE));
    assert!(ctx.param_exponential_ramp(60, "gain", 0.1, 256.0 / RATE));
    assert!(ctx.param_set_target(60, "gain", 0.3, 384.0 / RATE, 64.0 / RATE));
    assert!(ctx.param_cancel_scheduled(60, "gain", 512.0 / RATE));
    ctx.with_node_typed::<OscillatorNode, _>(10, |n| n.set_type(OscillatorType::Triangle));
    ctx.with_node_typed::<BiquadFilterNode, _>(20, |n| n.set_type(BiquadFilterType::Highpass));
    assert!(ctx.set_wave_shaper_oversample(30, OversampleType::FourX));
    ctx.with_node_typed::<PannerNode, _>(40, |n| {
        n.set_panning_model(PanningModel::HRTF);
        n.set_distance_model(DistanceModel::Exponential);
        n.set_ref_distance(2.0);
    });
    ctx.with_node_typed::<AnalyserNode, _>(50, |n| n.set_smoothing_time_constant(0.35));
}
fn matching(actual: &mut AudioContext, expected: &mut AudioContext) -> bool {
    let mut ok = true;
    let mut audible = false;
    for _ in 0..12 {
        let mut a = [0.0_f32; 256];
        let mut b = [0.0_f32; 256];
        actual.process_native_device(&mut a, 48000, 2).unwrap();
        expected.process_native_device(&mut b, 48000, 2).unwrap();
        ok &= a.iter().zip(b).all(|(a, b)| a.to_bits() == b.to_bits());
        audible |= b.iter().any(|v| v.abs() > 1e-5);
    }
    let a = actual
        .with_node_typed::<AnalyserNode, _>(50, |n| n.get_float_frequency_data())
        .unwrap();
    let b = expected
        .with_node_typed::<AnalyserNode, _>(50, |n| n.get_float_frequency_data())
        .unwrap();
    ok && audible && a.len() == b.len() && a.iter().zip(b).all(|(a, b)| a.to_bits() == b.to_bits())
}

pub(crate) fn parameter_inputs() {
    for closed in [false, true] {
        let host = migo_io::scheduler::IoScheduler::new(1);
        let cpu = host.audio_cpu();
        let ledger = Arc::new(AudioAggregateLedger::new(64 << 20));
        let usage = Arc::new(PcmUsage::new(32 << 20, 32));
        let process = Arc::new(PcmUsage::new(32 << 20, 32));
        let mut actual = context(&cpu, &ledger, &usage, &process, 2);
        let mut expected = AudioContext::new(1, 48000, 2);
        graph(&mut actual);
        graph(&mut expected);
        // Closing refuses fresh storage admission. Keep this test focused on
        // closed-host parameter delivery and input cleanup by preparing the
        // reusable FIR and event capacities while its host is still open.
        if closed {
            assert!(actual.set_wave_shaper_oversample(30, OversampleType::TwoX));
            assert!(actual.set_wave_shaper_oversample(30, OversampleType::None));
            for i in 0..4 {
                assert!(actual.param_set_value_at_time(60, "gain", 0.5, 1.0e9 + i as f64));
            }
            assert!(actual.param_cancel_scheduled(60, "gain", 1.0e9));
        }
        reference(&mut expected);
        let mut contexts = HashMap::from([(1, actual)]);
        let mut index = NodeContextIndex::new();
        for id in [10, 20, 30, 40, 50, 60] {
            index.register(id, 1);
        }
        let commands = vec![
            AudioCmd::SetNodeParam {
                node_id: 60,
                param_name: wide("gain"),
                value: 0.5,
            },
            AudioCmd::AudioParamSetValueAtTime {
                node_id: 60,
                param_name: wide("gain"),
                value: 0.75,
                time: 0.0,
            },
            AudioCmd::AudioParamLinearRamp {
                node_id: 60,
                param_name: wide("gain"),
                value: 0.4,
                end_time: 128.0 / RATE,
            },
            AudioCmd::AudioParamExponentialRamp {
                node_id: 60,
                param_name: wide("gain"),
                value: 0.1,
                end_time: 256.0 / RATE,
            },
            AudioCmd::AudioParamSetTarget {
                node_id: 60,
                param_name: wide("gain"),
                target: 0.3,
                start_time: 384.0 / RATE,
                time_constant: 64.0 / RATE,
            },
            AudioCmd::AudioParamCancelScheduled {
                node_id: 60,
                param_name: wide("gain"),
                cancel_time: 512.0 / RATE,
            },
            AudioCmd::SetOscillatorType {
                node_id: 10,
                osc_type: wide("triangle"),
            },
            AudioCmd::SetBiquadFilterType {
                node_id: 20,
                filter_type: wide("highpass"),
            },
            AudioCmd::SetWaveShaperOversample {
                node_id: 30,
                oversample: wide("4x"),
            },
            AudioCmd::SetPanningModel {
                node_id: 40,
                model: wide("HRTF"),
            },
            AudioCmd::SetDistanceModel {
                node_id: 40,
                model: wide("exponential"),
            },
            AudioCmd::SetAnalyserScalar {
                node_id: 50,
                prop: wide("smoothingTimeConstant"),
                value: 0.35,
            },
            AudioCmd::SetPannerScalar {
                node_id: 40,
                prop: wide("refDistance"),
                value: 2.0,
            },
            AudioCmd::SetNodeParam {
                node_id: 60,
                param_name: "x".repeat(BYTES),
                value: 1.0,
            },
            AudioCmd::SetOscillatorType {
                node_id: 20,
                osc_type: wide("triangle"),
            },
        ];
        let count = commands.len();
        let bytes: usize = commands.iter().map(AudioCmd::queued_payload_bytes).sum();
        assert_eq!(bytes, count * BYTES, "parameter capacity was not charged");
        let (tx, rx) = shared::audio_channel::channel();
        for command in commands {
            tx.try_send(command).unwrap();
        }
        let backlog = (0..7).map(|_| rx.try_recv_owned().unwrap()).collect();
        let mut fillers = vec![
            tx.try_reserve_data(MAX_AUDIO_COMMAND_QUEUED_BYTES - bytes)
                .unwrap(),
        ];
        for _ in count + 1..AUDIO_COMMAND_CAPACITY {
            fillers.push(tx.try_reserve_data(0).unwrap());
        }
        let mut inbox = crate::command_inbox::Inbox::new(rx, backlog, cpu.clone());
        let state = Arc::new(Dropped::default());
        inbox.observe(witness(&state));
        let gate = block(&cpu);
        if closed {
            host.close();
        }
        let before = migo_alloc_probe::thread_counts();
        let mut handled = 0;
        let mut scalar_applied = false;
        for step in 0..count {
            let delivery = inbox.next().unwrap();
            if forward(delivery, &mut contexts, &index, &mut inbox).is_none() {
                handled += 1;
            }
            if step == 0 {
                scalar_applied = contexts
                    .get_mut(&1)
                    .unwrap()
                    .with_node_typed::<crate::nodes::GainNode, _>(60, |n| {
                        crate::nodes::AudioNodeProcessor::get_param_mut(n, "gain")
                            .unwrap()
                            .compute_value(0.0)
                            .to_bits()
                            == 0.5_f32.to_bits()
                    })
                    .unwrap();
            }
        }
        let freed = migo_alloc_probe::thread_counts().bytes_freed - before.bytes_freed;
        let item = matches!(tx.try_reserve_data(0), Err(AudioCommandReserveError::Full));
        drop(fillers.pop());
        let bytes_held = matches!(
            tx.try_reserve_data(1),
            Err(AudioCommandReserveError::ByteLimit)
        );
        let effect = matching(contexts.get_mut(&1).unwrap(), &mut expected) && scalar_applied;
        gate.open();
        assert_eq!(handled, count, "bulk parameter bypassed input retirement");
        assert!(
            freed < (BYTES / 2) as u64,
            "parameter input escaped CPU retirement"
        );

        assert!(
            effect,
            "parameter commands changed PCM, automation or analyser"
        );
        wait(|| state.count.load(Ordering::Acquire) == count);
        assert!(
            !state.wrong.load(Ordering::Acquire),
            "parameter input escaped CPU retirement"
        );
        assert!(
            item && bytes_held,
            "parameter credit returned before CPU drop"
        );
        let returned = tx.try_reserve_data(bytes).unwrap();
        drop((returned, fillers));
        contexts.clear();
        drop(inbox);
        drop(host);
        wait(|| {
            ledger.used_bytes() == 0 && usage.snapshot().bytes == 0 && process.snapshot().bytes == 0
        });
    }
    compatibility();
}

fn compatibility() {
    let host = migo_io::scheduler::IoScheduler::new(1);
    let cpu = host.audio_cpu();
    let mut actual = AudioContext::new(1, 48000, 2);
    let mut expected = AudioContext::new(1, 48000, 2);
    graph(&mut actual);
    graph(&mut expected);
    let mut contexts = HashMap::from([(1, actual)]);
    let mut index = NodeContextIndex::new();
    for id in [10, 20, 30, 40, 50, 60] {
        index.register(id, 1);
    }
    let (tx, rx) = shared::audio_channel::channel();
    let mut inbox = crate::command_inbox::Inbox::new(rx, vec![], cpu.clone());
    let gate = block(&cpu);
    let state = Arc::new(Dropped::default());
    inbox.observe(witness(&state));
    for unmanaged in [false, true] {
        let value = if unmanaged { 0.75 } else { 0.25 };
        let param_name = if unmanaged {
            wide("gain")
        } else {
            "gain".to_owned()
        };
        tx.try_send(AudioCmd::SetNodeParam {
            node_id: 60,
            param_name,
            value,
        })
        .unwrap();
        let delivery = inbox.next().unwrap();
        let delivery = if unmanaged {
            let (command, permit) = delivery.into_parts();
            drop(permit);
            AudioCommandDelivery::unmanaged(command)
        } else {
            delivery
        };
        if let Some(delivery) = forward(delivery, &mut contexts, &index, &mut inbox) {
            let (command, permit) = delivery.into_parts();
            assert!(apply(&command, &mut contexts, &index));
            drop((command, permit));
        }
        expected.set_node_param(60, "gain", value);
        let available = tx.try_reserve_data(MAX_AUDIO_COMMAND_QUEUED_BYTES);
        let inline = available.is_ok();
        drop(available);
        assert!(inline, "small parameter unnecessarily handed to CPU");
    }
    // A large non-String command must keep its original owner and dispatch.
    tx.try_send(AudioCmd::SetWaveShaperCurve {
        node_id: 30,
        curve: Some(vec![0.0; BYTES / 4]),
    })
    .unwrap();
    let delivery = inbox.next().unwrap();
    let forwarded = forward(delivery, &mut contexts, &index, &mut inbox);
    let other = forwarded
        .as_ref()
        .is_some_and(|d| matches!(d.command(), AudioCmd::SetWaveShaperCurve { .. }));
    let credit = matches!(
        tx.try_reserve_data(MAX_AUDIO_COMMAND_QUEUED_BYTES),
        Err(AudioCommandReserveError::ByteLimit)
    );
    drop(forwarded);
    gate.open();
    assert!(
        other && credit,
        "parameter dispatcher consumed unrelated command"
    );
    assert_eq!(
        state.count.load(Ordering::Acquire),
        0,
        "small parameter unnecessarily handed to CPU"
    );
    assert!(
        matching(contexts.get_mut(&1).unwrap(), &mut expected),
        "small or legacy parameter effects changed"
    );
}
