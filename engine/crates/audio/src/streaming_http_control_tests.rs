//! Public controls over the real HTTP response sealed by the session Fs lane.
use super::*;
use crate::inner_audio::InnerAudioPlayer;
use crate::stream_open::file_tests::controls::{
    deterministic, finish, pull_seek, seek_reference, suffix, wait,
};
pub(super) struct Context {
    root: std::path::PathBuf,
    paths: shared::vfs::GamePaths,
    pub(super) host: Arc<migo_io::scheduler::IoScheduler>,
    mode: Mode,
}
impl Context {
    pub(super) fn new(mode: Mode) -> Self {
        let root = std::env::temp_dir().join(format!(
            "migo-http-control-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let paths = shared::vfs::GamePaths::new(root.join("files"), root.join("cache"), "audio", 1)
            .unwrap();
        paths.ensure_directories().unwrap();
        Self {
            root,
            paths,
            host: Arc::new(migo_io::scheduler::IoScheduler::new(1086)),
            mode,
        }
    }
    pub(super) fn spool(&self) -> Arc<dyn shared::audio_file::AudioHttpSpool> {
        migo_services::audio::LocalAudioFile::session(self.host.clone(), &self.paths).http_spool()
    }
}
impl Drop for Context {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
pub(super) struct Loaded {
    player: InnerAudioPlayer,
    pub(super) state: Arc<StreamingState>,
    pub(super) server: Server,
    /// Range requests made while loading; none may follow a complete body.
    ranges_at_load: usize,
}
impl std::ops::Deref for Loaded {
    type Target = InnerAudioPlayer;
    fn deref(&self) -> &Self::Target {
        &self.player
    }
}
impl std::ops::DerefMut for Loaded {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.player
    }
}
impl Drop for Loaded {
    fn drop(&mut self) {
        assert!(self.state.control_sender().is_some());
        assert_eq!(
            self.server.ranges.load(Ordering::Acquire),
            self.ranges_at_load,
            "a completely spooled source went back to HTTP"
        );
    }
}
pub(super) async fn player(f: &Context, data: &[u8], rate: u32) -> Loaded {
    let server = server(data.to_vec(), f.mode, false, false).await;
    let state = StreamingState::new_url();
    assert!(
        !state.retains_source(),
        "URL retained an unselected source before its real control port"
    );
    let rx = crate::streaming::start_streaming_download_with_spool(
        f.host.audio_cpu(),
        server.client(f.host.pools().clone()),
        "http://indexed-product-fixture.invalid/wrong.mp3".into(),
        state.clone(),
        rate,
        Some(f.spool()),
    );
    let mut p = InnerAudioPlayer::new(919, crate::decoder::decode(data).unwrap().channels);
    let load = p.begin_load_for(8181).unwrap();
    p.complete_stream_load(
        load,
        "http://indexed-product-fixture.invalid/wrong.mp3".into(),
        rx,
        state.clone(),
    )
    .unwrap();
    wait(&mut p, |p| p.shared.is_loaded()).await;
    assert!(
        state.retains_source() && state.control_sender().is_some(),
        "HTTP sealed source never activated its original control port"
    );
    assert_eq!(server.initial_eof.load(Ordering::Acquire), 1);
    let ranges_at_load = server.ranges.load(Ordering::Acquire);
    Loaded {
        player: p,
        state,
        server,
        ranges_at_load,
    }
}
fn compare(got: &[f32], want: &[f32], key: &str) {
    assert_eq!(
        got.len(),
        want.len(),
        "HTTP controlled complete suffix length: {key}"
    );
    let max = got
        .iter()
        .zip(want)
        .map(|(a, z)| (*a - *z).abs())
        .fold(0., f32::max);
    assert!(
        max <= 0.00001 && got.iter().chain(want).all(|x| x.is_finite()),
        "HTTP controlled complete suffix PCM: {key},error={max}"
    );
    if std::env::var_os("MIGO_DUMP_HTTP_CONTROL_PCM").is_some() {
        println!(
            "MIGO_HTTP_CONTROL_PCM {key} {:?}",
            got.iter().map(|s| s.to_bits()).collect::<Vec<_>>()
        );
    }
}

#[tokio::test]
async fn public_http_seek_restart_and_exact_endpoint_preserve_complete_pcm() {
    for mode in [Mode::NoTag, Mode::Ignored] {
        let f = Context::new(mode);
        for (name, data) in deterministic() {
            for rate in [16000, 48000] {
                let reference = crate::decoder::decode_for_rate(data, rate).unwrap();
                let mut p = player(&f, data, rate).await;

                let native = crate::decoder::decode(data).unwrap();
                let frames = native.samples.len() / native.channels as usize;
                for target in [frames / 2, frames - 1, frames, 0] {
                    p.pause();
                    let old_position = p.shared.position_frames();
                    let seconds = if target == frames {
                        p.shared.duration()
                    } else {
                        (target as f64 + 0.25) / native.sample_rate as f64
                    };
                    let mapped = ((target as u128 * rate as u128)
                        .div_ceil(native.sample_rate as u128))
                        as usize;
                    p.seek(seconds);
                    assert_eq!(
                        p.shared.position_frames(),
                        old_position,
                        "file seek published before original source ACK"
                    );
                    wait(&mut p, |p| {
                        p.retained_stream_start() == mapped as u64
                            && p.shared.position_frames() == mapped as u64
                    })
                    .await;
                    let n = reference.samples.len() / reference.channels as usize - mapped;
                    let got = suffix(&mut p, n).await;
                    if n == 0 {
                        wait(&mut p, |p| p.is_stream_complete()).await;
                    }
                    compare(
                        &got,
                        &reference.samples[mapped * reference.channels as usize..],
                        &format!(
                            "{}/{name}/{rate}/seek-{target}",
                            if mode == Mode::NoTag {
                                "no-tag"
                            } else {
                                "ignored"
                            }
                        ),
                    );
                    p.stop();
                    p.play();
                    wait(&mut p, |p| {
                        p.retained_stream_start() == 0 && p.shared.position_frames() == 0
                    })
                    .await;
                    let got = suffix(
                        &mut p,
                        reference.samples.len() / reference.channels as usize,
                    )
                    .await;
                    compare(
                        &got,
                        &reference.samples,
                        &format!(
                            "{}/{name}/{rate}/restart-{target}",
                            if mode == Mode::NoTag {
                                "no-tag"
                            } else {
                                "ignored"
                            }
                        ),
                    );
                }
                drop(p);
            }
        }
    }
}

#[tokio::test]
async fn public_loop_cutovers_keep_the_global_sinc_grid_and_finish_the_consumed_cycle() {
    for mode in [Mode::NoTag, Mode::Ignored] {
        let f = Context::new(mode);
        for (name, data) in deterministic()
            .into_iter()
            .filter(|(n, _)| matches!(*n, "seek-front" | "seek-co64" | "seek-edit"))
        {
            for rate in [3000, 16000, 48000] {
                let native = crate::decoder::decode(data).unwrap();
                let frames = native.samples.len() / native.channels as usize;
                let full = |cycles| {
                    crate::resample_job::tests::reference(
                        &crate::decoder::DecodedAudio {
                            samples: native.samples.repeat(cycles),
                            sample_rate: native.sample_rate,
                            channels: native.channels,
                        },
                        rate,
                    )
                };
                let mut p = player(&f, data, rate).await;
                p.set_loop_enabled(true);
                wait(&mut p, |p| p.shared.loop_enabled()).await;
                let prefix = ((frames as u128 * 2 * rate as u128) / (native.sample_rate as u128))
                    as usize
                    + 3;
                let got = suffix(&mut p, prefix).await;
                compare(
                    &got,
                    &full(4)[..got.len()],
                    &format!(
                        "{}/{name}/{rate}/loop-prefix",
                        if mode == Mode::NoTag {
                            "no-tag"
                        } else {
                            "ignored"
                        }
                    ),
                );
                assert!(p.shared.current_time() < p.shared.duration());
                p.pause();
                let at = p.shared.position_frames();
                p.set_loop_enabled(false);
                wait(&mut p, |p| {
                    !p.shared.loop_enabled() && p.retained_stream_start() == at
                })
                .await;
                assert_eq!(
                    p.shared.position_frames(),
                    at,
                    "loop disable moved the consumer boundary"
                );
                let cycle = ((at as u128 * native.sample_rate as u128) / (rate as u128))
                    / (frames as u128)
                    + 1;
                let reference = full(cycle as usize);
                let tail = reference.len() / native.channels as usize - at as usize;
                let got = suffix(&mut p, tail).await;
                compare(
                    &got,
                    &reference[at as usize * native.channels as usize..],
                    &format!(
                        "{}/{name}/{rate}/loop-disabled-tail",
                        if mode == Mode::NoTag {
                            "no-tag"
                        } else {
                            "ignored"
                        }
                    ),
                );
                finish(&mut p).await;
                p.set_loop_enabled(true);
                p.play();
                wait(&mut p, |p| {
                    p.shared.loop_enabled() && p.retained_stream_start() == 0
                })
                .await;
                let got = suffix(&mut p, 17).await;
                compare(
                    &got,
                    &full(2)[..got.len()],
                    &format!(
                        "{}/{name}/{rate}/loop-restart",
                        if mode == Mode::NoTag {
                            "no-tag"
                        } else {
                            "ignored"
                        }
                    ),
                );
            }
        }
    }
}

#[tokio::test]
async fn pns_http_seeks_match_an_independent_pull_seek() {
    for mode in [Mode::NoTag, Mode::Ignored] {
        let f = Context::new(mode);
        for (name, data) in cases()
            .into_iter()
            .filter(|(n, _)| matches!(*n, "1-front" | "2-co64" | "edit-1537"))
        {
            for rate in [16000, 48000] {
                let native = pull_seek(data, 0);
                let frames = native.samples.len() / native.channels as usize;
                let target = frames / 2;
                let start =
                    (target as u128 * rate as u128).div_ceil(native.sample_rate as u128) as u64;
                let reference = seek_reference(data, rate, start, 1, Some(target as u64));
                let mut p = player(&f, data, rate).await;
                p.seek((target as f64 + 0.25) / native.sample_rate as f64);
                wait(&mut p, |p| {
                    p.retained_stream_start() == start && p.shared.position_frames() == start
                })
                .await;
                let got = suffix(&mut p, reference.len() / native.channels as usize).await;
                compare(
                    &got,
                    &reference,
                    &format!(
                        "{}/pns-{name}/{rate}/seek",
                        if mode == Mode::NoTag {
                            "no-tag"
                        } else {
                            "ignored"
                        }
                    ),
                );
            }
        }
    }
}

#[tokio::test]
async fn very_short_http_loops_publish_pcm_before_reposition_credit_is_exhausted() {
    for mode in [Mode::NoTag, Mode::Ignored] {
        let f = Context::new(mode);
        let data = include_bytes!("decoder/aac-fixtures/0-front.m4a");
        let native = crate::decoder::decode(data).unwrap();
        assert_eq!(native.samples.len(), 17);
        for rate in [3000, 16000, 48000] {
            let mut p = player(&f, data, rate).await;
            p.set_loop_enabled(true);
            wait(&mut p, |p| p.shared.loop_enabled()).await;
            let got = suffix(&mut p, 137).await;
            let whole = crate::resample_job::tests::reference(
                &crate::decoder::DecodedAudio {
                    samples: native.samples.repeat(3000),
                    sample_rate: native.sample_rate,
                    channels: 1,
                },
                rate,
            );
            compare(
                &got,
                &whole[..137],
                &format!(
                    "{}/short/{rate}/loop",
                    if mode == Mode::NoTag {
                        "no-tag"
                    } else {
                        "ignored"
                    }
                ),
            );
        }
    }
}

#[test]
fn url_retention_requires_a_real_port_and_preserves_existing_single_pass_and_fixed_modes() {
    let url = StreamingState::new_url();
    assert!(url.accepts_controls());
    assert!(
        !url.retains_source(),
        "URL retained an unselected source before its real control port"
    );
    let (tx, _rx) = crate::stream_control::channel();
    url.install_controls(tx)
        .unwrap_or_else(|e| panic!("URL could not install its selected source port: {e}"));
    assert!(
        url.retains_source(),
        "installed URL port did not activate source retention"
    );
    let single = StreamingState::new();
    let (tx, _rx) = crate::stream_control::channel();
    assert!(!single.retains_source() && single.install_controls(tx).is_err());
    let fixed = StreamingState::new_reusable();
    assert!(
        fixed.retains_source(),
        "fixed reusable mode lost its retention contract"
    );
    let retired = StreamingState::new_url();
    retired.cancel();
    let (tx, _rx) = crate::stream_control::channel();
    assert!(
        retired.install_controls(tx).is_err(),
        "retired URL installed a new source control port"
    );
    assert!(!retired.retains_source());
}

#[tokio::test]
async fn ordinary_progressive_formats_keep_complete_pcm_and_controls_follow_a_known_length() {
    let f = Context::new(Mode::NoTag);
    let mut wav = b"RIFF".to_vec();
    let frames = 9737u32;
    wav.extend((frames * 2 + 36).to_le_bytes());
    wav.extend(b"WAVEfmt \x10\0\0\0\x01\0\x01\0");
    wav.extend(48000u32.to_le_bytes());
    wav.extend(96000u32.to_le_bytes());
    wav.extend(b"\x02\0\x10\0data");
    wav.extend((frames * 2).to_le_bytes());
    wav.extend([0, 32].repeat(frames as usize));
    for (name, data) in [
        ("wave", wav),
        (
            "vorbis",
            include_bytes!("decoder/ogg-fixtures/cross.ogg").to_vec(),
        ),
        (
            "adts",
            include_bytes!("decoder/aac-fixtures/adts-1.aac").to_vec(),
        ),
        ("mp3", crate::mp3_fixture::stream(32)),
    ] {
        for rate in [16000, 48000] {
            let server = server(data.clone(), Mode::NoTag, false, false).await;
            let state = StreamingState::new_url();
            let mut rx = crate::streaming::start_streaming_download_with_spool(
                f.host.audio_cpu(),
                server.client(f.host.pools().clone()),
                "http://indexed-product-fixture.invalid/wrong.m4a".into(),
                state.clone(),
                rate,
                Some(f.spool()),
            );
            let mut got = Vec::new();
            loop {
                match tokio::time::timeout(WAIT, rx.recv())
                    .await
                    .expect("ordinary URL publication stalled")
                    .expect("ordinary URL closed before Done")
                {
                    StreamMsg::Samples { pcm, .. } => got.extend_from_slice(&pcm),
                    StreamMsg::Done { .. } => break,
                    StreamMsg::Error { message, .. } => panic!("ordinary URL failed: {message}"),
                    StreamMsg::Ready { .. } => {}
                }
            }
            let expected = crate::decoder::decode_for_rate(&data, rate).unwrap();
            assert_eq!(
                got.iter().map(|s| s.to_bits()).collect::<Vec<_>>(),
                expected
                    .samples
                    .iter()
                    .map(|s| s.to_bits())
                    .collect::<Vec<_>>(),
                "ordinary URL complete PCM changed"
            );
            // Without a validator seeks replay the spool or read on, so only
            // a known presentation length decides whether controls exist; an
            // unknown-length source ends with its single pass.
            let controlled = state.timeline().is_some();
            assert_eq!(
                state.control_sender().is_some(),
                controlled,
                "{name}: controls do not follow its known presentation length"
            );
            if controlled {
                state.cancel();
            }
            assert!(
                tokio::time::timeout(WAIT, rx.recv())
                    .await
                    .unwrap()
                    .is_none(),
                "{name}: producer outlived its stream"
            );
            assert_eq!(server.ranges.load(Ordering::Acquire), 0);
            if std::env::var_os("MIGO_DUMP_HTTP_CONTROL_PCM").is_some() {
                println!(
                    "MIGO_HTTP_ORDINARY_PCM {name}/{rate} {:?}",
                    got.iter().map(|s| s.to_bits()).collect::<Vec<_>>()
                );
            }
        }
    }
}
