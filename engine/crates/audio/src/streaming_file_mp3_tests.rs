//! Actual sealed File, original CPU and public player acknowledgement.
use super::controls::{finish, player, suffix, wait};
use super::*;
fn compare(got: &[f32], want: &[f32], key: &str) {
    assert!(
        got.len() == want.len()
            && got
                .iter()
                .zip(want)
                .all(|(a, b)| a.is_finite() && b.is_finite() && (*a - *b).abs() <= 0.00001),
        "public File MP3 complete PCM changed: {key}, lengths={}/{}, first={:?}",
        got.len(),
        want.len(),
        got.iter()
            .zip(want)
            .position(|(a, b)| (*a - *b).abs() > 0.00001)
    );
    if let Some(dir) = std::env::var_os("MIGO_MP3_CONTROL_PCM_DIR") {
        std::fs::write(
            std::path::Path::new(&dir).join(format!("file-{key}.f32le")),
            got.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<_>>(),
        )
        .unwrap();
    }
}

#[tokio::test]
async fn public_mp3_seek_restart_and_exact_endpoint_keep_complete_gapless_pcm() {
    let f = Fixture::new().await;
    for (name, data) in [
        (
            "music",
            include_bytes!("decoder/mp3/fixtures/music.mp3").as_slice(),
        ),
        (
            "mpeg2",
            include_bytes!("decoder/mp3/fixtures/gapless-mpeg2.mp3").as_slice(),
        ),
    ] {
        let native = crate::decoder::decode(data).unwrap();
        let frames = native.samples.len() / native.channels as usize;
        for rate in [16000, 44100, 48000] {
            let reference = crate::decoder::decode_for_rate(data, rate).unwrap();
            let mut p = player(&f, data, rate).await;
            assert!(
                p.retains_stream_source(),
                "actual MP3 actor did not activate source controls"
            );
            for target in [frames / 2, frames - 1, frames, 0] {
                p.pause();
                let old = p.shared.position_frames();
                let seconds = if target == frames {
                    p.shared.duration()
                } else {
                    (target as f64 + 0.25) / native.sample_rate as f64
                };
                let start = (target as u64 * rate as u64).div_ceil(native.sample_rate as u64);
                p.seek(seconds);
                assert_eq!(
                    p.shared.position_frames(),
                    old,
                    "MP3 seek published before real source ACK"
                );
                wait(&mut p, |p| {
                    p.retained_stream_start() == start && p.shared.position_frames() == start
                })
                .await;
                let got = suffix(
                    &mut p,
                    reference.samples.len() / native.channels as usize - start as usize,
                )
                .await;
                let expected = &reference.samples[start as usize * native.channels as usize..];
                assert!(
                    got.len() == expected.len()
                        && got
                            .iter()
                            .zip(expected)
                            .all(|(a, b)| a.to_bits() == b.to_bits()),
                    "public File MP3 complete suffix changed"
                );
                compare(&got, expected, &format!("{name}-{rate}-{target}-seek"));
                if got.is_empty() {
                    wait(&mut p, |p| p.is_stream_complete()).await;
                }
                p.stop();
                p.play();
                wait(&mut p, |p| {
                    p.retained_stream_start() == 0 && p.shared.position_frames() == 0
                })
                .await;
                let got = suffix(&mut p, reference.samples.len() / native.channels as usize).await;
                assert!(
                    got.len() == reference.samples.len()
                        && got
                            .iter()
                            .zip(&reference.samples)
                            .all(|(a, b)| a.to_bits() == b.to_bits()),
                    "public File MP3 restart changed complete PCM"
                );
                compare(
                    &got,
                    &reference.samples,
                    &format!("{name}-{rate}-{target}-restart"),
                );
            }
            finish(&mut p).await;
        }
    }
}
#[tokio::test]
async fn unknown_duration_mp3_keeps_single_pass_and_never_exposes_source_controls() {
    let f = Fixture::new().await;
    let data = crate::mp3_fixture::stream(32);
    let file = f.source(&data).await;
    let state = StreamingState::new_reusable();
    let (tx, mut rx) = streaming::stream_channel();
    let errors = Arc::new(std::sync::Mutex::new(None));
    let task = tokio::spawn(run(
        f.host.audio_cpu(),
        file,
        tx,
        state.clone(),
        16000,
        errors.clone(),
    ));
    let mut pcm = Vec::new();
    let (mut ready, mut done) = (0, 0);
    while let Some(msg) = tokio::time::timeout(WAIT, rx.recv()).await.unwrap() {
        match msg {
            StreamMsg::Ready { .. } => {
                ready += 1;
                assert!(
                    state.timeline().is_none() && state.control_sender().is_none(),
                    "unknown MP3 exposed source replay without a clock"
                );
            }
            StreamMsg::Samples { pcm: out, .. } => pcm.extend_from_slice(&out),
            StreamMsg::Done { .. } => done += 1,
            StreamMsg::Error { message, .. } => {
                panic!("unknown MP3 failed sequential PCM: {message}")
            }
        }
    }
    task.await.unwrap();
    assert_eq!((ready, done), (1, 1));
    assert_eq!(
        pcm,
        crate::decoder::decode_for_rate(&data, 16000)
            .unwrap()
            .samples
    );
    assert!(errors.lock().unwrap().is_none());
}

#[tokio::test]
async fn public_mp3_loop_cutovers_keep_global_phase_and_tiny_tracks_reuse_the_original_window() {
    let f = Fixture::new().await;
    for (name, data) in [
        (
            "tiny-mono",
            include_bytes!("decoder/mp3/fixtures/tiny-47-mono.mp3").as_slice(),
        ),
        (
            "tiny-stereo",
            include_bytes!("decoder/mp3/fixtures/tiny-47-stereo.mp3").as_slice(),
        ),
        (
            "mpeg2",
            include_bytes!("decoder/mp3/fixtures/gapless-mpeg2.mp3").as_slice(),
        ),
    ] {
        let native = crate::decoder::decode(data).unwrap();
        let frames = native.samples.len() / native.channels as usize;
        for rate in [8000, 16000, 44100, 48000] {
            let full = |n| {
                crate::resample_job::tests::reference(
                    &crate::decoder::DecodedAudio {
                        samples: native.samples.repeat(n),
                        sample_rate: native.sample_rate,
                        channels: native.channels,
                    },
                    rate,
                )
            };
            let mut p = player(&f, data, rate).await;
            p.set_loop_enabled(true);
            wait(&mut p, |p| p.shared.loop_enabled()).await;
            // More than the shared 64 source-read burst, also across SRC tails.
            let cycles = if frames < 1024 { 100 } else { 3 };
            let count =
                (frames as u64 * cycles * rate as u64 / native.sample_rate as u64) as usize + 3;
            let got = suffix(&mut p, count).await;
            let a = full(cycles as usize + 512);
            let b = full(cycles as usize + 1024);
            assert_eq!(
                &a[..got.len()],
                &b[..got.len()],
                "MP3 loop oracle has a finite future tail"
            );
            compare(&got, &a[..got.len()], &format!("{name}-{rate}-loop"));
            p.pause();
            let at = p.shared.position_frames();
            p.set_loop_enabled(false);
            wait(&mut p, |p| {
                !p.shared.loop_enabled() && p.retained_stream_start() == at
            })
            .await;
            let cycles = (at as u128 * native.sample_rate as u128 / rate as u128 / frames as u128
                + 1) as usize;
            let finite = full(cycles);
            let got = suffix(
                &mut p,
                finite.len() / native.channels as usize - at as usize,
            )
            .await;
            compare(
                &got,
                &finite[at as usize * native.channels as usize..],
                &format!("{name}-{rate}-disabled"),
            );
            finish(&mut p).await;
            p.set_loop_enabled(true);
            p.play();
            wait(&mut p, |p| {
                p.shared.loop_enabled()
                    && p.retained_stream_start() == 0
                    && p.shared.position_frames() == 0
            })
            .await;
            let got = suffix(&mut p, 137).await;
            compare(
                &got,
                &a[..got.len()],
                &format!("{name}-{rate}-loop-restart"),
            );
        }
    }
}
#[tokio::test]
async fn mp3_invalid_and_expired_requests_preserve_pcm_until_real_consumer_ack() {
    use super::controls::Producer;
    use crate::stream_control::{Reply, request};
    let f = Fixture::new().await;
    let data = include_bytes!("decoder/mp3/fixtures/gapless-mpeg2.mp3");
    let mut p = Producer::new(&f, f.source(data).await);
    let full = p.drain(streaming::StreamRevision::INITIAL).await;
    assert_eq!(
        full,
        crate::decoder::decode_for_rate(data, 16000)
            .unwrap()
            .samples
    );
    let clock = p.state.timeline().unwrap();
    for target in [-1, clock.seek_frame(clock.seconds()).unwrap() + 1] {
        let reply = request(
            p.state.control_sender().unwrap(),
            target,
            tokio::time::Instant::now() + WAIT,
        )
        .unwrap()
        .await
        .unwrap();
        assert!(
            matches!(reply, Reply::Rejected(ErrorCode::InvalidArgument)),
            "invalid MP3 request mutated the original source"
        );
        assert!(p.rx.try_recv().is_err() && !p.state.is_cancelled());
    }
    let reply = request(
        p.state.control_sender().unwrap(),
        0,
        tokio::time::Instant::now() - Duration::from_millis(1),
    )
    .unwrap()
    .await
    .unwrap();
    assert!(
        matches!(reply, Reply::NotStarted(ErrorCode::Timeout)),
        "expired MP3 request mutated the original source"
    );
    let Reply::Prepared(prepared) = request(
        p.state.control_sender().unwrap(),
        211,
        tokio::time::Instant::now() + WAIT,
    )
    .unwrap()
    .await
    .unwrap() else {
        panic!("MP3 did not prepare a source acknowledgement")
    };
    assert!(
        tokio::time::timeout(Duration::from_millis(20), p.rx.recv())
            .await
            .is_err(),
        "MP3 published a revision before consumer ACK"
    );
    assert!(
        !p.task.as_ref().unwrap().is_finished(),
        "MP3 EOF lost its original owner"
    );
    let revision = prepared.revision;
    let start = prepared.mapped_output().unwrap() as usize;
    prepared.accept.send(()).unwrap();
    let got = p.drain(revision).await;
    compare(&got, &full[start..], "refusal-then-ack");
    assert!(p.errors.lock().unwrap().is_none());
    p.state.cancel();
    p.ended().await;
}

#[tokio::test]
async fn cached_mp3_revocation_between_preparation_and_ack_keeps_the_old_revision() {
    use super::controls::{Producer, Stalled};
    let f = Fixture::new().await;
    let file = Arc::new(Stalled {
        file: f
            .source(include_bytes!("decoder/mp3/fixtures/tiny-47-mono.mp3"))
            .await,
        pause: AtomicBool::new(false),
        revoked: AtomicBool::new(false),
        changed: tokio::sync::Notify::new(),
        pending: std::sync::Mutex::new(None),
    });
    let mut p = Producer::new(&f, file.clone());
    p.drain(streaming::StreamRevision::INITIAL).await;
    let crate::stream_control::Reply::Prepared(prepared) = crate::stream_control::request(
        p.state.control_sender().unwrap(),
        7,
        tokio::time::Instant::now() + WAIT,
    )
    .unwrap()
    .await
    .unwrap() else {
        panic!("cached MP3 did not prepare source controls")
    };
    // No source read is necessary for the retained tiny encoded window.
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
        "cached MP3 published a revision after source revocation"
    );
    p.ended().await;
    assert_eq!(
        p.errors.lock().unwrap().as_ref().map(|e| e.code),
        Some(ErrorCode::IoError)
    );
    assert!(p.rx.try_recv().is_err());
}
#[tokio::test]
async fn mp3_replay_read_uses_the_original_absolute_control_deadline_before_ack() {
    use super::controls::{Producer, Stalled};
    let f = Fixture::new().await;
    let file = Arc::new(Stalled {
        file: f
            .source(include_bytes!("decoder/mp3/fixtures/music.mp3"))
            .await,
        pause: AtomicBool::new(false),
        revoked: AtomicBool::new(false),
        changed: tokio::sync::Notify::new(),
        pending: std::sync::Mutex::new(None),
    });
    let mut p = Producer::new(&f, file.clone());
    p.drain(streaming::StreamRevision::INITIAL).await;
    file.pause.store(true, Ordering::Release);
    let answer = crate::stream_control::request(
        p.state.control_sender().unwrap(),
        211,
        tokio::time::Instant::now() + Duration::from_millis(500),
    )
    .unwrap();
    assert!(
        matches!(
            tokio::time::timeout(Duration::from_secs(2), p.rx.recv()).await,
            Ok(Some(StreamMsg::Error {
                revision: streaming::StreamRevision::INITIAL,
                ..
            }))
        ),
        "MP3 replay read renewed or ignored its control deadline"
    );
    p.ended().await;
    assert_eq!(
        p.errors.lock().unwrap().as_ref().map(|e| e.code),
        Some(ErrorCode::Timeout)
    );
    assert!(
        answer.await.is_err(),
        "failed MP3 replay read acknowledged a new revision"
    );
    assert!(
        file.pending.lock().unwrap().is_some(),
        "MP3 replay skipped the actual sealed-source read"
    );
}
#[tokio::test]
async fn actual_large_mp3_reclaims_pcm_then_replays_the_same_file_and_checks_the_complete_tail() {
    use crate::mp3_fixture as f;
    let fixture = Fixture::new().await;
    let count = crate::limits::MAX_AUDIO_PCM_SAMPLES as usize / f::SAMPLES_PER_FRAME + 1;
    let mut tag = f::stream(1);
    tag[36..40].copy_from_slice(b"Info");
    tag[40..44].copy_from_slice(&1u32.to_be_bytes());
    tag[44..48].copy_from_slice(&(count as u32).to_be_bytes());
    let data = [tag, f::stream(count)].concat();
    let frames = (count * f::SAMPLES_PER_FRAME / f::CHANNELS) as u64;
    assert!(
        crate::decoder::decode(&data).is_err(),
        "whole-file PCM quota changed"
    );
    let source = fixture.source(&data).await;
    let mut p = InnerAudioPlayer::new(919, 2);
    let token = p.begin_load_for(8181).unwrap();
    let (tx, rx) = tokio::sync::oneshot::channel();
    crate::audio_thread::start_inner_file_stream(
        &mut p,
        &fixture.host.audio_cpu(),
        f::SAMPLE_RATE,
        token,
        source,
        tx,
    );
    wait(&mut p, |p| p.shared.is_loaded()).await;
    rx.await.unwrap().unwrap();
    assert!(
        p.retains_stream_source(),
        "large MP3 actor did not retain source controls"
    );
    async fn zeros(p: &mut InnerAudioPlayer, frames: u64) {
        let mut got = 0;
        let mut out = [0.; 8192];
        let end = Instant::now() + WAIT;
        p.play();
        while got < frames {
            p.poll_stream();
            assert!(
                !p.drain_events()
                    .any(|e| e.event_type == InnerAudioEventType::Error),
                "large MP3 failed actual replay"
            );
            let offered = ((frames - got) * 2).min(out.len() as u64) as usize;
            out.fill(0.);
            let before = p.shared.position_frames();
            let active = p.process(&mut out[..offered]);
            let n = if active {
                p.shared.position_frames().saturating_sub(before)
            } else if p.shared.state() == PlaybackState::Stopped && p.shared.is_loaded() {
                frames - got
            } else {
                0
            };
            assert!(
                n * 2 <= offered as u64,
                "large MP3 ended before the complete tail"
            );
            assert!(
                out[..offered]
                    .iter()
                    .all(|n| n.to_bits() == 0.0f32.to_bits()),
                "large MP3 actual replay PCM changed"
            );
            got += n;
            assert!(
                p.physical_pcm_bytes() < 4 * 1024 * 1024,
                "large MP3 retained whole-track PCM"
            );
            assert!(Instant::now() < end, "large MP3 actual replay stalled");
            tokio::task::yield_now().await;
        }
        assert_eq!(got, frames);
    }
    zeros(&mut p, 2_200_007).await;
    assert!(
        p.retained_stream_start() > 0,
        "large MP3 never reclaimed old PCM"
    );
    p.pause();
    p.seek(0.);
    wait(&mut p, |p| {
        p.retained_stream_start() == 0 && p.shared.position_frames() == 0
    })
    .await;
    zeros(&mut p, frames).await;
    finish(&mut p).await;
    p.seek(p.shared.duration());
    wait(&mut p, |p| {
        p.retained_stream_start() == frames
            && p.shared.position_frames() == frames
            && p.is_stream_complete()
    })
    .await;
    println!(
        "MIGO_LARGE_MP3_CONTROL samples={} encoded={} complete_zero_pcm_checked=true reclaimed_then_source_replay=true",
        frames * 2,
        data.len()
    );
}

#[tokio::test]
async fn actual_file_actor_retains_known_vorbis_and_keeps_unknown_clocks_single_pass() {
    let f = Fixture::new().await;
    for (name, data) in [
        ("unknown-mp3", crate::mp3_fixture::stream(32)),
        (
            "vorbis",
            include_bytes!("decoder/ogg-fixtures/cross.ogg").to_vec(),
        ),
        (
            "adts",
            include_bytes!("decoder/aac-fixtures/adts-1.aac").to_vec(),
        ),
    ] {
        let reference = crate::decoder::decode_for_rate(&data, 16000).unwrap();
        let mut p = player(&f, &data, 16000).await;
        assert!(
            p.retains_stream_source() == (name == "vorbis"),
            "{name} actor chose the wrong source control capability"
        );
        let got = suffix(
            &mut p,
            reference.samples.len() / reference.channels as usize,
        )
        .await;
        compare(&got, &reference.samples, &format!("{name}-single-pass"));
        finish(&mut p).await;
    }
}

#[tokio::test]
async fn cached_mp3_cpu_step_revalidates_the_same_pinned_file_without_a_read() {
    use super::controls::Stalled;
    let f = Fixture::new().await;
    let data = include_bytes!("decoder/mp3/fixtures/tiny-47-mono.mp3");
    let file = Arc::new(Stalled {
        file: f.source(data).await,
        pause: AtomicBool::new(false),
        revoked: AtomicBool::new(false),
        changed: tokio::sync::Notify::new(),
        pending: std::sync::Mutex::new(None),
    });
    let capability: Arc<dyn AudioFile> = file.clone();
    let weak = Arc::downgrade(&capability);
    let length = capability.length();
    let slot = OffWorker::new(None::<FileDecoder>, f.host.audio_cpu());
    let (slot, ()) = slot
        .with(move |slot| {
            let mut d = FileDecoder::new(16000, length, false, true, weak);
            let mut out = Vec::with_capacity(32768);
            let mut pos = 0;
            let mut eof = false;
            let mut done = false;
            for _ in 0..100000 {
                out.clear();
                let step = d
                    .step(&data[pos..], eof, &mut out, &AtomicBool::new(false))
                    .unwrap();
                pos += step.consumed;
                if step.progress == transport::Progress::Finished {
                    done = true;
                    break;
                }
                if step.progress == transport::Progress::NeedInput && pos == data.len() {
                    eof = true;
                }
            }
            assert!(done);
            let FileDecoder::Sequential(decoder, _) = &mut d else {
                panic!("MP3 File lost its sequential codec")
            };
            assert_eq!(
                decoder.reposition_mp3(0, None).unwrap().0,
                None,
                "tiny MP3 replay should use its original window"
            );
            *slot = Some(d);
        })
        .await
        .unwrap();
    file.revoked.store(true, Ordering::Release);
    let (slot, ()) = slot
        .with(|slot| {
            let mut pcm = Vec::with_capacity(32768);
            let error = slot
                .as_mut()
                .unwrap()
                .step(&[], true, &mut pcm, &AtomicBool::new(false))
                .err();
            assert_eq!(
                error.map(|e| e.code),
                Some(ErrorCode::IoError),
                "cached MP3 CPU replay ignored its revoked pinned file"
            );
            assert!(
                pcm.is_empty(),
                "cached MP3 published PCM after pinned-file revocation"
            );
        })
        .await
        .unwrap();
    assert!(file.pending.lock().unwrap().is_none());
    drop(slot);
    drop(capability);
}
