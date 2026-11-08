//! Canonical media seeks retain requested loop mode and ordered source ACK.
use super::*;
use crate::stream_control::LoopRequest;

#[test]
fn cyclic_seek_waits_for_ack_and_fresh_pcm_in_every_play_state() {
    for state in [
        PlaybackState::Playing,
        PlaybackState::Paused,
        PlaybackState::Stopped,
    ] {
        let (mut p, source, tx, mut requests) = setup(true, false);
        p.process(&mut [0.0; 137]);
        p.shared.set_state(state);
        let before = p.shared.position_frames();
        p.seek(0.375);
        let command = requests.try_recv().unwrap();
        let request = command.cutover.unwrap();
        assert_eq!(command.target, 18000);
        assert_eq!(request.seek_target, Some(18000));
        assert!(request.enabled);
        assert_eq!(request.output_start, 16538);
        let mut bus = [0.125; 16];
        p.process(&mut bus);
        assert_eq!(bus, [0.125; 16]);
        assert_eq!(p.shared.position_frames(), before);
        assert_eq!(p.drain_events().count(), 0);
        let revision = StreamRevision::INITIAL.checked_next().unwrap();
        let mut ack = prepared(command, &source, revision, |_| {});
        p.poll_stream();
        assert!(ack.try_recv().is_ok());
        assert_eq!(p.shared.position_frames(), 16538);
        assert_eq!(p.shared.state(), state);
        assert!(p.shared.loop_enabled());
        let events: Vec<_> = p.drain_events().collect();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, InnerAudioEventType::Seeking);
        p.process(&mut bus);
        assert_eq!(bus, [0.125; 16]);
        send(&tx, revision, 24000, 0.75);
        p.poll_stream();
        let events: Vec<_> = p.drain_events().collect();
        assert_eq!(
            events
                .iter()
                .filter(|e| e.event_type == InnerAudioEventType::Seeked)
                .count(),
            1
        );
        assert!(events.iter().all(|e| !matches!(
            e.event_type,
            InnerAudioEventType::Error | InnerAudioEventType::Play | InnerAudioEventType::Ended
        )));
        assert_eq!(p.shared.state(), state);
        let mut out = [0.0; 16];
        p.process(&mut out);
        assert_eq!(
            out,
            if state == PlaybackState::Playing {
                [0.75; 16]
            } else {
                [0.0; 16]
            }
        );
    }
}

#[test]
fn queued_disable_then_seek_duration_has_one_empty_terminal_suffix() {
    let (mut p, source, tx, mut requests) = setup(true, true);
    p.process(&mut vec![0.0; 6000]);
    p.drain_events().for_each(drop);
    p.set_loop_enabled(false);
    p.seek(p.shared.duration());
    let first = requests.try_recv().unwrap();
    let second = requests.try_recv().unwrap();
    let request = second.cutover.unwrap();
    assert_eq!(second.target, 17);
    assert_eq!(request.seek_target, Some(17));
    assert!(!request.enabled);
    assert_eq!(request.output_start, 16);
    let mut revision = StreamRevision::INITIAL.checked_next().unwrap();
    let mut ack = prepared(first, &source, revision, |_| {});
    p.poll_stream();
    assert!(ack.try_recv().is_ok());
    revision = revision.checked_next().unwrap();
    let mut ack = prepared(second, &source, revision, |t| {
        assert_eq!(
            t.unwrap().presentation(),
            StreamPresentation::single(source.timeline().unwrap())
        );
    });
    p.poll_stream();
    assert!(ack.try_recv().is_ok());
    assert_eq!(p.shared.position_frames(), 16);
    assert!(!p.shared.loop_enabled());
    assert!(tx.try_send(StreamMsg::Done { revision }).is_ok());
    p.poll_stream();
    let mut out = [0.0; 16];
    assert!(!p.process(&mut out));
    assert_eq!(out, [0.0; 16]);
    let events: Vec<_> = p.drain_events().collect();
    for kind in [
        InnerAudioEventType::Seeking,
        InnerAudioEventType::Seeked,
        InnerAudioEventType::Ended,
    ] {
        assert_eq!(events.iter().filter(|e| e.event_type == kind).count(), 1);
    }
    let ended = events
        .iter()
        .find(|e| e.event_type == InnerAudioEventType::Ended)
        .unwrap();
    assert_eq!(ended.current_time, 17.0 / 48000.0);
    assert_eq!(ended.duration, 17.0 / 48000.0);
    assert_eq!(p.shared.current_time(), 0.0);
    assert!(
        !events
            .iter()
            .any(|e| e.event_type == InnerAudioEventType::Error)
    );
}

#[test]
fn queued_cyclic_seek_stop_play_preserves_last_mode_and_zero_position() {
    for enabled in [true, false] {
        let (mut p, source, tx, mut requests) = setup(true, false);
        p.process(&mut [0.0; 137]);
        if !enabled {
            p.set_loop_enabled(false);
        }
        p.seek(0.5);
        p.stop();
        p.play();
        p.drain_events().for_each(drop);
        let mut revision = StreamRevision::INITIAL;
        let count = if enabled { 2 } else { 3 };
        for _ in 0..count {
            let command = requests.try_recv().unwrap();
            assert_eq!(command.cutover.unwrap().enabled, enabled);
            revision = revision.checked_next().unwrap();
            let mut ack = prepared(command, &source, revision, |_| {});
            p.poll_stream();
            assert!(ack.try_recv().is_ok());
            assert_eq!(p.shared.position_frames(), 0);
            assert_eq!(p.shared.state(), PlaybackState::Playing);
            assert!(!p.drain_events().any(|e| matches!(
                e.event_type,
                InnerAudioEventType::Seeking
                    | InnerAudioEventType::Seeked
                    | InnerAudioEventType::Error
            )));
        }
        assert!(requests.try_recv().is_err());
        assert_eq!(p.shared.loop_enabled(), enabled);
        send(&tx, revision, 24000, 0.75);
        p.poll_stream();
        let mut out = [0.0; 16];
        assert!(p.process(&mut out));
        assert_eq!(out, [0.75; 16]);
    }
}

#[test]
fn cyclic_seek_receipt_must_match_exact_mapping_mode_and_source_clock() {
    for bad in 0..4 {
        let (mut p, source, _tx, mut requests) = setup(true, false);
        p.seek(0.375);
        let command = requests.try_recv().unwrap();
        let mut ack = prepared(
            command,
            &source,
            StreamRevision::INITIAL.checked_next().unwrap(),
            |t| {
                *t = match bad {
                    0 => None,
                    1 => Some(
                        StreamTraversal::cutover(source.timeline().unwrap(), 16539, true).unwrap(),
                    ),
                    2 => Some(
                        StreamTraversal::seek(source.timeline().unwrap(), 18000, false).unwrap(),
                    ),
                    _ => Some(
                        StreamTraversal::seek(
                            StreamTimeline::new(48001, 48000, 44100, 1).unwrap(),
                            18000,
                            true,
                        )
                        .unwrap(),
                    ),
                };
            },
        );
        p.poll_stream();
        assert!(ack.try_recv().is_err());
        assert!(source.is_cancelled());
        assert!(!p.shared.is_loaded());
    }
}

#[test]
fn seek_descriptor_is_integer_bounded_and_distinct_from_cycle_cutover() {
    for frames in [17, 48000, (1_u64 << 53) + 17] {
        let clock = StreamTimeline::new(frames, 48000, 44100, 1).unwrap();
        for target in [0, 1, frames - 1, frames] {
            let expected = (u128::from(target) * 44100).div_ceil(48000) as u64;
            for enabled in [false, true] {
                let t = StreamTraversal::seek(clock, target, enabled).unwrap();
                assert_eq!(t.output_start(), expected);
                assert_eq!(
                    t.presentation().output_end(),
                    if enabled {
                        None
                    } else {
                        Some(clock.output_frames())
                    }
                );
                assert_eq!(t.presentation().is_cyclic(), enabled);
                let r = LoopRequest {
                    output_start: expected,
                    enabled,
                    seek_target: Some(target),
                };
                assert_eq!(r.traversal(clock).unwrap(), t);
                assert!(
                    LoopRequest {
                        output_start: expected + 1,
                        ..r
                    }
                    .traversal(clock)
                    .is_err()
                );
            }
        }
        assert!(StreamTraversal::seek(clock, frames + 1, true).is_err());
        let end = clock.output_frames();
        assert_ne!(
            StreamTraversal::seek(clock, frames, false).unwrap(),
            StreamTraversal::cutover(clock, end, false).unwrap()
        );
    }
}

#[test]
fn rejected_cyclic_seek_retains_pcm_clock_mode_and_play_state() {
    let (mut p, source, _tx, mut requests) = setup(true, false);
    p.process(&mut [0.0; 137]);
    p.pause();
    p.drain_events().for_each(drop);
    let before = (p.shared.position_frames(), p.source.samples().to_vec());
    p.seek(0.375);
    requests
        .try_recv()
        .unwrap()
        .reply
        .send(Reply::Rejected(shared::error::ErrorCode::InvalidArgument))
        .unwrap_or_else(|_| panic!("closed"));
    p.poll_stream();
    assert_eq!(p.shared.position_frames(), before.0);
    assert_eq!(p.source.samples(), before.1);
    assert!(p.shared.loop_enabled());
    assert_eq!(p.shared.state(), PlaybackState::Paused);
    assert!(!source.is_cancelled());
    assert_eq!(
        p.drain_events()
            .filter(|e| e.event_type == InnerAudioEventType::Error)
            .count(),
        1
    );
}

#[test]
fn pending_cyclic_seek_poll_and_mix_have_no_steady_heap_activity() {
    use migo_alloc_probe::{Burst, assert_no_steady_state_allocation};
    let (mut p, _source, _tx, mut requests) = setup(true, false);
    p.seek(0.375);
    let _command = requests.try_recv().unwrap();
    assert_no_steady_state_allocation(
        Burst {
            path: "pending cyclic seek",
            warmup: 64,
            measured: 256,
        },
        |_| {
            p.poll_stream();
            let mut out = [0.125; 16];
            p.process(&mut out);
            assert_eq!(out, [0.125; 16]);
            p.drain_events().for_each(drop);
        },
    );
}

#[test]
fn repeated_stop_restart_keeps_continuous_mode_and_never_announces_seek() {
    let (mut p, source, tx, mut requests) = setup(true, false);
    let mut revision = StreamRevision::INITIAL;
    for _ in 0..3 {
        p.process(&mut [0.0; 137]);
        p.stop();
        p.play();
        p.drain_events().for_each(drop);
        let command = requests.try_recv().unwrap();
        let request = command.cutover.unwrap();
        assert_eq!(request.output_start, 0);
        assert!(request.enabled);
        assert!(request.seek_target.is_none());
        revision = revision.checked_next().unwrap();
        let mut ack = prepared(command, &source, revision, |_| {});
        p.poll_stream();
        assert!(ack.try_recv().is_ok());
        assert_eq!(p.shared.position_frames(), 0);
        assert!(p.shared.loop_enabled());
        send(&tx, revision, 24000, 0.75);
        p.poll_stream();
        assert!(p.process(&mut [0.0; 16]));
        assert!(!p.drain_events().any(|e| matches!(
            e.event_type,
            InnerAudioEventType::Seeking
                | InnerAudioEventType::Seeked
                | InnerAudioEventType::Error
                | InnerAudioEventType::Ended
        )));
    }
}
