//! Public WAVE controls over the real session-sealed File and native consumer.
use super::controls::{finish, suffix, wait};
use super::*;
fn data(frames: usize, channels: u16, rate: u32, tail: usize) -> Vec<u8> {
    let mut data = crate::streaming_codecs::tests::wav(rate, channels, frames);
    if tail != 0 {
        data.extend(b"LIST");
        data.extend((tail as u32).to_le_bytes());
        data.resize(data.len() + tail + tail % 2, 0);
        let size = (data.len() - 8) as u32;
        data[4..8].copy_from_slice(&size.to_le_bytes());
    }
    data
}
async fn load(f: &Fixture, file: Arc<dyn AudioFile>, channels: u32, rate: u32) -> InnerAudioPlayer {
    let mut p = InnerAudioPlayer::new(919, channels);
    let token = p.begin_load_for(8181).unwrap();
    let (tx, rx) = tokio::sync::oneshot::channel();
    crate::audio_thread::start_inner_file_stream(
        &mut p,
        &f.host.audio_cpu(),
        rate,
        token,
        file,
        tx,
    );
    wait(&mut p, |p| p.shared.is_loaded()).await;
    rx.await.unwrap().unwrap();
    p
}
fn compare(got: &[f32], want: &[f32], key: &str) {
    assert_eq!(
        got.len(),
        want.len(),
        "WAVE control complete suffix length: {key}"
    );
    let max = got
        .iter()
        .zip(want)
        .map(|(a, b)| (*a - *b).abs())
        .fold(0f32, f32::max);
    assert!(
        max <= 0.00001 && got.iter().chain(want).all(|n| n.is_finite()),
        "WAVE control complete PCM: {key},error={max}"
    );
    if std::env::var_os("MIGO_DUMP_WAVE_CONTROL_PCM").is_some() {
        println!(
            "MIGO_WAVE_CONTROL_PCM {key} {:?}",
            got.iter().map(|x| x.to_bits()).collect::<Vec<_>>()
        );
    }
}
#[tokio::test]
async fn public_wave_seek_restart_and_exact_endpoint_keep_every_pcm_sample() {
    let f = Fixture::new().await;
    for (ch, frames, source_rate, tail) in [
        (1, 9737, 44100, 0),
        (2, 9737, 44100, 37),
        (6, 9737, 48000, 37),
        (32, 257, 48000, 0),
    ] {
        let bytes = data(frames, ch, source_rate, tail);
        for rate in [3000, 16000, 48000] {
            let reference = crate::decoder::decode_for_rate(&bytes, rate).unwrap();
            let mut p = load(&f, f.source(&bytes).await, ch as u32, rate).await;
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
    let f = Fixture::new().await;
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
            let mut p = load(&f, f.source(&bytes).await, ch as u32, rate).await;
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
async fn actual_large_wave_reclaims_pcm_then_reopens_the_sealed_source_and_plays_the_full_tail() {
    let f = Fixture::new().await;
    let frames = 8_388_737u32;
    let path = f.paths.user_data_dir().join("large-wave");
    let mut file = fs::File::create(&path).unwrap();
    file.write_all(&super::wave(frames)).unwrap();
    let chunk = [0u8, 32].repeat(32768);
    let mut left = frames as usize;
    while left != 0 {
        let n = left.min(32768);
        file.write_all(&chunk[..n * 2]).unwrap();
        left -= n;
    }
    drop(file);
    let source = migo_services::audio::LocalAudioFile::session(f.host.clone(), &f.paths)
        .source(f.vfs.clone(), "/user/large-wave".into())
        .await
        .unwrap();
    assert!(source.length() > 16 * 1024 * 1024);
    let mut p = load(&f, source, 1, 48000).await;
    assert!(
        p.retains_stream_source(),
        "actual WAVE selector did not activate source controls"
    );
    let got = suffix(&mut p, 2_200_007).await;
    assert!(got.iter().all(|n| n.to_bits() == 0.25f32.to_bits()));
    assert!(
        p.retained_stream_start() > 0,
        "large WAVE never reclaimed its old PCM"
    );
    p.pause();
    p.seek(0.0);
    wait(&mut p, |p| {
        p.shared.position_frames() == 0 && p.retained_stream_start() == 0
    })
    .await;
    let got = suffix(&mut p, frames as usize).await;
    assert_eq!(got.len(), frames as usize);
    assert!(
        got.iter().all(|n| n.to_bits() == 0.25f32.to_bits()),
        "restarted large WAVE lost actual PCM or complete tail"
    );
    finish(&mut p).await;
    p.seek(p.shared.duration());
    wait(&mut p, |p| {
        p.shared.position_frames() == frames as u64 && p.retained_stream_start() == frames as u64
    })
    .await;
    wait(&mut p, |p| p.is_stream_complete()).await;
    println!(
        "MIGO_LARGE_WAVE_CONTROL samples={} bits={} complete_tail=true",
        frames,
        0.25f32.to_bits()
    );
}

use super::controls::{Producer, Stalled};
#[tokio::test]
async fn wave_revocation_between_preparation_and_ack_keeps_the_old_revision() {
    let f = Fixture::new().await;
    let file = Arc::new(Stalled {
        file: f.source(&data(120000, 2, 44100, 37)).await,
        pause: AtomicBool::new(false),
        revoked: AtomicBool::new(false),
        changed: tokio::sync::Notify::new(),
        pending: std::sync::Mutex::new(None),
    });
    let mut p = Producer::new(&f, file.clone());
    p.drain(streaming::StreamRevision::INITIAL).await;
    let answer = crate::stream_control::request(
        p.state.control_sender().unwrap(),
        211,
        tokio::time::Instant::now() + WAIT,
    )
    .unwrap()
    .await
    .unwrap();
    let crate::stream_control::Reply::Prepared(prepared) = answer else {
        panic!("live file did not prepare seek before revocation")
    };
    file.revoked.store(true, Ordering::Release);
    file.changed.notify_waiters();
    prepared.accept.send(()).unwrap();
    assert!(
        matches!(
            tokio::time::timeout(WAIT, p.rx.recv()).await.unwrap(),
            Some(StreamMsg::Error {
                revision: streaming::StreamRevision::INITIAL,
                ..
            })
        ),
        "revoked File published a new revision after consumer ACK"
    );
    p.ended().await;
    assert_eq!(
        p.errors.lock().unwrap().as_ref().map(|e| e.code),
        Some(ErrorCode::IoError)
    );
    assert!(p.rx.try_recv().is_err());
}

#[tokio::test]
async fn wave_absolute_control_deadline_covers_the_first_actual_media_read() {
    let f = Fixture::new().await;
    let bytes = data(120000, 2, 44100, 37);
    let file = f.source(&bytes).await;
    let stalled = Arc::new(Stalled {
        file,
        pause: AtomicBool::new(false),
        revoked: AtomicBool::new(false),
        changed: tokio::sync::Notify::new(),
        pending: std::sync::Mutex::new(None),
    });
    let mut p = Producer::new(&f, stalled.clone());
    p.drain(streaming::StreamRevision::INITIAL).await;
    stalled.pause.store(true, Ordering::Release);
    let answer = crate::stream_control::request(
        p.state.control_sender().unwrap(),
        4868,
        tokio::time::Instant::now() + Duration::from_secs(1),
    )
    .unwrap();
    let result = tokio::time::timeout(Duration::from_secs(2), p.rx.recv()).await;
    assert!(
        matches!(
            result,
            Ok(Some(StreamMsg::Error {
                revision: streaming::StreamRevision::INITIAL,
                ..
            }))
        ),
        "file control media read renewed or ignored its original deadline"
    );
    p.ended().await;
    assert_eq!(
        p.errors.lock().unwrap().as_ref().map(|e| e.code),
        Some(ErrorCode::Timeout)
    );
    assert!(
        answer.await.is_err(),
        "failed file source acknowledged a new revision"
    );
    assert!(
        stalled.pending.lock().unwrap().is_some(),
        "control never reached the actual blocked media read"
    );
    assert!(p.rx.try_recv().is_err());
}

#[tokio::test]
async fn wave_refusal_and_actual_consumer_ack_preserve_original_revision() {
    use crate::stream_control::{Reply, request};
    let f = Fixture::new().await;
    let bytes = data(9737, 2, 44100, 37);
    let mut p = Producer::new(&f, f.source(&bytes).await);
    let first = p.drain(streaming::StreamRevision::INITIAL).await;
    assert_eq!(
        first,
        crate::decoder::decode_for_rate(&bytes, 16000)
            .unwrap()
            .samples
    );
    for target in [-1, 100000] {
        let answer = request(
            p.state.control_sender().unwrap(),
            target,
            tokio::time::Instant::now() + WAIT,
        )
        .unwrap()
        .await
        .unwrap();
        assert!(
            matches!(answer, Reply::Rejected(ErrorCode::InvalidArgument)),
            "invalid WAVE seek changed its original source"
        );
        assert!(p.rx.try_recv().is_err() && !p.state.is_cancelled());
    }
    let answer = request(
        p.state.control_sender().unwrap(),
        0,
        tokio::time::Instant::now() + WAIT,
    )
    .unwrap()
    .await
    .unwrap();
    let Reply::Prepared(prepared) = answer else {
        panic!("WAVE original source did not prepare restart")
    };
    tokio::task::yield_now().await;
    assert!(
        !prepared.accept.is_closed(),
        "WAVE dropped actual consumer ACK"
    );
    assert!(
        p.rx.try_recv().is_err(),
        "WAVE published a new revision before consumer ACK"
    );
    let revision = prepared.revision;
    prepared.accept.send(()).unwrap();
    let pcm = p.drain(revision).await;
    compare(&pcm, &first, "ack/accepted-complete");
    let answer = request(
        p.state.control_sender().unwrap(),
        0,
        tokio::time::Instant::now() + WAIT,
    )
    .unwrap()
    .await
    .unwrap();
    let Reply::Prepared(prepared) = answer else {
        panic!()
    };
    drop(prepared);
    p.ended().await;
    assert!(p.state.is_cancelled());
}

#[tokio::test]
async fn live_wave_source_seek_discards_the_original_unaccepted_encoded_suffix() {
    use crate::stream_control::{Reply, request};
    let f = Fixture::new().await;
    let bytes = data(240137, 2, 44100, 37);
    let reference = crate::decoder::decode_for_rate(&bytes, 16000).unwrap();
    let mut p = Producer::new(&f, f.source(&bytes).await);
    let end = Instant::now() + WAIT;
    while !p.state.publication_waiting.load(Ordering::Acquire) {
        assert!(
            Instant::now() < end,
            "WAVE publication never reached real backpressure"
        );
        tokio::task::yield_now().await;
    }
    assert_eq!(p.rx.len(), streaming::STREAM_CHANNEL_CAPACITY);
    let target = 170003u64;
    assert!(
        p.state.bytes_downloaded.load(Ordering::Acquire) < target * 4,
        "WAVE seek fixture already read the requested source position"
    );
    let answer = request(
        p.state.control_sender().unwrap(),
        target as i64,
        tokio::time::Instant::now() + WAIT,
    )
    .unwrap()
    .await
    .unwrap();
    let Reply::Prepared(prepared) = answer else {
        panic!("WAVE live source seek refused")
    };
    let revision = prepared.revision;
    let mut prefix = Vec::new();
    while let Ok(message) = p.rx.try_recv() {
        match message {
            StreamMsg::Ready { revision, .. } => {
                assert_eq!(revision, streaming::StreamRevision::INITIAL)
            }
            StreamMsg::Samples { revision, pcm } => {
                assert_eq!(revision, streaming::StreamRevision::INITIAL);
                prefix.extend_from_slice(&pcm);
            }
            _ => panic!("WAVE live source ended before the consumer ACK"),
        }
    }
    assert!(!prefix.is_empty());
    compare(
        &prefix,
        &reference.samples[..prefix.len()],
        "live-seek/16000/prefix",
    );
    prepared.accept.send(()).unwrap();
    let got = p.drain(revision).await;
    let mapped = (target as u128 * 16000u128).div_ceil(44100) as usize;
    compare(
        &got,
        &reference.samples[mapped * 2..],
        "live-seek/16000/source-suffix",
    );
}

#[tokio::test]
async fn unknown_length_wave_keeps_single_pass_eof_and_existing_buffered_controls() {
    let f = Fixture::new().await;
    let mut bytes = data(9737, 2, 44100, 0);
    bytes[4..8].copy_from_slice(&u32::MAX.to_le_bytes());
    bytes[40..44].copy_from_slice(&u32::MAX.to_le_bytes());
    let state = StreamingState::new_optional_controls();
    let (tx, mut rx) = streaming::stream_channel();
    let error = Arc::new(std::sync::Mutex::new(None));
    let task = tokio::spawn(run(
        f.host.audio_cpu(),
        f.source(&bytes).await,
        tx,
        state.clone(),
        16000,
        error.clone(),
    ));
    let mut pcm = Vec::new();
    let mut ended = 0;
    while let Some(message) = tokio::time::timeout(WAIT, rx.recv()).await.unwrap() {
        match message {
            StreamMsg::Ready { .. } => assert!(
                state.control_sender().is_none() && !state.retains_source(),
                "unknown WAVE installed unavailable source controls"
            ),
            StreamMsg::Samples { pcm: p, .. } => pcm.extend_from_slice(&p),
            StreamMsg::Done { .. } => ended += 1,
            StreamMsg::Error { message, .. } => {
                panic!("unknown WAVE single pass failed: {message}")
            }
        }
    }
    task.await.unwrap();
    assert_eq!(ended, 1);
    assert!(
        state.control_sender().is_none() && !state.retains_source(),
        "unknown WAVE installed unavailable source controls"
    );
    assert!(error.lock().unwrap().is_none());
    compare(
        &pcm,
        &crate::decoder::decode_for_rate(&bytes, 16000)
            .unwrap()
            .samples,
        "unknown/16000/single-pass",
    );
    let mut p = load(&f, f.source(&bytes).await, 2, 16000).await;
    let got = suffix(&mut p, pcm.len() / 2).await;
    compare(&got, &pcm, "unknown/16000/public-complete");
    finish(&mut p).await;
    let at = 311u64;
    p.seek((at as f64 + 0.25) / 16000.0);
    assert_eq!(
        p.shared.position_frames(),
        at,
        "unknown WAVE lost buffered seek"
    );
    let got = suffix(&mut p, pcm.len() / 2 - at as usize).await;
    compare(&got, &pcm[at as usize * 2..], "unknown/16000/buffered-seek");
}

#[tokio::test]
async fn truncated_pinned_wave_refuses_before_ready_or_source_control_installation() {
    let f = Fixture::new().await;
    let mut bytes = data(9737, 2, 44100, 0);
    bytes.pop();
    let state = StreamingState::new_optional_controls();
    let (tx, mut rx) = streaming::stream_channel();
    let error = Arc::new(std::sync::Mutex::new(None));
    let task = tokio::spawn(run(
        f.host.audio_cpu(),
        f.source(&bytes).await,
        tx,
        state.clone(),
        16000,
        error.clone(),
    ));
    let mut errors = 0;
    while let Some(message) = tokio::time::timeout(WAIT, rx.recv()).await.unwrap() {
        assert!(
            matches!(message, StreamMsg::Error { .. }),
            "truncated pinned WAVE published successful PCM or Ready"
        );
        errors += 1;
    }
    task.await.unwrap();
    assert_eq!(errors, 1);
    assert!(
        state.control_sender().is_none() && !state.retains_source(),
        "truncated WAVE exposed unavailable source controls"
    );
    assert_eq!(
        error.lock().unwrap().as_ref().map(|e| e.code),
        Some(ErrorCode::InvalidArgument)
    );
}

#[tokio::test]
async fn retained_wave_restarts_while_both_original_file_slots_are_in_flight() {
    use crate::stream_control::{Reply, request};
    let f = Fixture::new().await;
    let bytes = data(9737, 2, 44100, 37);
    let file = f.source(&bytes).await;
    let reference = crate::decoder::decode_for_rate(&bytes, 16000).unwrap();
    let mut producer = Producer::new(&f, file.clone());
    let pcm = producer.drain(streaming::StreamRevision::INITIAL).await;
    compare(&pcm, &reference.samples, "slots/16000/initial");
    let cancel = StreamingState::new();
    let first = file
        .clone()
        .read_at(0, 8, cancel.clone(), Instant::now() + WAIT)
        .unwrap()
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let second = file
        .clone()
        .read_at(8, 8, cancel.clone(), Instant::now() + WAIT)
        .unwrap()
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(first.as_ref(), &bytes[..8]);
    assert_eq!(second.as_ref(), &bytes[8..16]);
    let refused = file
        .clone()
        .read_at(0, 8, cancel, Instant::now() + WAIT)
        .unwrap()
        .await
        .unwrap()
        .unwrap();
    assert!(
        refused.is_err(),
        "WAVE pressure fixture did not occupy both original slots"
    );
    let reply = request(
        producer.state.control_sender().unwrap(),
        0,
        tokio::time::Instant::now() + WAIT,
    )
    .unwrap()
    .await
    .expect("retained WAVE restart requested saturated source IO");
    let Reply::Prepared(prepared) = reply else {
        panic!("retained WAVE restart was refused under existing slot pressure")
    };
    let revision = prepared.revision;
    prepared.accept.send(()).unwrap();
    let got = producer.drain(revision).await;
    compare(&got, &reference.samples, "slots/16000/restart");
    assert_eq!(first.as_ref(), &bytes[..8]);
    assert_eq!(second.as_ref(), &bytes[8..16]);
    drop((first, second));
}
