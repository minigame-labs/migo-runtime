//! Repeated PCM has a single-track duration and an exact rational media phase.
use super::*;
use crate::streaming::{
    PcmPool, StreamPresentation, StreamPublication, StreamTimeline, stream_channel,
};
use tokio::sync::mpsc;

fn oracle(frame: u64, frames: u64, input: u32, output: u32) -> f64 {
    // Split seconds and subsecond frames before converting to input units.
    // This differs from the implementation's one modulo of a scaled product.
    let whole = u128::from(frame / u64::from(output)) * u128::from(input);
    let fraction = u128::from(frame % u64::from(output)) * u128::from(input);
    let input_whole = whole + fraction / u128::from(output);
    ((input_whole % u128::from(frames)) as f64
        + (fraction % u128::from(output)) as f64 / f64::from(output))
        / f64::from(input)
}
fn source(
    presentation: StreamPresentation,
    samples: usize,
    done: bool,
) -> (
    InnerAudioPlayer,
    Arc<StreamingState>,
    mpsc::Sender<StreamMsg>,
) {
    source_with_retention(presentation, samples, done, true)
}
fn source_with_retention(
    presentation: StreamPresentation,
    samples: usize,
    done: bool,
    retain: bool,
) -> (
    InnerAudioPlayer,
    Arc<StreamingState>,
    mpsc::Sender<StreamMsg>,
) {
    let state = if retain {
        StreamingState::new_reusable()
    } else {
        StreamingState::new()
    };
    state.bind_presentation(presentation).unwrap();
    let (tx, rx) = stream_channel();
    let mut p = InnerAudioPlayer::new(920, 1);
    p.start_streaming("cyclic media clock".into(), rx, state.clone())
        .unwrap();
    assert!(
        tx.try_send(StreamMsg::Ready {
            revision: StreamRevision::INITIAL,
            sample_rate: presentation.timeline().format().0,
            channels: 1
        })
        .is_ok()
    );
    let mut pool = PcmPool::new();
    let mut pcm = pool.take();
    pcm.buffer_mut().resize(samples, 0.25);
    assert!(
        tx.try_send(StreamMsg::Samples {
            revision: StreamRevision::INITIAL,
            pcm
        })
        .is_ok()
    );
    if done {
        assert!(
            tx.try_send(StreamMsg::Done {
                revision: StreamRevision::INITIAL
            })
            .is_ok()
        );
    }
    p.poll_stream();
    (p, state, tx)
}
fn publish(publication: &mut StreamPublication, samples: usize) -> mpsc::Receiver<StreamMsg> {
    let (tx, rx) = stream_channel();
    let mut pool = PcmPool::new();
    let mut pcm = pool.take();
    pcm.buffer_mut().resize(samples, 0.25);
    let mut pending = publication.prepare_chunk(pcm, 48000, 1).unwrap();
    while publication.has_pending(&pending) {
        publication.publish_next(&mut pending, tx.try_reserve().unwrap());
    }
    rx
}

#[test]
fn rational_loop_time_does_not_accumulate_rounded_cycles_or_float_modulo() {
    for output in [16000, 44100, 48000] {
        let t = StreamTimeline::new(17, 48000, output, 1).unwrap();
        let p = StreamPresentation::repeated(t, None).unwrap();
        for frame in [
            0,
            1,
            16,
            257,
            10001,
            1_000_000_001,
            9_007_199_254_740_993,
            u64::MAX,
        ] {
            assert!((p.seconds_at(frame) - oracle(frame, 17, 48000, output)).abs() < 1e-18);
            assert!(p.seconds_at(frame) < t.seconds());
        }
        let finite = StreamPresentation::repeated(t, Some(257)).unwrap();
        let total = (17_u128 * 257 * output as u128).div_ceil(48000) as u64;
        assert_eq!(finite.output_end(), Some(total));
        assert_eq!(finite.seconds_at(total), t.seconds());
        if output != 48000 {
            assert_ne!(total, t.output_frames() * 257);
        }
    }
}

#[test]
fn finite_cycles_keep_one_duration_and_emit_one_ended_without_local_recycling() {
    let t = StreamTimeline::new(147, 44100, 48000, 1).unwrap();
    let presentation = StreamPresentation::repeated(t, Some(7)).unwrap();
    let total = presentation.output_end().unwrap() as usize;
    let (mut p, _state, _tx) = source(presentation, total, true);
    assert!(p.shared.is_loaded());
    assert_eq!(p.shared.duration(), t.seconds());
    assert_eq!(p.shared.duration_frames(), 160);
    p.shared.set_loop_enabled(true);
    p.play();
    let mut output = vec![0.0; total + 17];
    assert!(!p.process(&mut output));
    assert_eq!(&output[..total], vec![0.25; total]);
    assert_eq!(&output[total..], [0.0; 17]);
    let ended: Vec<_> = p
        .drain_events()
        .filter(|e| e.event_type == InnerAudioEventType::Ended)
        .collect();
    assert_eq!(ended.len(), 1);
    assert_eq!(ended[0].duration, t.seconds());
    assert_eq!(ended[0].current_time, t.seconds());
    p.process(&mut output);
    assert!(
        !p.drain_events()
            .any(|e| e.event_type == InnerAudioEventType::Ended)
    );
    assert_eq!(p.shared.current_time(), 0.0);
}

#[test]
fn continuous_media_time_wraps_while_raw_pcm_progress_remains_monotonic() {
    let t = StreamTimeline::new(17, 48000, 44100, 1).unwrap();
    let presentation = StreamPresentation::repeated(t, None).unwrap();
    let (mut p, _state, _tx) = source(presentation, 24000, false);
    p.play();
    for size in [1, 7, 137, 512, 10001] {
        let before = p.shared.position_frames();
        let mut output = vec![0.0; size];
        assert!(p.process(&mut output));
        assert_eq!(output, vec![0.25; size]);
        let frame = p.shared.position_frames();
        assert_eq!(frame, before + size as u64);
        assert!((p.shared.current_time() - oracle(frame, 17, 48000, 44100)).abs() < 1e-18);
        assert_eq!(p.shared.duration(), t.seconds());
        assert!(
            !p.drain_events()
                .any(|e| e.event_type == InnerAudioEventType::Ended)
        );
    }
    p.pause();
    let time = p.shared.current_time();
    let frame = p.shared.position_frames();
    let mut bus = [0.75; 8];
    p.process(&mut bus);
    assert_eq!(bus, [0.75; 8]);
    assert_eq!(p.shared.current_time(), time);
    assert_eq!(p.shared.position_frames(), frame);
}

#[test]
fn continuous_done_and_finite_short_or_excess_pcm_are_rejected() {
    let t = StreamTimeline::new(147, 44100, 48000, 1).unwrap();
    let p = StreamPresentation::repeated(t, None).unwrap();
    let (mut player, state, tx) = source(p, 24000, false);
    assert!(
        tx.try_send(StreamMsg::Done {
            revision: StreamRevision::INITIAL
        })
        .is_ok()
    );
    player.poll_stream();
    assert!(state.is_cancelled());
    assert!(!player.shared.is_loaded());
    assert_eq!(
        player
            .drain_events()
            .filter(|e| e.event_type == InnerAudioEventType::Error)
            .count(),
        1
    );
    let mut ledger = StreamPublication::default();
    ledger.bind_presentation(p, 0).unwrap();
    let _rx = publish(&mut ledger, 320);
    assert!(ledger.validate_finish(true, 0).is_err());
    let finite = StreamPresentation::repeated(t, Some(3)).unwrap();
    let mut ledger = StreamPublication::default();
    ledger.bind_presentation(finite, 0).unwrap();
    let _rx = publish(&mut ledger, 479);
    assert!(ledger.validate_finish(true, 0).is_err());
    let mut pool = PcmPool::new();
    let mut pcm = pool.take();
    pcm.buffer_mut().resize(2, 0.25);
    assert!(ledger.prepare_chunk(pcm, 48000, 1).is_err());
    let mut ledger = StreamPublication::default();
    ledger.bind_presentation(finite, 480).unwrap();
    let _rx = publish(&mut ledger, 0);
    assert!(ledger.validate_finish(true, 0).is_ok());
}

#[test]
fn rejected_mode_rebinding_preserves_the_unbounded_extent_and_source_identity() {
    let t = StreamTimeline::new(147, 44100, 48000, 1).unwrap();
    let p = StreamPresentation::repeated(t, None).unwrap();
    let state = StreamingState::new_reusable();
    state.bind_presentation(p).unwrap();
    assert!(
        state
            .bind_presentation(StreamPresentation::single(t))
            .is_err()
    );
    assert_eq!(state.presentation(), Some(p));
    let mut ledger = StreamPublication::default();
    ledger.bind_presentation(p, 0).unwrap();
    assert!(
        ledger
            .bind_presentation(StreamPresentation::single(t), 0)
            .is_err()
    );
    assert!(ledger.bind_timeline(t, 0).is_err());
    let _rx = publish(&mut ledger, 321);
    assert!(ledger.validate_finish(true, 0).is_err());
}

#[test]
fn replacing_a_cyclic_source_clears_its_atomic_media_time_and_cache_handoff() {
    let t = StreamTimeline::new(17, 48000, 44100, 1).unwrap();
    let p = StreamPresentation::repeated(t, Some(257)).unwrap();
    let (mut player, _state, _tx) =
        source_with_retention(p, p.output_end().unwrap() as usize, true, false);
    player.play();
    player.process(&mut [0.0; 137]);
    assert_ne!(player.shared.current_time(), 137.0 / 44100.0);
    assert!(player.take_streamed_audio_with_source().is_none());
    player
        .load_audio(DecodedAudio {
            samples: vec![0.5; 32],
            sample_rate: 48000,
            channels: 1,
        })
        .unwrap();
    assert_eq!(player.shared.current_time(), 0.0);
    player.shared.set_position_frames(1);
    assert_eq!(player.shared.current_time(), 1.0 / 48000.0);
}

#[test]
fn cyclic_extent_constructors_reject_zero_overflow_and_invalid_start() {
    let t = StreamTimeline::new(17, 48000, 44100, 1).unwrap();
    assert!(StreamPresentation::repeated(t, Some(0)).is_err());
    assert!(StreamPresentation::repeated(t, Some(u64::MAX)).is_err());
    let p = StreamPresentation::repeated(t, Some(257)).unwrap();
    let mut ledger = StreamPublication::default();
    assert!(
        ledger
            .bind_presentation(p, p.output_end().unwrap() + 1)
            .is_err()
    );
    let t = StreamTimeline::new(1, 16000, 16000, 1).unwrap();
    let p = StreamPresentation::repeated(t, Some(u64::MAX)).unwrap();
    assert_eq!(p.output_end(), Some(u64::MAX));
    assert_eq!(p.seconds_at(u64::MAX - 1), 0.0);
    assert_eq!(p.seconds_at(u64::MAX), t.seconds());
}

#[test]
fn continuous_clock_publication_and_mix_have_no_steady_heap_activity() {
    use migo_alloc_probe::{Burst, assert_no_steady_state_allocation};
    let t = StreamTimeline::new(17, 48000, 44100, 1).unwrap();
    let (mut p, _state, _tx) = source(StreamPresentation::repeated(t, None).unwrap(), 24000, false);
    p.play();
    p.drain_events().for_each(drop);
    let mut output = [0.0; 8];
    assert_no_steady_state_allocation(
        Burst {
            path: "cyclic media clock and mix",
            warmup: 64,
            measured: 256,
        },
        |_| {
            output.fill(0.0);
            p.process(&mut output);
            assert_eq!(output, [0.25; 8]);
            p.drain_events().for_each(drop);
        },
    );
}
