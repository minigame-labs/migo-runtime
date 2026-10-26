//! Actual producer, player, pause/resume and ended boundary, without a device.
use super::*;
use crate::{
    inner_audio::{InnerAudioPlayer, PlaybackState},
    resampled_streaming::Batched,
};
use shared::protocol::audio_cmd::InnerAudioEventType;

#[derive(Default)]
struct Counters {
    steps: AtomicUsize,
    step_allocations: AtomicU64,
}
struct Counted<D>(D, Arc<Counters>);
impl<D: Decoder> Decoder for Counted<D> {
    fn step(
        &mut self,
        input: &[u8],
        eof: bool,
        pcm: &mut Vec<f32>,
        cancel: &AtomicBool,
    ) -> EngineResult<Step> {
        self.1.steps.fetch_add(1, Ordering::Relaxed);
        let before = migo_alloc_probe::thread_counts().allocation_events();
        let result = self.0.step(input, eof, pcm, cancel);
        let after = migo_alloc_probe::thread_counts().allocation_events();
        self.1
            .step_allocations
            .fetch_add(after - before, Ordering::Relaxed);
        result
    }
}

use std::sync::atomic::AtomicU64;
pub(super) const HASH_SEED: u64 = 0xcbf29ce484222325;
pub(super) fn hash_samples(mut hash: u64, samples: &[f32]) -> u64 {
    for sample in samples {
        hash = (hash ^ sample.to_bits() as u64).wrapping_mul(0x100000001b3);
    }
    hash
}

fn process_cpu_ns() -> u64 {
    let mut value = std::mem::MaybeUninit::<libc::timespec>::uninit();
    // SAFETY: clock_gettime writes one timespec to this valid destination.
    assert_eq!(
        unsafe { libc::clock_gettime(libc::CLOCK_PROCESS_CPUTIME_ID, value.as_mut_ptr()) },
        0
    );
    let value = unsafe { value.assume_init() };
    value.tv_sec as u64 * 1_000_000_000 + value.tv_nsec as u64
}

async fn play<const Q: usize>(
    name: &str,
    target: u32,
    channels: u32,
    frames: usize,
    pause: bool,
    silence: bool,
    client: crate::network::http::HttpClient,
) -> serde_json::Value {
    let data = fixture(name);
    let (url, serving, _) = server(data, 8192, Reply::Chunked).await;
    play_format::<Q, crate::push_decoder::Mp3>(
        name,
        target,
        channels,
        frames,
        pause,
        silence,
        client,
        (url, serving),
    )
    .await
}

pub(super) async fn play_format<const Q: usize, P: crate::push_decoder::incremental::Packetizer>(
    name: &str,
    target: u32,
    channels: u32,
    frames: usize,
    pause: bool,
    silence: bool,
    client: crate::network::http::HttpClient,
    source: (String, tokio::task::JoinHandle<()>),
) -> serde_json::Value {
    let counters = Arc::new(Counters::default());
    let state = StreamingState::new();
    let (url, serving) = source;
    let (tx, rx) = crate::streaming::stream_channel();
    let worker_counters = counters.clone();
    let io_before = migo_alloc_probe::thread_counts().allocation_events();
    let cpu_before = process_cpu_ns();
    let started = Instant::now();
    let task = tokio::spawn(run_on_cpu(
        client,
        url.clone(),
        tx,
        state.clone(),
        move || Counted(Batched::<Q, P>::new(target), worker_counters),
    ));
    let mut player = InnerAudioPlayer::new(1, channels);
    let shared = player.shared.clone();
    shared.set_autoplay(true);
    player.start_streaming(url, rx, state.clone()).unwrap();
    let mut output = vec![0.0; 512 * channels as usize];
    let mut hash = HASH_SEED;
    let mut rendered = 0;
    let mut ended = 0;
    let mut canplay = 0;
    let mut pauses = 0;
    let mut first_sample_ns = None;
    let mut max_pcm_bytes = 0;
    let mut max_ahead_frames = 0;
    while ended == 0 {
        assert!(
            started.elapsed() < Duration::from_secs(45),
            "player made no progress"
        );
        player.poll_stream();
        max_pcm_bytes = max_pcm_bytes.max(player.physical_pcm_bytes());
        let ahead = shared
            .duration_frames()
            .saturating_sub(shared.position_frames());
        max_ahead_frames = max_ahead_frames.max(ahead);
        if pause && pauses == 0 && rendered >= target as usize {
            player.pause();
            let position = shared.position_frames();
            for _ in 0..2048 {
                player.poll_stream();
                let mut bus = [0.125; 8];
                player.process(&mut bus);
                assert_eq!(bus, [0.125; 8]);
                assert_eq!(shared.position_frames(), position);
                if shared.duration_frames().saturating_sub(position) >= target as u64 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
            for _ in 0..16 {
                player.poll_stream();
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
            // Give a submitted CPU step time to complete and its send to block.
            tokio::time::sleep(Duration::from_millis(20)).await;
            player.poll_stream();
            tokio::time::sleep(Duration::from_millis(20)).await;
            let steps = counters.steps.load(Ordering::Acquire);
            tokio::time::sleep(Duration::from_millis(20)).await;
            assert_eq!(
                counters.steps.load(Ordering::Acquire),
                steps,
                "paused player defeated producer backpressure"
            );
            assert!(
                shared.duration_frames().saturating_sub(position)
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
            assert!(after >= before, "stream cursor moved backward");
            (after - before) as usize
        } else if shared.state() == PlaybackState::Stopped && shared.is_loaded() {
            frames
                .checked_sub(rendered)
                .expect("player exceeded declared duration")
        } else {
            0
        };
        assert!(
            count <= 512,
            "ended failed to align with the last PCM quantum"
        );
        if count > 0 {
            first_sample_ns.get_or_insert_with(|| started.elapsed().as_nanos() as u64);
            let pcm = &output[..count * channels as usize];
            if silence {
                assert!(pcm.iter().all(|sample| sample.abs() < 1e-7));
            }
            hash = hash_samples(hash, pcm);
            rendered += count;
        } else {
            assert!(
                output.iter().all(|sample| *sample == 0.0),
                "underflow replayed consumed PCM"
            );
        }
        assert!(
            output[count * channels as usize..]
                .iter()
                .all(|sample| *sample == 0.0)
        );
        for event in player.drain_events() {
            match event.event_type {
                InnerAudioEventType::CanPlay => canplay += 1,
                InnerAudioEventType::Ended => ended += 1,
                InnerAudioEventType::Error => {
                    panic!("stream player errored after {rendered} frames")
                }
                _ => {}
            }
        }
        if count == 0 {
            // No spin loop while the worker/network makes progress. This is a
            // virtual playback consumer with a requested 1ms underflow poll,
            // not a hardware callback or a wall-clock playback latency probe.
            tokio::time::sleep(Duration::from_millis(1)).await;
        } else {
            tokio::task::yield_now().await;
        }
    }
    let wall_ns = started.elapsed().as_nanos() as u64;
    let cpu_ns = process_cpu_ns() - cpu_before;
    let io_allocations = migo_alloc_probe::thread_counts().allocation_events() - io_before;
    assert_eq!((rendered, canplay, ended), (frames, 1, 1));
    assert_eq!(shared.duration_frames(), frames as u64);
    assert!(state.download_complete.load(Ordering::Acquire));
    assert!(player.is_stream_complete());
    player.process(&mut output);
    assert!(
        !player
            .drain_events()
            .any(|event| event.event_type == InnerAudioEventType::Ended)
    );
    finished(task).await;
    finished(serving).await;
    serde_json::json!({"batch":Q,"fixture":name,"target_rate":target,"channels":channels,"frames":frames,
        "pcm_hash":format!("{hash:016x}"),"steps":counters.steps.load(Ordering::Relaxed),
        "wall_ns":wall_ns,"cpu_ns":cpu_ns,"first_pcm_ns":first_sample_ns,
        "io_thread_rust_allocations":io_allocations,"inside_step_rust_allocations":counters.step_allocations.load(Ordering::Relaxed),
        "player_peak_pcm_capacity_bytes":max_pcm_bytes,"max_prefetch_frames":max_ahead_frames,"pauses":pauses})
}

fn reference(name: &str, target: u32) -> (usize, u32, u64) {
    use rubato::{
        Resampler, audioadapter::Adapter, audioadapter_buffers::direct::InterleavedSlice,
    };
    let decoded = crate::candidate::decode(
        &fixture(name),
        crate::limits::MAX_AUDIO_PCM_SAMPLES as usize,
    )
    .unwrap();
    let frames = (decoded.samples.len() as u128 / decoded.channels as u128 * target as u128)
        .div_ceil(decoded.sample_rate as u128) as usize;
    if decoded.sample_rate == target {
        return (
            frames,
            decoded.channels,
            hash_samples(HASH_SEED, &decoded.samples),
        );
    }
    let input = InterleavedSlice::new(
        &decoded.samples,
        decoded.channels as usize,
        decoded.samples.len() / decoded.channels as usize,
    )
    .unwrap();
    let output = crate::resampled_streaming::core(decoded.sample_rate, target, decoded.channels)
        .unwrap()
        .process_all(
            &input,
            decoded.samples.len() / decoded.channels as usize,
            None,
        )
        .unwrap();
    let mut hash = HASH_SEED;
    for frame in 0..frames {
        for channel in 0..decoded.channels as usize {
            hash = hash_samples(hash, &[output.read_sample(channel, frame).unwrap()]);
        }
    }
    (frames, decoded.channels, hash)
}

#[tokio::test(flavor = "current_thread")]
async fn real_player_pause_resume_and_exact_ended_match_the_library() {
    for (name, target) in [
        ("long.mp3", 44100),
        ("long.mp3", 48000),
        ("cbr-44100-1.mp3", 48000),
        ("tiny.mp3", 44100),
        ("mpeg25-8000-2.mp3", 192000),
    ] {
        let (frames, channels, expected) = reference(name, target);
        for batch in [1, 4, 8, 16] {
            let pause = name == "long.mp3";
            let row = match batch {
                1 => play::<1>(name, target, channels, frames, pause, false, client()).await,
                4 => play::<4>(name, target, channels, frames, pause, false, client()).await,
                8 => play::<8>(name, target, channels, frames, pause, false, client()).await,
                16 => play::<16>(name, target, channels, frames, pause, false, client()).await,
                _ => unreachable!(),
            };
            assert_eq!(row["pcm_hash"], format!("{expected:016x}"));
            println!(
                "W09_PLAYER_ORACLE {}",
                serde_json::json!({"fixture":name,"target_rate":target,"channels":channels,"frames":frames,"pcm_hash":format!("{expected:016x}")})
            );
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn long_stream_crosses_the_cache_quota_with_bounded_player_memory() {
    let row = play::<8>(
        "duration-over-cache-cap.mp3",
        8000,
        1,
        17_600_000,
        false,
        true,
        client(),
    )
    .await;
    assert!(row["player_peak_pcm_capacity_bytes"].as_u64().unwrap() < 4 * 1024 * 1024);
    println!("W09_LONG_STREAM {row}");
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "fresh-process benchmark, run only by the measurement driver"]
async fn pipeline_benchmark() {
    let fixture = std::env::var("W09_BENCH_FIXTURE").unwrap();
    let target = std::env::var("W09_BENCH_RATE").unwrap().parse().unwrap();
    let channels = std::env::var("W09_BENCH_CHANNELS")
        .unwrap()
        .parse()
        .unwrap();
    let frames = std::env::var("W09_BENCH_FRAMES").unwrap().parse().unwrap();
    let batch: usize = std::env::var("W09_BENCH_BATCH").unwrap().parse().unwrap();
    let repeats: usize = std::env::var("W09_BENCH_REPEATS")
        .unwrap_or_else(|_| "3".into())
        .parse()
        .unwrap();
    let shared_client = client();
    for iteration in 0..repeats {
        let row = match batch {
            1 => {
                play::<1>(
                    &fixture,
                    target,
                    channels,
                    frames,
                    false,
                    false,
                    shared_client.clone(),
                )
                .await
            }
            4 => {
                play::<4>(
                    &fixture,
                    target,
                    channels,
                    frames,
                    false,
                    false,
                    shared_client.clone(),
                )
                .await
            }
            8 => {
                play::<8>(
                    &fixture,
                    target,
                    channels,
                    frames,
                    false,
                    false,
                    shared_client.clone(),
                )
                .await
            }
            16 => {
                play::<16>(
                    &fixture,
                    target,
                    channels,
                    frames,
                    false,
                    false,
                    shared_client.clone(),
                )
                .await
            }
            _ => panic!("unsupported benchmark batch"),
        };
        println!(
            "W09_PIPELINE {}",
            serde_json::json!({"iteration":iteration,"measurement":row})
        );
    }
}
