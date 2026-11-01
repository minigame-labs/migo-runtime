//! Public cyclic seek/restart uses one media grid and the real trait hook.
use super::*;
use crate::{resampled_streaming::Repetitions, stream_control};

fn events(p: &mut InnerAudioPlayer) -> (usize, usize, usize) {
    let mut counts = (0, 0, 0);
    for e in p.drain_events() {
        assert_ne!(
            e.event_type,
            InnerAudioEventType::Error,
            "cyclic seek source error"
        );
        match e.event_type {
            InnerAudioEventType::Seeking => counts.0 += 1,
            InnerAudioEventType::Seeked => counts.1 += 1,
            InnerAudioEventType::Ended => counts.2 += 1,
            _ => {}
        }
    }
    counts
}

async fn seek_ready(p: &mut InnerAudioPlayer, target: u64, state: PlaybackState) {
    tokio::time::timeout(Duration::from_secs(20), async {
        let mut counts = (0, 0, 0);
        loop {
            p.poll_stream();
            let got = events(p);
            counts.0 += got.0;
            counts.1 += got.1;
            counts.2 += got.2;
            assert!(counts.0 <= 1 && counts.1 <= 1);
            assert_eq!(counts.2, 0);
            assert_eq!(p.shared.state(), state);
            if counts == (1, 1, 0) {
                assert_eq!(p.shared.position_frames(), target);
                break;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap();
}

async fn case<const B: bool, S: SeekSource<Observed<B>> + 'static>(
    owner: migo_io::scheduler::IoScheduler,
    source: S,
    data: &[u8],
    rate: u32,
) {
    let full = candidate::decode(data, 16_777_216).unwrap();
    let frames = full.samples.len() / full.channels as usize;
    let single = (frames as u128 * rate as u128).div_ceil(full.sample_rate as u128) as usize;
    let state = StreamingState::new_reusable();
    let _cancel = Cancel(state.clone());
    let controls = Controls::for_source(&state).unwrap();
    let (tx, rx) = stream_channel();
    let trace = Arc::new(AtomicUsize::new(0));
    let counting = trace.clone();
    let task = tokio::spawn(streaming_transport::run_source_controlled(
        owner.audio_cpu(),
        source,
        None,
        tx,
        state.clone(),
        move |len| Observed {
            inner: Timed::<B>::new(rate, len),
            cutovers: counting,
            bad_metadata: None,
        },
        controls,
    ));
    let mut p = InnerAudioPlayer::new(952, full.channels);
    p.start_streaming("cyclic public seek".into(), rx, state.clone())
        .unwrap();
    wait(&mut p, |p| p.shared.is_loaded() && p.is_stream_complete()).await;
    p.set_loop_enabled(true);
    wait(&mut p, |p| p.shared.loop_enabled()).await;
    let count = 137;
    let needed =
        ((single + count) as u128 * full.sample_rate as u128).div_ceil(rate as u128) + 1024;
    let cycles = needed.div_ceil(frames as u128) as usize + 1;
    let continuous = resample(
        &full.samples.repeat(cycles),
        full.sample_rate,
        rate,
        full.channels,
    );
    let finite = resample(&full.samples, full.sample_rate, rate, full.channels);
    let mut seeks = 0;
    for (target, play_state) in [
        (frames / 3, PlaybackState::Playing),
        (frames, PlaybackState::Paused),
        (0, PlaybackState::Stopped),
    ] {
        match play_state {
            PlaybackState::Playing => p.play(),
            PlaybackState::Paused => p.pause(),
            PlaybackState::Stopped => p.stop(),
        }
        events(&mut p);
        let time = if target == frames {
            p.shared.duration()
        } else {
            (target as f64 + 0.25) / full.sample_rate as f64
        };
        p.seek(time);
        let output = (target as u128 * rate as u128).div_ceil(full.sample_rate as u128) as u64;
        seek_ready(&mut p, output, play_state).await;
        assert!(p.shared.loop_enabled());
        assert_eq!(p.shared.duration(), frames as f64 / full.sample_rate as f64);
        let media = if target == frames {
            0.0
        } else {
            target as f64 / full.sample_rate as f64
        };
        assert!((p.shared.current_time() - media).abs() <= 1.0 / rate as f64);
        p.play();
        events(&mut p);
        assert_pcm(
            &render(&mut p, count, full.channels, false).await,
            &continuous[output as usize * full.channels as usize
                ..(output as usize + count) * full.channels as usize],
        );
        seeks += 1;
    }
    for _ in 0..2 {
        let before = trace.load(Ordering::Acquire);
        p.stop();
        p.play();
        wait(&mut p, |p| {
            trace.load(Ordering::Acquire) > before && p.shared.position_frames() == 0
        })
        .await;
        assert!(p.shared.loop_enabled());
        assert_pcm(
            &render(&mut p, count, full.channels, false).await,
            &continuous[..count * full.channels as usize],
        );
    }
    // A later requested off mode must govern the queued seek, rather than the
    // still-active continuous mode or the decoder's prefetched cycle count.
    p.set_loop_enabled(false);
    let target = frames / 3;
    p.seek((target as f64 + 0.25) / full.sample_rate as f64);
    let output = (target as u128 * rate as u128).div_ceil(full.sample_rate as u128) as usize;
    seek_ready(&mut p, output as u64, PlaybackState::Playing).await;
    assert!(!p.shared.loop_enabled());
    assert_pcm(
        &render(&mut p, single - output, full.channels, true).await,
        &finite[output * full.channels as usize..],
    );
    seeks += 1;
    p.play();
    wait(&mut p, |p| p.physical_pcm_bytes() > 0).await;
    assert!(!p.shared.loop_enabled());
    assert_pcm(&render(&mut p, single, full.channels, true).await, &finite);
    p.set_loop_enabled(true);
    wait(&mut p, |p| p.shared.loop_enabled()).await;
    p.play();
    events(&mut p);
    p.set_loop_enabled(false);
    p.seek(p.shared.duration());
    seek_ready(&mut p, single as u64, PlaybackState::Playing).await;
    assert!(!p.shared.loop_enabled());
    assert!(render(&mut p, 0, full.channels, true).await.is_empty());
    assert_eq!(p.shared.current_time(), 0.0);
    seeks += 1;
    p.set_loop_enabled(true);
    wait(&mut p, |p| p.shared.loop_enabled()).await;
    p.play();
    events(&mut p);
    assert_pcm(
        &render(&mut p, count, full.channels, false).await,
        &continuous[..count * full.channels as usize],
    );
    assert_eq!(seeks, 5);
    assert_eq!(
        state.presentation(),
        Some(StreamPresentation::single(state.timeline().unwrap()))
    );
    assert!(!state.is_cancelled());
    state.cancel();
    let _ = tokio::time::timeout(Duration::from_secs(3), task)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn real_file_http_cyclic_seek_restart_and_disabled_endpoints_match_pcm() {
    let mut sources = 0;
    for name in ["0-seek-front.m4a", "1-seek-front.m4a", "1-seek-tail.m4a"] {
        let data = fixture(name);
        let path =
            std::path::PathBuf::from(std::env::var_os("W09_M4A_FIXTURES").unwrap()).join(name);
        for rate in [16000, 44100, 48000] {
            for buffered in [false, true] {
                if !buffered && name.starts_with('0') {
                    continue;
                }
                let owner = migo_io::scheduler::IoScheduler::new(0);
                let source = IndexedFile::open(std::fs::File::open(&path).unwrap(), &owner)
                    .await
                    .unwrap();
                if buffered {
                    case::<true, _>(owner, source, &data, rate).await;
                } else {
                    case::<false, _>(owner, source, &data, rate).await;
                }
                let server = start_server(data.clone(), Config::default()).await;
                let client = client();
                if buffered {
                    let response =
                        <IndexedHttp as HttpSource<Observed<true>>>::request(client.get(
                            crate::network::gate::GateKind::AudioStream,
                            server.url.clone(),
                        ))
                        .send()
                        .await
                        .unwrap();
                    let source =
                        <IndexedHttp as HttpSource<Observed<true>>>::new(client, response).unwrap();
                    case::<true, _>(migo_io::scheduler::IoScheduler::new(0), source, &data, rate)
                        .await;
                } else {
                    let response =
                        <IndexedHttp as HttpSource<Observed<false>>>::request(client.get(
                            crate::network::gate::GateKind::AudioStream,
                            server.url.clone(),
                        ))
                        .send()
                        .await
                        .unwrap();
                    let source =
                        <IndexedHttp as HttpSource<Observed<false>>>::new(client, response)
                            .unwrap();
                    case::<false, _>(migo_io::scheduler::IoScheduler::new(0), source, &data, rate)
                        .await;
                }
                sources += 2;
            }
        }
    }
    assert_eq!(sources, 30);
    println!("W09_CYCLIC_SEEK30 FileHTTP sources/150 publicseeks/60 repeatedrestarts");
}

#[test]
fn cyclic_codec_seek_matches_continuous_and_single_reference_at_boundaries() {
    let mut comparisons = 0;
    for name in ["0-seek-front.m4a", "1-seek-front.m4a", "1-seek-tail.m4a"] {
        let data = fixture(name);
        let full = candidate::decode(&data, 16_777_216).unwrap();
        let frames = full.samples.len() / full.channels as usize;
        for rate in [16000, 44100, 48000] {
            let single = resample(&full.samples, full.sample_rate, rate, full.channels);
            for buffered in [false, true] {
                fn check<const B: bool>(
                    data: &[u8],
                    full: &crate::decoder::DecodedAudio,
                    frames: usize,
                    rate: u32,
                    single: &[f32],
                ) -> usize {
                    let mut d =
                        Timed::<B>::looping(rate, Some(data.len() as u64), Repetitions::Continuous)
                            .unwrap();
                    let mut cursor = 0;
                    super::super::phase_tests::render_cursor(
                        &mut d,
                        data,
                        &mut cursor,
                        137,
                        Some(3 * full.channels as usize),
                    );
                    for target in [0, 1, frames - 1, frames] {
                        let output = (target as u128 * rate as u128)
                            .div_ceil(full.sample_rate as u128)
                            as usize;
                        let count = 37;
                        let needed = ((output + count) as u128 * full.sample_rate as u128)
                            .div_ceil(rate as u128)
                            + 1024;
                        let cycles = needed.div_ceil(frames as u128) as usize + 1;
                        let continuous = resample(
                            &full.samples.repeat(cycles),
                            full.sample_rate,
                            rate,
                            full.channels,
                        );
                        let seeked =
                            M4aSeekDecoder::seek_loop(&mut d, Timestamp::new(target as i64), true)
                                .unwrap();
                        assert_eq!(seeked.required_ts.get(), target as i64);
                        assert_pcm(
                            &super::super::phase_tests::render_cursor(
                                &mut d,
                                data,
                                &mut cursor,
                                137,
                                Some(count * full.channels as usize),
                            ),
                            &continuous[output * full.channels as usize
                                ..(output + count) * full.channels as usize],
                        );
                        M4aSeekDecoder::seek_loop(&mut d, Timestamp::new(target as i64), false)
                            .unwrap();
                        let got = super::super::phase_tests::render_cursor(
                            &mut d,
                            data,
                            &mut cursor,
                            137,
                            None,
                        );
                        assert_pcm(&got, &single[output * full.channels as usize..]);
                        assert!(!d.presentation().unwrap().unwrap().is_cyclic());
                        M4aSeekDecoder::seek_loop(&mut d, Timestamp::ZERO, true).unwrap();
                        let restarted = super::super::phase_tests::render_cursor(
                            &mut d,
                            data,
                            &mut cursor,
                            137,
                            Some(count * full.channels as usize),
                        );
                        assert_pcm(&restarted, &continuous[..restarted.len()]);
                    }
                    12
                }
                comparisons += if buffered {
                    check::<true>(&data, &full, frames, rate, &single)
                } else {
                    check::<false>(&data, &full, frames, rate, &single)
                };
            }
        }
    }
    assert_eq!(comparisons, 216);
    println!("W09_CYCLIC_SEEK_CODEC216 continuous/single/terminal restart PCM suffixes");
}

#[tokio::test]
async fn malformed_and_expired_cyclic_source_requests_preserve_the_old_owner() {
    async fn check<S: SeekSource<Observed<true>> + 'static>(
        owner: migo_io::scheduler::IoScheduler,
        source: S,
        data: &[u8],
    ) {
        let full = candidate::decode(data, 16_777_216).unwrap();
        let frames = full.samples.len() / full.channels as usize;
        let state = StreamingState::new_reusable();
        let _cancel = Cancel(state.clone());
        let controls = Controls::for_source(&state).unwrap();
        let (tx, rx) = stream_channel();
        let trace = Arc::new(AtomicUsize::new(0));
        let counting = trace.clone();
        let task = tokio::spawn(streaming_transport::run_source_controlled(
            owner.audio_cpu(),
            source,
            None,
            tx,
            state.clone(),
            move |len| Observed {
                inner: Timed::<true>::new(44100, len),
                cutovers: counting,
                bad_metadata: None,
            },
            controls,
        ));
        let mut p = InnerAudioPlayer::new(953, full.channels);
        p.start_streaming("refused cyclic source request".into(), rx, state.clone())
            .unwrap();
        wait(&mut p, |p| p.shared.is_loaded() && p.is_stream_complete()).await;
        let before = (
            p.shared.position_frames(),
            p.physical_pcm_bytes(),
            p.shared.duration(),
        );
        let target = frames as u64 / 3;
        let output = (u128::from(target) * 44100).div_ceil(full.sample_rate as u128) as u64;
        let request = stream_control::LoopRequest {
            output_start: output,
            enabled: true,
            seek_target: Some(target),
        };
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        for (target, request, deadline, expired) in [
            (target as i64 + 1, request, deadline, false),
            (
                target as i64,
                request,
                tokio::time::Instant::now() - Duration::from_secs(1),
                true,
            ),
            (
                (frames + 1) as i64,
                stream_control::LoopRequest {
                    output_start: state.timeline().unwrap().output_frames() + 1,
                    enabled: true,
                    seek_target: Some(frames as u64 + 1),
                },
                deadline,
                false,
            ),
        ] {
            let reply = stream_control::request_as(
                state.control_sender().unwrap(),
                target,
                Some(request),
                deadline,
            )
            .unwrap();
            let reply = tokio::time::timeout(Duration::from_secs(3), reply)
                .await
                .unwrap()
                .unwrap();
            if expired {
                assert!(matches!(
                    reply,
                    stream_control::Reply::NotStarted(shared::error::ErrorCode::Timeout)
                ));
            } else {
                assert!(matches!(
                    reply,
                    stream_control::Reply::Rejected(shared::error::ErrorCode::InvalidArgument)
                ));
            }
            assert_eq!(trace.load(Ordering::Acquire), 0);
            assert!(!state.is_cancelled());
            assert_eq!(
                (
                    p.shared.position_frames(),
                    p.physical_pcm_bytes(),
                    p.shared.duration()
                ),
                before
            );
        }
        p.play();
        let reference = resample(&full.samples, full.sample_rate, 44100, full.channels);
        assert_pcm(
            &render(
                &mut p,
                reference.len() / full.channels as usize,
                full.channels,
                true,
            )
            .await,
            &reference,
        );
        state.cancel();
        let _ = tokio::time::timeout(Duration::from_secs(3), task)
            .await
            .unwrap()
            .unwrap();
    }
    let name = "1-seek-front.m4a";
    let data = fixture(name);
    let path = std::path::PathBuf::from(std::env::var_os("W09_M4A_FIXTURES").unwrap()).join(name);
    let owner = migo_io::scheduler::IoScheduler::new(0);
    let source = IndexedFile::open(std::fs::File::open(path).unwrap(), &owner)
        .await
        .unwrap();
    check(owner, source, &data).await;
    let server = start_server(data.clone(), Config::default()).await;
    let client = client();
    let response = <IndexedHttp as HttpSource<Observed<true>>>::request(client.get(
        crate::network::gate::GateKind::AudioStream,
        server.url.clone(),
    ))
    .send()
    .await
    .unwrap();
    let source = <IndexedHttp as HttpSource<Observed<true>>>::new(client, response).unwrap();
    check(migo_io::scheduler::IoScheduler::new(0), source, &data).await;
}

#[test]
fn cached_cyclic_seek_refusals_retirement_and_repeated_changes_keep_pcm_and_heap_contracts() {
    use migo_alloc_probe::{Burst, assert_no_steady_state_allocation};
    let data = fixture("0-seek-front.m4a");
    let full = candidate::decode(&data, 16_777_216).unwrap();
    let mut d = Timed::<true>::new(44100, Some(data.len() as u64));
    super::super::phase_tests::render(&mut d, &data, 0, 137);
    M4aSeekDecoder::seek_loop(&mut d, Timestamp::new(1), true).unwrap();
    let before = super::super::cutover_tests::cached_prefix(&mut d, 512, full.channels);
    let presentation = d.presentation().unwrap();
    for target in [-1, 18, i64::MAX] {
        for enabled in [true, false] {
            assert!(M4aSeekDecoder::seek_loop(&mut d, Timestamp::new(target), enabled).is_err());
            assert_eq!(d.presentation().unwrap(), presentation);
        }
    }
    let after = super::super::cutover_tests::cached_prefix(&mut d, 512, full.channels);
    let reference = resample(
        &full.samples.repeat(4096),
        full.sample_rate,
        44100,
        full.channels,
    );
    let start = (44100_u128.div_ceil(full.sample_rate as u128) as usize) * full.channels as usize;
    assert_pcm(&before, &reference[start..start + before.len()]);
    assert_pcm(
        &after,
        &reference[start + before.len()..start + before.len() + after.len()],
    );
    let mut out = Vec::with_capacity(crate::bandlimited::MAX_SCRATCH_SAMPLES);
    assert_no_steady_state_allocation(
        Burst {
            path: "cached canonical cyclic seek",
            warmup: 64,
            measured: 256,
        },
        |i| {
            M4aSeekDecoder::seek_loop(&mut d, Timestamp::new(1), i.is_multiple_of(2)).unwrap();
            for _ in 0..128 {
                out.clear();
                let step = d
                    .step(&[], true, &mut out, &AtomicBool::new(false))
                    .unwrap();
                assert!(!matches!(
                    step.progress,
                    Progress::NeedSource | Progress::Cancelled
                ));
                if !out.is_empty() {
                    break;
                }
            }
            assert!(!out.is_empty());
        },
    );
    d.inner.retire_loop_error();
    for enabled in [true, false] {
        assert!(M4aSeekDecoder::seek_loop(&mut d, Timestamp::new(1), enabled).is_err());
    }
    out.push(1.0);
    assert!(
        d.step(&[], true, &mut out, &AtomicBool::new(false))
            .is_err()
    );
    assert!(out.is_empty());
}
