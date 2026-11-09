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
        "public File Vorbis complete PCM changed: {key}, lengths={}/{}, first={:?}",
        got.len(),
        want.len(),
        got.iter()
            .zip(want)
            .position(|(a, b)| (*a - *b).abs() > 0.00001)
    );
    if let Some(dir) = std::env::var_os("MIGO_VORBIS_CONTROL_PCM_DIR") {
        std::fs::write(
            std::path::Path::new(&dir).join(format!("file-{key}.f32le")),
            got.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<_>>(),
        )
        .unwrap();
    }
}

#[tokio::test]
async fn public_vorbis_seek_restart_and_exact_endpoint_keep_complete_pcm() {
    let f = Fixture::new().await;
    for (name, data) in [
        (
            "stereo",
            include_bytes!("decoder/ogg-fixtures/1.ogg").as_slice(),
        ),
        (
            "mono",
            include_bytes!("decoder/ogg-fixtures/2.ogg").as_slice(),
        ),
    ] {
        let native = crate::decoder::decode(data).unwrap();
        let frames = native.samples.len() / native.channels as usize;
        for rate in [16000, 44100, 48000] {
            let reference = crate::decoder::decode_for_rate(data, rate).unwrap();
            let mut p = player(&f, data, rate).await;
            assert!(
                p.retains_stream_source(),
                "actual Vorbis actor did not activate source controls"
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
                    "Vorbis seek published before real source ACK"
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
                            .all(|(a, b)| (*a - *b).abs() <= 1e-6),
                    "public File Vorbis complete suffix changed: {name}/{rate}/{target}, lengths={}/{}, max={}, first={:?}",
                    got.len(),
                    expected.len(),
                    got.iter()
                        .zip(expected)
                        .map(|(a, b)| (*a - *b).abs())
                        .fold(0f32, f32::max),
                    got.iter()
                        .zip(expected)
                        .position(|(a, b)| a.to_bits() != b.to_bits())
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
                    "public File Vorbis restart changed complete PCM"
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
async fn public_vorbis_loop_cutovers_keep_global_phase_and_tiny_tracks_reuse_the_original_window() {
    let f = Fixture::new().await;
    for (name, data) in [(
        "tiny",
        include_bytes!("decoder/ogg-fixtures/0.ogg").as_slice(),
    )] {
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
                "Vorbis loop oracle has a finite future tail"
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
async fn vorbis_invalid_and_expired_requests_preserve_pcm_until_real_consumer_ack() {
    use super::controls::Producer;
    use crate::stream_control::{Reply, request};
    let f = Fixture::new().await;
    let data = include_bytes!("decoder/ogg-fixtures/2.ogg");
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
            "invalid Vorbis request mutated the original source"
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
        "expired Vorbis request mutated the original source"
    );
    let Reply::Prepared(prepared) = request(
        p.state.control_sender().unwrap(),
        211,
        tokio::time::Instant::now() + WAIT,
    )
    .unwrap()
    .await
    .unwrap() else {
        panic!("Vorbis did not prepare a source acknowledgement")
    };
    assert!(
        tokio::time::timeout(Duration::from_millis(20), p.rx.recv())
            .await
            .is_err(),
        "Vorbis published a revision before consumer ACK"
    );
    assert!(
        !p.task.as_ref().unwrap().is_finished(),
        "Vorbis EOF lost its original owner"
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
async fn cross_page_and_eight_channel_public_seek_keep_complete_suffix_bits() {
    let f = Fixture::new().await;
    for (name, bytes) in [
        (
            "cross",
            include_bytes!("decoder/ogg-fixtures/cross.ogg").as_slice(),
        ),
        (
            "wide",
            include_bytes!("decoder/ogg-fixtures/max-window-8.ogg").as_slice(),
        ),
    ] {
        let native = crate::decoder::decode(bytes).unwrap();
        let frames = native.samples.len() / native.channels as usize;
        for rate in [16000, 48000] {
            let reference = crate::decoder::decode_for_rate(bytes, rate).unwrap();
            let mut p = player(&f, bytes, rate).await;
            let target = frames / 2;
            let start = (target as u64 * rate as u64).div_ceil(native.sample_rate as u64);
            p.seek((target as f64 + 0.25) / native.sample_rate as f64);
            wait(&mut p, |p| {
                p.shared.position_frames() == start && p.retained_stream_start() == start
            })
            .await;
            let want = &reference.samples[start as usize * native.channels as usize..];
            let got = suffix(&mut p, want.len() / native.channels as usize).await;
            // Equal values, not equal bits: played samples are mixed into a
            // cleared buffer, and `0.0 + -0.0` is `+0.0`. The reference is not
            // mixed, so a decoder that emits -0.0 -- NEON does where SSE gives
            // +0.0 -- differs from it only in the sign of zero. Any other change
            // still fails, and so does a NaN.
            assert!(
                got.len() == want.len() && got.iter().zip(want).all(|(a, b)| a == b),
                "Vorbis cross-page/multichannel complete suffix changed"
            );
            compare(&got, want, &format!("{name}-{rate}-seek"));
            finish(&mut p).await;
        }
    }
}
#[tokio::test]
async fn cached_vorbis_revalidates_the_same_file_capability_before_ack() {
    use super::controls::{Producer, Stalled};
    let f = Fixture::new().await;
    let file = Arc::new(Stalled {
        file: f.source(include_bytes!("decoder/ogg-fixtures/2.ogg")).await,
        revoked: AtomicBool::new(false),
        changed: tokio::sync::Notify::new(),
    });
    let mut p = Producer::new(&f, file.clone());
    p.drain(streaming::StreamRevision::INITIAL).await;
    let crate::stream_control::Reply::Prepared(first) = crate::stream_control::request(
        p.state.control_sender().unwrap(),
        0,
        tokio::time::Instant::now() + WAIT,
    )
    .unwrap()
    .await
    .unwrap() else {
        panic!("Vorbis did not prepare its original source")
    };
    let old = first.revision;
    first.accept.send(()).unwrap();
    p.drain(old).await;
    let crate::stream_control::Reply::Prepared(prepared) = crate::stream_control::request(
        p.state.control_sender().unwrap(),
        211,
        tokio::time::Instant::now() + WAIT,
    )
    .unwrap()
    .await
    .unwrap() else {
        panic!("Vorbis lost cached source preparation")
    };
    file.revoked.store(true, Ordering::Release);
    file.changed.notify_waiters();
    let accepted = prepared.revision;
    prepared.accept.send(()).unwrap();
    // The consumer already moved to the accepted revision: the failure is its.
    assert!(
        matches!(tokio::time::timeout(WAIT,p.rx.recv()).await.unwrap(),Some(StreamMsg::Error{revision,..}) if revision==accepted),
        "revoked Vorbis File published a new revision after ACK"
    );
    p.ended().await;
}
