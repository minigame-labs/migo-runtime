//! Borrow normalization input; preserve node admission, DSP and original tickets.
use super::super::context_retirement_tests::{Dropped, block, wait, witness};
use super::*;
use crate::limits::{AudioAggregateLedger, PcmBudget, PcmUsage};
use crate::nodes::AudioNodeProcessor;
use shared::audio_channel::{
    AUDIO_COMMAND_CAPACITY, AudioCommandReserveError, MAX_AUDIO_COMMAND_QUEUED_BYTES,
};
use std::sync::atomic::Ordering;
const BYTES: usize = 4 << 20;
const LIMIT: usize = 64 << 20;
fn values(data: &[f64], bytes: usize) -> Vec<f64> {
    let mut v = Vec::with_capacity(bytes / 8);
    v.extend_from_slice(data);
    v
}
fn coefficients(mode: &str, i: usize) -> (Vec<f64>, Vec<f64>) {
    let (mut ff, mut fb) = match i {
        0 => (vec![0.6, 0.25, -0.1], vec![2.0, -0.3, 0.15]),
        1 => (vec![1.0, -0.2, 0.05], vec![-2.0, 0.3, -0.1]),
        _ => (vec![0.4, -0.1, 0.05], vec![1e-21, 0.1, -0.01]),
    };
    if matches!(mode, "empty-ff" | "empty-both") {
        ff.clear();
    }
    if matches!(mode, "empty-fb" | "empty-both") {
        fb.clear();
    }
    if mode == "non-finite" {
        ff[0] = f64::NAN;
        fb[0] = f64::INFINITY;
    }
    match i {
        0 => (values(&ff, BYTES), fb),
        1 => (ff, values(&fb, BYTES)),
        _ => (values(&ff, BYTES / 2), values(&fb, BYTES / 2)),
    }
}
fn contexts(
    cpu: &migo_io::audio_cpu::AudioCpu,
    ledger: &Arc<AudioAggregateLedger>,
    usage: &Arc<PcmUsage>,
    process: &Arc<PcmUsage>,
    mode: &str,
) -> HashMap<AudioContextId, AudioContext> {
    let mut all = HashMap::new();
    for (id, rate, ch) in [(1, 48000, 1), (2, 44100, 2)] {
        let budget = PcmBudget::with_aggregate(usage.clone(), process.clone(), ledger.clone());
        let mut ctx = AudioContext::new_with_pcm_budget(id, rate, ch, budget);
        ctx.prepare_node_retirement(cpu.clone(), ledger.clone())
            .unwrap();
        assert!(ctx.add_node(Box::new(OscillatorNode::new(1000 + id, rate))));
        ctx.with_node_typed::<OscillatorNode, _>(1000 + id, |n| n.start(0.0));
        if mode == "duplicate" && id == 1 {
            assert!(ctx.create_gain(10));
        }
        if mode == "closed-context" {
            ctx.close();
        }
        all.insert(id, ctx);
    }
    all
}
fn legacy(
    command: AudioCmd,
    contexts: &mut HashMap<AudioContextId, AudioContext>,
    index: &mut NodeContextIndex,
) {
    let AudioCmd::CreateIIRFilter {
        ctx_id,
        node_id,
        feedforward,
        feedback,
    } = command
    else {
        unreachable!()
    };
    if let Some(ctx) = contexts.get_mut(&ctx_id) {
        let ch = ctx.channels();
        if ctx.add_node(Box::new(IIRFilterNode::new(
            node_id,
            feedforward,
            feedback,
            ch,
        ))) {
            index.register(node_id, ctx_id);
        }
    }
}
pub(crate) fn construction_inputs() {
    golden();
    for mode in [
        "success",
        "duplicate",
        "aggregate-full",
        "closed-host",
        "closed-context",
        "missing-context",
        "empty-ff",
        "empty-fb",
        "empty-both",
        "non-finite",
    ] {
        case(mode);
    }
    compatibility();
}
fn case(mode: &str) {
    let host = migo_io::scheduler::IoScheduler::new(1);
    let cpu = host.audio_cpu();
    let ledger = Arc::new(AudioAggregateLedger::new(LIMIT));
    let expected_ledger = Arc::new(AudioAggregateLedger::new(LIMIT));
    let usage = Arc::new(PcmUsage::new(32 << 20, 32));
    let process = Arc::new(PcmUsage::new(32 << 20, 32));
    let mut actual = contexts(&cpu, &ledger, &usage, &process, mode);
    let mut expected = contexts(&cpu, &expected_ledger, &usage, &process, mode);
    let mut index = NodeContextIndex::new();
    let mut reference = NodeContextIndex::new();
    if mode == "duplicate" {
        index.register(10, 1);
        reference.register(10, 1);
    }
    let quota = (mode == "aggregate-full").then(|| {
        ledger
            .try_reserve(LIMIT - ledger.used_bytes(), "IIR refusal fixture")
            .unwrap()
    });
    let expected_quota = (mode == "aggregate-full").then(|| {
        expected_ledger
            .try_reserve(
                LIMIT - expected_ledger.used_bytes(),
                "IIR refusal reference",
            )
            .unwrap()
    });
    let (tx, rx) = shared::audio_channel::channel();
    let mut originals = Vec::new();
    let mut bytes = 0;
    for (i, (owner, id)) in [(1, 10), (2, 20), (1, 30)].into_iter().enumerate() {
        let owner = if mode == "missing-context" { 99 } else { owner };
        let (feedforward, feedback) = coefficients(mode, i);
        originals.push(AudioCmd::CreateIIRFilter {
            ctx_id: owner,
            node_id: id,
            feedforward: feedforward.to_vec(),
            feedback: feedback.to_vec(),
        });
        let cmd = AudioCmd::CreateIIRFilter {
            ctx_id: owner,
            node_id: id,
            feedforward,
            feedback,
        };
        bytes += cmd.queued_payload_bytes();
        tx.try_send(cmd).unwrap();
    }
    let backlog = (0..2).map(|_| rx.try_recv_owned().unwrap()).collect();
    let mut fillers = vec![
        tx.try_reserve_data(MAX_AUDIO_COMMAND_QUEUED_BYTES - bytes)
            .unwrap(),
    ];
    for _ in 4..AUDIO_COMMAND_CAPACITY {
        fillers.push(tx.try_reserve_data(0).unwrap());
    }
    let mut inbox = crate::command_inbox::Inbox::new(rx, backlog, cpu.clone());
    let state = Arc::new(Dropped::default());
    inbox.observe(witness(&state));
    let gate = block(&cpu);
    if mode == "closed-host" {
        host.close();
    }
    let before = migo_alloc_probe::thread_counts();
    let mut handled = 0;
    for original in originals {
        legacy(original, &mut expected, &mut reference);
        if forward(inbox.next().unwrap(), &mut actual, &mut index, &mut inbox).is_none() {
            handled += 1;
        }
    }
    let freed = migo_alloc_probe::thread_counts().bytes_freed - before.bytes_freed;
    let item = matches!(tx.try_reserve_data(0), Err(AudioCommandReserveError::Full));
    drop(fillers.pop());
    let byte = matches!(
        tx.try_reserve_data(1),
        Err(AudioCommandReserveError::ByteLimit)
    );
    let mut spare = Vec::new();
    while let Ok(p) = tx.try_reserve_data(0) {
        spare.push(p);
    }
    let free = spare.len();
    drop(spare);
    let mut same = true;
    let admitted = !matches!(
        mode,
        "aggregate-full" | "closed-host" | "closed-context" | "missing-context"
    );
    for (id, owner) in [(10, 1), (20, 2), (30, 1)] {
        same &= index.get_context(id) == reference.get_context(id);
        same &= index.get_context(id) == admitted.then_some(owner);
        let a = actual
            .get_mut(&owner)
            .unwrap()
            .with_node_typed::<IIRFilterNode, _>(id, |n| {
                n.get_frequency_response(48000.0, &[0.0, 1000.0, 12000.0, 24000.0])
            });
        let b = expected
            .get_mut(&owner)
            .unwrap()
            .with_node_typed::<IIRFilterNode, _>(id, |n| {
                n.get_frequency_response(48000.0, &[0.0, 1000.0, 12000.0, 24000.0])
            });
        same &= match (a, b) {
            (Some(a), Some(b)) => eq(&a.0, &b.0) && eq(&a.1, &b.1),
            (None, None) => true,
            _ => false,
        };
    }
    let no_drop = state.count.load(Ordering::Acquire) == 0;
    gate.open();
    assert_eq!(
        handled, 3,
        "bulk IIR construction bypassed borrowed dispatch"
    );
    assert!(
        freed < (BYTES / 2) as u64 && no_drop,
        "IIR input escaped CPU retirement"
    );
    assert!(
        item && byte && free == 1,
        "IIR input credit returned before CPU drop"
    );
    assert!(
        same,
        "IIR construction changed node admission, index or frequency response"
    );
    wait(|| state.count.load(Ordering::Acquire) == 3);
    assert!(
        !state.wrong.load(Ordering::Acquire),
        "IIR input escaped CPU retirement"
    );
    let returned = tx.try_reserve_data(bytes).unwrap();
    drop((returned, fillers, quota, expected_quota));
    for ctxs in [&mut actual, &mut expected] {
        let a = ctxs.get_mut(&1).unwrap();
        for (src, dst) in [
            (1001, 10),
            (10, 30),
            (30, crate::context::DESTINATION_NODE_ID),
        ] {
            a.connect(src, dst);
        }
        let b = ctxs.get_mut(&2).unwrap();
        for (src, dst) in [(1002, 20), (20, crate::context::DESTINATION_NODE_ID)] {
            b.connect(src, dst);
        }
    }
    let mut audible = false;
    let mut pcm = true;
    for (owner, rate, ch) in [(1, 48000, 1), (2, 44100, 2)] {
        for _ in 0..8 {
            let mut a = vec![0.0; 128 * ch as usize];
            let mut b = vec![0.0; 128 * ch as usize];
            let ae = actual
                .get_mut(&owner)
                .unwrap()
                .process_native_device(&mut a, rate, ch)
                .unwrap()
                .to_vec();
            let be = expected
                .get_mut(&owner)
                .unwrap()
                .process_native_device(&mut b, rate, ch)
                .unwrap()
                .to_vec();
            pcm &= eq(&a, &b) && ae == be;
            audible |= b.iter().any(|v| v.abs() > 1e-5);
        }
    }
    assert!(
        pcm && (!admitted || mode == "non-finite" || audible),
        "IIR construction changed complete rendered PCM or ended set"
    );
    actual.clear();
    expected.clear();
    drop(inbox);
    drop(host);
    wait(|| {
        ledger.used_bytes() == 0
            && expected_ledger.used_bytes() == 0
            && usage.snapshot().bytes == 0
            && process.snapshot().bytes == 0
    });
}
fn eq(a: &[f32], b: &[f32]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(a, b)| a.to_bits() == b.to_bits())
}
fn golden() {
    let mut node = IIRFilterNode::from_coefficients(7, &[1.0], &[2.0, -1.0], 2);
    let mut input = [0.0; 16];
    input[0] = 1.0;
    input[1] = -1.0;
    let mut actual = [0.0; 16];
    let mut at = 0;
    for frames in [1, 3, 4] {
        let end = at + frames * 2;
        assert_eq!(
            node.process(&input[at..end], &mut actual[at..end], 48000, 2, 0.0),
            frames,
            "IIR borrowed constructor changed frame or channel layout"
        );
        at = end;
    }
    for frame in 0..8 {
        let expected = 0.5_f32.powi(frame as i32 + 1);
        assert!(
            actual[frame * 2].to_bits() == expected.to_bits()
                && actual[frame * 2 + 1].to_bits() == (-expected).to_bits(),
            "IIR borrowed constructor changed normalization or channel history"
        );
    }
    let (mag, phase) = node.get_frequency_response(48000.0, &[0.0]);
    assert!(
        mag == [1.0] && phase == [0.0],
        "IIR borrowed constructor changed original response"
    );
    let mut fir = IIRFilterNode::from_coefficients(8, &[0.25, 0.5, 0.25], &[1.0], 1);
    let mut out = [0.0; 8];
    fir.process(
        &[1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
        &mut out,
        48000,
        1,
        0.0,
    );
    assert!(
        eq(&out, &[0.25, 0.5, 0.25, 0.0, 0.0, 0.0, 0.0, 0.0]),
        "IIR borrowed constructor changed FIR impulse"
    );
    for (ff, fb) in [(vec![], vec![1.0]), (vec![1.0], vec![]), (vec![], vec![])] {
        let mut n = IIRFilterNode::from_coefficients(9, &ff, &fb, 0);
        let mut out = [0.0; 4];
        n.process(&[0.25, -0.5, 1.0, 0.0], &mut out, 48000, 1, 0.0);
        assert!(
            eq(&out, &[0.25, -0.5, 1.0, 0.0]),
            "IIR borrowed constructor changed native degenerate fallback"
        );
    }
}
fn compatibility() {
    for (bytes, unmanaged) in [(4088, false), (4096, false), (BYTES, true)] {
        let host = migo_io::scheduler::IoScheduler::new(1);
        let cpu = host.audio_cpu();
        let (tx, rx) = shared::audio_channel::channel();
        tx.try_send(AudioCmd::CreateIIRFilter {
            ctx_id: 1,
            node_id: 10,
            feedforward: values(&[0.5], bytes - 8),
            feedback: vec![1.0],
        })
        .unwrap();
        let mut inbox = crate::command_inbox::Inbox::new(rx, vec![], cpu.clone());
        let state = Arc::new(Dropped::default());
        inbox.observe(witness(&state));
        let gate = block(&cpu);
        let mut all = HashMap::from([(1, AudioContext::new(1, 48000, 1))]);
        let mut index = NodeContextIndex::new();
        let delivery = inbox.next().unwrap();
        let delivery = if unmanaged {
            let (c, p) = delivery.into_parts();
            drop(p);
            AudioCommandDelivery::unmanaged(c)
        } else {
            delivery
        };
        let selected = match forward(delivery, &mut all, &mut index, &mut inbox) {
            Some(d) => {
                let (c, p) = d.into_parts();
                legacy(c, &mut all, &mut index);
                drop(p);
                false
            }
            None => true,
        };
        let large = bytes >= 4096 && !unmanaged;
        let held = matches!(
            tx.try_reserve_data(MAX_AUDIO_COMMAND_QUEUED_BYTES),
            Err(AudioCommandReserveError::ByteLimit)
        );
        gate.open();
        assert!(
            selected == large && held == large,
            "IIR small boundary or legacy dispatch changed"
        );
        assert_eq!(index.get_context(10), Some(1));
        if large {
            wait(|| state.count.load(Ordering::Acquire) == 1);
            assert!(
                !state.wrong.load(Ordering::Acquire),
                "IIR input escaped CPU retirement"
            );
        } else {
            assert_eq!(state.count.load(Ordering::Acquire), 0);
        }
    }
    let host = migo_io::scheduler::IoScheduler::new(1);
    let (tx, rx) = shared::audio_channel::channel();
    tx.try_send(AudioCmd::SetNodeParam {
        node_id: 10,
        param_name: "x".repeat(BYTES),
        value: 1.0,
    })
    .unwrap();
    let mut inbox = crate::command_inbox::Inbox::new(rx, vec![], host.audio_cpu());
    assert!(
        forward(
            inbox.next().unwrap(),
            &mut HashMap::new(),
            &mut NodeContextIndex::new(),
            &mut inbox
        )
        .is_some(),
        "IIR dispatcher selected unrelated command"
    );
}
