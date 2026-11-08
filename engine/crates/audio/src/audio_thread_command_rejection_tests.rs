//! Real rejected deliveries answer immediately while original tickets and bulk
//! payloads wait for the existing CPU retirement lane, including a closed host.
use super::super::context_retirement_tests::{Dropped, block, wait, witness};
use super::*;
use shared::audio_channel::{
    AUDIO_COMMAND_CAPACITY, AudioCommandReserveError, MAX_AUDIO_COMMAND_QUEUED_BYTES,
};
use shared::audio_resources::{AudioBufferFormat, AudioResourceRegistry};
use std::sync::atomic::Ordering;

pub(crate) fn rejected_payloads() {
    for closed in [false, true] {
        let host = migo_io::scheduler::IoScheduler::new(1);
        let cpu = host.audio_cpu();
        let (tx, rx) = shared::audio_channel::channel();
        let bytes = 4 << 20;
        let mut ctx = AudioContext::new(1, 48000, 1);
        assert!(ctx.create_gain(10));
        assert!(ctx.create_buffer_source(11));
        ctx.collect_device_controls();
        let mut contexts = HashMap::from([(1, ctx)]);
        let mut other = AudioContext::new(2, 48000, 1);
        assert!(other.create_buffer_source(11));
        other.collect_device_controls();
        contexts.insert(2, other);
        let mut index = NodeContextIndex::new();
        index.register(10, 1);
        index.register(11, 1);
        let mut player = InnerAudioPlayer::new(1, 1);
        let old = player.begin_load().unwrap();
        let old_url = player.begin_load().unwrap();
        let current = player.begin_load().unwrap();
        let players = HashMap::from([(1, player)]);
        let data = Arc::new(vec![0_u8; bytes]);
        let weak = Arc::downgrade(&data);
        let (decode, mut decode_rx) = tokio::sync::oneshot::channel();
        let (copy, mut copy_rx) = tokio::sync::oneshot::channel();
        let (frequency, mut frequency_rx) = tokio::sync::oneshot::channel();
        let (load, mut load_rx) = tokio::sync::oneshot::channel();
        let (complete, mut complete_rx) = tokio::sync::oneshot::channel();
        let (url, mut url_rx) = tokio::sync::oneshot::channel();
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
        let commands = vec![
            AudioCmd::DecodeAudioData {
                ctx_id: 99,
                data,
                resp: decode,
            },
            AudioCmd::CopyToChannel {
                ctx_id: 99,
                buffer_id: 1,
                data: vec![0.0; bytes / 4],
                channel: 0,
                start: 0,
                resp: copy,
            },
            AudioCmd::GetFrequencyResponse {
                node_id: 10,
                frequencies: vec![0.0; bytes / 4],
                resp: frequency,
            },
            AudioCmd::SetWaveShaperCurve {
                node_id: 10,
                curve: Some(vec![0.5; bytes / 4]),
            },
            AudioCmd::CreateIIRFilter {
                ctx_id: 99,
                node_id: 30,
                feedforward: vec![0.5; bytes / 16],
                feedback: vec![0.5; bytes / 16],
            },
            AudioCmd::InnerAudioLoad {
                id: 99,
                data: vec![0; bytes],
                resp: load,
            },
            AudioCmd::InnerAudioCompleteLoad {
                id: 1,
                load: old,
                data: vec![0; bytes],
                resp: complete,
            },
            AudioCmd::InnerAudioLoadUrl {
                id: 1,
                load: old_url,
                url: "x".repeat(bytes),
                spool: None,
                resp: url,
            },
            AudioCmd::SetStartedBuffer {
                ctx_id: 1,
                node_id: 10,
                buffer: Some(snapshot.clone()),
            },
            AudioCmd::StartBuffer {
                ctx_id: 2,
                node_id: 11,
                buffer: Some(snapshot),
                when: 0.0,
                offset: 0.0,
                duration: None,
            },
            AudioCmd::SetNodeParam {
                node_id: 99,
                param_name: "x".repeat(bytes),
                value: 1.0,
            },
        ];
        let count = commands.len();
        let charged: usize = commands.iter().map(AudioCmd::queued_payload_bytes).sum();
        for command in commands {
            tx.try_send(command).unwrap();
        }
        let mut fillers = vec![
            tx.try_reserve_data(MAX_AUDIO_COMMAND_QUEUED_BYTES - charged)
                .unwrap(),
        ];
        for _ in count + 1..AUDIO_COMMAND_CAPACITY {
            fillers.push(tx.try_reserve_data(0).unwrap());
        }
        let mut inbox = crate::command_inbox::Inbox::new(rx, vec![], cpu.clone());
        let state = Arc::new(Dropped::default());
        inbox.observe(witness(&state));
        let gate = block(&cpu);
        if closed {
            host.close();
        }
        let before = migo_alloc_probe::thread_counts();
        let mut rejected = 0;
        for _ in 0..count {
            let command = inbox.next().unwrap();
            if forward(command, &mut contexts, &index, &players, &mut inbox).is_none() {
                rejected += 1;
            }
        }
        let freed = migo_alloc_probe::thread_counts().bytes_freed - before.bytes_freed;
        let held = weak.upgrade().is_some() && snapshot_weak.upgrade().is_some();
        let credit = matches!(tx.try_reserve_data(0), Err(AudioCommandReserveError::Full));
        drop(fillers.pop()); // Open one item slot so only byte credit can refuse.
        let byte_credit = matches!(
            tx.try_reserve_data(1),
            Err(AudioCommandReserveError::ByteLimit)
        );
        let answers = [
            decode_rx.try_recv().map(|r| r.err().unwrap().code),
            copy_rx.try_recv().map(|r| r.err().unwrap().code),
            frequency_rx.try_recv().map(|r| r.err().unwrap().code),
            load_rx.try_recv().map(|r| r.err().unwrap().code),
            complete_rx.try_recv().map(|r| r.err().unwrap().code),
            url_rx.try_recv().map(|r| r.err().unwrap().code),
        ];
        gate.open();
        assert_eq!(rejected, count, "invalid command escaped owned rejection");
        assert!(
            freed < bytes as u64 / 2 && held,
            "rejected command payload escaped CPU retirement"
        );
        assert!(
            credit && byte_credit,
            "rejected command returned original credit before CPU drop"
        );
        assert_eq!(
            answers,
            [
                Ok(ErrorCode::NotFound),
                Ok(ErrorCode::NotFound),
                Ok(ErrorCode::NotFound),
                Ok(ErrorCode::NotFound),
                Ok(ErrorCode::Cancelled),
                Ok(ErrorCode::Cancelled)
            ],
            "rejected command lost original response semantics"
        );
        assert!(
            players[&1].shared.owns_load(&current),
            "rejected command changed current load"
        );
        assert_eq!(index.get_context(11), Some(1));
        assert!(
            !contexts[&1].has_device_controls(),
            "command target inspection changed playback controls"
        );
        wait(|| {
            state.count.load(Ordering::Acquire) == count
                && weak.upgrade().is_none()
                && snapshot_weak.upgrade().is_none()
        });
        assert!(
            !state.wrong.load(Ordering::Acquire),
            "rejected command payload escaped CPU retirement"
        );
        let returned = tx.try_reserve_data(charged).unwrap();
        drop((returned, fillers));
        // Forwarding an executable command keeps its actual original credit.
        let (resp, answer) = tokio::sync::oneshot::channel();
        tx.try_send(AudioCmd::DecodeAudioData {
            ctx_id: 1,
            data: Arc::new(vec![0; bytes]),
            resp,
        })
        .unwrap();
        let delivery = inbox.next().unwrap();
        let forwarded = forward(delivery, &mut contexts, &index, &players, &mut inbox);
        assert!(
            forwarded.is_some(),
            "valid command rejected before dispatch"
        );
        assert!(
            matches!(
                tx.try_reserve_data(MAX_AUDIO_COMMAND_QUEUED_BYTES),
                Err(AudioCommandReserveError::ByteLimit)
            ),
            "forwarded command lost original credit"
        );
        drop((forwarded, answer));
        for unmanaged in [false, true] {
            let (resp, mut answer) = tokio::sync::oneshot::channel();
            tx.try_send(AudioCmd::DecodeAudioData {
                ctx_id: 99,
                data: Arc::new(vec![0; 16]),
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
            assert!(forward(delivery, &mut contexts, &index, &players, &mut inbox).is_none());
            assert_eq!(
                answer.try_recv().unwrap().err().unwrap().code,
                ErrorCode::NotFound,
                "small rejected command lost original response semantics"
            );
            assert_eq!(
                state.count.load(Ordering::Acquire),
                count,
                "small payload unnecessarily handed to CPU"
            );
        }

        tx.try_send(AudioCmd::SetStartedBuffer {
            ctx_id: 1,
            node_id: 11,
            buffer: None,
        })
        .unwrap();
        let delivery = inbox.next().unwrap();
        assert!(
            forward(delivery, &mut contexts, &index, &players, &mut inbox).is_some(),
            "valid snapshot rejected before dispatch"
        );
        assert!(
            !contexts[&1].has_device_controls(),
            "command target inspection changed playback controls"
        );
    }
}
