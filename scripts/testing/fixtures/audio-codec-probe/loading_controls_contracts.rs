//! Loading commands must reach the existing source/codec transaction exactly once.
use super::*;
use crate::streaming::StreamTimeline;

async fn loading<const B: bool, S: SeekSource<Timed<B>> + 'static>(
    owner: migo_io::scheduler::IoScheduler,
    source: S,
    data: &[u8],
    rate: u32,
    mode: u8,
    cyclic: bool,
) {
    let full = candidate::decode(data, 16_777_216).unwrap();
    let input_frames = full.samples.len() / full.channels as usize;
    let clock =
        StreamTimeline::new(input_frames as u64, full.sample_rate, rate, full.channels).unwrap();
    let time = clock.seconds() / 3.0;
    let target = clock.seek_frame(time).unwrap() as u64;
    let mapped = clock.position(target).unwrap();
    let at = mapped.frame + u64::from(mapped.phase != 0);
    let state = StreamingState::new_reusable();
    let _cancel = Cancel(state.clone());
    let controls = Controls::for_source(&state).unwrap();
    let (tx, rx) = stream_channel();
    let mut p = InnerAudioPlayer::new(961, full.channels);
    let autoplay = mode != 0 && mode != 5;
    p.shared.set_autoplay(autoplay);
    if mode == 4 {
        p.set_start_time(time);
    }
    let load = p.begin_load_for(1961).unwrap();
    if mode < 4 {
        p.play();
        p.seek(time);
        p.pause();
    } else if mode != 4 {
        p.set_start_time(clock.seconds() / 2.0);
        p.play();
    }
    p.complete_stream_load(load, "loading controls".into(), rx, state.clone())
        .unwrap();
    if cyclic {
        p.set_loop_enabled(true);
    }
    match mode {
        0 => p.play(),
        1 => p.pause(),
        2 => {
            p.stop();
            p.seek(time);
        }
        3 => {
            p.stop();
            p.play();
        }
        4 => {}
        5 | 8 => p.set_start_time(time),
        6 => p.seek(0.0),
        7 => {
            p.seek(0.0);
            p.stop();
            p.set_start_time(time);
            p.play();
        }
        _ => unreachable!(),
    }
    let expected_state = match mode {
        0 | 3..=8 => PlaybackState::Playing,
        1 => PlaybackState::Paused,
        _ => PlaybackState::Stopped,
    };
    let expected_at = if mode == 3 || mode == 6 { 0 } else { at };
    let seeks = usize::from(mode != 3);
    let mut bus = [0.125; 16];
    p.process(&mut bus);
    assert_eq!(bus, [0.125; 16]);
    assert!(p.drain_events().next().is_none());
    let task = tokio::spawn(streaming_transport::run_source_controlled(
        owner.audio_cpu(),
        source,
        None,
        tx,
        state.clone(),
        move |len| Timed::<B>::new(rate, len),
        controls,
    ));
    let mut counts = (0, 0, 0, 0);
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            p.poll_stream();
            for e in p.drain_events() {
                assert_eq!(e.source_id, 1961);
                assert_ne!(e.event_type, InnerAudioEventType::Error);
                match e.event_type {
                    InnerAudioEventType::CanPlay => counts.0 += 1,
                    InnerAudioEventType::Play => counts.1 += 1,
                    InnerAudioEventType::Seeking => counts.2 += 1,
                    InnerAudioEventType::Seeked => counts.3 += 1,
                    InnerAudioEventType::Ended => panic!("loading cannot end without mixing"),
                    _ => {}
                }
            }
            assert!(counts.0 <= 1 && counts.1 <= 1 && counts.2 <= seeks && counts.3 <= seeks);
            assert_eq!(state.rendered_frames(), 0);
            if p.shared.is_loaded()
                && counts.2 == seeks
                && counts.3 == seeks
                && (!cyclic || p.shared.loop_enabled())
            {
                break;
            }
            p.process(&mut bus);
            assert_eq!(
                bus, [0.125; 16],
                "no stale PCM before the loading seek is ready"
            );
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        counts,
        (
            1,
            usize::from(expected_state == PlaybackState::Playing),
            seeks,
            seeks
        )
    );
    assert_eq!(p.shared.state(), expected_state);
    assert_eq!(p.shared.position_frames(), expected_at);
    assert_eq!(p.shared.autoplay(), autoplay);
    assert_eq!(
        state.presentation(),
        Some(StreamPresentation::single(clock))
    );
    for _ in 0..4 {
        p.poll_stream();
        assert!(p.drain_events().next().is_none());
    }
    if expected_state != PlaybackState::Playing {
        p.process(&mut bus);
        assert_eq!(bus, [0.125; 16]);
        p.play();
        no_errors(&mut p);
    }
    if cyclic {
        let needed =
            ((expected_at + 137) as u128 * full.sample_rate as u128).div_ceil(rate as u128) + 4096;
        let cycles = needed.div_ceil(input_frames as u128) as usize + 2;
        let expected = resample(
            &full.samples.repeat(cycles),
            full.sample_rate,
            rate,
            full.channels,
        );
        let got = render(&mut p, 137, full.channels, false).await;
        assert_pcm(
            &got,
            &expected[expected_at as usize * full.channels as usize
                ..(expected_at as usize + 137) * full.channels as usize],
        );
    } else {
        let expected = resample(&full.samples, full.sample_rate, rate, full.channels);
        let got = render(
            &mut p,
            expected.len() / full.channels as usize - expected_at as usize,
            full.channels,
            true,
        )
        .await;
        assert_pcm(
            &got,
            &expected[expected_at as usize * full.channels as usize..],
        );
        if mode == 4 {
            // The first replay seek is still awaiting its source ACK. A second
            // play must not queue a zero restart over that configured target.
            p.play();
            p.play();
            let mut replay_events = (0, 0, 0);
            tokio::time::timeout(Duration::from_secs(20), async {
                loop {
                    p.poll_stream();
                    for event in p.drain_events() {
                        assert_eq!(event.source_id, 1961);
                        match event.event_type {
                            InnerAudioEventType::Play => replay_events.0 += 1,
                            InnerAudioEventType::Seeking => replay_events.1 += 1,
                            InnerAudioEventType::Seeked => replay_events.2 += 1,
                            InnerAudioEventType::Error | InnerAudioEventType::Ended => {
                                panic!("replay before PCM: {:?}", event.event_type)
                            }
                            _ => {}
                        }
                    }
                    if replay_events.2 == 1 {
                        break;
                    }
                    let mut silent = [0.125; 16];
                    p.process(&mut silent);
                    assert_eq!(silent, [0.125; 16]);
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
            })
            .await
            .unwrap();
            assert_eq!(replay_events, (1, 1, 1));
            assert_eq!(p.shared.position_frames(), expected_at);
            let replay = render(
                &mut p,
                expected.len() / full.channels as usize - expected_at as usize,
                full.channels,
                true,
            )
            .await;
            assert_pcm(
                &replay,
                &expected[expected_at as usize * full.channels as usize..],
            );
        }
    }
    state.cancel();
    tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    drop(owner);
}

async fn file_http<const B: bool>(name: &str, rate: u32, mode: u8, cyclic: bool) {
    let data = fixture(name);
    let path = std::path::PathBuf::from(std::env::var_os("W09_M4A_FIXTURES").unwrap()).join(name);
    let owner = migo_io::scheduler::IoScheduler::new(0);
    let source = IndexedFile::open(std::fs::File::open(path).unwrap(), &owner)
        .await
        .unwrap();
    loading::<B, _>(owner, source, &data, rate, mode, cyclic).await;
    let server = start_server(data.clone(), Config::default()).await;
    let client = client();
    let response = <IndexedHttp as HttpSource<Timed<B>>>::request(client.get(
        crate::network::gate::GateKind::AudioStream,
        server.url.clone(),
    ))
    .send()
    .await
    .unwrap();
    let source = <IndexedHttp as HttpSource<Timed<B>>>::new(client, response).unwrap();
    loading::<B, _>(
        migo_io::scheduler::IoScheduler::new(0),
        source,
        &data,
        rate,
        mode,
        cyclic,
    )
    .await;
    assert_eq!(server.full_requests.load(Ordering::Acquire), 1);
}
#[tokio::test]
async fn real_file_http_loading_play_pause_stop_and_seek_keep_exact_single_suffixes() {
    for name in ["0-seek-front.m4a", "1-seek-front.m4a", "1-seek-tail.m4a"] {
        for mode in 0..4 {
            file_http::<true>(name, 44100, mode, false).await;
            if name.starts_with('1') {
                file_http::<false>(name, 16000, mode, false).await;
            }
        }
    }
    println!(
        "W09_LOADING40 realFileHTTP loadingcommands,suffix/zeroTail/oneEnded/autoplay/sourceACK"
    );
}
#[tokio::test]
async fn loading_loop_then_seek_and_pause_use_the_last_mode_before_first_pcm() {
    file_http::<true>("0-seek-front.m4a", 44100, 1, true).await;
    file_http::<false>("1-seek-tail.m4a", 16000, 1, true).await;
    println!("W09_LOADING_LOOP4 realFileHTTP loop+seek+paused,crosscyclePCM/noimplicitplay");
}

#[tokio::test]
async fn real_file_http_start_time_property_order_and_zero_seek_keep_exact_suffixes() {
    for name in ["0-seek-front.m4a", "1-seek-front.m4a", "1-seek-tail.m4a"] {
        for mode in 4..9 {
            file_http::<true>(name, 44100, mode, false).await;
            if name.starts_with('1') {
                file_http::<false>(name, 16000, mode, false).await;
            }
        }
    }
    println!("W09_START_TIME50 realFileHTTP propertyorder/autoplay/explicitZero/stop/suffix");
}

#[tokio::test]
async fn real_file_http_loop_start_time_uses_the_source_ack_before_any_pcm() {
    for mode in 4..9 {
        file_http::<true>("0-seek-front.m4a", 44100, mode, true).await;
        file_http::<false>("1-seek-tail.m4a", 16000, mode, true).await;
    }
    println!("W09_START_TIME_LOOP20 realFileHTTP loop/firstPCM/ACK/explicitZero");
}
