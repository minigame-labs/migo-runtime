//! Cyclic filtering is compared with one full render of concatenated PCM.
use super::phase_tests::render;
use super::*;
use crate::streaming_transport::Progress;
use crate::{
    candidate,
    decoder_seek_contracts::{assert_pcm, resample},
    isomp4_source_contracts::fixture,
};

fn sequence<const B: bool>(data: &[u8], rate: u32, cycles: u64, fragment: usize) -> Vec<f32> {
    let mut d =
        Timed::<B>::looping(rate, Some(data.len() as u64), Repetitions::Finite(cycles)).unwrap();
    let pcm = render(&mut d, data, 0, fragment);
    assert_eq!(d.inner.decoded_cycles(), cycles);
    let timeline = d.timeline().unwrap().unwrap();
    assert_eq!(
        pcm.len() as u64,
        timeline.output_frames() * u64::from(timeline.format().1)
    );
    let mut out = Vec::with_capacity(crate::bandlimited::MAX_SCRATCH_SAMPLES);
    assert_eq!(
        d.step(&[], true, &mut out, &AtomicBool::new(false))
            .unwrap()
            .progress,
        Progress::Finished
    );
    assert!(out.is_empty());
    assert_eq!(d.inner.decoded_cycles(), cycles);
    pcm
}
#[test]
fn loop_keeps_one_filter_grid_across_seams() {
    let mut comparisons = 0;
    for name in [
        "0-seek-front.m4a",
        "0-seek-tail.m4a",
        "1-seek-front.m4a",
        "1-seek-tail.m4a",
        "1-seek-scaled.m4a",
        "1-seek-edit.m4a",
    ] {
        let data = fixture(name);
        let full = candidate::decode(&data, 16 * 1024 * 1024).unwrap();
        for cycles in [1, 2, 7] {
            for rate in [16000, 44100, 48000] {
                let expected = resample(
                    &full.samples.repeat(cycles),
                    full.sample_rate,
                    rate,
                    full.channels,
                );
                for fragment in [137, 100000] {
                    for buffered in [false, true] {
                        let pcm = if buffered {
                            sequence::<true>(&data, rate, cycles as u64, fragment)
                        } else {
                            sequence::<false>(&data, rate, cycles as u64, fragment)
                        };
                        assert_pcm(&pcm, &expected);
                        comparisons += 1;
                    }
                }
            }
        }
    }
    let data = fixture("2-seek-front.m4a");
    let full = candidate::decode(&data, 16 * 1024 * 1024).unwrap();
    assert_pcm(
        &sequence::<true>(&data, 44100, 3, 137),
        &resample(
            &full.samples.repeat(3),
            full.sample_rate,
            44100,
            full.channels,
        ),
    );
    assert_eq!(comparisons + 1, 217);
    println!("W09_LOOP_PCM217 whole-concatenation comparisons;no intermediate padding/reset");
}
#[test]
fn tiny_cycles_accumulate_rational_length_instead_of_per_cycle_ceil() {
    let data = fixture("0-seek-front.m4a");
    let full = candidate::decode(&data, 16 * 1024 * 1024).unwrap();
    assert_eq!(full.samples.len(), 17);
    for rate in [16000, 44100, 48000] {
        let expected = resample(&full.samples.repeat(257), 48000, rate, 1);
        assert_pcm(&sequence::<true>(&data, rate, 257, 137), &expected);
        assert_pcm(&sequence::<false>(&data, rate, 257, 137), &expected);
        let per_cycle = (17_u128 * rate as u128).div_ceil(48000);
        if rate != 48000 {
            assert_ne!(expected.len() as u128, per_cycle * 257);
        }
    }
    println!("W09_LOOP_TINY6 exact257-cycle sequences");
}
#[test]
fn loop_configuration_and_clock_are_validated_before_publication() {
    assert!(Timed::<true>::looping(48000, None, Repetitions::Finite(0)).is_err());
    let data = fixture("1-seek-front.m4a");
    let mut d = Timed::<true>::looping(
        44100,
        Some(data.len() as u64),
        Repetitions::Finite(u64::MAX),
    )
    .unwrap();
    let mut out = Vec::with_capacity(crate::bandlimited::MAX_SCRATCH_SAMPLES);
    assert!(
        d.step(&data, false, &mut out, &AtomicBool::new(false))
            .is_err()
    );
    assert!(out.is_empty());
    let error = d
        .step(&[], true, &mut out, &AtomicBool::new(false))
        .err()
        .expect("clock error must retire the decoder");
    assert!(error.to_string().contains("previously failed"));
    assert!(d.source_request().is_none());
    let mut d =
        Timed::<true>::looping(48000, Some(data.len() as u64), Repetitions::Finite(3)).unwrap();
    assert!(d.inner.bind_loop_frames(0).is_err());
    let step = d
        .step(&data, false, &mut out, &AtomicBool::new(false))
        .unwrap();
    assert!(out.is_empty());
    assert!(d.seek(Timestamp::ZERO).is_err());
    let full = candidate::decode(&data, 16 * 1024 * 1024).unwrap();
    assert!(
        d.inner
            .bind_loop_frames((full.samples.len() / full.channels as usize) as u64 + 1)
            .is_err()
    );
    assert_pcm(
        &render(&mut d, &data, step.consumed, 137),
        &resample(
            &full.samples.repeat(3),
            full.sample_rate,
            48000,
            full.channels,
        ),
    );
}
#[test]
fn continuous_cancelled_and_failed_loops_do_not_reopen() {
    let data = fixture("1-seek-front.m4a");
    for bad in [false, true] {
        let mut d = Timed::<true>::looping(48000, Some(data.len() as u64), Repetitions::Continuous)
            .unwrap();
        let mut out = Vec::with_capacity(crate::bandlimited::MAX_SCRATCH_SAMPLES);
        d.step(&data, false, &mut out, &AtomicBool::new(false))
            .unwrap();
        assert!(d.timeline().unwrap().is_none());
        if bad {
            out.resize(crate::bandlimited::MAX_SCRATCH_SAMPLES, 0.0);
            assert!(
                d.step(&[], false, &mut out, &AtomicBool::new(false))
                    .is_err()
            );
            assert!(out.is_empty());
            assert!(
                d.step(&[], true, &mut out, &AtomicBool::new(false))
                    .is_err()
            );
        } else {
            assert_eq!(
                d.step(&[], true, &mut out, &AtomicBool::new(true))
                    .unwrap()
                    .progress,
                Progress::Cancelled
            );
            assert_eq!(
                d.step(&[], true, &mut out, &AtomicBool::new(false))
                    .unwrap()
                    .progress,
                Progress::Cancelled
            );
        }
        assert!(d.seek(Timestamp::ZERO).is_err());
        assert!(d.source_request().is_none());
    }
}
#[test]
fn cached_continuous_loop_steps_have_no_steady_heap_activity() {
    use migo_alloc_probe::{Burst, assert_no_steady_state_allocation};
    let data = fixture("0-seek-front.m4a");
    let mut d =
        Timed::<true>::looping(44100, Some(data.len() as u64), Repetitions::Continuous).unwrap();
    let mut out = Vec::with_capacity(crate::bandlimited::MAX_SCRATCH_SAMPLES);
    let mut at = 0;
    for _ in 0..10000 {
        out.clear();
        let step = d
            .step(
                &data[at..],
                at == data.len(),
                &mut out,
                &AtomicBool::new(false),
            )
            .unwrap();
        at += step.consumed;
        if d.inner.decoded_cycles() >= 4 {
            break;
        }
    }
    assert_eq!(at, data.len());
    let before = d.inner.decoded_cycles();
    assert_no_steady_state_allocation(
        Burst {
            path: "cached AAC loop with retained sinc history",
            warmup: 64,
            measured: 1024,
        },
        |_| {
            out.clear();
            let step = d
                .step(&[], true, &mut out, &AtomicBool::new(false))
                .unwrap();
            assert_eq!(step.consumed, 0);
            assert!(!matches!(
                step.progress,
                Progress::Finished | Progress::Cancelled | Progress::NeedSource
            ));
        },
    );
    assert!(d.inner.decoded_cycles() > before + 32);
}

use crate::{
    isomp4_file::IndexedFile,
    isomp4_source_contracts::{Config, client, start_server},
    isomp4_transport::IndexedHttp,
    streaming::{StreamMsg, StreamingState, stream_channel},
    streaming_player_contracts::{PlaybackCase, StreamInput, consume_player},
    streaming_transport::{self, HttpSource, Source},
};
use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
struct Cancel(Arc<StreamingState>);
impl Drop for Cancel {
    fn drop(&mut self) {
        self.0.cancel();
    }
}
async fn finite_source<const B: bool, S: Source<Timed<B>> + 'static>(
    owner: migo_io::scheduler::IoScheduler,
    source: S,
    name: &str,
    data: &[u8],
    rate: u32,
    object_len: Option<u64>,
    cycles: u64,
) {
    let full = candidate::decode(data, 16 * 1024 * 1024).unwrap();
    let expected = resample(
        &full.samples.repeat(cycles as usize),
        full.sample_rate,
        rate,
        full.channels,
    );
    let frames = expected.len() / full.channels as usize;
    let (tx, rx) = stream_channel();
    let state = StreamingState::new();
    let _cancel = Cancel(state.clone());
    let active = state.clone();
    let cpu = owner.audio_cpu();
    let task = tokio::spawn(async move {
        let result =
            streaming_transport::run_source(cpu, source, object_len, &tx, &active, move |length| {
                Timed::<B>::looping(rate, length, Repetitions::Finite(cycles)).unwrap()
            })
            .await;
        if let Err(error) = &result {
            eprintln!("cyclic source failed: {error}");
        }
        streaming_transport::publish_result(&tx, &active, result).await;
    });
    consume_player::<B>(
        PlaybackCase {
            name,
            source_name: format!("finite-three-cycles/{name}"),
            target: rate,
            pause: false,
            long: false,
            expected,
            channels: full.channels,
            frames,
        },
        StreamInput {
            state: state.clone(),
            steps: Arc::new(AtomicUsize::new(0)),
            task,
            rx,
        },
    )
    .await;
    assert_eq!(
        state.timeline().unwrap().seconds(),
        (full.samples.len() / full.channels as usize) as f64 / full.sample_rate as f64
    );
    drop(owner);
}
#[tokio::test]
async fn real_file_http_loops_keep_exact_pcm_and_emit_one_ended() {
    let mut count = 0;
    for name in ["1-seek-front.m4a", "1-seek-tail.m4a"] {
        let data = fixture(name);
        let path =
            std::path::PathBuf::from(std::env::var_os("W09_M4A_FIXTURES").unwrap()).join(name);
        for rate in [16000, 48000] {
            for buffered in [false, true] {
                let owner = migo_io::scheduler::IoScheduler::new(0);
                let source = IndexedFile::open(std::fs::File::open(&path).unwrap(), &owner)
                    .await
                    .unwrap();
                if buffered {
                    finite_source::<true, _>(
                        owner,
                        source,
                        name,
                        &data,
                        rate,
                        Some(data.len() as u64),
                        3,
                    )
                    .await;
                } else {
                    finite_source::<false, _>(
                        owner,
                        source,
                        name,
                        &data,
                        rate,
                        Some(data.len() as u64),
                        3,
                    )
                    .await;
                }
                let server = start_server(data.clone(), Config::default()).await;
                let client = client();
                let response = <IndexedHttp as HttpSource<Timed<false>>>::request(client.get(
                    crate::network::gate::GateKind::AudioStream,
                    server.url.clone(),
                ))
                .send()
                .await
                .unwrap();
                let source =
                    <IndexedHttp as HttpSource<Timed<false>>>::new(client, response).unwrap();
                let owner = migo_io::scheduler::IoScheduler::new(0);
                if buffered {
                    finite_source::<true, _>(owner, source, name, &data, rate, None, 3).await;
                } else {
                    finite_source::<false, _>(owner, source, name, &data, rate, None, 3).await;
                }
                drop(server);
                count += 2;
            }
        }
    }
    assert_eq!(count, 16);
    println!("W09_LOOP_SOURCE16 exact File/HTTP three-cycle player outputs;oneEnded");
}
async fn cancel_source<S: Source<Timed<true>> + 'static>(
    owner: migo_io::scheduler::IoScheduler,
    source: S,
    object_len: Option<u64>,
) {
    let (tx, mut rx) = stream_channel();
    let state = StreamingState::new();
    let _cancel = Cancel(state.clone());
    let active = state.clone();
    let cpu = owner.audio_cpu();
    let task = tokio::spawn(async move {
        let result =
            streaming_transport::run_source(cpu, source, object_len, &tx, &active, move |length| {
                Timed::<true>::looping(44100, length, Repetitions::Continuous).unwrap()
            })
            .await;
        streaming_transport::publish_result(&tx, &active, result).await;
    });
    tokio::time::timeout(Duration::from_secs(5), async {
        while rx.len() != crate::streaming::STREAM_CHANNEL_CAPACITY
            || !state.publication_waiting.load(Ordering::Acquire)
        {
            assert!(!task.is_finished());
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let bytes = state.bytes_downloaded.load(Ordering::Acquire);
    tokio::time::sleep(Duration::from_millis(40)).await;
    assert_eq!(state.bytes_downloaded.load(Ordering::Acquire), bytes);
    assert_eq!(rx.len(), crate::streaming::STREAM_CHANNEL_CAPACITY);
    assert!(state.timeline().is_some());
    assert!(state.presentation().unwrap().output_end().is_none());
    assert!(!state.download_complete.load(Ordering::Acquire));
    assert!(!task.is_finished());
    state.cancel();
    tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .unwrap()
        .unwrap();
    let mut ready = 0;
    let mut samples = 0;
    while let Ok(message) = rx.try_recv() {
        match message {
            StreamMsg::Ready { .. } => ready += 1,
            StreamMsg::Samples { .. } => samples += 1,
            StreamMsg::Done { .. } => panic!("continuous loop ended before cancellation"),
            StreamMsg::Error { message, .. } => panic!("continuous loop error: {message}"),
        }
    }
    assert_eq!(ready, 1);
    assert_eq!(samples, 3);
    drop(owner);
}
#[tokio::test]
async fn continuous_file_http_backpressure_cancels_without_done() {
    let name = "0-seek-front.m4a";
    let data = fixture(name);
    let path = std::path::PathBuf::from(std::env::var_os("W09_M4A_FIXTURES").unwrap()).join(name);
    let owner = migo_io::scheduler::IoScheduler::new(0);
    let source = IndexedFile::open(std::fs::File::open(&path).unwrap(), &owner)
        .await
        .unwrap();
    cancel_source(owner, source, Some(data.len() as u64)).await;
    let server = start_server(
        data.clone(),
        Config {
            chunked: true,
            no_tag: true,
            ..Config::default()
        },
    )
    .await;
    let client = client();
    let response = <IndexedHttp as HttpSource<Timed<true>>>::request(client.get(
        crate::network::gate::GateKind::AudioStream,
        server.url.clone(),
    ))
    .send()
    .await
    .unwrap();
    let source = <IndexedHttp as HttpSource<Timed<true>>>::new(client, response).unwrap();
    cancel_source(migo_io::scheduler::IoScheduler::new(0), source, None).await;
    assert_eq!(server.full_requests.load(Ordering::Acquire), 1);
    assert!(server.ranges.lock().unwrap().is_empty());
    drop(server);
    println!("W09_LOOP_CONTINUOUS2 bounded full-queue actors canceled withoutDone");
}

#[tokio::test]
async fn tiny_file_http_cycles_keep_single_duration_and_exact_rational_clock() {
    let name = "0-seek-front.m4a";
    let data = fixture(name);
    let path = std::path::PathBuf::from(std::env::var_os("W09_M4A_FIXTURES").unwrap()).join(name);
    let mut count = 0;
    for rate in [16000, 44100, 48000] {
        for buffered in [false, true] {
            let owner = migo_io::scheduler::IoScheduler::new(0);
            let source = IndexedFile::open(std::fs::File::open(&path).unwrap(), &owner)
                .await
                .unwrap();
            if buffered {
                finite_source::<true, _>(
                    owner,
                    source,
                    name,
                    &data,
                    rate,
                    Some(data.len() as u64),
                    257,
                )
                .await;
            } else {
                finite_source::<false, _>(
                    owner,
                    source,
                    name,
                    &data,
                    rate,
                    Some(data.len() as u64),
                    7,
                )
                .await;
            }
            let server = start_server(
                data.clone(),
                Config {
                    chunked: buffered,
                    no_tag: buffered,
                    ..Config::default()
                },
            )
            .await;
            let client = client();
            let response = <IndexedHttp as HttpSource<Timed<false>>>::request(client.get(
                crate::network::gate::GateKind::AudioStream,
                server.url.clone(),
            ))
            .send()
            .await
            .unwrap();
            let source = <IndexedHttp as HttpSource<Timed<false>>>::new(client, response).unwrap();
            let owner = migo_io::scheduler::IoScheduler::new(0);
            if buffered {
                finite_source::<true, _>(owner, source, name, &data, rate, None, 257).await;
            } else {
                finite_source::<false, _>(owner, source, name, &data, rate, None, 7).await;
            }
            assert_eq!(server.full_requests.load(Ordering::Acquire), 1);
            let ranges = server.ranges.lock().unwrap();
            if buffered {
                assert!(ranges.is_empty());
            } else {
                assert!(!ranges.is_empty());
                assert!(
                    ranges.len()
                        <= crate::isomp4_source::Limits::default().max_resumes as usize
                            + migo_services::network::http::range::Limits::default().max_requests
                                as usize
                );
            }
            drop(ranges);
            drop(server);
            count += 2;
        }
    }
    assert_eq!(count, 12);
    let owner = migo_io::scheduler::IoScheduler::new(0);
    let source = IndexedFile::open(std::fs::File::open(&path).unwrap(), &owner)
        .await
        .unwrap();
    reject_uncached_cycles(owner, source, Some(data.len() as u64)).await;
    let server = start_server(data, Config::default()).await;
    let client = client();
    let response = <IndexedHttp as HttpSource<Timed<false>>>::request(client.get(
        crate::network::gate::GateKind::AudioStream,
        server.url.clone(),
    ))
    .send()
    .await
    .unwrap();
    let source = <IndexedHttp as HttpSource<Timed<false>>>::new(client, response).unwrap();
    reject_uncached_cycles(migo_io::scheduler::IoScheduler::new(0), source, None).await;
    assert_eq!(server.full_requests.load(Ordering::Acquire), 1);
    // The server observes separate index-control and media-reposition requests.
    let ranges = server.ranges.lock().unwrap();
    assert!(!ranges.is_empty());
    assert!(
        ranges.len()
            <= crate::isomp4_source::Limits::default().max_resumes as usize
                + migo_services::network::http::range::Limits::default().max_requests as usize
    );
    drop(ranges);
    println!(
        "W09_CYCLIC_CLOCK_TINY12 6cached257cycle/6uncached7cycle players;2uncached257cycle lifetime quota rejections"
    );
}

#[tokio::test]
async fn consumed_uncached_file_http_cycles_renew_burst_without_changing_pcm() {
    let name = "1-seek-front.m4a";
    let data = fixture(name);
    let path = std::path::PathBuf::from(std::env::var_os("W09_M4A_FIXTURES").unwrap()).join(name);
    for rate in [16000, 44100, 48000] {
        let owner = migo_io::scheduler::IoScheduler::new(0);
        let source = IndexedFile::open(std::fs::File::open(&path).unwrap(), &owner)
            .await
            .unwrap();
        finite_source::<false, _>(
            owner,
            source,
            name,
            &data,
            rate,
            Some(data.len() as u64),
            257,
        )
        .await;
        let server = start_server(data.clone(), Config::default()).await;
        let client = client();
        let response = <IndexedHttp as HttpSource<Timed<false>>>::request(client.get(
            crate::network::gate::GateKind::AudioStream,
            server.url.clone(),
        ))
        .send()
        .await
        .unwrap();
        let source = <IndexedHttp as HttpSource<Timed<false>>>::new(client, response).unwrap();
        finite_source::<false, _>(
            migo_io::scheduler::IoScheduler::new(0),
            source,
            name,
            &data,
            rate,
            None,
            257,
        )
        .await;
        assert_eq!(server.full_requests.load(Ordering::Acquire), 1);
        assert!(server.ranges.lock().unwrap().len() > 64);
    }
    println!(
        "W09_IO_RENEWAL6 uncached257cycle players,exact PCM/single-duration/oneEnded;unmixed tiny startup still exhausts64"
    );
}

async fn reject_uncached_cycles<S: Source<Timed<false>>>(
    owner: migo_io::scheduler::IoScheduler,
    source: S,
    object_len: Option<u64>,
) {
    let state = StreamingState::new();
    let (tx, mut rx) = stream_channel();
    let active = state.clone();
    let run = async move {
        let result = streaming_transport::run_source(
            owner.audio_cpu(),
            source,
            object_len,
            &tx,
            &active,
            |length| Timed::<false>::looping(16000, length, Repetitions::Finite(257)).unwrap(),
        )
        .await;
        drop(tx);
        result
    };
    let drain = async {
        let mut ready = 0;
        while let Some(message) = rx.recv().await {
            match message {
                StreamMsg::Ready { .. } => ready += 1,
                StreamMsg::Samples { .. } => {}
                _ => panic!("quota failure must not publish a successful terminal"),
            }
        }
        assert_eq!(ready, 1);
    };
    let (result, ()) =
        tokio::time::timeout(Duration::from_secs(10), async { tokio::join!(run, drain) })
            .await
            .expect("uncached quota rejection stalled");
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("source reposition budget exceeded")
    );
    assert!(!state.download_complete.load(Ordering::Acquire));
}

async fn continuous_player<const B: bool, S: Source<Timed<B>> + 'static>(
    owner: migo_io::scheduler::IoScheduler,
    source: S,
    data: &[u8],
    pause: bool,
) {
    use crate::inner_audio::InnerAudioPlayer;
    use crate::streaming_player_contracts::assert_cyclic_media_time;
    use shared::protocol::audio_cmd::InnerAudioEventType;
    let full = candidate::decode(data, 16 * 1024 * 1024).unwrap();
    let channels = full.channels as usize;
    let track_frames = full.samples.len() / channels;
    let limit = if B {
        22050
    } else {
        (track_frames * 44100).div_ceil(full.sample_rate as usize) * 80
    };
    let cycles = ((limit + 1024) * full.sample_rate as usize).div_ceil(44100 * track_frames) + 2;
    let expected = resample(
        &full.samples.repeat(cycles),
        full.sample_rate,
        44100,
        full.channels,
    );
    assert!(expected.len() / channels > limit + 1024);
    let (tx, rx) = stream_channel();
    let state = StreamingState::new();
    let _cancel = Cancel(state.clone());
    let active = state.clone();
    let cpu = owner.audio_cpu();
    let task = tokio::spawn(async move {
        let result = streaming_transport::run_source(cpu, source, None, &tx, &active, |len| {
            Timed::<B>::looping(44100, len, Repetitions::Continuous).unwrap()
        })
        .await;
        streaming_transport::publish_result(&tx, &active, result).await;
    });
    let mut player = InnerAudioPlayer::new(922, full.channels);
    player.shared.set_autoplay(true);
    player.shared.set_loop_enabled(true);
    player
        .start_streaming("continuous loop media time".into(), rx, state.clone())
        .unwrap();
    let start = tokio::time::Instant::now();
    let mut rendered = 0;
    let mut paused = false;
    let mut output = vec![0.0; 512 * channels];
    while rendered < limit {
        assert!(start.elapsed() < Duration::from_secs(60));
        player.poll_stream();
        if pause && !paused && rendered >= 1024 {
            player.pause();
            let fill_started = tokio::time::Instant::now();
            // A full channel alone can precede a full player window. Drain
            // only to the existing prefetch ceiling before observing a
            // stable producer wait; later poll calls must then consume none.
            while player.needs_stream_poll() {
                assert!(fill_started.elapsed() < Duration::from_secs(10));
                player.poll_stream();
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
            while player.queued_stream_messages() < crate::streaming::STREAM_CHANNEL_CAPACITY
                || !state.publication_waiting.load(Ordering::Acquire)
            {
                assert!(fill_started.elapsed() < Duration::from_secs(10));
                player.poll_stream();
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
            let credit = state.rendered_frames();
            let position = player.shared.position_frames();
            for _ in 0..50 {
                player.poll_stream();
                let mut bus = [0.125; 16];
                player.process(&mut bus);
                assert_eq!(bus, [0.125; 16]);
                assert_eq!(state.rendered_frames(), credit);
                assert_eq!(player.shared.position_frames(), position);
                assert_eq!(
                    player.queued_stream_messages(),
                    crate::streaming::STREAM_CHANNEL_CAPACITY
                );
                assert!(state.publication_waiting.load(Ordering::Acquire));
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
            player.play();
            paused = true;
        }
        let size = 512.min(limit - rendered);
        output.fill(0.0);
        let before = player.shared.position_frames();
        player.process(&mut output[..size * channels]);
        let after = player.shared.position_frames();
        let count = after.checked_sub(before).unwrap() as usize;
        assert!(count <= size);
        assert_pcm(
            &output[..count * channels],
            &expected[rendered * channels..(rendered + count) * channels],
        );
        assert!(
            output[count * channels..size * channels]
                .iter()
                .all(|s| *s == 0.0)
        );
        rendered += count;
        assert_cyclic_media_time(&player.shared, &state);
        for e in player.drain_events() {
            assert!(!matches!(
                e.event_type,
                InnerAudioEventType::Ended | InnerAudioEventType::Error
            ));
        }
        assert!(player.physical_pcm_bytes() < 4 * 1024 * 1024);
        assert!(!state.download_complete.load(Ordering::Acquire));
        if count == 0 {
            tokio::time::sleep(Duration::from_millis(1)).await;
        } else {
            tokio::task::yield_now().await;
        }
    }
    assert_eq!(player.shared.position_frames(), limit as u64);
    assert_eq!(state.rendered_frames(), limit as u64);
    assert_eq!(paused, pause);
    assert_eq!(
        player.shared.duration(),
        track_frames as f64 / f64::from(full.sample_rate)
    );
    state.cancel();
    tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .unwrap()
        .unwrap();
    drop(owner);
}
#[tokio::test]
async fn continuous_file_http_players_wrap_time_without_ending_or_rounding_each_cycle() {
    let name = "0-seek-front.m4a";
    let data = fixture(name);
    let path = std::path::PathBuf::from(std::env::var_os("W09_M4A_FIXTURES").unwrap()).join(name);
    let owner = migo_io::scheduler::IoScheduler::new(0);
    let source = IndexedFile::open(std::fs::File::open(path).unwrap(), &owner)
        .await
        .unwrap();
    continuous_player::<true, _>(owner, source, &data, false).await;
    let server = start_server(
        data.clone(),
        Config {
            chunked: true,
            no_tag: true,
            ..Config::default()
        },
    )
    .await;
    let client = client();
    let response = <IndexedHttp as HttpSource<Timed<true>>>::request(client.get(
        crate::network::gate::GateKind::AudioStream,
        server.url.clone(),
    ))
    .send()
    .await
    .unwrap();
    let source = <IndexedHttp as HttpSource<Timed<true>>>::new(client, response).unwrap();
    continuous_player::<true, _>(
        migo_io::scheduler::IoScheduler::new(0),
        source,
        &data,
        false,
    )
    .await;
    assert_eq!(server.full_requests.load(Ordering::Acquire), 1);
    assert!(server.ranges.lock().unwrap().is_empty());
    drop(server);
    println!("W09_CYCLIC_CLOCK_CONTINUOUS2 File/HTTP players;22050 frames each;noEnded");
}

#[tokio::test]
async fn continuous_uncached_file_http_consumption_renews_and_pause_keeps_backpressure() {
    let name = "1-seek-front.m4a";
    let data = fixture(name);
    let path = std::path::PathBuf::from(std::env::var_os("W09_M4A_FIXTURES").unwrap()).join(name);
    let owner = migo_io::scheduler::IoScheduler::new(0);
    let source = IndexedFile::open(std::fs::File::open(path).unwrap(), &owner)
        .await
        .unwrap();
    continuous_player::<false, _>(owner, source, &data, true).await;
    let server = start_server(data.clone(), Config::default()).await;
    let client = client();
    let response = <IndexedHttp as HttpSource<Timed<false>>>::request(client.get(
        crate::network::gate::GateKind::AudioStream,
        server.url.clone(),
    ))
    .send()
    .await
    .unwrap();
    let source = <IndexedHttp as HttpSource<Timed<false>>>::new(client, response).unwrap();
    continuous_player::<false, _>(migo_io::scheduler::IoScheduler::new(0), source, &data, true)
        .await;
    assert_eq!(server.full_requests.load(Ordering::Acquire), 1);
    assert!(server.ranges.lock().unwrap().len() > 64);
    println!(
        "W09_IO_RENEWAL_CONTINUOUS2 uncached File/HTTP80cycle frames,actual paused publication wait,PCM exact,canceled withoutDone"
    );
}
