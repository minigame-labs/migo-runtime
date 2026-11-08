//! Real pinned HTTP WAVE uses the existing packetizer, source ACK and Reader.
use super::range_controls::{done, start_controlled};
use super::*;
use crate::inner_audio::InnerAudioPlayer;
use crate::stream_control::{Reply, request};
use crate::stream_open::file_tests::controls::{finish, suffix, wait};
fn data(frames: usize, channels: u16, rate: u32, tail: usize) -> Vec<u8> {
    let mut data = crate::test_media::wav(rate, channels, frames);
    if tail != 0 {
        data.extend(b"LIST");
        data.extend((tail as u32).to_le_bytes());
        data.resize(data.len() + tail + tail % 2, 0);
        let size = (data.len() - 8) as u32;
        data[4..8].copy_from_slice(&size.to_le_bytes());
    }
    data
}
async fn load(
    f: &controls::Context,
    bytes: &[u8],
    channels: u32,
    rate: u32,
    hold: bool,
) -> (InnerAudioPlayer, Arc<StreamingState>, Server) {
    // Public canplay needs 0.5 s; keep the large original body held after a
    // sufficient fixed prefix without changing the production threshold.
    let server = server_with_initial_prefix(
        bytes.to_vec(),
        Mode::Normal,
        hold.then_some(1024 * 1024),
        false,
    )
    .await;
    let state = StreamingState::new_url();
    let rx = crate::streaming::start_streaming_download_with_spool(
        f.host.audio_cpu(),
        server.client(f.host.pools().clone()),
        "http://indexed-product-fixture.invalid/wrong.mp3".into(),
        state.clone(),
        rate,
        Some(f.spool()),
    );
    let mut p = InnerAudioPlayer::new(919, channels);
    let token = p.begin_load_for(8181).unwrap();
    p.complete_stream_load(
        token,
        "http://indexed-product-fixture.invalid/wrong.mp3".into(),
        rx,
        state.clone(),
    )
    .unwrap();
    wait(&mut p, |p| p.shared.is_loaded()).await;
    assert!(
        state.retains_source() && state.control_sender().is_some(),
        "pinned WAVE never installed its real source controls"
    );
    (p, state, server)
}
fn compare(got: &[f32], want: &[f32], key: &str) {
    assert_eq!(
        got.len(),
        want.len(),
        "HTTP WAVE control complete suffix length: {key}"
    );
    let max = got
        .iter()
        .zip(want)
        .map(|(a, b)| (*a - *b).abs())
        .fold(0f32, f32::max);
    assert!(
        max <= 0.00001 && got.iter().chain(want).all(|n| n.is_finite()),
        "HTTP WAVE control complete PCM: {key},error={max}"
    );
    if std::env::var_os("MIGO_DUMP_HTTP_WAVE_PCM").is_some() {
        println!(
            "MIGO_HTTP_WAVE_PCM {key} {:?}",
            got.iter().map(|x| x.to_bits()).collect::<Vec<_>>()
        );
    }
}
#[tokio::test]
async fn public_wave_seek_restart_and_exact_endpoint_keep_every_pcm_sample() {
    let f = controls::Context::new(Mode::Normal);
    for (ch, frames, source_rate, tail) in [
        (1, 9737, 44100, 0),
        (2, 9737, 44100, 37),
        (6, 9737, 48000, 37),
        (32, 257, 48000, 0),
    ] {
        let bytes = data(frames, ch, source_rate, tail);
        for rate in [3000, 16000, 48000] {
            let reference = crate::decoder::decode_for_rate(&bytes, rate).unwrap();
            let (mut p, _state, _server) = load(&f, &bytes, ch as u32, rate, false).await;
            for target in [frames / 2, frames - 1, frames, 0] {
                p.pause();
                let before = p.shared.position_frames();
                let start = (target as u128 * rate as u128).div_ceil(source_rate as u128) as u64;
                p.seek(if target == frames {
                    p.shared.duration()
                } else {
                    (target as f64 + 0.25) / source_rate as f64
                });
                assert_eq!(
                    p.shared.position_frames(),
                    before,
                    "WAVE seek published before actual source ACK"
                );
                wait(&mut p, |p| {
                    p.shared.position_frames() == start && p.retained_stream_start() == start
                })
                .await;
                let want = &reference.samples[start as usize * ch as usize..];
                let got = suffix(&mut p, want.len() / ch as usize).await;
                if want.is_empty() {
                    wait(&mut p, |p| p.is_stream_complete()).await;
                }
                compare(&got, want, &format!("{ch}/{rate}/seek-{target}"));
                p.stop();
                p.play();
                wait(&mut p, |p| {
                    p.retained_stream_start() == 0 && p.shared.position_frames() == 0
                })
                .await;
                let got = suffix(&mut p, reference.samples.len() / ch as usize).await;
                compare(
                    &got,
                    &reference.samples,
                    &format!("{ch}/{rate}/restart-{target}"),
                );
            }
        }
    }
}
#[tokio::test]
async fn wave_loops_with_metadata_keep_global_sinc_phase_and_consumer_cutovers() {
    let f = controls::Context::new(Mode::Normal);
    for (ch, frames, source_rate, tail) in
        [(1, 17, 8000, 37), (2, 9737, 44100, 37), (32, 257, 48000, 0)]
    {
        let bytes = data(frames, ch, source_rate, tail);
        let native = crate::decoder::decode(&bytes).unwrap();
        for rate in [3000, 16000, 48000] {
            let full = |n| {
                crate::resample_job::tests::reference(
                    &crate::decoder::DecodedAudio {
                        samples: native.samples.repeat(n),
                        sample_rate: source_rate,
                        channels: ch as u32,
                    },
                    rate,
                )
            };
            let (mut p, _state, _server) = load(&f, &bytes, ch as u32, rate, false).await;
            p.set_loop_enabled(true);
            wait(&mut p, |p| p.shared.loop_enabled()).await;
            let count = ((frames as u128 * 2 * rate as u128) / (source_rate as u128)) as usize + 3;
            let got = suffix(&mut p, count).await;
            let oracle_cycles = if frames < 1024 { 128 } else { 6 };
            let a = full(oracle_cycles);
            let b = full(oracle_cycles * 2);
            assert_eq!(
                a[..got.len()]
                    .iter()
                    .map(|s| s.to_bits())
                    .collect::<Vec<_>>(),
                b[..got.len()]
                    .iter()
                    .map(|s| s.to_bits())
                    .collect::<Vec<_>>(),
                "WAVE loop oracle was contaminated by a finite future endpoint"
            );
            compare(
                &got,
                &full(if frames < 1024 { 128 } else { 6 })[..got.len()],
                &format!("{ch}/{rate}/loop-prefix"),
            );
            p.pause();
            let at = p.shared.position_frames();
            p.set_loop_enabled(false);
            wait(&mut p, |p| {
                !p.shared.loop_enabled() && p.retained_stream_start() == at
            })
            .await;
            let cycles =
                ((at as u128 * source_rate as u128 / rate as u128) / (frames as u128) + 1) as usize;
            let expected = full(cycles);
            let got = suffix(&mut p, expected.len() / ch as usize - at as usize).await;
            compare(
                &got,
                &expected[at as usize * ch as usize..],
                &format!("{ch}/{rate}/disabled-tail"),
            );
            finish(&mut p).await;
            p.set_loop_enabled(true);
            p.play();
            wait(&mut p, |p| {
                p.shared.loop_enabled() && p.retained_stream_start() == 0
            })
            .await;
            let got = suffix(&mut p, 137).await;
            assert_eq!(
                a[..got.len()]
                    .iter()
                    .map(|s| s.to_bits())
                    .collect::<Vec<_>>(),
                b[..got.len()]
                    .iter()
                    .map(|s| s.to_bits())
                    .collect::<Vec<_>>(),
                "WAVE restart oracle was contaminated by a finite future endpoint"
            );
            compare(
                &got,
                &full(if frames < 1024 { 128 } else { 6 })[..got.len()],
                &format!("{ch}/{rate}/loop-restart"),
            );
        }
    }
}

#[tokio::test]
async fn wave_without_reliable_range_keeps_the_complete_original_body_without_unpinned_ranges() {
    let f = controls::Context::new(Mode::Normal);
    let bytes = data(9737, 2, 44100, 37);
    for mode in [
        Mode::NoTag,
        Mode::Weak,
        Mode::Ignored,
        Mode::Chunked,
        Mode::Gzip,
        Mode::UnknownLength,
    ] {
        for rate in [16000, 48000] {
            // The existing server applies its one HTTP coding itself.
            let server = server(bytes.clone(), mode, false, false).await;
            let state = StreamingState::new_url();
            let mut rx = crate::streaming::start_streaming_download_with_spool(
                f.host.audio_cpu(),
                server.client(f.host.pools().clone()),
                "http://indexed-product-fixture.invalid/progressive".into(),
                state.clone(),
                rate,
                Some(f.spool()),
            );
            let mut pcm = Vec::new();
            while let Some(message) = tokio::time::timeout(WAIT, rx.recv()).await.unwrap() {
                match message {
                    StreamMsg::Samples { pcm: p, .. } => pcm.extend_from_slice(&p),
                    StreamMsg::Done { .. } => break,
                    StreamMsg::Error { message, .. } => panic!(
                        "fallback WAVE lost original body: mode={} rate={rate} samples={} error={message}",
                        match mode {
                            Mode::NoTag => 0,
                            Mode::Weak => 1,
                            Mode::Ignored => 2,
                            Mode::Chunked => 3,
                            Mode::Gzip => 4,
                            _ => 5,
                        },
                        pcm.len()
                    ),
                    _ => {}
                }
            }
            let expected = crate::decoder::decode_for_rate(&bytes, rate).unwrap();
            compare(
                &pcm,
                &expected.samples,
                &format!(
                    "fallback/{mode_index}/{rate}",
                    mode_index = match mode {
                        Mode::NoTag => 0,
                        Mode::Weak => 1,
                        Mode::Ignored => 2,
                        Mode::Chunked => 3,
                        Mode::Gzip => 4,
                        _ => 5,
                    }
                ),
            );
            // Seeks replay the spool or read on: controls follow the known
            // presentation length, and nothing goes back to the network.
            assert_eq!(
                state.control_sender().is_some(),
                state.timeline().is_some(),
                "WAVE controls do not follow its known presentation length"
            );
            state.cancel();
            assert!(
                tokio::time::timeout(WAIT, rx.recv())
                    .await
                    .unwrap()
                    .is_none()
            );
            assert_eq!(server.range_requests(), 0);
        }
    }
}
#[tokio::test]
async fn unknown_wave_length_never_activates_controls_and_keeps_complete_pcm() {
    let host = Arc::new(migo_io::scheduler::IoScheduler::new(1101));
    let mut bytes = data(9737, 2, 44100, 0);
    bytes[4..8].copy_from_slice(&u32::MAX.to_le_bytes());
    bytes[40..44].copy_from_slice(&u32::MAX.to_le_bytes());
    let server = server(bytes.clone(), Mode::Normal, false, false).await;
    let (mut rx, state) = start_controlled(&server, &host);
    let mut pcm = Vec::new();
    let mut ended = 0;
    while let Some(m) = tokio::time::timeout(WAIT, rx.recv()).await.unwrap() {
        match m {
            StreamMsg::Ready { .. } => assert!(
                state.control_sender().is_none() && !state.retains_source(),
                "unknown HTTP WAVE activated source controls"
            ),
            StreamMsg::Samples { pcm: p, .. } => pcm.extend_from_slice(&p),
            StreamMsg::Done { .. } => ended += 1,
            StreamMsg::Error { message, .. } => panic!("unknown HTTP WAVE failed: {message}"),
        }
    }
    assert_eq!(ended, 1);
    assert!(state.control_sender().is_none());
    compare(
        &pcm,
        &crate::decoder::decode_for_rate(&bytes, 16000)
            .unwrap()
            .samples,
        "unknown/16000/complete",
    );
}
#[tokio::test]
async fn truncated_wave_extent_refuses_before_ready_or_controls() {
    let host = Arc::new(migo_io::scheduler::IoScheduler::new(1102));
    let mut bytes = data(9737, 2, 44100, 0);
    bytes.pop();
    let server = server(bytes, Mode::Normal, false, false).await;
    let (mut rx, state) = start_controlled(&server, &host);
    let mut errors = 0;
    while let Some(m) = tokio::time::timeout(WAIT, rx.recv()).await.unwrap() {
        assert!(
            matches!(m, StreamMsg::Error { .. }),
            "truncated HTTP WAVE published Ready or PCM"
        );
        errors += 1;
    }
    assert_eq!(errors, 1);
    assert!(state.control_sender().is_none() && !state.retains_source());
}
#[tokio::test]
async fn wave_refusals_and_actual_consumer_ack_preserve_the_original_revision() {
    let host = Arc::new(migo_io::scheduler::IoScheduler::new(1103));
    let bytes = data(9737, 2, 44100, 37);
    let server = server(bytes.clone(), Mode::Normal, false, false).await;
    let (mut rx, state) = start_controlled(&server, &host);
    let (first, old) = done(&mut rx).await;
    let port = state.control_sender().unwrap();
    let count = server.range_requests();
    for target in [-1, i64::MAX] {
        let reply = request(port, target, Instant::now() + WAIT)
            .unwrap()
            .await
            .unwrap();
        assert!(
            matches!(reply, Reply::Rejected(ErrorCode::InvalidArgument)),
            "invalid HTTP WAVE seek changed original state"
        );
        assert_eq!(server.range_requests(), count);
        assert!(rx.try_recv().is_err());
    }
    let reply = request(port, 0, Instant::now() + WAIT)
        .unwrap()
        .await
        .unwrap();
    let Reply::Prepared(prepared) = reply else {
        panic!("HTTP WAVE restart was not prepared")
    };
    crate::streaming::get_stream_runtime()
        .spawn(async {})
        .await
        .unwrap();
    assert!(
        !prepared.accept.is_closed(),
        "HTTP WAVE dropped actual consumer ACK"
    );
    assert!(
        rx.try_recv().is_err(),
        "HTTP WAVE published a revision before consumer ACK"
    );
    assert_ne!(prepared.revision, old);
    let revision = prepared.revision;
    prepared.accept.send(()).unwrap();
    let (pcm, new) = done(&mut rx).await;
    assert_eq!(new, revision);
    compare(&pcm, &first, "ack/16000/accepted");
    let Reply::Prepared(prepared) = request(port, 0, Instant::now() + WAIT)
        .unwrap()
        .await
        .unwrap()
    else {
        panic!()
    };
    drop(prepared);
    assert!(
        tokio::time::timeout(WAIT, rx.recv())
            .await
            .unwrap()
            .is_none()
    );
    assert!(state.is_cancelled());
}
#[tokio::test]
async fn wave_source_seek_before_original_response_eof_preserves_the_entire_nonconstant_tail() {
    let host = Arc::new(migo_io::scheduler::IoScheduler::new(1106));
    let bytes = data(240137, 2, 44100, 37);
    let reference = crate::decoder::decode_for_rate(&bytes, 16000).unwrap();
    // One complete PCM frame lets the mature decoder validate the format;
    // no additional PCM may be required after the first Ready to prepare seek.
    let server = server_with_initial_prefix(bytes, Mode::Normal, Some(48), false).await;
    let (mut rx, state) = start_controlled(&server, &host);
    loop {
        match tokio::time::timeout(WAIT, rx.recv())
            .await
            .unwrap()
            .unwrap()
        {
            StreamMsg::Ready { .. } => break,
            StreamMsg::Samples { .. } => {}
            _ => panic!("HTTP WAVE failed before early Ready"),
        }
    }
    assert_eq!(
        server.initial_eof.load(Ordering::Acquire),
        0,
        "initial WAVE body was not really held before EOF"
    );
    let target = 170003u64;
    let Reply::Prepared(prepared) = request(
        state.control_sender().unwrap(),
        target as i64,
        Instant::now() + WAIT,
    )
    .unwrap()
    .await
    .expect("HTTP WAVE source preparation failed before original EOF") else {
        panic!("HTTP WAVE early source seek was refused before original EOF")
    };
    while let Ok(m) = rx.try_recv() {
        assert!(
            matches!(m, StreamMsg::Samples { .. }),
            "old HTTP WAVE ended before original response EOF"
        );
    }
    let revision = prepared.revision;
    prepared.accept.send(()).unwrap();
    let (pcm, new) = done(&mut rx).await;
    assert_eq!(new, revision);
    let mapped = (target as u128 * 16000).div_ceil(44100) as usize;
    compare(
        &pcm,
        &reference.samples[mapped * 2..],
        "early/16000/complete-tail",
    );
    assert_eq!(server.initial_eof.load(Ordering::Acquire), 0);
    state.cancel();
}

#[tokio::test]
async fn actual_large_http_wave_reclaims_pcm_reopens_the_pinned_source_and_checks_every_tail_sample()
 {
    let f = controls::Context::new(Mode::Normal);
    let frames = 8_388_737u32;
    let mut bytes = b"RIFF".to_vec();
    bytes.extend((frames * 2 + 36).to_le_bytes());
    bytes.extend(b"WAVEfmt \x10\0\0\0\x01\0\x01\0");
    bytes.extend(48000u32.to_le_bytes());
    bytes.extend(96000u32.to_le_bytes());
    bytes.extend(b"\x02\0\x10\0data");
    bytes.extend((frames * 2).to_le_bytes());
    bytes.extend([0, 32].repeat(frames as usize));
    assert!(bytes.len() > 16 * 1024 * 1024);
    let (mut p, _state, server) = load(&f, &bytes, 1, 48000, true).await;
    assert_eq!(server.initial_eof.load(Ordering::Acquire), 0);
    let at = 2_200_007u64;
    eprintln!(
        "large WAVE first source seek; pos={} base={} queued={} ranges={}",
        p.shared.position_frames(),
        p.retained_stream_start(),
        p.queued_stream_messages(),
        server.range_requests()
    );
    p.seek((at as f64 + 0.25) / 48000.0);
    let mut last = (u64::MAX, u64::MAX, usize::MAX);
    wait(&mut p, |p| {
        let now = (
            p.shared.position_frames(),
            p.retained_stream_start(),
            server.range_requests(),
        );
        if now != last {
            eprintln!("large WAVE first seek observed {now:?}");
            last = now;
        }
        p.shared.position_frames() == at && p.retained_stream_start() == at
    })
    .await;
    eprintln!("large WAVE forward suffix begins");
    let got = suffix(&mut p, 1_100_003).await;
    assert!(got.iter().all(|x| x.to_bits() == 0.25f32.to_bits()));
    assert!(
        p.retained_stream_start() > at,
        "large HTTP WAVE never reclaimed actual PCM"
    );
    let count = server.range_requests();
    p.pause();
    eprintln!(
        "large WAVE backward source seek; pos={} base={} queued={} ranges={}",
        p.shared.position_frames(),
        p.retained_stream_start(),
        p.queued_stream_messages(),
        server.range_requests()
    );
    p.seek(0.0);
    wait(&mut p, |p| {
        p.shared.position_frames() == 0 && p.retained_stream_start() == 0
    })
    .await;
    eprintln!("large WAVE complete restarted suffix begins");
    let got = suffix(&mut p, frames as usize).await;
    assert_eq!(got.len(), frames as usize);
    assert!(
        got.iter().all(|x| x.to_bits() == 0.25f32.to_bits()),
        "large HTTP WAVE lost complete PCM or final samples"
    );
    assert!(
        server.range_requests() > count,
        "large HTTP WAVE did not reopen the original pinned source"
    );
    finish(&mut p).await;
    assert_eq!(server.initial_eof.load(Ordering::Acquire), 0);
    println!(
        "MIGO_LARGE_HTTP_WAVE_CONTROL samples={} bits={} complete_tail=true",
        got.len(),
        0.25f32.to_bits()
    );
}
