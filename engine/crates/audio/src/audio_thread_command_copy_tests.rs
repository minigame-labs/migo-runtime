//! Successful and refused copies must answer before the CPU disposes input.
use super::super::context_retirement_tests::{Dropped, block, context, wait, witness};
use super::*;
use crate::limits::{AudioAggregateLedger, PcmUsage};
use shared::audio_channel::{
    AUDIO_COMMAND_CAPACITY, AudioCommandReserveError, MAX_AUDIO_COMMAND_QUEUED_BYTES,
};
use std::sync::atomic::Ordering;

const INPUT_BYTES: usize = 4 << 20;
const FRAMES: u32 = (INPUT_BYTES / 4) as u32;
const PCM_BYTES: usize = INPUT_BYTES * 2;
const LIMIT: usize = 64 << 20;
fn sample(index: usize) -> f32 {
    match index % 257 {
        0 => f32::from_bits(0x7fc0_0123),
        1 => -0.0,
        _ => (index % 1024) as f32 / 512.0 - 1.0,
    }
}

pub(crate) fn completed_inputs() {
    for mode in [
        "unique",
        "cow",
        "bad-buffer",
        "bad-channel",
        "past-end",
        "quota-ledger",
        "quota-pcm",
        "closed-response",
        "spare-capacity",
        "empty-capacity",
        "closed-host",
    ] {
        let host = migo_io::scheduler::IoScheduler::new(1);
        let cpu = host.audio_cpu();
        let ledger = Arc::new(AudioAggregateLedger::new(LIMIT));
        let usage = Arc::new(PcmUsage::new(
            if mode == "quota-pcm" {
                PCM_BYTES
            } else {
                32 << 20
            },
            16,
        ));
        let process = Arc::new(PcmUsage::new(32 << 20, 16));
        let mut ctx = context(&cpu, &ledger, &usage, &process, 2);
        let id = ctx.create_empty_buffer(2, FRAMES, 48000).unwrap();
        let cow = matches!(mode, "cow" | "quota-ledger" | "quota-pcm");
        let frozen = cow.then(|| ctx.get_buffer(id).unwrap());
        let mut contexts = HashMap::from([(1, ctx)]);
        let quota = (mode == "quota-ledger").then(|| {
            ledger
                .try_reserve(LIMIT - ledger.used_bytes(), "copy rejection fixture")
                .unwrap()
        });
        let mut data = Vec::with_capacity(INPUT_BYTES / 4);
        let length = match mode {
            "spare-capacity" => 3,
            "empty-capacity" => 0,
            _ => INPUT_BYTES / 4,
        };
        data.extend((0..length).map(sample));
        assert_eq!(data.capacity() * std::mem::size_of::<f32>(), INPUT_BYTES);
        let buffer_id = if mode == "bad-buffer" { u32::MAX } else { id };
        let channel = if mode == "bad-channel" { 2 } else { 1 };
        let start = if mode == "past-end" { u32::MAX } else { 7 };
        let (resp, answer) = tokio::sync::oneshot::channel();
        let mut answer = Some(answer);
        if mode == "closed-response" {
            drop(answer.take());
        }
        let (tx, rx) = shared::audio_channel::channel();
        tx.try_send(AudioCmd::CopyToChannel {
            ctx_id: 1,
            buffer_id,
            data,
            channel,
            start,
            resp,
        })
        .unwrap();
        let mut fillers = vec![
            tx.try_reserve_data(MAX_AUDIO_COMMAND_QUEUED_BYTES - INPUT_BYTES)
                .unwrap(),
        ];
        for _ in 2..AUDIO_COMMAND_CAPACITY {
            fillers.push(tx.try_reserve_data(0).unwrap());
        }
        let mut inbox = crate::command_inbox::Inbox::new(rx, vec![], cpu.clone());
        let state = Arc::new(Dropped::default());
        inbox.observe(witness(&state));
        let gate = block(&cpu);
        if mode == "closed-host" {
            host.close();
        }
        let delivery = inbox.next().unwrap();
        let before = migo_alloc_probe::thread_counts();
        let handled = forward(delivery, &mut contexts, &mut inbox).is_none();
        let freed = migo_alloc_probe::thread_counts().bytes_freed - before.bytes_freed;
        let credit = matches!(tx.try_reserve_data(0), Err(AudioCommandReserveError::Full));
        drop(fillers.pop());
        let bytes_held = matches!(
            tx.try_reserve_data(1),
            Err(AudioCommandReserveError::ByteLimit)
        );
        let reply = answer
            .as_mut()
            .map(|a| a.try_recv().map(|v| v.map_err(|e| e.code)));
        let expected = match mode {
            "bad-buffer" | "bad-channel" => Err(ErrorCode::NotFound),
            "quota-ledger" | "quota-pcm" => Err(ErrorCode::InputSaturated),
            _ => Ok(()),
        };
        let mut unchanged = true;
        if let Some(old) = &frozen {
            unchanged = old.samples.iter().all(|v| v.to_bits() == 0.0_f32.to_bits());
        }
        let current = contexts[&1].get_buffer(id).unwrap();
        let mut correct = current.samples.iter().enumerate().all(|(i, v)| {
            let frame = i / 2;
            let ch = i % 2;
            let expected = if expected.is_ok()
                && ch == channel as usize
                && frame >= start as usize
                && frame - (start as usize) < length
            {
                sample(frame - start as usize)
            } else {
                0.0
            };
            v.to_bits() == expected.to_bits()
        });
        if expected.is_err() && cow {
            correct &= Arc::ptr_eq(&current, frozen.as_ref().unwrap());
        }
        drop(current);
        gate.open();
        assert!(handled, "bulk copy fell through caller cleanup");
        assert!(
            freed < (INPUT_BYTES / 2) as u64,
            "completed copy input escaped CPU retirement"
        );
        assert!(
            credit && bytes_held,
            "completed copy returned original credit before CPU drop"
        );
        if mode != "closed-response" {
            assert_eq!(
                reply,
                Some(Ok(expected)),
                "copy response changed or waited for CPU retirement"
            );
        }
        assert!(
            correct && unchanged,
            "copy completion changed complete PCM or frozen COW owner"
        );
        wait(|| state.count.load(Ordering::Acquire) == 1);
        assert!(
            !state.wrong.load(Ordering::Acquire),
            "completed copy input escaped CPU retirement"
        );
        let returned = tx.try_reserve_data(INPUT_BYTES).unwrap();
        drop((returned, fillers, quota, frozen));
        contexts.clear();
        drop(inbox);
        drop(host);
        wait(|| {
            ledger.used_bytes() == 0 && usage.snapshot().bytes == 0 && process.snapshot().bytes == 0
        });
    }
    fallthrough();
}

fn fallthrough() {
    let host = migo_io::scheduler::IoScheduler::new(1);
    let cpu = host.audio_cpu();
    let mut ctx = AudioContext::new(1, 48000, 1);
    let id = ctx.create_empty_buffer(1, 4, 48000).unwrap();
    let mut contexts = HashMap::from([(1, ctx)]);
    let (tx, rx) = shared::audio_channel::channel();
    let mut inbox = crate::command_inbox::Inbox::new(rx, vec![], cpu.clone());
    let gate = block(&cpu);
    let state = Arc::new(Dropped::default());
    inbox.observe(witness(&state));
    for unmanaged in [false, true] {
        let data = if unmanaged {
            vec![0.5; INPUT_BYTES / 4]
        } else {
            vec![0.5; 4]
        };
        let (resp, mut answer) = tokio::sync::oneshot::channel();
        tx.try_send(AudioCmd::CopyToChannel {
            ctx_id: 1,
            buffer_id: id,
            data,
            channel: 0,
            start: 0,
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
        if let Some(delivery) = forward(delivery, &mut contexts, &mut inbox) {
            assert!(matches!(
                answer.try_recv(),
                Err(tokio::sync::oneshot::error::TryRecvError::Empty)
            ));
            let (command, permit) = delivery.into_parts();
            let AudioCmd::CopyToChannel {
                ctx_id,
                buffer_id,
                data,
                channel,
                start,
                resp,
            } = command
            else {
                unreachable!()
            };
            let _ = resp.send(copy(
                &mut contexts,
                ctx_id,
                buffer_id,
                &data,
                channel,
                start,
            ));
            drop((data, permit));
        }
        assert!(answer.try_recv().unwrap().is_ok());
        let available = tx.try_reserve_data(MAX_AUDIO_COMMAND_QUEUED_BYTES);
        let inline = available.is_ok();
        drop(available);
        assert!(inline, "small copy unnecessarily handed to CPU");
        assert_eq!(
            state.count.load(Ordering::Acquire),
            0,
            "small copy unnecessarily handed to CPU"
        );
    }
    gate.open();
    assert!(
        contexts[&1]
            .get_channel_data(id, 0)
            .unwrap()
            .iter()
            .all(|v| *v == 0.5)
    );
}
