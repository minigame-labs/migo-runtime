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
    while ended == 0 {
        assert!(
            start.elapsed() < Duration::from_secs(60),
            "streaming player stalled"
        );
        player.poll_stream();
        peak = peak.max(player.physical_pcm_bytes());
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
            tokio::time::sleep(Duration::from_millis(50)).await;
            let count = steps.load(Ordering::Acquire);
            tokio::time::sleep(Duration::from_millis(50)).await;
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
                InnerAudioEventType::Ended => ended += 1,
                InnerAudioEventType::Error => panic!("streaming player Error at {rendered}"),
                _ => {}
            }
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
    assert_eq!(shared.duration_frames(), frames as u64);
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
        "W09_SOURCE_PLAYER {{\"asset\":\"{name}\",\"buffered\":{B},\"rate\":{target},\"frames\":{frames},\"peak_pcm_bytes\":{peak},\"pauses\":{pauses}}}"
    );
}
