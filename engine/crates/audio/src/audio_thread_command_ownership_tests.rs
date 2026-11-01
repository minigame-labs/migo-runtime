//! Real cancelled commands keep original queue credit through CPU destruction.
use super::context_retirement_tests::{Dropped, block, wait, witness};
use super::*;
use shared::audio_channel::{AudioCommandReserveError, MAX_AUDIO_COMMAND_QUEUED_BYTES};
use std::sync::atomic::Ordering;

pub(super) fn owned_fifo() {
    let host = migo_io::scheduler::IoScheduler::new(1);
    let (tx, rx) = shared::audio_channel::channel();
    tx.try_send(AudioCmd::CreateContext {
        ctx_id: 1,
        sample_rate: None,
    })
    .unwrap();
    let backlog = vec![rx.try_recv_owned().unwrap()];
    tx.try_send(AudioCmd::CreateContext {
        ctx_id: 2,
        sample_rate: None,
    })
    .unwrap();
    let mut inbox = crate::command_inbox::Inbox::new(rx, backlog, host.audio_cpu());
    let fill = tx.try_reserve_data(MAX_AUDIO_COMMAND_QUEUED_BYTES).unwrap();
    let one = inbox.next().unwrap();
    let (command, credit) = one.into_parts();
    assert!(
        matches!(command, AudioCmd::CreateContext { ctx_id: 1, .. }),
        "owned startup FIFO reordered"
    );
    // Keep two data deliveries plus the filler; capacity itself is unchanged.
    let mut other = Vec::new();
    for _ in 3..shared::audio_channel::AUDIO_COMMAND_CAPACITY {
        other.push(tx.try_reserve_data(0).unwrap());
    }
    assert!(
        matches!(tx.try_reserve_data(0), Err(AudioCommandReserveError::Full)),
        "owned dequeue returned item credit before processing"
    );
    drop(command);
    drop(credit);
    let returned = tx.try_reserve_data(0).unwrap();
    assert!(
        matches!(
            inbox.next().unwrap().command(),
            AudioCmd::CreateContext { ctx_id: 2, .. }
        ),
        "owned startup FIFO reordered"
    );
    drop((returned, fill, other));
}

pub(super) fn restart_and_shutdown() {
    for shutdown in [false, true] {
        let host = migo_io::scheduler::IoScheduler::new(1);
        let cpu = host.audio_cpu();
        let (tx, rx) = shared::audio_channel::channel();
        let bytes = 4 << 20;
        let data = Arc::new(vec![0_u8; bytes]);
        let weak = Arc::downgrade(&data);
        let (decode, mut decode_answer) = tokio::sync::oneshot::channel();
        tx.try_send(AudioCmd::DecodeAudioData {
            ctx_id: 1,
            data,
            resp: decode,
        })
        .unwrap();
        let backlog = vec![rx.try_recv_owned().unwrap()];
        let (copy, mut copy_answer) = tokio::sync::oneshot::channel();
        tx.try_send(AudioCmd::CopyToChannel {
            ctx_id: 1,
            buffer_id: 1,
            data: vec![0.5; bytes / 4],
            channel: 0,
            start: 0,
            resp: copy,
        })
        .unwrap();
        tx.try_send(AudioCmd::SetWaveShaperCurve {
            node_id: 1,
            curve: Some(vec![0.5; bytes / 4]),
        })
        .unwrap();
        tx.try_send(AudioCmd::CreateIIRFilter {
            ctx_id: 1,
            node_id: 1,
            feedforward: vec![0.5; bytes / 16],
            feedback: vec![0.5; bytes / 16],
        })
        .unwrap();
        tx.try_send(AudioCmd::SetNodeParam {
            node_id: 1,
            param_name: "x".repeat(bytes),
            value: 1.0,
        })
        .unwrap();
        let (load, mut load_answer) = tokio::sync::oneshot::channel();
        tx.try_send(AudioCmd::InnerAudioLoad {
            id: 1,
            data: vec![0; bytes],
            resp: load,
        })
        .unwrap();
        let (frequency, mut frequency_answer) = tokio::sync::oneshot::channel();
        tx.try_send(AudioCmd::GetFrequencyResponse {
            node_id: 1,
            frequencies: vec![0.0; bytes / 4],
            resp: frequency,
        })
        .unwrap();
        use shared::audio_resources::{AudioBufferFormat, AudioResourceRegistry};
        let registry = AudioResourceRegistry::new();
        let key = registry
            .adopt_frozen(
                1,
                AudioBufferFormat {
                    channels: 1,
                    frames: (bytes / 4) as u32,
                    sample_rate: 48000,
                },
                vec![0.0; bytes / 4],
            )
            .unwrap()
            .key();
        let snapshot = registry.prepare_snapshot(key, None).unwrap().commit();
        let snapshot_weak = Arc::downgrade(&snapshot);
        registry.release_buffer(key);
        tx.try_send(AudioCmd::SetStartedBuffer {
            ctx_id: 1,
            node_id: 1,
            buffer: Some(snapshot),
        })
        .unwrap();
        let filler = tx
            .try_reserve_data(MAX_AUDIO_COMMAND_QUEUED_BYTES - bytes * 7)
            .unwrap();
        let state = Arc::new(Dropped::default());
        let mut inbox = crate::command_inbox::Inbox::new(rx, backlog, cpu.clone());
        inbox.observe(witness(&state));
        let gate = block(&cpu);
        let before = migo_alloc_probe::thread_counts();
        let mut original_credit_held = true;
        if shutdown {
            host.close();
            drop(inbox);
        } else {
            let ticket = tx.request_release_all_contexts().unwrap();
            assert!(matches!(
                inbox.next().unwrap().command(),
                AudioCmd::ReleaseAllContexts
            ));
            handle_owned_release_all_contexts(
                &mut inbox,
                &mut HashMap::new(),
                &mut NodeContextIndex::new(),
            );
            assert!(
                ticket.is_complete(),
                "owned cleanup acknowledgement was lost"
            );
            tx.finish_release_all_contexts();
            original_credit_held = matches!(
                tx.try_reserve_data(1),
                Err(AudioCommandReserveError::ByteLimit)
            );
            drop(inbox);
        }
        let freed = migo_alloc_probe::thread_counts().bytes_freed - before.bytes_freed;
        let held = weak.upgrade().is_some() && snapshot_weak.upgrade().is_some();
        let cancelled = matches!(
            decode_answer.try_recv(),
            Err(tokio::sync::oneshot::error::TryRecvError::Closed)
        ) && matches!(
            copy_answer.try_recv(),
            Err(tokio::sync::oneshot::error::TryRecvError::Closed)
        ) && matches!(
            load_answer.try_recv(),
            Err(tokio::sync::oneshot::error::TryRecvError::Closed)
        ) && matches!(
            frequency_answer.try_recv(),
            Err(tokio::sync::oneshot::error::TryRecvError::Closed)
        );
        gate.open();
        assert!(
            freed < bytes as u64 / 2 && held,
            "cancelled command payload escaped CPU retirement"
        );
        assert!(
            cancelled,
            "cancelled response stayed alive until payload retirement"
        );
        wait(|| {
            weak.upgrade().is_none()
                && snapshot_weak.upgrade().is_none()
                && state.count.load(Ordering::Acquire) == 8
        });
        assert!(
            !state.wrong.load(Ordering::Acquire),
            "cancelled command payload escaped CPU retirement"
        );
        assert!(
            original_credit_held,
            "restart returned payload credit before CPU destruction"
        );
        drop(filler);
    }
}
