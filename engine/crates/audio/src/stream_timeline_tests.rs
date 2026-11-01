//! An authoritative track duration must not masquerade as buffered PCM.
use super::*;
use crate::streaming::{PcmPool, StreamTimeline, stream_channel};

fn setup(
    frames: u64,
    input: u32,
    output: u32,
) -> (
    InnerAudioPlayer,
    tokio::sync::mpsc::Sender<StreamMsg>,
    Arc<StreamingState>,
    PcmPool,
) {
    let state = StreamingState::new_reusable();
    state
        .bind_timeline(StreamTimeline::new(frames, input, output, 1).unwrap())
        .unwrap();
    let (tx, rx) = stream_channel();
    let mut player = InnerAudioPlayer::new(803, 1);
    player
        .start_streaming("timeline".into(), rx, state.clone())
        .unwrap();
    ready(&tx, StreamRevision::INITIAL, output);
    player.poll_stream();
    (player, tx, state, PcmPool::new())
}
fn ready(tx: &tokio::sync::mpsc::Sender<StreamMsg>, revision: StreamRevision, rate: u32) {
    assert!(
        tx.try_send(StreamMsg::Ready {
            revision,
            sample_rate: rate,
            channels: 1
        })
        .is_ok()
    );
}
fn samples(
    tx: &tokio::sync::mpsc::Sender<StreamMsg>,
    pool: &mut PcmPool,
    revision: StreamRevision,
    frames: usize,
) {
    let mut pcm = pool.take();
    pcm.buffer_mut().resize(frames, 0.25);
    assert!(tx.try_send(StreamMsg::Samples { revision, pcm }).is_ok());
}
fn done(tx: &tokio::sync::mpsc::Sender<StreamMsg>, revision: StreamRevision) {
    assert!(tx.try_send(StreamMsg::Done { revision }).is_ok());
}
#[test]
fn rational_positions_keep_phase_and_large_frame_precision() {
    for (input, output) in [
        (44100, 48000),
        (48000, 44100),
        (3000, 768000),
        (768000, 3000),
        (48000, 48000),
    ] {
        let frames = (1_u64 << 53) + 137;
        let t = StreamTimeline::new(frames, input, output, 1).unwrap();
        for n in [0, 1, 137, frames / 2, frames - 1, frames] {
            let p = t.position(n).unwrap();
            assert!(p.phase < p.divisor);
            assert_eq!(
                u128::from(p.frame) * u128::from(p.divisor) + u128::from(p.phase),
                u128::from(n) * u128::from(output)
            );
        }
        let whole = u128::from(frames / u64::from(input)) * u128::from(output);
        let tail = u128::from(frames % u64::from(input)) * u128::from(output);
        assert_eq!(
            u128::from(t.output_frames()),
            whole + tail / u128::from(input) + u128::from(!tail.is_multiple_of(u128::from(input)))
        );
        assert!(t.position(frames + 1).is_err());
    }
}
#[test]
fn timeline_validates_formats_overflow_and_source_immutability() {
    for (frames, input, output, ch) in [
        (0, 48000, 48000, 1),
        (1, 0, 48000, 1),
        (1, 48000, 0, 1),
        (1, 48000, 48000, 0),
        (1, 48000, 48000, 33),
        (u64::MAX, 3000, 768000, 1),
    ] {
        assert!(StreamTimeline::new(frames, input, output, ch).is_err());
    }
    let state = StreamingState::new_reusable();
    let first = StreamTimeline::new(9737, 44100, 48000, 2).unwrap();
    state.bind_timeline(first).unwrap();
    state.bind_timeline(first).unwrap();
    assert!(
        state
            .bind_timeline(StreamTimeline::new(9738, 44100, 48000, 2).unwrap())
            .is_err()
    );
    assert_eq!(state.timeline(), Some(first));
    let retired = StreamingState::new();
    retired.cancel();
    assert!(retired.bind_timeline(first).is_err());
    assert_eq!(retired.timeline(), None);
}
#[test]
fn exact_duration_is_available_before_pcm_and_survives_output_rounding() {
    let (mut p, tx, _, mut pool) = setup(3, 44100, 48000);
    assert_eq!(p.shared.duration_frames(), 4);
    assert_eq!(p.shared.duration(), 3.0 / 44100.0);
    assert_eq!(p.stream_decoded_end(), 0);
    samples(&tx, &mut pool, StreamRevision::INITIAL, 4);
    done(&tx, StreamRevision::INITIAL);
    p.poll_stream();
    assert_eq!(p.shared.duration(), 3.0 / 44100.0);
    p.play();
    let mut output = [0.0; 4];
    p.process(&mut output);
    let ended = p
        .drain_events()
        .find(|e| e.event_type == InnerAudioEventType::Ended)
        .unwrap();
    assert_eq!(ended.current_time, 3.0 / 44100.0);
    p.seek(p.shared.duration());
    assert_eq!(p.shared.position_frames(), 4);
    assert_eq!(p.shared.current_time(), p.shared.duration());
}
#[test]
fn known_duration_does_not_finish_forward_seek_before_fresh_pcm() {
    let (mut p, tx, _, mut pool) = setup(80000, 8000, 8000);
    samples(&tx, &mut pool, StreamRevision::INITIAL, 4000);
    p.poll_stream();
    p.drain_events().for_each(drop);
    p.seek(2.0);
    assert_eq!(p.pending_seek, Some(16000));
    assert_eq!(p.shared.duration(), 10.0);
    samples(&tx, &mut pool, StreamRevision::INITIAL, 10000);
    p.poll_stream();
    assert_eq!(p.pending_seek, Some(16000));
    assert_eq!(p.stream_decoded_end(), 14000);
    assert!(
        !p.drain_events()
            .any(|e| e.event_type == InnerAudioEventType::Seeked)
    );
    samples(&tx, &mut pool, StreamRevision::INITIAL, 3000);
    p.poll_stream();
    assert_eq!(p.pending_seek, None);
    assert_eq!(p.stream_decoded_end(), 17000);
    assert_eq!(p.shared.duration_frames(), 80000);
    assert_eq!(
        p.drain_events()
            .filter(|e| e.event_type == InnerAudioEventType::Seeked)
            .count(),
        1
    );
}
#[test]
fn accepted_revision_keeps_total_duration_and_waits_for_its_tail() {
    let (mut p, tx, state, mut pool) = setup(8000, 8000, 8000);
    samples(&tx, &mut pool, StreamRevision::INITIAL, 8000);
    done(&tx, StreamRevision::INITIAL);
    p.poll_stream();
    p.drain_events().for_each(drop);
    let next = StreamRevision::INITIAL.checked_next().unwrap();
    assert!(p.accept_stream_revision(&state, next, 8001).is_err());
    p.accept_stream_revision(&state, next, 2000).unwrap();
    assert_eq!(p.shared.duration(), 1.0);
    assert_eq!(p.stream_decoded_end(), 2000);
    ready(&tx, next, 8000);
    samples(&tx, &mut pool, next, 6000);
    done(&tx, next);
    p.poll_stream();
    assert!(p.is_stream_complete());
    assert_eq!(p.shared.duration_frames(), 8000);
    assert_eq!(p.shared.duration(), 1.0);
    assert_eq!(
        p.drain_events()
            .filter(|e| e.event_type == InnerAudioEventType::Seeked)
            .count(),
        1
    );
}
#[test]
fn declared_duration_rejects_short_eof_and_excess_pcm() {
    for frames in [3999, 8001] {
        let (mut p, tx, state, mut pool) = setup(8000, 8000, 8000);
        samples(&tx, &mut pool, StreamRevision::INITIAL, frames);
        if frames < 8000 {
            done(&tx, StreamRevision::INITIAL);
        }
        p.poll_stream();
        assert!(state.is_cancelled());
        assert_eq!(p.physical_pcm_bytes(), 0);
        assert!(!p.shared.is_loaded());
        assert!(
            p.drain_events()
                .any(|e| e.event_type == InnerAudioEventType::Error)
        );
    }
}
#[test]
fn new_source_retires_exact_duration_and_old_source_cannot_rebind() {
    let (mut p, _tx, old, mut pool) = setup(8000, 8000, 8000);
    let state = StreamingState::new();
    let (tx, rx) = stream_channel();
    p.start_streaming("unknown replacement".into(), rx, state)
        .unwrap();
    assert_eq!(p.shared.duration(), 0.0);
    ready(&tx, StreamRevision::INITIAL, 4000);
    samples(&tx, &mut pool, StreamRevision::INITIAL, 2000);
    p.poll_stream();
    assert_eq!(p.shared.duration(), 0.5);
    assert!(old.is_cancelled());
    assert!(
        old.bind_timeline(StreamTimeline::new(10000, 8000, 8000, 1).unwrap())
            .is_err()
    );
}

#[test]
fn ready_format_is_checked_after_revision_filtering() {
    let (mut p, tx, state, _) = setup(8000, 8000, 8000);
    ready(&tx, StreamRevision::INITIAL.checked_next().unwrap(), 48000);
    p.poll_stream();
    assert!(!state.is_cancelled());
    assert_eq!(p.shared.sample_rate(), 8000);
    assert_eq!(p.shared.duration(), 1.0);
    ready(&tx, StreamRevision::INITIAL, 48000);
    p.poll_stream();
    assert!(state.is_cancelled());
    assert!(
        p.drain_events()
            .any(|e| e.event_type == InnerAudioEventType::Error)
    );
}

#[test]
fn cache_handoff_and_reload_keep_the_original_media_duration() {
    let state = StreamingState::new();
    state
        .bind_timeline(StreamTimeline::new(3, 44100, 48000, 1).unwrap())
        .unwrap();
    let (tx, rx) = stream_channel();
    let mut p = InnerAudioPlayer::new(904, 1);
    p.start_streaming("clock cache".into(), rx, state).unwrap();
    let mut pool = PcmPool::new();
    ready(&tx, StreamRevision::INITIAL, 48000);
    samples(&tx, &mut pool, StreamRevision::INITIAL, 4);
    done(&tx, StreamRevision::INITIAL);
    p.poll_stream();
    assert_eq!(p.shared.duration(), 3.0 / 44100.0);
    assert!(
        p.take_streamed_audio().is_none(),
        "PCM-only cache API must not discard the clock"
    );
    let (audio, permit, timeline) = p.take_streamed_audio_with_source().unwrap();
    let cache = crate::cache::GlobalAudioCache::new();
    let cached = cache
        .insert_with_source("clock cache".into(), audio, permit, timeline)
        .unwrap();
    p.attach_cached_backing(cached.clone());
    assert_eq!(p.shared.duration(), 3.0 / 44100.0);
    let mut next = InnerAudioPlayer::new(905, 1);
    let load = next.begin_load().unwrap();
    next.complete_cached_load(load, cached).unwrap();
    assert_eq!(next.shared.duration(), 3.0 / 44100.0);
    next.seek(next.shared.duration());
    assert_eq!(next.shared.position_frames(), 4);
}

#[test]
fn cache_rejects_clock_that_does_not_describe_owned_pcm() {
    let cache = crate::cache::GlobalAudioCache::new();
    for (samples, rate, ch, clock) in [
        (
            4,
            48000,
            1,
            StreamTimeline::new(5, 44100, 48000, 1).unwrap(),
        ),
        (
            4,
            48000,
            1,
            StreamTimeline::new(4, 48000, 44100, 1).unwrap(),
        ),
        (
            4,
            48000,
            1,
            StreamTimeline::new(4, 48000, 48000, 2).unwrap(),
        ),
        (
            5,
            48000,
            2,
            StreamTimeline::new(2, 48000, 48000, 2).unwrap(),
        ),
    ] {
        let ledger = Arc::new(AudioAggregateLedger::new(1024));
        let audio = DecodedAudio {
            samples: vec![0.0; samples],
            sample_rate: rate,
            channels: ch,
        };
        let permit = ledger
            .try_reserve(audio.samples.capacity() * 4, "bad cache clock")
            .unwrap();
        assert!(
            cache
                .insert_with_source("bad clock".into(), audio, permit, Some(clock))
                .is_err()
        );
        assert!(cache.get("bad clock").is_none());
        assert_eq!(ledger.used_bytes(), 0);
    }
}
