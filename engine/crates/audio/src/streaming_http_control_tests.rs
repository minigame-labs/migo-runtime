//! Public controls over the real HTTP response sealed by the session Fs lane.
use super::*;
use crate::inner_audio::InnerAudioPlayer;
use crate::streaming_file::tests::controls::{
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
    mode: Mode,
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
        if self.mode != Mode::Normal {
            assert_eq!(
                self.server.ranges.load(Ordering::Acquire),
                usize::from(self.mode == Mode::Ignored),
                "sealed source retried HTTP after preparing controls"
            );
        }
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
    if f.mode != Mode::Normal {
        assert_eq!(
            server.ranges.load(Ordering::Acquire),
            usize::from(f.mode == Mode::Ignored)
        );
    } else {
        assert!(server.ranges.load(Ordering::Acquire) > 0);
    }
    Loaded {
        player: p,
        state,
        server,
        mode: f.mode,
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
async fn pns_http_seeks_match_independent_mature_pull_seek_instead_of_continuous_noise() {
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
                if name == "1-front" && rate == 16000 {
                    let continuous = crate::decoder::decode_for_rate(data, rate).unwrap();
                    let delta = reference
                        .iter()
                        .zip(&continuous.samples[start as usize * native.channels as usize..])
                        .map(|(a, b)| (*a - *b).abs())
                        .fold(0.0f32, f32::max);
                    assert!(
                        delta > 0.00001,
                        "PNS oracle did not expose the real mature-seek noise discontinuity"
                    );
                }
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
async fn ordinary_progressive_formats_keep_complete_pcm_and_close_the_uncontrolled_url() {
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
            let got = collect(&mut rx)
                .await
                .expect("ordinary URL lost its progressive source");
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
            assert!(
                !state.retains_source() && state.control_sender().is_none(),
                "ordinary progressive URL retained an unavailable control port"
            );
            assert!(rx.is_closed());
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

#[tokio::test]
async fn loop_intent_before_the_late_http_port_keeps_the_last_requested_mode() {
    let f = Context::new(Mode::NoTag);
    let data = include_bytes!("decoder/aac-fixtures/1-seek-edit.m4a");
    let native = crate::decoder::decode(data).unwrap();
    for changed in [false, true] {
        let server = server(data.to_vec(), Mode::NoTag, false, false).await;
        let state = StreamingState::new_url();
        let (tx, rx) = crate::streaming::stream_channel();
        let mut p = InnerAudioPlayer::new(919, native.channels);
        let load = p.begin_load_for(8181).unwrap();
        p.set_loop_enabled(!changed);
        p.complete_stream_load(
            load,
            "http://indexed-product-fixture.invalid/late".into(),
            rx,
            state.clone(),
        )
        .unwrap();
        if changed {
            p.set_loop_enabled(true);
        }
        let task = tokio::spawn(crate::streaming_http_prepare::run(
            f.host.audio_cpu(),
            server.client(f.host.pools().clone()),
            "http://indexed-product-fixture.invalid/late".into(),
            tx,
            state.clone(),
            3000,
            f.spool(),
        ));
        wait(&mut p, |p| p.shared.is_loaded()).await;
        let got = suffix(
            &mut p,
            (native.samples.len() / native.channels as usize * 3000)
                .div_ceil(native.sample_rate as usize)
                + 37,
        )
        .await;
        let expected = crate::resample_job::tests::reference(
            &crate::decoder::DecodedAudio {
                samples: native.samples.repeat(3),
                sample_rate: native.sample_rate,
                channels: native.channels,
            },
            3000,
        );
        compare(
            &got,
            &expected[..got.len()],
            &format!("loading-{changed}/loop"),
        );
        assert!(state.control_sender().is_some() && state.retains_source());
        drop(p);
        tokio::time::timeout(WAIT, task).await.unwrap().unwrap();
    }
}

use shared::audio_file::{
    AudioFile, AudioFileCancel, AudioFileRead, AudioHttpSpool, AudioSpoolBody, AudioSpoolFuture,
};
pub(crate) struct Seen {
    pub(crate) file: std::sync::Weak<dyn AudioFile>,
    pub(crate) watch: Arc<migo_alloc_probe::DeallocationWatch>,
}
pub(crate) struct Freed {
    pub(crate) bytes: u64,
    pub(crate) allocation: Option<usize>,
}
struct ObservedSpool {
    source: Arc<dyn AudioHttpSpool>,
    seen: std::sync::Mutex<Option<tokio::sync::oneshot::Sender<Seen>>>,
    freed: std::sync::Mutex<Option<tokio::sync::oneshot::Sender<Freed>>>,
}
struct ObservedFile {
    file: Option<Arc<dyn AudioFile>>,
    watch: Arc<migo_alloc_probe::DeallocationWatch>,
    freed: Option<tokio::sync::oneshot::Sender<Freed>>,
}
impl Drop for ObservedFile {
    fn drop(&mut self) {
        let before = migo_alloc_probe::thread_counts().bytes_freed;
        drop(self.file.take());
        let freed = Freed {
            bytes: migo_alloc_probe::thread_counts().bytes_freed - before,
            allocation: self.watch.freed_bytes(),
        };
        if let Some(reply) = self.freed.take() {
            let _ = reply.send(freed);
        }
    }
}
impl ObservedFile {
    fn source(&self) -> &Arc<dyn AudioFile> {
        self.file.as_ref().unwrap()
    }
}
impl AudioFile for ObservedFile {
    fn read_blocking(&self, _offset: u64, _output: &mut [u8]) -> shared::error::EngineResult<()> {
        unimplemented!("the stepped pipeline never reads on the calling thread")
    }
    fn length(&self) -> u64 {
        self.source().length()
    }
    fn prefix(&self) -> &[u8] {
        self.source().prefix()
    }
    fn queued_bytes(&self) -> usize {
        self.source().queued_bytes()
    }
    fn is_closed(&self) -> bool {
        self.source().is_closed()
    }
    fn closed_signal(&self) -> &tokio::sync::Notify {
        self.source().closed_signal()
    }
    fn read_at(
        self: Arc<Self>,
        offset: u64,
        length: usize,
        cancel: Arc<dyn AudioFileCancel>,
        deadline: std::time::Instant,
    ) -> Result<AudioFileRead, EngineError> {
        self.source()
            .clone()
            .read_at(offset, length, cancel, deadline)
    }
    fn read_control_at(
        self: Arc<Self>,
        offset: u64,
        length: usize,
        cancel: Arc<dyn AudioFileCancel>,
        deadline: std::time::Instant,
    ) -> Result<AudioFileRead, EngineError> {
        self.source()
            .clone()
            .read_control_at(offset, length, cancel, deadline)
    }
}
impl AudioHttpSpool for ObservedSpool {
    fn prepare(
        self: Arc<Self>,
        body: Box<dyn AudioSpoolBody>,
        cancel: Arc<dyn AudioFileCancel>,
        deadline: std::time::Instant,
    ) -> AudioSpoolFuture<'static, Arc<dyn AudioFile>> {
        Box::pin(async move {
            let file = self
                .source
                .clone()
                .prepare(body, cancel.clone(), deadline)
                .await?;
            let chunk = file
                .clone()
                .read_at(0, 1, cancel, deadline)?
                .await
                .unwrap()
                .unwrap()?;
            let watch = Arc::new(
                migo_alloc_probe::DeallocationWatch::new(chunk.as_ref().as_ptr()).unwrap(),
            );
            drop(chunk);
            let file: Arc<dyn AudioFile> = Arc::new(ObservedFile {
                file: Some(file),
                watch: watch.clone(),
                freed: self.freed.lock().unwrap().take(),
            });
            self.seen
                .lock()
                .unwrap()
                .take()
                .unwrap()
                .send(Seen {
                    file: Arc::downgrade(&file),
                    watch,
                })
                .ok()
                .unwrap();
            Ok(file)
        })
    }
}
pub(crate) fn observe(
    source: Arc<dyn AudioHttpSpool>,
) -> (
    Arc<dyn AudioHttpSpool>,
    tokio::sync::oneshot::Receiver<Seen>,
    tokio::sync::oneshot::Receiver<Freed>,
) {
    let (tx, seen) = tokio::sync::oneshot::channel();
    let (drop_tx, freed) = tokio::sync::oneshot::channel();
    (
        Arc::new(ObservedSpool {
            source,
            seen: std::sync::Mutex::new(Some(tx)),
            freed: std::sync::Mutex::new(Some(drop_tx)),
        }),
        seen,
        freed,
    )
}
