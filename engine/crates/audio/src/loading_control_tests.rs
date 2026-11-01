//! A source becomes ready after user commands, not before them.
use super::*;
use crate::{
    cache::AudioCache,
    streaming::{PcmPool, stream_channel},
};

fn audio() -> DecodedAudio {
    DecodedAudio {
        samples: (0..8000).map(|n| n as f32).collect(),
        sample_rate: 8000,
        channels: 1,
    }
}
fn finish(p: &mut InnerAudioPlayer, load: InnerAudioLoadToken, cached: bool) {
    if cached {
        let mut cache =
            AudioCache::with_max_size_and_aggregate(128000, AudioAggregateLedger::process_global());
        let item = cache.insert("loading".into(), audio()).unwrap();
        p.complete_cached_load(load, item).unwrap();
    } else {
        p.complete_load(load, audio()).unwrap();
    }
}
fn samples(tx: &tokio::sync::mpsc::Sender<StreamMsg>, n: usize) {
    let mut pcm = PcmPool::new().take();
    pcm.buffer_mut().extend((0..n).map(|x| x as f32));
    tx.try_send(StreamMsg::Samples {
        revision: StreamRevision::INITIAL,
        pcm,
    })
    .unwrap();
}
#[test]
fn last_loading_transport_wins_over_autoplay_without_mutating_the_property() {
    for cached in [false, true] {
        for autoplay in [false, true] {
            for last in [
                PlaybackState::Playing,
                PlaybackState::Paused,
                PlaybackState::Stopped,
            ] {
                let mut p = InnerAudioPlayer::new(941, 1);
                p.shared.set_autoplay(autoplay);
                let load = p.begin_load_for(901).unwrap();
                p.play();
                p.pause();
                p.play();
                match last {
                    PlaybackState::Playing => p.play(),
                    PlaybackState::Paused => p.pause(),
                    PlaybackState::Stopped => p.stop(),
                }
                let mut silent = [0.125; 8];
                p.process(&mut silent);
                assert_eq!(silent, [0.125; 8]);
                assert!(p.drain_events().next().is_none());
                finish(&mut p, load, cached);
                assert_eq!(p.shared.state(), last);
                assert_eq!(p.shared.autoplay(), autoplay);
                let events: Vec<_> = p.drain_events().collect();
                assert_eq!(events[0].event_type, InnerAudioEventType::CanPlay);
                assert_eq!(
                    events
                        .iter()
                        .filter(|e| e.event_type == InnerAudioEventType::Play)
                        .count(),
                    usize::from(last == PlaybackState::Playing)
                );
                assert!(p.loading_controls.is_none());
                p.poll_stream();
                assert_eq!(p.shared.state(), last);
            }
        }
    }
}
#[test]
fn loading_seek_waits_for_metadata_and_stop_cancels_only_the_earlier_target() {
    for cached in [false, true] {
        for after_stop in [false, true] {
            let mut p = InnerAudioPlayer::new(942, 1);
            let load = p.begin_load_for(902).unwrap();
            p.seek(0.25);
            p.play();
            p.stop();
            if after_stop {
                p.seek(0.375);
                p.seek(f64::NAN);
                p.seek(f64::INFINITY);
            }
            p.play();
            finish(&mut p, load, cached);
            let start = if after_stop { 3000 } else { 0 };
            assert_eq!(p.shared.position_frames(), start);
            assert_eq!(p.shared.state(), PlaybackState::Playing);
            let events: Vec<_> = p.drain_events().collect();
            assert_eq!(
                events
                    .iter()
                    .filter(|e| e.event_type == InnerAudioEventType::Seeked)
                    .count(),
                usize::from(after_stop)
            );
            let mut bus = [0.0; 3];
            p.process(&mut bus);
            assert_eq!(bus, [start as f32, start as f32 + 1.0, start as f32 + 2.0]);
        }
    }
}
#[test]
fn loading_seek_retains_paused_or_stopped_state_and_clamps_an_endpoint() {
    for paused in [false, true] {
        for target in [-0.5, 0.25, 1000.0] {
            let mut p = InnerAudioPlayer::new(943, 1);
            p.shared.set_autoplay(true);
            let load = p.begin_load().unwrap();
            p.play();
            if paused {
                p.pause()
            } else {
                p.stop()
            };
            p.seek(target);
            finish(&mut p, load, false);
            assert_eq!(
                p.shared.state(),
                if paused {
                    PlaybackState::Paused
                } else {
                    PlaybackState::Stopped
                }
            );
            assert_eq!(
                p.shared.position_frames(),
                if target < 0.0 {
                    0
                } else if target > 1.0 {
                    8000
                } else {
                    2000
                }
            );
            let mut bus = [0.125; 3];
            p.process(&mut bus);
            assert_eq!(bus, [0.125; 3]);
        }
    }
}
#[test]
fn source_replacement_rejects_old_completion_without_erasing_new_controls() {
    let mut p = InnerAudioPlayer::new(944, 1);
    p.shared.set_autoplay(true);
    let old = p.begin_load_for(904).unwrap();
    p.seek(0.75);
    p.play();
    let new = p.begin_load_for(905).unwrap();
    p.pause();
    p.seek(0.25);
    assert!(p.complete_load(old, audio()).is_err());
    finish(&mut p, new, false);
    assert_eq!(p.shared.state(), PlaybackState::Paused);
    assert_eq!(p.shared.position_frames(), 2000);
    let next = p.begin_load_for(906).unwrap();
    finish(&mut p, next, false);
    assert_eq!(p.shared.state(), PlaybackState::Playing);
    assert_eq!(p.shared.position_frames(), 0);
    let _retired = p.begin_load_for(907).unwrap();
    p.seek(0.75);
    p.pause();
    let replacement = p.begin_load_for(908).unwrap();
    finish(&mut p, replacement, false);
    assert_eq!(p.shared.state(), PlaybackState::Playing);
    assert_eq!(p.shared.position_frames(), 0);
}
#[test]
fn short_and_prefilled_streams_apply_loading_controls_once_at_canplay() {
    for short in [false, true] {
        let mut p = InnerAudioPlayer::new(945, 1);
        p.shared.set_autoplay(true);
        let load = p.begin_load_for(907).unwrap();
        p.play();
        p.pause();
        p.seek(0.001);
        let (tx, rx) = stream_channel();
        p.complete_stream_load(load, "waiting".into(), rx, StreamingState::new())
            .unwrap();
        tx.try_send(StreamMsg::Ready {
            revision: StreamRevision::INITIAL,
            sample_rate: 8000,
            channels: 1,
        })
        .unwrap();
        p.poll_stream();
        assert!(!p.shared.is_loaded());
        samples(&tx, if short { 32 } else { 4000 });
        if short {
            tx.try_send(StreamMsg::Done {
                revision: StreamRevision::INITIAL,
            })
            .unwrap();
        }
        p.poll_stream();
        assert!(p.shared.is_loaded());
        assert_eq!(p.shared.state(), PlaybackState::Paused);
        assert_eq!(p.shared.position_frames(), 8);
        let events: Vec<_> = p.drain_events().collect();
        assert_eq!(
            events
                .iter()
                .filter(|e| e.event_type == InnerAudioEventType::CanPlay)
                .count(),
            1
        );
        assert_eq!(
            events
                .iter()
                .filter(|e| e.event_type == InnerAudioEventType::Seeked)
                .count(),
            1
        );
        assert!(
            !events
                .iter()
                .any(|e| e.event_type == InnerAudioEventType::Play)
        );
        p.poll_stream();
        assert!(p.drain_events().next().is_none());
    }
}
#[test]
fn pending_forward_seek_uses_real_later_pcm_and_loading_play_does_not_skip_it() {
    let mut p = InnerAudioPlayer::new(946, 1);
    let load = p.begin_load().unwrap();
    p.seek(0.75);
    p.play();
    let (tx, rx) = stream_channel();
    p.complete_stream_load(load, "forward".into(), rx, StreamingState::new())
        .unwrap();
    tx.try_send(StreamMsg::Ready {
        revision: StreamRevision::INITIAL,
        sample_rate: 8000,
        channels: 1,
    })
    .unwrap();
    samples(&tx, 4000);
    p.poll_stream();
    assert_eq!(p.pending_seek, Some(6000));
    assert_eq!(p.shared.state(), PlaybackState::Playing);
    let mut bus = [0.125; 4];
    p.process(&mut bus);
    assert_eq!(bus, [0.125; 4]);
    p.drain_events().for_each(drop);
    let mut pcm = PcmPool::new().take();
    pcm.buffer_mut().extend((4000..8000).map(|x| x as f32));
    tx.try_send(StreamMsg::Samples {
        revision: StreamRevision::INITIAL,
        pcm,
    })
    .unwrap();
    p.poll_stream();
    let mut out = [0.0; 4];
    p.process(&mut out);
    assert_eq!(out, [6000.0, 6001.0, 6002.0, 6003.0]);
    assert_eq!(
        p.drain_events()
            .filter(|e| e.event_type == InnerAudioEventType::Seeked)
            .count(),
        1
    );
}
#[test]
fn failed_or_absent_source_cannot_replay_old_loading_controls() {
    let mut p = InnerAudioPlayer::new(947, 1);
    p.play();
    p.seek(0.5);
    p.pause();
    assert!(p.loading_controls.is_none());
    let load = p.begin_load().unwrap();
    p.play();
    p.seek(0.5);
    let (tx, rx) = stream_channel();
    let state = StreamingState::new();
    p.complete_stream_load(load, "error".into(), rx, state.clone())
        .unwrap();
    tx.try_send(StreamMsg::Error {
        revision: StreamRevision::INITIAL,
        message: "broken".into(),
    })
    .unwrap();
    p.poll_stream();
    assert!(state.is_cancelled());
    assert!(p.loading_controls.is_none());
    p.play();
    p.seek(0.75);
    assert!(p.loading_controls.is_none());
    let load = p.begin_load().unwrap();
    finish(&mut p, load, false);
    assert_eq!(p.shared.state(), PlaybackState::Stopped);
    assert_eq!(p.shared.position_frames(), 0);
}
#[test]
fn recording_pending_controls_and_waiting_for_ready_have_no_steady_heap_activity() {
    let mut p = InnerAudioPlayer::new(948, 1);
    let _load = p.begin_load().unwrap();
    migo_alloc_probe::assert_no_steady_state_allocation(
        migo_alloc_probe::Burst {
            path: "loading control coalescing",
            warmup: 64,
            measured: 256,
        },
        |_| {
            p.play();
            p.pause();
            p.seek(0.25);
            p.stop();
            p.play();
            p.poll_stream();
            let mut bus = [0.125; 4];
            p.process(&mut bus);
            assert_eq!(bus, [0.125; 4]);
            assert!(p.drain_events().next().is_none());
        },
    );
}

#[test]
fn start_time_property_order_reaches_first_play_for_owned_and_cached_audio() {
    for cached in [false, true] {
        for before_source in [false, true] {
            for autoplay in [false, true] {
                let mut p = InnerAudioPlayer::new(971, 1);
                if before_source {
                    p.set_start_time(0.25);
                }
                let load = p.begin_load_for(1971).unwrap();
                p.shared.set_autoplay(autoplay);
                if !before_source {
                    p.set_start_time(0.5);
                    p.set_start_time(0.25);
                }
                p.set_start_time(f64::NAN);
                p.set_start_time(f64::INFINITY);
                if !autoplay {
                    p.play();
                }
                assert_eq!(p.shared.position_frames(), 0);
                finish(&mut p, load, cached);
                assert_eq!(p.shared.position_frames(), 2000);
                assert_eq!(p.shared.state(), PlaybackState::Playing);
                let mut out = [0.0; 3];
                p.process(&mut out);
                assert_eq!(out, [2000.0, 2001.0, 2002.0]);
                p.drain_events().for_each(drop);
                p.play();
                assert_eq!(p.shared.position_frames(), 2003);
                assert!(p.drain_events().next().is_none());
            }
        }
    }
}

#[test]
fn explicit_loading_seek_even_zero_wins_and_stop_rearms_start_time() {
    for cached in [false, true] {
        for target in [0.0, 0.375] {
            let mut p = InnerAudioPlayer::new(972, 1);
            p.set_start_time(0.25);
            let load = p.begin_load().unwrap();
            p.seek(target);
            p.set_start_time(0.5);
            p.play();
            finish(&mut p, load, cached);
            assert_eq!(p.shared.position_frames(), (target * 8000.0) as u64);
            p.stop();
            p.play();
            assert_eq!(p.shared.position_frames(), 4000);
            p.stop();
            p.seek(0.0);
            p.play();
            assert_eq!(p.shared.position_frames(), 0);
            p.stop();
            p.set_start_time(-1.0);
            p.play();
            assert_eq!(p.shared.position_frames(), 0);
        }
    }
}

#[test]
fn start_time_does_not_seek_a_paused_or_playing_source_and_resume_keeps_position() {
    for pause_while_loading in [false, true] {
        let mut p = InnerAudioPlayer::new(973, 1);
        p.set_start_time(0.25);
        let load = p.begin_load().unwrap();
        if pause_while_loading {
            p.play();
            p.pause();
        }
        finish(&mut p, load, false);
        assert_eq!(p.shared.position_frames(), 0);
        assert_ne!(p.shared.state(), PlaybackState::Playing);
        p.set_start_time(0.375);
        assert_eq!(p.shared.position_frames(), 0);
        p.play();
        assert_eq!(p.shared.position_frames(), 3000);
        let mut out = [0.0; 3];
        p.process(&mut out);
        p.pause();
        p.set_start_time(0.5);
        p.drain_events().for_each(drop);
        p.play();
        assert_eq!(p.shared.position_frames(), 3003);
        assert!(
            !p.drain_events()
                .any(|e| e.event_type == InnerAudioEventType::Seeked)
        );
        p.stop();
        p.play();
        assert_eq!(p.shared.position_frames(), 4000);
    }
}

#[test]
fn source_replacement_and_natural_end_rearm_the_persistent_start_time() {
    let mut p = InnerAudioPlayer::new(974, 1);
    p.set_start_time(0.9995);
    p.shared.set_autoplay(true);
    let seed = p.begin_load_for(1973).unwrap();
    finish(&mut p, seed, false);
    p.seek(0.0);
    p.play();
    let old = p.begin_load_for(1974).unwrap();
    p.seek(0.25);
    let new = p.begin_load_for(1975).unwrap();
    assert!(p.complete_load(old, audio()).is_err());
    finish(&mut p, new, false);
    let mut out = [0.0; 8];
    p.process(&mut out);
    assert_eq!(out, [7996.0, 7997.0, 7998.0, 7999.0, 0.0, 0.0, 0.0, 0.0]);
    assert_eq!(
        p.drain_events()
            .filter(|e| e.event_type == InnerAudioEventType::Ended)
            .count(),
        1
    );
    p.set_start_time(0.25);
    p.play();
    assert_eq!(p.shared.position_frames(), 2000);
    p.stop();
    p.set_start_time(1000.0);
    p.play();
    let mut tail = [0.125; 3];
    p.process(&mut tail);
    assert_eq!(tail, [0.125; 3]);
    assert_eq!(
        p.drain_events()
            .filter(|e| e.event_type == InnerAudioEventType::Ended)
            .count(),
        1
    );
}

#[test]
fn streaming_start_time_waits_for_real_canplay_and_forward_pcm() {
    let mut p = InnerAudioPlayer::new(975, 1);
    p.set_start_time(0.75);
    p.shared.set_autoplay(true);
    let load = p.begin_load().unwrap();
    let (tx, rx) = stream_channel();
    p.complete_stream_load(load, "start-time".into(), rx, StreamingState::new())
        .unwrap();
    tx.try_send(StreamMsg::Ready {
        revision: StreamRevision::INITIAL,
        sample_rate: 8000,
        channels: 1,
    })
    .unwrap();
    p.poll_stream();
    assert!(!p.shared.is_loaded());
    assert!(p.start_pending);
    assert_eq!(p.shared.position_frames(), 0);
    samples(&tx, 4000);
    p.poll_stream();
    assert_eq!(p.pending_seek, Some(6000));
    let mut waiting = [0.125; 3];
    p.process(&mut waiting);
    assert_eq!(waiting, [0.125; 3]);
    let mut pcm = PcmPool::new().take();
    pcm.buffer_mut().extend((4000..8000).map(|n| n as f32));
    tx.try_send(StreamMsg::Samples {
        revision: StreamRevision::INITIAL,
        pcm,
    })
    .unwrap();
    p.poll_stream();
    let mut out = [0.0; 3];
    p.process(&mut out);
    assert_eq!(out, [6000.0, 6001.0, 6002.0]);
    assert_eq!(
        p.drain_events()
            .filter(|e| e.event_type == InnerAudioEventType::Seeked)
            .count(),
        1
    );
}

#[test]
fn setting_start_time_and_coalescing_loading_commands_have_no_steady_heap_activity() {
    let mut p = InnerAudioPlayer::new(976, 1);
    let _load = p.begin_load().unwrap();
    migo_alloc_probe::assert_no_steady_state_allocation(
        migo_alloc_probe::Burst {
            path: "start-time loading configuration",
            warmup: 64,
            measured: 256,
        },
        |_| {
            p.set_start_time(0.25);
            p.play();
            p.pause();
            p.stop();
            p.seek(0.0);
            p.poll_stream();
        },
    );
}
