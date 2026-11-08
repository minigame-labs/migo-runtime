//! Pinned MP3 controls through the real URL selector, Reader and original CPU.
use super::range_controls::{done, start_controlled};
use super::*;
use crate::inner_audio::InnerAudioPlayer;
use crate::stream_control::{Reply, request};
use crate::stream_open::file_tests::controls::{finish, suffix, wait};
const MUSIC: &[u8] = include_bytes!("decoder/mp3/fixtures/music.mp3");
const MPEG2: &[u8] = include_bytes!("decoder/mp3/fixtures/gapless-mpeg2.mp3");
async fn load(
    f: &controls::Context,
    bytes: &[u8],
    channels: u32,
    rate: u32,
    hold: Option<usize>,
) -> (InnerAudioPlayer, Arc<StreamingState>, Server) {
    let server = server_with_initial_prefix(bytes.to_vec(), Mode::Normal, hold, false).await;
    let state = StreamingState::new_url();
    let url = "http://indexed-product-fixture.invalid/wrong.wav";
    let rx = crate::streaming::start_streaming_download_with_spool(
        f.host.audio_cpu(),
        server.client(f.host.pools().clone()),
        url.into(),
        state.clone(),
        rate,
        Some(f.spool()),
    );
    let mut p = InnerAudioPlayer::new(919, channels);
    let token = p.begin_load_for(8181).unwrap();
    p.complete_stream_load(token, url.into(), rx, state.clone())
        .unwrap();
    wait(&mut p, |p| p.shared.is_loaded()).await;
    assert!(
        state.retains_source() && state.control_sender().is_some(),
        "pinned MP3 URL never activated its real source controls"
    );
    (p, state, server)
}
/// Restarts replay the converter from the same origin and must be bit-exact.
const EXACT: f32 = 0.0;
/// A seek restarts the converter's history at its own origin: the same filter
/// over a different accumulation order, so equal up to f32 rounding.
const SEEK: f32 = 1e-6;
const DECODED: f32 = 1e-5;
fn compare(got: &[f32], want: &[f32], key: &str, tolerance: f32) {
    assert!(
        got.len() == want.len()
            && got.iter().zip(want).all(|(a, b)| a.is_finite()
                && b.is_finite()
                && if tolerance == EXACT {
                    a.to_bits() == b.to_bits()
                } else {
                    (*a - *b).abs() <= tolerance
                }),
        "pinned MP3 complete PCM changed: {key}, lengths={}/{}, first={:?}",
        got.len(),
        want.len(),
        got.iter()
            .zip(want)
            .position(|(a, b)| a.to_bits() != b.to_bits())
    );
    if let Some(dir) = std::env::var_os("MIGO_HTTP_MP3_PCM_DIR") {
        std::fs::write(
            std::path::Path::new(&dir).join(format!("{key}.f32le")),
            got.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<_>>(),
        )
        .unwrap();
    }
}
#[tokio::test]
async fn public_mp3_seek_restart_and_exact_endpoint_keep_complete_gapless_pcm() {
    let f = controls::Context::new(Mode::Normal);
    for (name, data) in [("music", MUSIC), ("mpeg2", MPEG2)] {
        let native = crate::decoder::decode(data).unwrap();
        let frames = native.samples.len() / native.channels as usize;
        for rate in [16000, 44100, 48000] {
            let reference = crate::decoder::decode_for_rate(data, rate).unwrap();
            let (mut p, _state, _server) = load(&f, data, native.channels, rate, None).await;
            for target in [frames / 2, frames - 1, frames, 0] {
                p.pause();
                let old = p.shared.position_frames();
                let start = (target as u64 * rate as u64).div_ceil(native.sample_rate as u64);
                p.seek(if target == frames {
                    p.shared.duration()
                } else {
                    (target as f64 + 0.25) / native.sample_rate as f64
                });
                assert_eq!(
                    p.shared.position_frames(),
                    old,
                    "MP3 seek published before real HTTP source ACK"
                );
                wait(&mut p, |p| {
                    p.retained_stream_start() == start && p.shared.position_frames() == start
                })
                .await;
                let expected = &reference.samples[start as usize * native.channels as usize..];
                let got = suffix(&mut p, expected.len() / native.channels as usize).await;
                compare(
                    &got,
                    expected,
                    &format!("file-{name}-{rate}-{target}-seek"),
                    SEEK,
                );
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
                compare(
                    &got,
                    &reference.samples,
                    &format!("file-{name}-{rate}-{target}-restart"),
                    EXACT,
                );
            }
            finish(&mut p).await;
        }
    }
}
#[tokio::test]
async fn public_mp3_loops_keep_global_phase_and_reuse_the_original_cached_window() {
    let f = controls::Context::new(Mode::Normal);
    for (name, data) in [
        (
            "tiny-mono",
            include_bytes!("decoder/mp3/fixtures/tiny-47-mono.mp3").as_slice(),
        ),
        (
            "tiny-stereo",
            include_bytes!("decoder/mp3/fixtures/tiny-47-stereo.mp3").as_slice(),
        ),
        ("mpeg2", MPEG2),
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
            let (mut p, _state, server) = load(&f, data, native.channels, rate, None).await;
            p.set_loop_enabled(true);
            wait(&mut p, |p| p.shared.loop_enabled()).await;
            let cycles = if frames < 1024 { 100 } else { 3 };
            let count =
                (frames as u64 * cycles * rate as u64 / native.sample_rate as u64) as usize + 3;
            let got = suffix(&mut p, count).await;
            let a = full(cycles as usize + 512);
            let b = full(cycles as usize + 1024);
            assert_eq!(
                &a[..got.len()],
                &b[..got.len()],
                "MP3 loop reference has a finite future endpoint"
            );
            compare(
                &got,
                &a[..got.len()],
                &format!("file-{name}-{rate}-loop"),
                DECODED,
            );
            // A public cutover may precede original EOF. Its one cold replay
            // is distinct from reopening HTTP once per decoded loop cycle.
            let loop_requests = server.range_requests();
            assert!(
                loop_requests <= 2,
                "MP3 loop cycles repeatedly reopened HTTP: {name}/{rate}/{loop_requests}"
            );
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
                &format!("file-{name}-{rate}-disabled"),
                DECODED,
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
                &format!("file-{name}-{rate}-loop-restart"),
                DECODED,
            );
            assert!(
                server.range_requests() <= loop_requests + 2,
                "MP3 control cutovers lost bounded encoded-window reuse: {name}/{rate}"
            );
        }
    }
}
// A fallback must retain its original progressive response, without a spool.
struct NoSpool;
impl shared::audio_file::AudioHttpSpool for NoSpool {
    fn sparse(
        &self,
        len: Option<u64>,
    ) -> shared::error::EngineResult<Box<dyn shared::audio_file::AudioSparseSpool>> {
        crate::stream_open::memory_sparse(len)
    }
    fn prepare(
        self: Arc<Self>,
        _: Box<dyn shared::audio_file::AudioSpoolBody>,
        _: Arc<dyn shared::audio_file::AudioFileCancel>,
        _: std::time::Instant,
    ) -> shared::audio_file::AudioSpoolFuture<'static, Arc<dyn shared::audio_file::AudioFile>> {
        panic!("MP3 fallback attempted to spool its original progressive response")
    }
}
#[tokio::test]
async fn unreliable_range_mp3_keeps_the_complete_original_body_without_unpinned_ranges() {
    let f = controls::Context::new(Mode::Normal);
    for (mode_id, mode) in [
        Mode::NoTag,
        Mode::Weak,
        Mode::Ignored,
        Mode::Chunked,
        Mode::Gzip,
        Mode::UnknownLength,
    ]
    .into_iter()
    .enumerate()
    {
        for rate in [16000, 48000] {
            let server = server(MPEG2.to_vec(), mode, false, false).await;
            let state = StreamingState::new_url();
            let mut rx = crate::streaming::start_streaming_download_with_spool(
                f.host.audio_cpu(),
                server.client(f.host.pools().clone()),
                "http://indexed-product-fixture.invalid/original".into(),
                state.clone(),
                rate,
                Some(Arc::new(NoSpool)),
            );
            let (got, _) = done(&mut rx).await;
            compare(
                &got,
                &crate::decoder::decode_for_rate(MPEG2, rate)
                    .unwrap()
                    .samples,
                &format!("fallback-{mode_id}-{rate}"),
                EXACT,
            );
            // Without a pinned identity a seek is served from the spool or
            // reads on along the original body, so controls depend only on a
            // known presentation length.
            assert_eq!(
                state.control_sender().is_some(),
                state.timeline().is_some(),
                "MP3 controls do not follow its known presentation length"
            );
            state.cancel();
            assert!(
                tokio::time::timeout(WAIT, rx.recv())
                    .await
                    .unwrap()
                    .is_none(),
                "MP3 producer outlived its cancelled stream"
            );
            assert_eq!(
                server.range_requests(),
                0,
                "an unpinned MP3 response was followed by a Range request"
            );
        }
    }
}
#[tokio::test]
async fn unknown_mp3_and_adts_keep_the_original_single_pass_without_a_control_port() {
    let f = controls::Context::new(Mode::Normal);
    let adts = include_bytes!("decoder/aac-fixtures/adts-1.aac");
    let mut tagged = b"ID3\x04\0\0\0\0\0\0".to_vec();
    tagged.extend(adts);
    for (name, data) in [
        ("unknown-mp3", crate::mp3_fixture::stream(32)),
        ("adts", adts.to_vec()),
        ("tagged-adts", tagged),
    ] {
        let server = server(data.clone(), Mode::Normal, false, false).await;
        let state = StreamingState::new_url();
        let mut rx = crate::streaming::start_streaming_download_with_spool(
            f.host.audio_cpu(),
            server.client(f.host.pools().clone()),
            "http://indexed-product-fixture.invalid/single".into(),
            state.clone(),
            16000,
            Some(Arc::new(NoSpool)),
        );
        let (got, _) = done(&mut rx).await;
        compare(
            &got,
            &crate::decoder::decode_for_rate(&data, 16000)
                .unwrap()
                .samples,
            &format!("{name}-single"),
            EXACT,
        );
        assert!(
            tokio::time::timeout(WAIT, rx.recv())
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            state.control_sender().is_none() && !state.retains_source(),
            "unknown-duration source exposed an unusable control port"
        );
        assert_eq!(
            server.range_requests(),
            0,
            "{name}: a single pass needs no Range"
        );
    }
}
#[tokio::test]
async fn mp3_refusals_and_source_ack_preserve_the_old_revision_and_cached_pcm() {
    let host = Arc::new(migo_io::scheduler::IoScheduler::new(1111));
    let server = server(MPEG2.to_vec(), Mode::Normal, false, false).await;
    let (mut rx, state) = start_controlled(&server, &host);
    let (first, old) = done(&mut rx).await;
    compare(
        &first,
        &crate::decoder::decode_for_rate(MPEG2, 16000)
            .unwrap()
            .samples,
        "ack-original",
        EXACT,
    );
    let port = state
        .control_sender()
        .expect("known MP3 lost its retained source port");
    let clock = state.timeline().unwrap();
    for target in [-1, clock.seek_frame(clock.seconds()).unwrap() + 1] {
        assert!(
            matches!(
                request(port, target, Instant::now() + WAIT)
                    .unwrap()
                    .await
                    .unwrap(),
                Reply::Rejected(ErrorCode::InvalidArgument)
            ),
            "invalid HTTP MP3 seek mutated the old source"
        );
        assert!(rx.try_recv().is_err() && !state.is_cancelled());
    }
    assert!(
        matches!(
            request(port, 0, Instant::now() - Duration::from_millis(1))
                .unwrap()
                .await
                .unwrap(),
            Reply::NotStarted(ErrorCode::Timeout)
        ),
        "expired HTTP MP3 seek changed the old source"
    );
    let Reply::Prepared(prepared) = request(port, 211, Instant::now() + WAIT)
        .unwrap()
        .await
        .unwrap()
    else {
        panic!("MP3 source did not prepare its real acknowledgement")
    };
    assert!(
        tokio::time::timeout(Duration::from_millis(20), rx.recv())
            .await
            .is_err(),
        "HTTP MP3 published new revision PCM before consumer ACK"
    );
    let revision = prepared.revision;
    assert_ne!(revision, old);
    let start = prepared.mapped_output().unwrap() as usize;
    prepared.accept.send(()).unwrap();
    let (got, r) = done(&mut rx).await;
    assert_eq!(r, revision);
    compare(&got, &first[start..], "ack-suffix", SEEK);
    assert_eq!(
        server.range_requests(),
        0,
        "cached MP3 seek spent another Range request"
    );
    let Reply::Prepared(prepared) = request(port, 0, Instant::now() + WAIT)
        .unwrap()
        .await
        .unwrap()
    else {
        panic!("cached MP3 restart was lost")
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
async fn mp3_seek_while_original_response_is_held_preserves_the_complete_nonconstant_tail() {
    let f = controls::Context::new(Mode::Normal);
    let reference = crate::decoder::decode_for_rate(MUSIC, 16000).unwrap();
    let (mut p, _state, server) = load(&f, MUSIC, 2, 16000, Some(32768)).await;
    assert_eq!(
        server.initial_eof.load(Ordering::Acquire),
        0,
        "original MP3 response was not held before EOF"
    );
    let target = 240071u64;
    let start = (target * 16000).div_ceil(48000);
    p.pause();
    let before = p.shared.position_frames();
    p.seek((target as f64 + 0.25) / 48000.);
    assert_eq!(
        p.shared.position_frames(),
        before,
        "held MP3 seek skipped its source acknowledgement"
    );
    wait(&mut p, |p| {
        p.shared.position_frames() == start && p.retained_stream_start() == start
    })
    .await;
    let got = suffix(&mut p, reference.samples.len() / 2 - start as usize).await;
    compare(
        &got,
        &reference.samples[start as usize * 2..],
        "held-original-complete-tail",
        EXACT,
    );
    finish(&mut p).await;
    assert_eq!(server.initial_eof.load(Ordering::Acquire), 0);
    assert!(
        server.range_requests() >= 1,
        "media beyond a held original response did not arrive by pinned Range"
    );
}
#[tokio::test]
async fn actual_large_http_mp3_reclaims_pcm_reopens_the_pinned_source_and_checks_every_tail_sample()
{
    use crate::inner_audio::PlaybackState;
    use crate::mp3_fixture as m;
    use shared::protocol::audio_cmd::InnerAudioEventType;
    let f = controls::Context::new(Mode::Normal);
    let count = crate::limits::MAX_AUDIO_PCM_SAMPLES as usize / m::SAMPLES_PER_FRAME + 1;
    let mut tag = m::stream(1);
    tag[36..40].copy_from_slice(b"Info");
    tag[40..44].copy_from_slice(&1u32.to_be_bytes());
    tag[44..48].copy_from_slice(&(count as u32).to_be_bytes());
    let bytes = [tag, m::stream(count)].concat();
    let frames = (count * m::SAMPLES_PER_FRAME / m::CHANNELS) as u64;
    assert!(
        crate::decoder::decode(&bytes).is_err(),
        "whole-file PCM ceiling was relaxed"
    );
    let (mut p, _state, server) = load(&f, &bytes, 2, m::SAMPLE_RATE, Some(1024 * 1024)).await;
    assert_eq!(server.initial_eof.load(Ordering::Acquire), 0);
    async fn zeros(p: &mut InnerAudioPlayer, frames: u64) {
        let mut got = 0;
        let mut out = [0.; 8192];
        let end = std::time::Instant::now() + WAIT;
        p.play();
        while got < frames {
            p.poll_stream();
            assert!(
                !p.drain_events().any(|e| {
                    assert_eq!(e.source_id, 8181);
                    e.event_type == InnerAudioEventType::Error
                }),
                "large HTTP MP3 failed its actual replay"
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
                "large HTTP MP3 ended before its complete tail"
            );
            assert!(
                out[..offered]
                    .iter()
                    .all(|n| n.to_bits() == 0.0f32.to_bits()),
                "large HTTP MP3 actual PCM changed"
            );
            got += n;
            assert!(
                p.physical_pcm_bytes() < 4 * 1024 * 1024,
                "large HTTP MP3 retained whole-track PCM"
            );
            assert!(
                std::time::Instant::now() < end,
                "large HTTP MP3 source replay stalled"
            );
            tokio::task::yield_now().await;
        }
        assert_eq!(got, frames);
    }
    zeros(&mut p, 2_200_007).await;
    assert!(
        p.retained_stream_start() > 0,
        "large HTTP MP3 never reclaimed old PCM"
    );
    let ranges = server.range_requests();
    p.pause();
    p.seek(0.);
    wait(&mut p, |p| {
        p.shared.position_frames() == 0 && p.retained_stream_start() == 0
    })
    .await;
    zeros(&mut p, frames).await;
    finish(&mut p).await;
    assert!(
        server.range_requests() > ranges,
        "large MP3 replay did not reopen its original pinned reader"
    );
    assert_eq!(server.initial_eof.load(Ordering::Acquire), 0);
    p.seek(p.shared.duration());
    wait(&mut p, |p| {
        p.retained_stream_start() == frames
            && p.shared.position_frames() == frames
            && p.is_stream_complete()
    })
    .await;
    println!(
        "MIGO_LARGE_HTTP_MP3_CONTROL samples={} encoded={} complete_zero_pcm_checked=true original_held_before_eof=true reclaimed_then_source_replay=true",
        frames * 2,
        bytes.len()
    );
}

#[tokio::test]
async fn oversized_mp3_garbage_remains_terminal_before_successful_done() {
    let f = controls::Context::new(Mode::Normal);
    let mut bytes = MPEG2.to_vec();
    bytes.resize(bytes.len() + 65536, 0);
    let server = server(bytes, Mode::Normal, false, false).await;
    let state = StreamingState::new_url();
    let mut rx = crate::streaming::start_streaming_download_with_spool(
        f.host.audio_cpu(),
        server.client(f.host.pools().clone()),
        "http://indexed-product-fixture.invalid/garbage".into(),
        state.clone(),
        16000,
        Some(Arc::new(NoSpool)),
    );
    loop {
        match tokio::time::timeout(WAIT, rx.recv())
            .await
            .unwrap()
            .expect("malformed MP3 closed without its terminal error")
        {
            StreamMsg::Error { message, .. } => {
                assert!(
                    message.contains("MP3"),
                    "malformed MP3 lost its decode failure: {message}"
                );
                break;
            }
            StreamMsg::Done { .. } => panic!("oversized MP3 garbage published successful Done"),
            _ => (),
        }
    }
    assert!(
        tokio::time::timeout(WAIT, rx.recv())
            .await
            .unwrap()
            .is_none()
    );
}
