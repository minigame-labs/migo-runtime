//! Idle players used to leave seek commands in a mixer-only atomic mailbox.
//! Assert completion, event snapshots and the first audible frame without
//! rendering as a prerequisite, including shared cache and streamed backing.
use super::*;

fn player(channels: u32, cached: bool) -> InnerAudioPlayer {
    let mut player = InnerAudioPlayer::new(9, channels);
    let load = player.begin_load_for(73).unwrap();
    let audio = DecodedAudio {
        samples: (0..128)
            .flat_map(|frame| (0..channels).map(move |ch| (frame * 4 + ch) as f32))
            .collect(),
        sample_rate: 8000,
        channels,
    };
    if cached {
        let mut cache = crate::cache::AudioCache::new();
        let cached = cache.insert("seek".into(), audio).unwrap();
        player.complete_cached_load(load, cached).unwrap();
    } else {
        player.complete_load(load, audio).unwrap();
    }
    player.drain_events().for_each(drop);
    player
}

#[test]
fn paused_seeks_complete_before_rendering_and_preserve_the_backing() {
    for cached in [false, true] {
        let mut player = player(1, cached);
        player.shared.set_playback_rate(1.1);
        player.play();
        assert!(player.process(&mut [0.0; 3]));
        player.pause();
        player.drain_events().for_each(drop);
        let backing = player.source.samples().as_ptr();
        player.waiting_notified = true;
        player.frames_since_time_update = 1999;

        player.seek(0.008);

        assert_eq!(player.shared.position_frames(), 64);
        assert_eq!(player.shared.state(), PlaybackState::Paused);
        assert_eq!(player.position_phase, 0);
        assert_eq!(player.frames_since_time_update, 0);
        assert!(!player.waiting_notified);
        assert_eq!(player.source.samples().as_ptr(), backing);
        assert_eq!(matches!(player.source, AudioSource::Cached(_)), cached);
        let events: Vec<_> = player.drain_events().collect();
        assert_eq!(events.len(), 2);
        for (event, kind, frame) in [
            (&events[0], InnerAudioEventType::Seeking, 3),
            (&events[1], InnerAudioEventType::Seeked, 64),
        ] {
            assert_eq!(event.event_type, kind);
            assert_eq!(event.current_time, frame as f64 / 8000.0);
            assert_eq!((event.id, event.source_id), (9, 73));
            assert_eq!(event.duration, 128.0 / 8000.0);
            assert!(event.paused && event.buffered);
        }
        let mut mix = [7.0; 3];
        assert!(player.process(&mut mix));
        assert_eq!(mix, [7.0; 3]);
        assert_eq!(player.shared.position_frames(), 64);
        assert_eq!(player.drain_events().count(), 0);
        player.play();
        let mut first = [0.0];
        assert!(player.process(&mut first));
        assert_eq!(first, [256.0]);
        assert_eq!(player.position_phase, 100);
    }
}

#[test]
fn stopped_seek_stop_and_play_observe_command_order() {
    let mut player = player(1, false);
    player.seek(0.004);
    player.seek(0.008);
    assert_eq!(player.shared.position_frames(), 64);
    assert_eq!(player.shared.state(), PlaybackState::Stopped);
    let events: Vec<_> = player.drain_events().collect();
    assert_eq!(
        events.len(),
        4,
        "two commands must not collapse into one seek"
    );
    assert_eq!(events[1].current_time, 0.004);
    assert_eq!(events[2].current_time, 0.004);
    assert_eq!(events[3].current_time, 0.008);
    assert!(events.iter().all(|event| event.paused));
    assert!(!player.process(&mut [0.0; 1]));
    assert_eq!(player.shared.position_frames(), 64);

    player.stop();
    player.play();
    let play = player.drain_events().next().unwrap();
    assert_eq!(play.event_type, InnerAudioEventType::Play);
    assert_eq!(play.current_time, 0.0);
    let mut first = [0.0];
    assert!(player.process(&mut first));
    assert_eq!(first, [0.0], "a stopped seek must not reappear after play");

    player.stop();
    player.seek(0.012);
    player.play();
    let events: Vec<_> = player.drain_events().collect();
    assert_eq!(events.last().unwrap().event_type, InnerAudioEventType::Play);
    assert_eq!(events.last().unwrap().current_time, 0.012);
    first.fill(0.0);
    assert!(player.process(&mut first));
    assert_eq!(first, [384.0]);
}

#[test]
fn seek_rejects_nonfinite_and_unloaded_targets_and_clamps_finite_bounds() {
    let mut player = player(1, false);
    player.seek(0.008);
    player.drain_events().for_each(drop);
    for invalid in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        player.seek(invalid);
        assert_eq!(player.shared.position_frames(), 64);
        assert_eq!(player.drain_events().count(), 0);
    }
    player.seek(-1.0);
    assert_eq!(player.shared.position_frames(), 0);
    player.seek(f64::MAX);
    assert_eq!(player.shared.position_frames(), 128);
    player.drain_events().for_each(drop);
    player.play();
    let mut other = [0.75; 8];
    assert!(!player.process(&mut other));
    assert_eq!(other, [0.75; 8]);
    assert_eq!(
        player
            .drain_events()
            .filter(|e| e.event_type == InnerAudioEventType::Ended)
            .count(),
        1
    );
    let load = player.begin_load().unwrap();
    player.seek(0.008);
    assert_eq!(player.shared.position_frames(), 0);
    assert_eq!(player.drain_events().count(), 0);
    assert!(load.is_current());
}

#[test]
fn stereo_seek_targets_frames_and_keeps_seeked_before_play() {
    let mut player = player(2, false);
    player.seek(0.008);
    assert_eq!(player.position, 128);
    player.play();
    let events: Vec<_> = player.drain_events().collect();
    assert_eq!(events.len(), 3);
    assert_eq!(events[1].event_type, InnerAudioEventType::Seeked);
    assert!(events[1].paused);
    assert_eq!(events[2].event_type, InnerAudioEventType::Play);
    assert!(!events[2].paused);
    assert_eq!(events[1].current_time, events[2].current_time);
    let mut first = [0.0; 2];
    assert!(player.process(&mut first));
    assert_eq!(first, [256.0, 257.0]);
}

#[test]
fn retained_stream_seek_uses_absolute_frames_while_paused() {
    let (tx, rx) = tokio::sync::mpsc::channel(4);
    let state = StreamingState::new();
    let mut player = InnerAudioPlayer::new(9, 1);
    player
        .start_streaming("stream".into(), rx, state.clone())
        .unwrap();
    tx.try_send(StreamMsg::Ready {
        sample_rate: 8000,
        channels: 1,
    })
    .ok()
    .unwrap();
    let mut pool = crate::streaming::PcmPool::new();
    let mut pcm = pool.take();
    let frames = InnerAudioPlayer::STREAM_WINDOW_SAMPLES + 32;
    pcm.buffer_mut()
        .extend((0..frames).map(|frame| frame as f32));
    tx.try_send(StreamMsg::Samples(pcm)).ok().unwrap();
    player.poll_stream();
    player.play();
    player.process(&mut vec![0.0; InnerAudioPlayer::STREAM_WINDOW_SAMPLES + 16]);
    player.poll_stream();
    assert_eq!(player.stream_base_frames, 16);
    player.pause();
    player.drain_events().for_each(drop);
    let backing = player.source.samples().as_ptr();
    player.seek(0.008);
    assert_eq!(player.shared.position_frames(), 64);
    assert_eq!(player.position, 48);
    assert_eq!(player.source.samples().as_ptr(), backing);
    assert!(player.is_streaming());
    assert!(!state.is_cancelled());
    assert_eq!(player.shared.state(), PlaybackState::Paused);
    assert_eq!(player.drain_events().count(), 2);
    player.play();
    let mut first = [0.0];
    assert!(player.process(&mut first));
    assert_eq!(first, [64.0]);
}

#[test]
fn buffered_seek_and_event_delivery_have_no_steady_heap_activity() {
    use migo_alloc_probe::{Burst, assert_no_steady_state_allocation};
    for cached in [false, true] {
        let mut player = player(2, cached);
        assert_no_steady_state_allocation(
            Burst {
                path: "paused buffered seek and event delivery",
                warmup: 4,
                measured: 256,
            },
            |tick| {
                player.seek(if tick % 2 == 0 { 0.004 } else { 0.008 });
                assert_eq!(player.drain_events().count(), 2);
                assert_eq!(player.shared.state(), PlaybackState::Stopped);
            },
        );
    }
}
