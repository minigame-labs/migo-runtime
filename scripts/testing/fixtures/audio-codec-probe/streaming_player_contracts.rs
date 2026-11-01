//! Shared real-player PCM/pause/Ended assertions for every source and codec.
use crate::streaming::{StreamMsg, StreamingState};
use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::{sync::mpsc, time::Instant};
pub(crate) struct PlaybackCase<'a> {
    pub name: &'a str,
    pub source_name: String,
    pub target: u32,
    pub pause: bool,
    pub long: bool,
    pub expected: Vec<f32>,
    pub channels: u32,
    pub frames: usize,
}
pub(crate) struct StreamInput {
    pub state: Arc<StreamingState>,
    pub steps: Arc<AtomicUsize>,
    pub task: tokio::task::JoinHandle<()>,
    pub rx: mpsc::Receiver<StreamMsg>,
}
pub(crate) async fn consume_player<const B: bool>(case: PlaybackCase<'_>, input: StreamInput) {
    consume::<B>(case, input, false).await;
}

pub(crate) async fn consume_forward_player<const B: bool>(
    case: PlaybackCase<'_>,
    input: StreamInput,
) {
    consume::<B>(case, input, true).await;
}

async fn consume<const B: bool>(case: PlaybackCase<'_>, input: StreamInput, forward: bool) {
    let PlaybackCase {
        name,
        source_name,
        target,
        pause,
        long,
        expected,
        channels,
        frames,
    } = case;
    let StreamInput {
        state,
        steps,
        task,
        rx,
    } = input;
    use crate::inner_audio::{InnerAudioPlayer, PlaybackState};
    use shared::protocol::audio_cmd::InnerAudioEventType;
    let mut player = InnerAudioPlayer::new(1, channels);
    let shared = player.shared.clone();
    shared.set_autoplay(true);
    player
        .start_streaming(source_name, rx, state.clone())
        .unwrap();
    let mut output = vec![0.0; 512 * channels as usize];
    let (mut rendered, mut ended, mut canplay, mut pauses) = (0, 0, 0, 0);
    let start = Instant::now();
    let mut peak = 0;
    let (mut sought, mut seeked) = (false, 0);
    while ended == 0 {
        assert!(
            start.elapsed() < Duration::from_secs(60),
            "streaming player stalled"
        );
        player.poll_stream();
        peak = peak.max(player.physical_pcm_bytes());
        if forward && !sought && shared.is_loaded() {
            assert!(shared.duration_frames() < 2 * target as u64);
            assert!(frames > 2 * target as usize + 512);
            assert!(!player.is_stream_complete());
            player.pause();
            player.seek(2.0);
            rendered = 2 * target as usize;
            sought = true;
        }
        if pause && pauses == 0 && rendered >= target as usize {
            player.pause();
            let pos = shared.position_frames();
            for _ in 0..2000 {
                player.poll_stream();
                let mut bus = [0.125; 8];
                player.process(&mut bus);
                assert_eq!(bus, [0.125; 8]);
                assert_eq!(shared.position_frames(), pos);
                if shared.duration_frames().saturating_sub(pos) >= target as u64 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
            // Observe the producer blocked on a full publication queue after
            // returning its CPU owner. Codec-only and sinc-only steps need
            // not produce PCM, so queue saturation alone is not this edge.
            let fill_started = Instant::now();
            while (player.queued_stream_messages() < crate::streaming::STREAM_CHANNEL_CAPACITY
                || !state.publication_waiting.load(Ordering::Acquire))
                && !state.download_complete.load(Ordering::Acquire)
            {
                assert!(fill_started.elapsed() < Duration::from_secs(10));
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
            let count = steps.load(Ordering::Acquire);
            for _ in 0..50 {
                player.poll_stream();
                let mut bus = [0.125; 8];
                player.process(&mut bus);
                assert_eq!(bus, [0.125; 8]);
                assert_eq!(shared.position_frames(), pos);
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
            // No CPU transfer is in flight at the observed publication wait.
            assert_eq!(
                steps.load(Ordering::Acquire),
                count,
                "paused player drained producer"
            );
            assert!(
                shared.duration_frames().saturating_sub(pos)
                    <= target as u64 + 32768 / channels as u64
            );
            player.play();
            pauses += 1;
        }
        let before = shared.position_frames();
        output.fill(0.0);
        let active = player.process(&mut output);
        let after = shared.position_frames();
        assert_cyclic_media_time(&shared, &state);
        let count = if active {
            (after - before) as usize
        } else if shared.state() == PlaybackState::Stopped && shared.is_loaded() {
            frames.checked_sub(rendered).unwrap()
        } else {
            0
        };
        assert!(count <= 512);
        let len = count * channels as usize;
        if long {
            assert!(output[..len].iter().all(|v| v.abs() < 1e-7));
        } else {
            assert_eq!(
                &output[..len],
                &expected[rendered * channels as usize..(rendered + count) * channels as usize]
            );
        }
        assert!(output[len..].iter().all(|v| *v == 0.0));
        rendered += count;
        for event in player.drain_events() {
            match event.event_type {
                InnerAudioEventType::CanPlay => canplay += 1,
                InnerAudioEventType::Ended => {
                    ended += 1;
                    if let Some(presentation) = state.presentation().filter(|p| p.is_cyclic()) {
                        assert_eq!(event.current_time, presentation.timeline().seconds());
                        assert_eq!(event.duration, presentation.timeline().seconds());
                    }
                }
                InnerAudioEventType::Error => panic!("streaming player Error at {rendered}"),
                InnerAudioEventType::Seeked if forward => {
                    assert!(event.paused);
                    assert_eq!(event.current_time, 2.0);
                    seeked += 1;
                }
                _ => {}
            }
        }
        if forward && seeked == 1 && shared.state() == PlaybackState::Paused {
            player.play();
        }
        if count == 0 {
            tokio::time::sleep(Duration::from_millis(1)).await;
        } else {
            tokio::task::yield_now().await;
        }
    }
    task.await.unwrap();
    assert_eq!((rendered, canplay, ended), (frames, 1, 1));
    assert_eq!(pauses, usize::from(pause));
    assert_eq!(sought, forward);
    assert_eq!(seeked, usize::from(forward));
    assert_eq!(
        shared.duration_frames(),
        state
            .timeline()
            .map_or(frames as u64, |t| t.output_frames())
    );
    assert!(player.is_stream_complete());
    assert!(state.download_complete.load(Ordering::Acquire));
    player.process(&mut output);
    assert!(
        !player
            .drain_events()
            .any(|e| e.event_type == InnerAudioEventType::Ended)
    );
    assert!(peak < 4 * 1024 * 1024);
    println!(
        "W09_SOURCE_PLAYER {{\"asset\":\"{name}\",\"buffered\":{B},\"rate\":{target},\"frames\":{frames},\"peak_pcm_bytes\":{peak},\"pauses\":{pauses},\"forward_seek\":{forward}}}"
    );
}

pub(crate) fn assert_cyclic_media_time(
    shared: &crate::inner_audio::InnerAudioSharedState,
    state: &crate::streaming::StreamingState,
) {
    if let Some(presentation) = state
        .presentation()
        .filter(|p| p.is_cyclic() && shared.is_loaded())
    {
        let after = shared.position_frames();
        let clock = presentation.timeline();
        let input = clock.position(0).unwrap().divisor as u128;
        let output = clock.format().0 as u128;
        let frames = clock.seek_frame(clock.seconds()).unwrap() as u128;
        let seconds = after as u128 / output;
        let sub = (after as u128 % output) * input;
        let whole = seconds * input + sub / output;
        let expected = if presentation.output_end().is_some_and(|end| after >= end) {
            clock.seconds()
        } else {
            ((whole % frames) as f64 + (sub % output) as f64 / output as f64) / input as f64
        };
        assert!(
            (shared.current_time() - expected).abs() < 1e-14,
            "cyclic media time at {after}: {} != {expected}",
            shared.current_time()
        );
        assert_eq!(shared.duration(), clock.seconds());
    }
}
