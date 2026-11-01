//! Complete filter results must precede CPU drop of the borrowed large input.
use super::super::context_retirement_tests::{Dropped, block, wait, witness};
use super::*;
use crate::limits::{AudioAggregateLedger, PcmBudget, PcmUsage};
use crate::nodes::AudioNodeProcessor;
use shared::audio_channel::{
    AUDIO_COMMAND_CAPACITY, AudioCommandReserveError, MAX_AUDIO_COMMAND_QUEUED_BYTES,
};
use std::sync::atomic::Ordering;
const BYTES: usize = 4 << 20;

fn graph(ctx: &mut AudioContext, rate: u32) {
    assert!(ctx.add_node(Box::new(OscillatorNode::new(10, rate))));
    let mut biquad = BiquadFilterNode::new(20, 2, rate);
    biquad.set_type(BiquadFilterType::Highpass);
    biquad.get_param_mut("frequency").unwrap().set_value(1750.0);
    biquad.get_param_mut("Q").unwrap().set_value(0.75);
    biquad.get_param_mut("detune").unwrap().set_value(150.0);
    assert!(ctx.add_node(Box::new(biquad)));
    assert!(ctx.add_node(Box::new(iir())));
    assert!(ctx.create_gain(40));
    for (a, b) in [
        (10, 20),
        (20, 30),
        (30, crate::context::DESTINATION_NODE_ID),
    ] {
        ctx.connect(a, b);
    }
    ctx.with_node_typed::<OscillatorNode, _>(10, |n| n.start(0.0));
}
fn iir() -> IIRFilterNode {
    IIRFilterNode::new(30, vec![0.35, 0.2, -0.1], vec![2.0, -0.3, 0.15], 2)
}
fn frequencies(len: usize, rate: u32, capacity: usize) -> Vec<f32> {
    let mut values = Vec::with_capacity(capacity);
    values.extend((0..len).map(|i| i as f32 * rate as f32 / len.max(1) as f32 * 1.2));
    for (slot, value) in values.iter_mut().zip([
        0.0,
        -0.0,
        f32::NAN,
        f32::INFINITY,
        f32::NEG_INFINITY,
        -100.0,
        rate as f32 / 2.0,
        rate as f32,
    ]) {
        *slot = value;
    }
    values
}
fn original(ctx: &mut AudioContext, id: u32, values: &[f32]) -> (Vec<f32>, Vec<f32>) {
    let rate = ctx.sample_rate() as f64;
    match id {
        20 => ctx
            .with_node_typed::<BiquadFilterNode, _>(id, |n| n.get_frequency_response(rate, values))
            .unwrap(),
        30 => ctx
            .with_node_typed::<IIRFilterNode, _>(id, |n| n.get_frequency_response(rate, values))
            .unwrap(),
        _ => unreachable!(),
    }
}
fn same(a: &shared::audio_frequency_response::FrequencyResponse, b: &(Vec<f32>, Vec<f32>)) -> bool {
    let (magnitude, phase) = a.values();
    [magnitude, phase]
        .into_iter()
        .zip([b.0.as_slice(), b.1.as_slice()])
        .all(|(a, b)| {
            a.len() == b.len() && a.iter().zip(b).all(|(a, b)| a.to_bits() == b.to_bits())
        })
}
fn pcm(actual: &mut AudioContext, expected: &mut AudioContext, rate: u32) -> bool {
    let mut equal = true;
    let mut audible = false;
    for _ in 0..8 {
        let mut a = [0.0; 256];
        let mut b = [0.0; 256];
        actual.process_native_device(&mut a, rate, 2).unwrap();
        expected.process_native_device(&mut b, rate, 2).unwrap();
        equal &= a.iter().zip(b).all(|(a, b)| a.to_bits() == b.to_bits());
        audible |= b.iter().any(|v| v.abs() > 1e-5);
    }
    equal && audible
}

pub(crate) fn frequency_inputs() {
    super::output_tests::outputs();
    for rate in [44100, 48000] {
        for closed in [false, true] {
            let host = migo_io::scheduler::IoScheduler::new(1);
            let cpu = host.audio_cpu();
            let ledger = Arc::new(AudioAggregateLedger::new(64 << 20));
            let usage = Arc::new(PcmUsage::new(32 << 20, 32));
            let process = Arc::new(PcmUsage::new(32 << 20, 32));
            let budget = PcmBudget::with_aggregate(usage.clone(), process.clone(), ledger.clone());
            let mut actual = AudioContext::new_with_pcm_budget(1, rate, 2, budget);
            actual
                .prepare_node_retirement(cpu.clone(), ledger.clone())
                .unwrap();
            let mut expected = AudioContext::new(1, rate, 2);
            graph(&mut actual, rate);
            graph(&mut expected, rate);
            let mut contexts = HashMap::from([(1, actual)]);
            let mut index = NodeContextIndex::new();
            for id in [10, 20, 30, 40] {
                index.register(id, 1);
            }
            let (tx, rx) = shared::audio_channel::channel();
            let mut answers = Vec::new();
            for id in [20, 30] {
                for len in [
                    0,
                    1023,
                    1024,
                    migo_services::audio::MAX_FREQUENCY_RESPONSE_POINTS,
                ] {
                    let values = frequencies(len, rate, BYTES / 4);
                    assert_eq!(values.capacity() * 4, BYTES);
                    let expected = original(&mut expected, id, &values);
                    let (resp, answer) = tokio::sync::oneshot::channel();
                    tx.try_send(AudioCmd::GetFrequencyResponse {
                        node_id: id,
                        frequencies: values,
                        resp,
                    })
                    .unwrap();
                    answers.push(Some((answer, expected)));
                }
            }
            // Cancellation still performs the original typed-node operation.
            for id in [20, 30] {
                let (resp, answer) = tokio::sync::oneshot::channel();
                drop(answer);
                tx.try_send(AudioCmd::GetFrequencyResponse {
                    node_id: id,
                    frequencies: frequencies(16384, rate, BYTES / 4),
                    resp,
                })
                .unwrap();
                answers.push(None);
            }
            let count = answers.len();
            let bytes = count * BYTES;
            let backlog = (0..5).map(|_| rx.try_recv_owned().unwrap()).collect();
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
            let mut handled = 0;
            let mut responses = true;
            let mut side_effect = true;
            let before = migo_alloc_probe::thread_counts();
            for answer in &mut answers {
                contexts.get_mut(&1).unwrap().collect_device_controls();
                if forward(inbox.next().unwrap(), &mut contexts, &index, &mut inbox).is_none() {
                    handled += 1;
                }
                side_effect &= contexts.get(&1).unwrap().has_device_controls() != closed;
                if let Some((answer, expected)) = answer {
                    responses &= answer.try_recv().is_ok_and(|r| {
                        if closed {
                            r.is_err_and(|e| e.code == ErrorCode::IoError)
                        } else {
                            r.is_ok_and(|r| same(&r, expected))
                        }
                    });
                }
            }
            let freed = migo_alloc_probe::thread_counts().bytes_freed - before.bytes_freed;
            let item = matches!(tx.try_reserve_data(0), Err(AudioCommandReserveError::Full));
            drop(fillers.pop());
            let bytes_held = matches!(
                tx.try_reserve_data(1),
                Err(AudioCommandReserveError::ByteLimit)
            );
            gate.open();
            assert_eq!(
                handled, count,
                "bulk frequency input bypassed CPU retirement"
            );
            assert!(
                freed < (BYTES / 2) as u64,
                "frequency input escaped CPU retirement"
            );
            assert!(
                responses,
                "frequency response changed or waited for CPU retirement"
            );
            assert!(
                side_effect,
                "cancelled frequency query skipped original node effect"
            );
            wait(|| state.count.load(Ordering::Acquire) == count);
            assert!(
                !state.wrong.load(Ordering::Acquire),
                "frequency input escaped CPU retirement"
            );
            assert!(
                item && bytes_held,
                "frequency credit returned before CPU drop"
            );
            let returned = tx.try_reserve_data(bytes).unwrap();
            drop((returned, fillers));
            assert!(
                pcm(contexts.get_mut(&1).unwrap(), &mut expected, rate),
                "frequency query changed complete rendered PCM"
            );
            contexts.clear();
            drop(inbox);
            drop(host);
            wait(|| {
                ledger.used_bytes() == 0
                    && usage.snapshot().bytes == 0
                    && process.snapshot().bytes == 0
            });
        }
    }
    compatibility();
    boundary();
}

fn compatibility() {
    let host = migo_io::scheduler::IoScheduler::new(1);
    let cpu = host.audio_cpu();
    let mut ctx = AudioContext::new(1, 48000, 2);
    let mut expected = AudioContext::new(1, 48000, 2);
    graph(&mut ctx, 48000);
    graph(&mut expected, 48000);
    let mut contexts = HashMap::from([(1, ctx)]);
    let mut index = NodeContextIndex::new();
    for id in [20, 30, 40] {
        index.register(id, 1);
    }
    index.register(99, 2); // stale owner; context 2 is absent
    let (tx, rx) = shared::audio_channel::channel();
    let mut inbox = crate::command_inbox::Inbox::new(rx, vec![], cpu.clone());
    let state = Arc::new(Dropped::default());
    inbox.observe(witness(&state));
    let gate = block(&cpu);
    for unmanaged in [false, true] {
        for len in [0, 1, 1023] {
            let capacity = if unmanaged { BYTES / 4 } else { len };
            let values = frequencies(len, 48000, capacity);
            let expected = original(&mut expected, 20, &values);
            let (resp, mut answer) = tokio::sync::oneshot::channel();
            tx.try_send(AudioCmd::GetFrequencyResponse {
                node_id: 20,
                frequencies: values,
                resp,
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
                let AudioCmd::GetFrequencyResponse {
                    node_id,
                    frequencies,
                    resp,
                } = command
                else {
                    unreachable!()
                };
                let _ = resp.send(query(&mut contexts, &index, node_id, &frequencies));
                drop((frequencies, permit));
            }
            assert!(
                answer
                    .try_recv()
                    .unwrap()
                    .is_ok_and(|r| same(&r, &expected)),
                "small or legacy frequency result changed"
            );
            let available = tx.try_reserve_data(MAX_AUDIO_COMMAND_QUEUED_BYTES);
            let inline = available.is_ok();
            drop(available);
            assert!(inline, "small frequency input unnecessarily handed to CPU");
        }
    }
    // Empty/boundary response and errors preserve the original mature API.
    for id in [20, 30] {
        for len in [0, 1024, 16384] {
            let values = frequencies(len, 48000, len);
            let want = original(&mut expected, id, &values);
            let got = query(&mut contexts, &index, id, &values).unwrap();
            assert!(
                same(&got, &want),
                "frequency response changed or waited for CPU retirement"
            );
        }
    }
    for id in [40, 99, 100] {
        let error = query(&mut contexts, &index, id, &[100.0]).unwrap_err();
        assert_eq!(error.code, ErrorCode::NotFound, "frequency error changed");
        assert!(
            error
                .to_string()
                .contains(&format!("Filter node {} not found", id)),
            "frequency error detail changed"
        );
    }
    let delivery = AudioCommandDelivery::unmanaged(AudioCmd::GetReduction {
        node_id: 40,
        resp: tokio::sync::oneshot::channel().0,
    });
    let other = forward(delivery, &mut contexts, &index, &mut inbox);
    gate.open();
    assert!(
        other.is_some_and(|d| matches!(d.command(), AudioCmd::GetReduction { .. })),
        "frequency dispatcher consumed unrelated command"
    );
    assert_eq!(
        state.count.load(Ordering::Acquire),
        0,
        "small frequency input unnecessarily handed to CPU"
    );
}

fn boundary() {
    let host = migo_io::scheduler::IoScheduler::new(1);
    let cpu = host.audio_cpu();
    let mut ctx = AudioContext::new(1, 48000, 2);
    graph(&mut ctx, 48000);
    let mut contexts = HashMap::from([(1, ctx)]);
    let mut index = NodeContextIndex::new();
    index.register(20, 1);
    let (tx, rx) = shared::audio_channel::channel();
    let mut inbox = crate::command_inbox::Inbox::new(rx, vec![], cpu.clone());
    let state = Arc::new(Dropped::default());
    inbox.observe(witness(&state));
    let gate = block(&cpu);
    let (resp, mut answer) = tokio::sync::oneshot::channel();
    tx.try_send(AudioCmd::GetFrequencyResponse {
        node_id: 20,
        frequencies: Vec::with_capacity(1024),
        resp,
    })
    .unwrap();
    let handled = forward(inbox.next().unwrap(), &mut contexts, &index, &mut inbox).is_none();
    let responded = answer
        .try_recv()
        .is_ok_and(|r| r.is_ok_and(|r| r.values().0.is_empty() && r.values().1.is_empty()));
    let held = matches!(
        tx.try_reserve_data(MAX_AUDIO_COMMAND_QUEUED_BYTES),
        Err(AudioCommandReserveError::ByteLimit)
    );
    gate.open();
    assert!(
        handled && responded && held,
        "4096-byte frequency input lost retirement boundary"
    );
    wait(|| state.count.load(Ordering::Acquire) == 1);
    assert!(
        !state.wrong.load(Ordering::Acquire),
        "frequency input escaped CPU retirement"
    );
    assert!(tx.try_reserve_data(MAX_AUDIO_COMMAND_QUEUED_BYTES).is_ok());
}
