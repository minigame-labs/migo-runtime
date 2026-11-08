//! Pinned Vorbis controls through the real URL selector, Reader and original CPU.
use super::range_controls::{done, start_controlled};
use super::*;
use crate::inner_audio::InnerAudioPlayer;
use crate::stream_control::{Reply, request};
use crate::stream_open::file_tests::controls::{finish, suffix, wait};
const MUSIC: &[u8] = include_bytes!("decoder/ogg-fixtures/1.ogg");
const MONO: &[u8] = include_bytes!("decoder/ogg-fixtures/2.ogg");
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
        "pinned Vorbis URL never activated its real source controls"
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
        "pinned Vorbis complete PCM changed: {key}, lengths={}/{}, first={:?}",
        got.len(),
        want.len(),
        got.iter()
            .zip(want)
            .position(|(a, b)| a.to_bits() != b.to_bits())
    );
    if let Some(dir) = std::env::var_os("MIGO_HTTP_VORBIS_PCM_DIR") {
        std::fs::write(
            std::path::Path::new(&dir).join(format!("{key}.f32le")),
            got.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<_>>(),
        )
        .unwrap();
    }
}
const CROSS: &[u8] = include_bytes!("decoder/ogg-fixtures/cross.ogg");
#[tokio::test]
async fn public_vorbis_seek_restart_and_exact_endpoint_keep_complete_pcm() {
    let f = controls::Context::new(Mode::Normal);
    for (name, data) in [("stereo", MUSIC), ("mono", MONO)] {
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
                    "Vorbis seek published before real HTTP source ACK"
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
async fn public_vorbis_loops_keep_global_phase_and_reuse_the_original_cached_window() {
    let f = controls::Context::new(Mode::Normal);
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
                "Vorbis loop reference has a finite future endpoint"
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
                "Vorbis loop cycles repeatedly reopened HTTP: {name}/{rate}/{loop_requests}"
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
                "Vorbis control cutovers lost bounded encoded-window reuse: {name}/{rate}"
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
        panic!("Vorbis fallback attempted to spool its original progressive response")
    }
}
#[tokio::test]
async fn unreliable_range_vorbis_keeps_the_complete_original_body_without_unpinned_ranges() {
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
            let server = server(MONO.to_vec(), mode, false, false).await;
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
                &crate::decoder::decode_for_rate(MONO, rate).unwrap().samples,
                &format!("fallback-{mode_id}-{rate}"),
                EXACT,
            );
            // Without a pinned identity a seek is served from the spool or
            // reads on along the original body, so controls depend only on a
            // known presentation length.
            assert_eq!(
                state.control_sender().is_some(),
                state.timeline().is_some(),
                "Vorbis controls do not follow its known presentation length"
            );
            state.cancel();
            assert!(
                tokio::time::timeout(WAIT, rx.recv())
                    .await
                    .unwrap()
                    .is_none(),
                "Vorbis producer outlived its cancelled stream"
            );
            assert_eq!(
                server.range_requests(),
                0,
                "an unpinned Vorbis response was followed by a Range request"
            );
        }
    }
}
#[tokio::test]
async fn vorbis_refusals_and_source_ack_preserve_the_old_revision_and_cached_pcm() {
    let host = Arc::new(migo_io::scheduler::IoScheduler::new(1111));
    let server = server(MONO.to_vec(), Mode::Normal, false, false).await;
    let (mut rx, state) = start_controlled(&server, &host);
    let (first, old) = done(&mut rx).await;
    compare(
        &first,
        &crate::decoder::decode_for_rate(MONO, 16000)
            .unwrap()
            .samples,
        "ack-original",
        EXACT,
    );
    let port = state
        .control_sender()
        .expect("known Vorbis lost its retained source port");
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
            "invalid HTTP Vorbis seek mutated the old source"
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
        "expired HTTP Vorbis seek changed the old source"
    );
    let Reply::Prepared(prepared) = request(port, 211, Instant::now() + WAIT)
        .unwrap()
        .await
        .unwrap()
    else {
        panic!("Vorbis source did not prepare its real acknowledgement")
    };
    assert!(
        tokio::time::timeout(Duration::from_millis(20), rx.recv())
            .await
            .is_err(),
        "HTTP Vorbis published new revision PCM before consumer ACK"
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
        "cached Vorbis seek spent another Range request"
    );
    let Reply::Prepared(prepared) = request(port, 0, Instant::now() + WAIT)
        .unwrap()
        .await
        .unwrap()
    else {
        panic!("cached Vorbis restart was lost")
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
async fn vorbis_seek_while_original_response_is_held_preserves_the_complete_nonconstant_tail() {
    let f = controls::Context::new(Mode::Normal);
    let native = crate::decoder::decode(CROSS).unwrap();
    let channels = native.channels as usize;
    let reference = crate::decoder::decode_for_rate(CROSS, 16000).unwrap();
    let (mut p, _state, server) =
        load(&f, CROSS, native.channels, 16000, Some(CROSS.len() - 1)).await;
    assert_eq!(
        server.initial_eof.load(Ordering::Acquire),
        0,
        "original Vorbis response was not held before EOF"
    );
    let target = (native.samples.len() / channels / 2 + 17) as u64;
    let start = (target * 16000).div_ceil(native.sample_rate as u64);
    p.pause();
    let old = p.shared.position_frames();
    p.seek((target as f64 + 0.25) / native.sample_rate as f64);
    assert_eq!(
        p.shared.position_frames(),
        old,
        "held Vorbis seek skipped its source acknowledgement"
    );
    wait(&mut p, |p| {
        p.shared.position_frames() == start && p.retained_stream_start() == start
    })
    .await;
    let got = suffix(&mut p, reference.samples.len() / channels - start as usize).await;
    compare(
        &got,
        &reference.samples[start as usize * channels..],
        "held-original-complete-tail",
        EXACT,
    );
    finish(&mut p).await;
    // The held response never completed: its missing byte came by Range, and
    // the delivered bytes of the same pinned object were reused, not fetched
    // again.
    assert_eq!(server.initial_eof.load(Ordering::Acquire), 0);
    assert!(server.range_requests() >= 1);
}
#[tokio::test]
async fn cross_page_and_eight_channel_http_seek_keep_complete_file_suffix_bits() {
    let f = controls::Context::new(Mode::Normal);
    for (name, data) in [
        ("cross", CROSS),
        (
            "wide",
            include_bytes!("decoder/ogg-fixtures/max-window-8.ogg").as_slice(),
        ),
    ] {
        let native = crate::decoder::decode(data).unwrap();
        let channels = native.channels as usize;
        let target = native.samples.len() / channels / 2;
        for rate in [16000, 48000] {
            let expected = crate::decoder::decode_for_rate(data, rate).unwrap();
            let (mut p, _, _server) = load(&f, data, native.channels, rate, None).await;
            p.pause();
            p.seek((target as f64 + 0.25) / native.sample_rate as f64);
            let start = (target as u64 * rate as u64).div_ceil(native.sample_rate as u64);
            wait(&mut p, |p| {
                p.retained_stream_start() == start && p.shared.position_frames() == start
            })
            .await;
            let got = suffix(&mut p, expected.samples.len() / channels - start as usize).await;
            compare(
                &got,
                &expected.samples[start as usize * channels..],
                &format!("file-{name}-{rate}-seek"),
                SEEK,
            );
            finish(&mut p).await;
        }
    }
}
#[tokio::test]
async fn a_corrupt_final_vorbis_page_is_skipped_as_libogg_does_and_the_rest_plays() {
    let f = controls::Context::new(Mode::Normal);
    let reference = crate::decoder::decode_for_rate(CROSS, 16000).unwrap();
    let channels = reference.channels as usize;
    let mut bytes = CROSS.to_vec();
    *bytes.last_mut().unwrap() ^= 1;
    let server = server(bytes, Mode::Normal, false, false).await;
    let state = StreamingState::new_url();
    let mut rx = crate::streaming::start_streaming_download_with_spool(
        f.host.audio_cpu(),
        server.client(f.host.pools().clone()),
        "http://indexed-product-fixture.invalid/bad-final-page".into(),
        state.clone(),
        16000,
        Some(Arc::new(NoSpool)),
    );
    let mut got = Vec::new();
    loop {
        match tokio::time::timeout(WAIT, rx.recv())
            .await
            .unwrap()
            .expect("Vorbis closed without Done")
        {
            StreamMsg::Samples { pcm, .. } => got.extend_from_slice(&pcm),
            StreamMsg::Done { .. } => break,
            StreamMsg::Error { message, .. } => {
                panic!("a skippable page failed the stream: {message}")
            }
            StreamMsg::Ready { .. } => {}
        }
    }
    // The presentation ends at the last intact page; everything before it
    // (away from the converter's earlier end) is the intact decode.
    assert!(
        got.len() < reference.samples.len(),
        "the corrupt page was decoded"
    );
    let settled = got.len().saturating_sub(64 * channels);
    compare(
        &got[..settled],
        &reference.samples[..settled],
        "skipped-final-page",
        DECODED,
    );
    state.cancel();
}
