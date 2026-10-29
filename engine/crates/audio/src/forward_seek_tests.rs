//! Forward seeks must wait for real sequential PCM, not the downloaded prefix.
use super::*;
use crate::streaming::PcmPool;
use tokio::sync::mpsc;

fn send_frames(
    tx: &mpsc::Sender<StreamMsg>,
    pool: &mut PcmPool,
    start: usize,
    frames: usize,
    channels: u32,
) {
    let mut pcm = pool.take();
    pcm.buffer_mut().extend(
        (start..start + frames)
            .flat_map(|f| (0..channels).map(move |ch| (f * 4 + ch as usize) as f32)),
    );
    assert!(tx.try_send(StreamMsg::Samples(pcm)).is_ok());
}

fn stream(
    channels: u32,
) -> (
    InnerAudioPlayer,
    mpsc::Sender<StreamMsg>,
    PcmPool,
    Arc<StreamingState>,
) {
    let (tx, rx) = mpsc::channel(16);
    let state = StreamingState::new();
    let mut player = InnerAudioPlayer::new(9, channels);
    let load = player.begin_load_for(73).unwrap();
    player
        .complete_stream_load(load, "stream".into(), rx, state.clone())
        .unwrap();
    assert!(
        tx.try_send(StreamMsg::Ready {
            sample_rate: 8000,
            channels
        })
        .is_ok()
    );
    let mut pool = PcmPool::new();
    send_frames(&tx, &mut pool, 0, 4000, channels);
    player.poll_stream();
    assert!(player.shared.is_loaded());
    player.drain_events().for_each(drop);
    (player, tx, pool, state)
}

#[test]
fn paused_seek_reaches_undecoded_stereo_pcm_without_replacing_the_source() {
    let (mut player, tx, mut pool, state) = stream(2);
    player.play();
    player.pause();
    player.drain_events().for_each(drop);
    let owner = player.stream_state.as_ref().unwrap().clone();
    let backing = player.source.samples().as_ptr();
    let bytes = player.physical_pcm_bytes();
    player.seek(1.0);
    assert_eq!(player.pending_seek, Some(8000));
    assert_eq!(player.shared.position_frames(), 8000);
    assert!(player.needs_stream_poll());
    let events: Vec<_> = player.drain_events().collect();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].event_type, InnerAudioEventType::Seeking);
    assert_eq!(events[0].current_time, 0.0);
    for start in (4000..8000).step_by(1000) {
        send_frames(&tx, &mut pool, start, 1000, 2);
        player.poll_stream();
        assert_eq!(player.pending_seek, Some(8000));
        assert_eq!(player.drain_events().count(), 0);
        assert_eq!(player.source.len(), 0);
    }
    // Equality at a chunk end is still waiting: no first target frame yet.
    send_frames(&tx, &mut pool, 8000, 1000, 2);
    player.poll_stream();
    assert_eq!(player.pending_seek, None);
    assert_eq!(player.shared.state(), PlaybackState::Paused);
    assert_eq!(player.source.samples().as_ptr(), backing);
    assert_eq!(player.physical_pcm_bytes(), bytes);
    assert!(Arc::ptr_eq(player.stream_state.as_ref().unwrap(), &owner));
    assert!(!state.is_cancelled());
    let events: Vec<_> = player.drain_events().collect();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].event_type, InnerAudioEventType::Seeked);
    assert_eq!(events[0].current_time, 1.0);
    assert_eq!(events[0].source_id, 73);
    assert!(events[0].paused);
    player.play();
    let mut first = [0.0; 2];
    assert!(player.process(&mut first));
    assert_eq!(first, [32000.0, 32001.0]);
}

#[test]
fn pending_seek_preserves_the_mix_and_emits_one_waiting_event() {
    let (mut player, tx, mut pool, _) = stream(1);
    player.play();
    player.drain_events().for_each(drop);
    player.seek(1.0);
    player.drain_events().for_each(drop);
    for start in [4000, 4100, 4200] {
        let mut mix = [0.75; 16];
        assert!(player.process(&mut mix));
        assert_eq!(mix, [0.75; 16]);
        assert_eq!(player.shared.position_frames(), 8000);
        send_frames(&tx, &mut pool, start, 100, 1);
        player.poll_stream();
    }
    let events: Vec<_> = player.drain_events().collect();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].event_type, InnerAudioEventType::Waiting);
    assert_eq!(player.frames_since_time_update, 0);
}

#[test]
fn a_new_seek_replaces_the_pending_target_or_returns_to_retained_pcm() {
    let (mut player, tx, mut pool, _) = stream(1);
    player.seek(1.0);
    player.seek(0.25);
    assert_eq!(player.pending_seek, None);
    assert_eq!(player.shared.position_frames(), 2000);
    player.play();
    let mut first = [0.0];
    assert!(player.process(&mut first));
    assert_eq!(first, [8000.0]);
    player.seek(1.0);
    player.seek(0.75);
    player.drain_events().for_each(drop);
    send_frames(&tx, &mut pool, 4000, 3000, 1);
    player.poll_stream();
    assert_eq!(player.pending_seek, None);
    let events: Vec<_> = player.drain_events().collect();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].event_type, InnerAudioEventType::Seeked);
    assert_eq!(events[0].current_time, 0.75);
    first.fill(0.0);
    assert!(player.process(&mut first));
    assert_eq!(first, [24000.0]);
}

#[test]
fn eof_clamps_a_pending_seek_and_ended_waits_for_play() {
    for playing in [false, true] {
        let (mut player, tx, mut pool, _) = stream(1);
        player.play();
        if !playing {
            player.pause();
        }
        player.seek(f64::MAX);
        player.drain_events().for_each(drop);
        send_frames(&tx, &mut pool, 4000, 100, 1);
        assert!(tx.try_send(StreamMsg::Done).is_ok());
        player.poll_stream();
        assert_eq!(player.pending_seek, None);
        assert_eq!(player.shared.duration_frames(), 4100);
        assert_eq!(player.shared.position_frames(), 4100);
        assert!(player.source.is_empty());
        assert!(player.take_streamed_audio().is_none());
        let events: Vec<_> = player.drain_events().collect();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, InnerAudioEventType::Seeked);
        assert_eq!(events[0].current_time, 4100.0 / 8000.0);
        assert_eq!(events[0].paused, !playing);
        if !playing {
            assert!(player.process(&mut [0.75; 8]));
            assert_eq!(player.drain_events().count(), 0);
            player.play();
            player.drain_events().for_each(drop);
        }
        let mut mix = [0.75; 8];
        assert!(!player.process(&mut mix));
        assert_eq!(mix, [0.75; 8]);
        let events: Vec<_> = player.drain_events().collect();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, InnerAudioEventType::Ended);
        assert_eq!(events[0].current_time, 4100.0 / 8000.0);
        assert!(!player.process(&mut mix));
        assert_eq!(player.drain_events().count(), 0);
    }
}

#[test]
fn seek_at_the_current_end_waits_for_the_next_frame_or_eof() {
    let (mut player, tx, _, _) = stream(1);
    player.seek(0.5);
    assert_eq!(player.pending_seek, Some(4000));
    assert_eq!(player.drain_events().count(), 1);
    assert!(tx.try_send(StreamMsg::Done).is_ok());
    player.poll_stream();
    assert_eq!(player.pending_seek, None);
    let events: Vec<_> = player.drain_events().collect();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].event_type, InnerAudioEventType::Seeked);
    assert_eq!(events[0].current_time, 0.5);
}

#[test]
fn poll_budget_bounds_paused_seek_work_and_preserves_pcm_credit() {
    let (mut player, tx, mut pool, _) = stream(1);
    player.seek(f64::MAX);
    player.drain_events().for_each(drop);
    let bytes = player.physical_pcm_bytes();
    for n in 0..9 {
        send_frames(&tx, &mut pool, 4000 + n * 100, 100, 1);
    }
    let capacity = tx.capacity();
    player.poll_stream();
    assert_eq!(tx.capacity() - capacity, STREAM_CHANNEL_CAPACITY);
    assert_eq!(player.shared.duration_frames(), 4400);
    assert_eq!(player.physical_pcm_bytes(), bytes);
    assert!(player.needs_stream_poll());
    assert_eq!(player.drain_events().count(), 0);
}

#[test]
fn stop_error_and_source_replacement_retire_a_pending_seek() {
    for boundary in 0..3 {
        let (mut player, tx, _, state) = stream(1);
        player.seek(1.0);
        player.drain_events().for_each(drop);
        match boundary {
            0 => player.stop(),
            1 => {
                assert!(tx.try_send(StreamMsg::Error("fixture".into())).is_ok());
                player.poll_stream();
            }
            _ => {
                player.begin_load().unwrap();
            }
        }
        assert_eq!(player.pending_seek, None);
        assert!(!player.is_active());
        assert!(
            player
                .drain_events()
                .all(|e| e.event_type != InnerAudioEventType::Seeked)
        );
        if boundary != 0 {
            assert!(state.is_cancelled());
        }
    }
}

#[test]
fn malformed_or_overflowing_seek_pcm_fails_without_seeked() {
    for overflow in [false, true] {
        let (mut player, tx, mut pool, state) = stream(2);
        player.seek(f64::MAX);
        if overflow {
            player.shared.set_duration_frames(u64::MAX - 1);
        }
        player.drain_events().for_each(drop);
        let mut pcm = pool.take();
        pcm.buffer_mut()
            .extend_from_slice(if overflow { &[0.0; 8] } else { &[0.0; 3] });
        assert!(tx.try_send(StreamMsg::Samples(pcm)).is_ok());
        player.poll_stream();
        assert!(state.is_cancelled());
        assert_eq!(player.pending_seek, None);
        assert!(!player.shared.is_loaded());
        assert_eq!(player.physical_pcm_bytes(), 0);
        let events: Vec<_> = player.drain_events().collect();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, InnerAudioEventType::Error);
    }
}

#[test]
fn discarded_seek_pcm_has_no_steady_heap_activity() {
    use migo_alloc_probe::{Burst, assert_no_steady_state_allocation};
    let (mut player, _tx, _, _) = stream(2);
    player.seek(f64::MAX);
    player.drain_events().for_each(drop);
    let bytes = player.physical_pcm_bytes();
    assert_no_steady_state_allocation(
        Burst {
            path: "discarding a forward-seek decoder loan",
            warmup: 4,
            measured: 256,
        },
        |_| {
            assert!(player.seek_stream_prefix(&[0.25; 128]).unwrap().is_empty());
            assert_eq!(player.physical_pcm_bytes(), bytes);
        },
    );
}
