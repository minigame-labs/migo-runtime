//! Rejected incoming storage must keep its original queue credit until CPU drop.
use super::super::context_retirement_tests::{Dropped, block, context, wait, witness};
use super::*;
use crate::limits::{AudioAggregateLedger, PcmUsage};
use shared::audio_channel::{
    AUDIO_COMMAND_CAPACITY, AudioCommandReserveError, MAX_AUDIO_COMMAND_QUEUED_BYTES,
};
use std::sync::atomic::Ordering;
const BYTES: usize = 4 << 20;
const LIMIT: usize = 32 << 20;
fn curve(capacity: usize, value: f32) -> Option<Vec<f32>> {
    let mut data = Vec::with_capacity(capacity);
    data.resize(2, value);
    Some(data)
}
fn snapshot(frames: usize, value: f32) -> Arc<AudioSnapshot> {
    use shared::audio_resources::{AudioBufferFormat, AudioResourceRegistry};
    let samples = (0..frames)
        .map(|i| value + (i % 19) as f32 * 0.003)
        .collect();
    let registry = AudioResourceRegistry::new();
    let lease = registry
        .adopt_frozen(
            1,
            AudioBufferFormat {
                channels: 1,
                frames: frames as u32,
                sample_rate: 48000,
            },
            samples,
        )
        .unwrap();
    let value = registry
        .prepare_snapshot(lease.key(), None)
        .unwrap()
        .commit();
    assert!(registry.release_buffer(lease.key()));
    value
}
fn graph(ctx: &mut AudioContext) {
    assert!(ctx.add_node(Box::new(WaveShaperNode::new(10))));
    assert!(ctx.create_buffer_source(20));
    assert!(ctx.create_buffer_source(30));
    assert!(ctx.add_node(Box::new(ConstantSourceNode::new(40))));
    assert!(ctx.set_wave_shaper_curve(10, curve(2, 0.125)));
    assert!(ctx.set_started_buffer(20, Some(snapshot(4096, 0.25))));
    assert!(ctx.set_started_buffer(30, Some(snapshot(4096, -0.125))));
    for (a, b) in [
        (40, 10),
        (10, crate::context::DESTINATION_NODE_ID),
        (20, crate::context::DESTINATION_NODE_ID),
        (30, crate::context::DESTINATION_NODE_ID),
    ] {
        ctx.connect(a, b);
    }
    assert!(ctx.start_source(20, 0.0, 0.0, None));
    ctx.with_node_typed::<ConstantSourceNode, _>(40, |n| n.start(0.0));
}
fn index() -> NodeContextIndex {
    let mut index = NodeContextIndex::new();
    for n in [10, 20, 30, 40] {
        index.register(n, 1);
    }
    index
}
fn commands(bytes: usize) -> (Vec<AudioCmd>, Vec<std::sync::Weak<AudioSnapshot>>) {
    let one = snapshot(bytes / 4, 0.5);
    let two = snapshot(bytes / 4, -0.2);
    let weak = vec![Arc::downgrade(&one), Arc::downgrade(&two)];
    (
        vec![
            AudioCmd::SetWaveShaperCurve {
                node_id: 10,
                curve: curve(bytes / 4, 0.75),
            },
            AudioCmd::SetStartedBuffer {
                ctx_id: 1,
                node_id: 20,
                buffer: Some(one),
            },
            AudioCmd::StartBuffer {
                ctx_id: 1,
                node_id: 30,
                buffer: Some(two),
                when: 32.0 / 48000.0,
                offset: 96.0 / 48000.0,
                duration: Some(240.0 / 48000.0),
            },
        ],
        weak,
    )
}
fn legacy(
    command: AudioCmd,
    contexts: &mut HashMap<AudioContextId, AudioContext>,
    index: &NodeContextIndex,
) -> Option<(u32, u32)> {
    match command {
        AudioCmd::SetWaveShaperCurve { node_id, curve } => {
            let ctx = index
                .get_context(node_id)
                .and_then(|id| contexts.get_mut(&id))
                .unwrap();
            ctx.set_wave_shaper_curve(node_id, curve);
        }
        AudioCmd::SetStartedBuffer {
            ctx_id,
            node_id,
            buffer,
        } => {
            set_started_buffer_scoped(contexts, index, ctx_id, node_id, buffer);
        }
        AudioCmd::StartBuffer {
            ctx_id,
            node_id,
            buffer,
            when,
            offset,
            duration,
        } => {
            // Keep the reference independent of the atomic production entry.
            let ctx = contexts.get_mut(&ctx_id).unwrap();
            return (ctx.set_started_buffer(node_id, buffer)
                && ctx.start_source(node_id, when, offset, duration))
            .then_some((ctx_id, node_id));
        }
        _ => unreachable!(),
    }
    None
}
fn matching(actual: &mut AudioContext, expected: &mut AudioContext) -> bool {
    let mut ok = true;
    let mut audible = false;
    for _ in 0..8 {
        let mut a = [0.0_f32; 128];
        let mut b = [0.0_f32; 128];
        let ae = actual
            .process_native_device(&mut a, 48000, 1)
            .unwrap()
            .to_vec();
        let be = expected
            .process_native_device(&mut b, 48000, 1)
            .unwrap()
            .to_vec();
        ok &= ae == be && a.iter().zip(b).all(|(a, b)| a.to_bits() == b.to_bits());
        audible |= b.iter().any(|v| v.abs() > 1e-5);
    }
    ok && audible
}
pub(crate) fn replacement_inputs() {
    for mode in ["accept", "quota", "closed"] {
        let host = migo_io::scheduler::IoScheduler::new(1);
        let cpu = host.audio_cpu();
        let ledger = Arc::new(AudioAggregateLedger::new(LIMIT));
        let usage = Arc::new(PcmUsage::new(16 << 20, 32));
        let process = Arc::new(PcmUsage::new(16 << 20, 32));
        let mut actual = context(&cpu, &ledger, &usage, &process, 1);
        let mut expected = AudioContext::new(1, 48000, 1);
        graph(&mut actual);
        graph(&mut expected);
        let mut contexts = HashMap::from([(1, actual)]);
        let index = index();
        let (tx, rx) = shared::audio_channel::channel();
        let (commands, weak) = commands(BYTES);
        let bytes: usize = commands.iter().map(AudioCmd::queued_payload_bytes).sum();
        assert_eq!(bytes, BYTES);
        for command in commands {
            tx.try_send(command).unwrap();
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
        let filler = (mode == "quota").then(|| {
            ledger
                .try_reserve(LIMIT - ledger.used_bytes(), "replacement input quota test")
                .unwrap()
        });
        let gate = block(&cpu);
        if mode == "closed" {
            host.close();
        }
        let before = migo_alloc_probe::thread_counts();
        let mut handled = 0;
        let mut resumes = Vec::new();
        for _ in 0..3 {
            match forward(inbox.next().unwrap(), &mut contexts, &index, &mut inbox) {
                Forward::Handled(started) => {
                    handled += 1;
                    resumes.push(started);
                }
                Forward::Next(delivery) => drop(delivery),
            }
        }
        let freed = migo_alloc_probe::thread_counts().bytes_freed - before.bytes_freed;
        let alive = weak.iter().all(|w| w.upgrade().is_some());
        let item = matches!(tx.try_reserve_data(0), Err(AudioCommandReserveError::Full));
        drop(fillers.pop());
        let byte = matches!(
            tx.try_reserve_data(1),
            Err(AudioCommandReserveError::ByteLimit)
        );
        let available = tx.try_reserve_data(bytes);
        let returned = available.is_ok();
        drop(available);
        // Snapshot transport byte charge is zero. Count all free item slots,
        // so retaining a consumed snapshot ticket cannot hide behind the
        // curve's returned byte allowance. One zero-byte filler was removed.
        let mut free_items = Vec::new();
        while let Ok(permit) = tx.try_reserve_data(0) {
            free_items.push(permit);
        }
        let free_item_count = free_items.len();
        drop(free_items);
        let expected_resume = if mode == "accept" {
            vec![None, None, Some((1, 30))]
        } else {
            vec![None, None, None]
        };
        let queued = state.count.load(Ordering::Acquire);
        if mode == "accept" {
            let mut refs = HashMap::from([(1, expected)]);
            for cmd in commands_for_reference() {
                legacy(cmd, &mut refs, &index);
            }
            expected = refs.remove(&1).unwrap();
        }
        let effect = matching(contexts.get_mut(&1).unwrap(), &mut expected);
        gate.open();
        assert_eq!(handled, 3, "bulk replacement bypassed owned dispatch");
        assert!(
            freed < (BYTES / 2) as u64 && alive,
            "replacement input escaped CPU retirement"
        );
        assert_eq!(
            resumes, expected_resume,
            "replacement changed start or resume semantics"
        );
        assert!(
            effect,
            "replacement changed complete PCM, offset, duration or old binding"
        );
        if mode == "accept" {
            assert!(
                returned && !item && !byte && queued == 0 && free_item_count == 4,
                "consumed replacement retained or cloned its input ticket"
            );
        } else {
            wait(|| state.count.load(Ordering::Acquire) == 3);
            assert!(
                !state.wrong.load(Ordering::Acquire),
                "replacement input escaped CPU retirement"
            );
            assert!(
                item && byte && !returned && free_item_count == 1,
                "replacement credit returned before CPU drop"
            );
            wait(|| weak.iter().all(|w| w.upgrade().is_none()));
            let returned = tx.try_reserve_data(bytes).unwrap();
            drop(returned);
        }
        drop(filler);
        drop(fillers);
        contexts.clear();
        drop(inbox);
        drop(host);
        wait(|| {
            ledger.used_bytes() == 0 && usage.snapshot().bytes == 0 && process.snapshot().bytes == 0
        });
        wait(|| weak.iter().all(|w| w.upgrade().is_none()));
    }
    compatibility();
}
fn commands_for_reference() -> Vec<AudioCmd> {
    commands(BYTES).0
}
fn compatibility() {
    for (bytes, unmanaged) in [(4092, false), (4096, false), (BYTES, true)] {
        let host = migo_io::scheduler::IoScheduler::new(1);
        let cpu = host.audio_cpu();
        let ledger = Arc::new(AudioAggregateLedger::new(LIMIT));
        let usage = Arc::new(PcmUsage::new(16 << 20, 32));
        let process = Arc::new(PcmUsage::new(16 << 20, 32));
        let mut actual = context(&cpu, &ledger, &usage, &process, 1);
        let mut expected = AudioContext::new(1, 48000, 1);
        graph(&mut actual);
        graph(&mut expected);
        let mut contexts = HashMap::from([(1, actual)]);
        let index = index();
        let (tx, rx) = shared::audio_channel::channel();
        let (commands, weak) = commands(bytes);
        for cmd in commands {
            tx.try_send(cmd).unwrap();
        }
        let mut inbox = crate::command_inbox::Inbox::new(rx, vec![], cpu.clone());
        let state = Arc::new(Dropped::default());
        inbox.observe(witness(&state));
        let fill = ledger
            .try_reserve(
                LIMIT - ledger.used_bytes(),
                "replacement compatibility quota",
            )
            .unwrap();
        let gate = block(&cpu);
        let mut handled = 0;
        for _ in 0..3 {
            let delivery = inbox.next().unwrap();
            let delivery = if unmanaged {
                let (cmd, permit) = delivery.into_parts();
                drop(permit);
                AudioCommandDelivery::unmanaged(cmd)
            } else {
                delivery
            };
            match forward(delivery, &mut contexts, &index, &mut inbox) {
                Forward::Next(d) => {
                    let (cmd, permit) = d.into_parts();
                    legacy(cmd, &mut contexts, &index);
                    drop(permit);
                }
                Forward::Handled(_) => handled += 1,
            }
        }
        let large = bytes >= 4096 && !unmanaged;
        let held = matches!(
            tx.try_reserve_data(MAX_AUDIO_COMMAND_QUEUED_BYTES),
            Err(AudioCommandReserveError::ByteLimit)
        );
        let alive = weak.iter().all(|w| w.upgrade().is_some());
        let queued = state.count.load(Ordering::Acquire);
        gate.open();
        assert_eq!(
            handled,
            if large { 3 } else { 0 },
            "replacement threshold or legacy dispatch changed"
        );
        assert!(
            held == large && alive == large && queued == 0,
            "small or legacy replacement unnecessarily retained input"
        );
        if large {
            wait(|| state.count.load(Ordering::Acquire) == 3);
            assert!(
                !state.wrong.load(Ordering::Acquire),
                "replacement input escaped CPU retirement"
            );
        }
        assert!(
            matching(contexts.get_mut(&1).unwrap(), &mut expected),
            "replacement changed complete PCM, offset, duration or old binding"
        );
        drop(fill);
        contexts.clear();
        drop(inbox);
        drop(host);
        wait(|| ledger.used_bytes() == 0);
    }
    let host = migo_io::scheduler::IoScheduler::new(1);
    let (tx, rx) = shared::audio_channel::channel();
    let mut inbox = crate::command_inbox::Inbox::new(rx, vec![], host.audio_cpu());
    tx.try_send(AudioCmd::SetNodeParam {
        node_id: 10,
        param_name: "x".repeat(BYTES),
        value: 0.5,
    })
    .unwrap();
    let other = forward(
        inbox.next().unwrap(),
        &mut HashMap::new(),
        &NodeContextIndex::new(),
        &mut inbox,
    );
    assert!(
        matches!(other, Forward::Next(_)),
        "replacement dispatcher consumed unrelated command"
    );
}
