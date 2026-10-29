//! A seek may leave every old message kind in flight, including EOF and error.
use super::*;
use crate::streaming::{PcmPool, stream_channel};
use tokio::sync::mpsc;

fn pcm(pool: &mut PcmPool, revision: StreamRevision, samples: &[f32]) -> StreamMsg {
    let mut pcm = pool.take();
    pcm.buffer_mut().extend_from_slice(samples);
    StreamMsg::Samples { revision, pcm }
}

fn stream(
    reusable: bool,
) -> (
    InnerAudioPlayer,
    mpsc::Sender<StreamMsg>,
    PcmPool,
    Arc<StreamingState>,
) {
    let (tx, rx) = stream_channel();
    let state = if reusable {
        StreamingState::new_reusable()
    } else {
        StreamingState::new()
    };
    let mut player = InnerAudioPlayer::new(12, 2);
    let load = player.begin_load_for(83).unwrap();
    player
        .complete_stream_load(load, "revision".into(), rx, state.clone())
        .unwrap();
    assert!(
        tx.try_send(StreamMsg::Ready {
            revision: StreamRevision::INITIAL,
            sample_rate: 8000,
            channels: 2
        })
        .is_ok()
    );
    let mut pool = PcmPool::new();
    assert!(
        tx.try_send(pcm(&mut pool, StreamRevision::INITIAL, &vec![0.25; 8000]))
            .is_ok()
    );
    player.poll_stream();
    assert!(player.shared.is_loaded());
    player.drain_events().for_each(drop);
    (player, tx, pool, state)
}

#[test]
fn every_old_message_and_unaccepted_future_message_is_discarded() {
    let (mut player, tx, mut pool, state) = stream(true);
    let next = StreamRevision::INITIAL.checked_next().unwrap();
    player.accept_stream_revision(&state, next, 100).unwrap();
    player.drain_events().for_each(drop);
    // The four-slot work budget includes discarded messages. A stale format
    // must not change the channel count before interpreting current PCM.
    for revision in [StreamRevision::INITIAL, next.checked_next().unwrap()] {
        assert!(
            tx.try_send(StreamMsg::Ready {
                revision,
                sample_rate: 1,
                channels: 7
            })
            .is_ok()
        );
        assert!(tx.try_send(pcm(&mut pool, revision, &[99.0, 98.0])).is_ok());
        assert!(tx.try_send(StreamMsg::Done { revision }).is_ok());
        assert!(
            tx.try_send(StreamMsg::Error {
                revision,
                message: "retired".into()
            })
            .is_ok()
        );
        player.poll_stream();
        assert!(player.source.is_empty());
        assert_eq!(player.shared.sample_rate(), 8000);
        assert_eq!(player.shared.channels(), 2);
        assert!(!player.stream_complete);
        assert_eq!(player.pending_seek, Some(100));
        assert!(player.drain_events().next().is_none());
        assert!(!state.is_cancelled());
    }
    assert!(tx.try_send(pcm(&mut pool, next, &[3.0, 4.0])).is_ok());
    player.poll_stream();
    assert_eq!(player.source.samples(), &[3.0, 4.0]);
    let events: Vec<_> = player.drain_events().collect();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].event_type, InnerAudioEventType::Seeked);
}

#[test]
fn accepted_reposition_reuses_storage_and_waits_for_fresh_pcm_in_every_play_state() {
    for playback in [
        PlaybackState::Playing,
        PlaybackState::Paused,
        PlaybackState::Stopped,
    ] {
        let (mut player, tx, mut pool, state) = stream(true);
        player.shared.set_state(playback);
        player.position_phase = 500;
        player.frames_since_time_update = 99;
        player.waiting_notified = true;
        let pointer = player.source.samples().as_ptr();
        let bytes = player.physical_pcm_bytes();
        let next = StreamRevision::INITIAL.checked_next().unwrap();
        player.accept_stream_revision(&state, next, 2000).unwrap();
        assert_eq!(player.source.samples().as_ptr(), pointer);
        assert_eq!(player.physical_pcm_bytes(), bytes);
        assert!(Arc::ptr_eq(player.stream_state.as_ref().unwrap(), &state));
        assert_eq!(player.shared.state(), playback);
        assert_eq!(player.shared.position_frames(), 2000);
        assert_eq!(player.position_phase, 0);
        assert_eq!(player.frames_since_time_update, 0);
        assert!(!player.waiting_notified);
        let events: Vec<_> = player.drain_events().collect();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, InnerAudioEventType::Seeking);
        assert_eq!(events[0].current_time, 0.0);
        assert_eq!(events[0].source_id, 83);
        assert_eq!(player.pending_seek, Some(2000));
        // Empty PCM cannot complete a seek.
        assert!(tx.try_send(pcm(&mut pool, next, &[])).is_ok());
        player.poll_stream();
        assert_eq!(player.pending_seek, Some(2000));
        assert!(player.drain_events().next().is_none());
        assert!(
            tx.try_send(pcm(&mut pool, next, &[0.5, -0.5, 0.25, -0.25]))
                .is_ok()
        );
        player.poll_stream();
        assert_eq!(player.shared.state(), playback);
        let events: Vec<_> = player.drain_events().collect();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, InnerAudioEventType::Seeked);
        assert_eq!(events[0].current_time, 0.25);
        assert_eq!(events[0].paused, playback != PlaybackState::Playing);
        player.shared.set_state(PlaybackState::Playing);
        let mut mix = [0.0; 2];
        assert!(player.process(&mut mix));
        assert_eq!(mix, [0.5, -0.5]);
    }
}

#[test]
fn duplicate_or_reordered_acknowledgements_preserve_the_active_traversal() {
    let (mut player, tx, mut pool, state) = stream(true);
    let first = StreamRevision::INITIAL.checked_next().unwrap();
    let latest = first.checked_next().unwrap();
    // Coalescing may skip a revision, but it may never move backwards.
    player.accept_stream_revision(&state, latest, 20).unwrap();
    player.drain_events().for_each(drop);
    for rejected in [StreamRevision::INITIAL, first, latest] {
        assert!(
            player
                .accept_stream_revision(&state, rejected, 999)
                .is_err()
        );
        assert_eq!(player.shared.position_frames(), 20);
        assert_eq!(player.stream_revision, latest);
        assert_eq!(player.pending_seek, Some(20));
        assert!(player.drain_events().next().is_none());
    }
    assert!(tx.try_send(pcm(&mut pool, latest, &[7.0, 8.0])).is_ok());
    player.poll_stream();
    assert_eq!(player.source.samples(), &[7.0, 8.0]);
}

#[test]
fn reusable_eof_keeps_ownership_without_polling_or_cache_handoff_and_can_resume() {
    let (mut player, tx, mut pool, state) = stream(true);
    assert!(
        tx.try_send(StreamMsg::Done {
            revision: StreamRevision::INITIAL
        })
        .is_ok()
    );
    player.poll_stream();
    assert!(player.stream_complete);
    assert!(player.is_streaming());
    assert!(!player.needs_stream_poll());
    assert!(Arc::ptr_eq(player.stream_state.as_ref().unwrap(), &state));
    let pointer = player.source.samples().as_ptr();
    let bytes = player.physical_pcm_bytes();
    assert!(player.take_streamed_audio().is_none());
    assert_eq!(player.source.samples().as_ptr(), pointer);
    assert_eq!(player.physical_pcm_bytes(), bytes);
    // EOF is known even when a command-capable receiver remains attached.
    // A buffered end seek must not wait for PCM from an already finished pass.
    player.seek(f64::MAX);
    assert_eq!(player.pending_seek, None);
    assert_eq!(player.shared.position_frames(), 4000);
    assert_eq!(player.drain_events().count(), 2);
    let next = StreamRevision::INITIAL.checked_next().unwrap();
    player.accept_stream_revision(&state, next, 0).unwrap();
    assert!(!player.stream_complete);
    assert!(player.needs_stream_poll());
    player.drain_events().for_each(drop);
    assert!(tx.try_send(pcm(&mut pool, next, &[1.0, 2.0])).is_ok());
    assert!(tx.try_send(StreamMsg::Done { revision: next }).is_ok());
    player.poll_stream();
    assert!(player.stream_complete);
    assert_eq!(player.source.samples(), &[1.0, 2.0]);
    let events: Vec<_> = player.drain_events().collect();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].event_type, InnerAudioEventType::Seeked);
    assert!(!state.is_cancelled());
    drop(player);
    assert!(state.is_cancelled());
    assert!(tx.is_closed());
}

#[test]
fn closed_cancelled_or_ordinary_source_rejects_reposition_before_clearing_pcm() {
    for case in 0..3 {
        let (mut player, tx, _, state) = stream(case != 0);
        if case == 1 {
            state.cancel();
        }
        if case == 2 {
            drop(tx);
        }
        let original = player.source.samples().to_vec();
        let pointer = player.source.samples().as_ptr();
        assert!(
            player
                .accept_stream_revision(
                    &state,
                    StreamRevision::INITIAL.checked_next().unwrap(),
                    200
                )
                .is_err()
        );
        assert_eq!(player.source.samples(), original);
        assert_eq!(player.source.samples().as_ptr(), pointer);
        assert_eq!(player.stream_revision, StreamRevision::INITIAL);
        assert_eq!(player.shared.position_frames(), 0);
        assert!(player.drain_events().next().is_none());
    }
}

#[test]
fn replacing_a_source_resets_revision_and_cancels_its_retained_eof() {
    let (mut player, tx, _, state) = stream(true);
    player
        .accept_stream_revision(&state, StreamRevision::INITIAL.checked_next().unwrap(), 42)
        .unwrap();
    let load = player.begin_load_for(84).unwrap();
    assert!(state.is_cancelled());
    assert!(tx.is_closed());
    assert_eq!(player.stream_revision, StreamRevision::INITIAL);
    assert_eq!(player.pending_seek, None);
    let (tx, rx) = stream_channel();
    player
        .complete_stream_load(load, "new".into(), rx, StreamingState::new())
        .unwrap();
    assert!(
        tx.try_send(StreamMsg::Ready {
            revision: StreamRevision::INITIAL,
            sample_rate: 8000,
            channels: 2
        })
        .is_ok()
    );
    assert!(
        tx.try_send(StreamMsg::Error {
            revision: StreamRevision::INITIAL,
            message: "current failure".into()
        })
        .is_ok()
    );
    player.poll_stream();
    assert!(!player.shared.is_loaded());
    assert!(!player.is_streaming());
    let events: Vec<_> = player.drain_events().collect();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].source_id, 84);
    assert_eq!(events[0].event_type, InnerAudioEventType::Error);
}

#[test]
fn discarded_pcm_returns_the_original_loan_and_polling_keeps_its_work_budget() {
    let (mut player, _, mut pool, state) = stream(true);
    let (tx, rx) = mpsc::channel(16);
    player.stream_rx = Some(rx);
    player
        .accept_stream_revision(&state, StreamRevision::INITIAL.checked_next().unwrap(), 0)
        .unwrap();
    let mut loan = pool.take();
    let pointer = loan.buffer_mut().as_ptr();
    loan.buffer_mut().extend_from_slice(&[99.0, 99.0]);
    assert!(
        tx.try_send(StreamMsg::Samples {
            revision: StreamRevision::INITIAL,
            pcm: loan
        })
        .is_ok()
    );
    for _ in 0..7 {
        assert!(
            tx.try_send(StreamMsg::Done {
                revision: StreamRevision::INITIAL
            })
            .is_ok()
        );
    }
    player.poll_stream();
    assert_eq!(player.stream_rx.as_ref().unwrap().len(), 4);
    assert!(player.source.is_empty());
    assert_eq!(pool.take().buffer_mut().as_ptr(), pointer);
    player.poll_stream();
    assert_eq!(player.stream_rx.as_ref().unwrap().len(), 0);
}

#[test]
fn accepted_revisions_and_discarded_loans_have_no_steady_heap_activity() {
    use migo_alloc_probe::{Burst, assert_no_steady_state_allocation};
    let (mut player, tx, mut pool, state) = stream(true);
    let bytes = player.physical_pcm_bytes();
    // Tokio mpsc allocates its next 32-message block on the first rollover.
    // Warm both the message and PCM-return queues through multiple rollovers;
    // the measured burst then exercises block recycling, not initial growth.
    assert_no_steady_state_allocation(
        Burst {
            path: "accepted stream revision, stale/current PCM and events",
            warmup: 64,
            measured: 256,
        },
        |_| {
            let old = player.stream_revision;
            let next = old.checked_next().unwrap();
            player.accept_stream_revision(&state, next, 10).unwrap();
            assert!(tx.try_send(pcm(&mut pool, old, &[99.0, 99.0])).is_ok());
            assert!(tx.try_send(pcm(&mut pool, next, &[1.0, 2.0])).is_ok());
            player.poll_stream();
            assert_eq!(player.source.samples(), &[1.0, 2.0]);
            assert_eq!(player.drain_events().count(), 2);
            assert_eq!(player.physical_pcm_bytes(), bytes);
        },
    );
}

#[test]
fn a_delayed_or_foreign_source_acknowledgement_cannot_clear_current_pcm() {
    let (mut player, _tx, _, old) = stream(true);
    let (_other_player, _other_tx, _, foreign) = stream(true);
    let next = StreamRevision::INITIAL.checked_next().unwrap();
    let original = player.source.samples().to_vec();
    assert!(player.accept_stream_revision(&foreign, next, 42).is_err());
    assert_eq!(player.source.samples(), original);
    assert!(!foreign.is_cancelled());
    assert_eq!(player.drain_events().count(), 0);
    let load = player.begin_load_for(84).unwrap();
    let (tx, rx) = stream_channel();
    let current = StreamingState::new_reusable();
    player
        .complete_stream_load(load, "replacement".into(), rx, current.clone())
        .unwrap();
    assert!(
        tx.try_send(StreamMsg::Ready {
            revision: StreamRevision::INITIAL,
            sample_rate: 8000,
            channels: 2
        })
        .is_ok()
    );
    let mut pool = PcmPool::new();
    assert!(
        tx.try_send(pcm(&mut pool, StreamRevision::INITIAL, &vec![0.5; 8000]))
            .is_ok()
    );
    player.poll_stream();
    player.drain_events().for_each(drop);
    let pointer = player.source.samples().as_ptr();
    assert!(old.is_cancelled());
    for rejected in [&old, &foreign] {
        assert!(player.accept_stream_revision(rejected, next, 42).is_err());
        assert_eq!(player.source.len(), 8000);
        assert_eq!(player.source.samples()[0], 0.5);
        assert_eq!(player.source.samples().as_ptr(), pointer);
        assert_eq!(player.shared.position_frames(), 0);
        assert_eq!(player.pending_seek, None);
        assert_eq!(player.stream_revision, StreamRevision::INITIAL);
        assert_eq!(player.drain_events().count(), 0);
    }
    player.accept_stream_revision(&current, next, 42).unwrap();
    assert!(player.source.is_empty());
    assert_eq!(player.shared.source_id.load(Ordering::Acquire), 84);
    assert_eq!(player.shared.position_frames(), 42);
}
