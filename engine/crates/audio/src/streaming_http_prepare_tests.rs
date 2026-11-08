use super::*;
struct Context {
    root: std::path::PathBuf,
    paths: shared::vfs::GamePaths,
    host: Arc<migo_io::scheduler::IoScheduler>,
}
impl Context {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "migo-http-spool-{}-{}",
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
            host: Arc::new(migo_io::scheduler::IoScheduler::new(1080)),
        }
    }
    fn spool(&self) -> Arc<dyn shared::audio_file::AudioHttpSpool> {
        migo_services::audio::LocalAudioFile::session(self.host.clone(), &self.paths).http_spool()
    }
    fn start(
        &self,
        server: &Server,
        rate: u32,
    ) -> (mpsc::Receiver<StreamMsg>, Arc<StreamingState>) {
        let state = StreamingState::new();
        let rx = crate::streaming::start_streaming_download_with_spool(
            self.host.audio_cpu(),
            server.client(self.host.pools().clone()),
            "http://indexed-product-fixture.invalid/wrong.mp3".into(),
            state.clone(),
            rate,
            Some(self.spool()),
        );
        (rx, state)
    }
}
impl Drop for Context {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
#[tokio::test]
async fn unpinned_and_range_ignoring_m4a_use_one_original_body_and_preserve_complete_pcm() {
    let context = Context::new();
    for mode in [Mode::NoTag, Mode::Ignored] {
        for (name, data) in cases() {
            for rate in [16000, 48000] {
                let server = server(data.to_vec(), mode, false, false).await;
                let (mut rx, state) = context.start(&server, rate);
                let actual = collect(&mut rx).await.unwrap_or_else(|e| {
                    panic!("spooled HTTP M4A failed complete PCM: {name}/{rate}: {e}")
                });
                let expected = crate::decoder::decode_for_rate(data, rate).unwrap();
                assert_eq!(
                    actual.iter().map(|s| s.to_bits()).collect::<Vec<_>>(),
                    expected
                        .samples
                        .iter()
                        .map(|s| s.to_bits())
                        .collect::<Vec<_>>(),
                    "spooled HTTP M4A complete bits changed"
                );
                assert!(state.download_complete.load(Ordering::Acquire));
                assert_eq!(server.initial_eof.load(Ordering::Acquire), 1);
                assert_eq!(
                    server.ranges.load(Ordering::Acquire),
                    usize::from(mode == Mode::Ignored),
                    "fallback retried Range after source selection"
                );
                if std::env::var_os("MIGO_DUMP_HTTP_SPOOL_PCM").is_some() {
                    println!(
                        "MIGO_HTTP_SPOOL_PCM {}/{name}/{rate} {:?}",
                        if mode == Mode::NoTag {
                            "no-tag"
                        } else {
                            "ignored"
                        },
                        actual.iter().map(|s| s.to_bits()).collect::<Vec<_>>()
                    );
                }
            }
        }
    }
}
#[tokio::test]
async fn valid_pinned_range_stays_progressive_and_capability_errors_never_fallback() {
    let context = Context::new();
    let server = server(padded(TAIL, 20 * 1024 * 1024), Mode::Normal, true, false).await;
    let (mut rx, _) = context.start(&server, 48000);
    assert_eq!(
        collect(&mut rx).await.expect("prepared Range path failed"),
        crate::decoder::decode_for_rate(TAIL, 48000)
            .unwrap()
            .samples
    );
    assert_eq!(
        server.initial_eof.load(Ordering::Acquire),
        0,
        "prepared Range downloaded original body before PCM"
    );
    assert!(server.ranges.load(Ordering::Acquire) > 1);
    for mode in [Mode::Changed, Mode::Offset, Mode::Truncated] {
        let server = super::server(padded(TAIL, 256 * 1024), mode, false, false).await;
        let (mut rx, _) = context.start(&server, 48000);
        assert!(
            collect(&mut rx).await.is_err(),
            "invalid pinned Range fell back to successful PCM"
        );
        assert_eq!(server.ranges.load(Ordering::Acquire), 1);
    }
}
#[tokio::test]
async fn fallback_cannot_publish_before_seal_and_cancellation_wakes_the_original_body_wait() {
    let context = Context::new();
    let server = server(padded(TAIL, 256 * 1024), Mode::NoTag, true, false).await;
    use shared::audio_file::{
        AudioFile, AudioFileCancel, AudioHttpSpool, AudioSpoolBody, AudioSpoolFuture,
    };
    struct Observe {
        source: Arc<dyn AudioHttpSpool>,
        waiting: Arc<tokio::sync::Notify>,
    }
    struct Body {
        source: Box<dyn AudioSpoolBody>,
        waiting: Arc<tokio::sync::Notify>,
        calls: usize,
    }
    impl AudioSpoolBody for Body {
        fn length(&self) -> Option<u64> {
            self.source.length()
        }
        fn chunk(&mut self) -> AudioSpoolFuture<'_, Option<shared::audio_file::AudioFileChunk>> {
            self.calls += 1;
            if self.calls == 2 {
                self.waiting.notify_one();
            }
            self.source.chunk()
        }
    }
    impl AudioHttpSpool for Observe {
        fn prepare(
            self: Arc<Self>,
            body: Box<dyn AudioSpoolBody>,
            cancel: Arc<dyn AudioFileCancel>,
            deadline: std::time::Instant,
        ) -> AudioSpoolFuture<'static, Arc<dyn AudioFile>> {
            self.source.clone().prepare(
                Box::new(Body {
                    source: body,
                    waiting: self.waiting.clone(),
                    calls: 0,
                }),
                cancel,
                deadline,
            )
        }
    }
    let waiting = Arc::new(tokio::sync::Notify::new());
    let state = StreamingState::new();
    let mut rx = crate::streaming::start_streaming_download_with_spool(
        context.host.audio_cpu(),
        server.client(context.host.pools().clone()),
        "http://indexed-product-fixture.invalid/wrong.mp3".into(),
        state.clone(),
        48000,
        Some(Arc::new(Observe {
            source: context.spool(),
            waiting: waiting.clone(),
        })),
    );
    tokio::time::timeout(WAIT, async {
        tokio::select! {
            _ = waiting.notified() => {},
            _ = rx.recv() => panic!("spool published Ready or PCM before sealing full response"),
        }
    })
    .await
    .expect("spool did not reach the original second body read");
    assert!(
        matches!(rx.try_recv(), Err(mpsc::error::TryRecvError::Empty)),
        "spool published Ready or PCM before sealing full response"
    );
    state.cancel();
    assert!(
        tokio::time::timeout(WAIT, rx.recv())
            .await
            .unwrap()
            .is_none(),
        "cancelled preparation published a stream message"
    );
    assert_eq!(server.ranges.load(Ordering::Acquire), 0);
}

#[tokio::test]
async fn cancelled_spool_append_holds_actual_loan_capacity_until_the_original_fs_job_finishes() {
    use migo_io::task::{PoolKind, PriorityClass};
    use shared::audio_file::{AudioFileChunk, AudioSpoolBody, AudioSpoolFuture};
    const CAP: usize = 2 * 1024 * 1024;
    struct Loan {
        data: Vec<u8>,
        _ticket: migo_io::pools::ByteTicket,
    }
    impl AsRef<[u8]> for Loan {
        fn as_ref(&self) -> &[u8] {
            &self.data
        }
    }
    struct Queued {
        loan: Option<AudioFileChunk>,
        host: Arc<migo_io::scheduler::IoScheduler>,
        held: Option<std::sync::mpsc::Receiver<()>>,
        counter: std::sync::mpsc::Sender<u64>,
        ready: Arc<tokio::sync::Notify>,
    }
    impl AudioSpoolBody for Queued {
        fn length(&self) -> Option<u64> {
            Some(1)
        }
        fn chunk(&mut self) -> AudioSpoolFuture<'_, Option<AudioFileChunk>> {
            Box::pin(async move {
                if let Some(held) = self.held.take() {
                    let (entered, waiting) = std::sync::mpsc::channel();
                    let counter = self.counter.clone();
                    let _block = self
                        .host
                        .pools()
                        .submit_async_bytes(
                            PoolKind::Fs,
                            PriorityClass::ForegroundAsync,
                            0,
                            move || {
                                entered.send(()).unwrap();
                                held.recv_timeout(WAIT).unwrap();
                                counter
                                    .send(migo_alloc_probe::thread_counts().bytes_freed)
                                    .unwrap();
                            },
                        )
                        .unwrap();
                    tokio::task::spawn_blocking(move || waiting.recv_timeout(WAIT).unwrap())
                        .await
                        .unwrap();
                    self.ready.notify_one();
                }
                Ok(self.loan.take())
            })
        }
    }
    let mut context = Context::new();
    context.host = Arc::new(
        migo_io::scheduler::IoScheduler::isolated_for_admission_probe(1083, 1, CAP as u64),
    );
    let ticket = context.host.pools().reserve_bytes(CAP as u64).unwrap();
    let mut data = Vec::with_capacity(CAP);
    data.push(0x35);
    assert_eq!(data.capacity(), CAP);
    let watch = migo_alloc_probe::DeallocationWatch::new(data.as_ptr()).unwrap();
    let (release, held) = std::sync::mpsc::channel();
    let (counter, counts) = std::sync::mpsc::channel();
    let ready = Arc::new(tokio::sync::Notify::new());
    let body = Queued {
        loan: Some(AudioFileChunk::new(Loan {
            data,
            _ticket: ticket,
        })),
        host: context.host.clone(),
        held: Some(held),
        counter,
        ready: ready.clone(),
    };
    let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let operation = tokio::spawn(context.spool().prepare(
        Box::new(body),
        cancel.clone(),
        std::time::Instant::now() + WAIT,
    ));
    tokio::time::timeout(WAIT, ready.notified()).await.unwrap();
    cancel.store(true, Ordering::Release);
    operation.abort();
    let _ = operation.await;
    let retained = watch.freed_bytes().is_none() && context.host.pools().reserve_bytes(1).is_err();
    release.send(()).unwrap();
    let before = counts.recv_timeout(WAIT).unwrap();
    tokio::time::timeout(WAIT, async {
        while watch.freed_bytes().is_none() {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap();
    let after = context
        .host
        .pools()
        .submit_async_bytes(PoolKind::Fs, PriorityClass::ForegroundAsync, 0, || {
            migo_alloc_probe::thread_counts().bytes_freed
        })
        .unwrap()
        .await
        .unwrap()
        .unwrap();
    assert!(
        retained,
        "cancelled append released actual loan capacity or credit before the original Fs worker returned"
    );
    assert_eq!(
        watch.freed_bytes(),
        Some(CAP),
        "original spool loan allocation was not really freed"
    );
    assert!(
        after - before >= CAP as u64,
        "spool loan did not actually free on original Fs worker"
    );
    assert!(context.host.pools().reserve_bytes(CAP as u64).is_ok());
}

#[tokio::test]
async fn weak_chunked_decoded_and_unknown_total_sources_spool_the_same_full_representation() {
    let context = Context::new();
    for mode in [Mode::Weak, Mode::Chunked, Mode::Gzip, Mode::UnknownLength] {
        let server = server(TAIL.to_vec(), mode, false, false).await;
        let (mut rx, _) = context.start(&server, 48000);
        let actual = collect(&mut rx)
            .await
            .expect("coherent fallback representation was refused");
        assert_eq!(
            actual.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
            crate::decoder::decode_for_rate(TAIL, 48000)
                .unwrap()
                .samples
                .iter()
                .map(|v| v.to_bits())
                .collect::<Vec<_>>(),
            "decoded/chunked spool changed full PCM bits"
        );
        assert_eq!(
            server.ranges.load(Ordering::Acquire),
            usize::from(mode == Mode::UnknownLength)
        );
    }
    let server = server(TAIL.to_vec(), Mode::OriginalShort, false, false).await;
    let (mut rx, state) = context.start(&server, 48000);
    assert!(
        collect(&mut rx).await.is_err(),
        "short original HTTP body was sealed and published"
    );
    assert!(!state.download_complete.load(Ordering::Acquire));
}

#[tokio::test]
async fn actual_http_spool_chunks_admit_exact_box_capacity_and_hold_it_until_free() {
    const CAP: u64 = 65536;
    let host = migo_io::scheduler::IoScheduler::isolated_for_admission_probe(1084, 1, CAP);
    let server = server(TAIL.to_vec(), Mode::NoTag, false, false).await;
    let client = server.client(host.pools().clone());
    let response = client
        .get(
            migo_services::network::gate::GateKind::AudioStream,
            migo_services::network::url::Url::parse("http://indexed-product-fixture.invalid/loan")
                .unwrap(),
        )
        .header(
            "accept-encoding".parse().unwrap(),
            "identity".parse().unwrap(),
        )
        .send()
        .await
        .unwrap();
    let length = response.content_length();
    let mut body = crate::streaming_http_prepare::body_for_spool(
        client.clone(),
        response,
        Default::default(),
        length,
    );
    let chunk = body.chunk().await.unwrap().unwrap();
    let n = chunk.as_ref().len();
    let watch = migo_alloc_probe::DeallocationWatch::new(chunk.as_ref().as_ptr()).unwrap();
    let rest = host.pools().reserve_bytes(CAP - n as u64).unwrap();
    assert!(
        host.pools().reserve_bytes(1).is_err(),
        "HTTP spool transport loan lost its actual allocation admission"
    );
    let before = host
        .pools()
        .submit_async_bytes(
            migo_io::task::PoolKind::Fs,
            migo_io::task::PriorityClass::ForegroundAsync,
            0,
            move || {
                let before = migo_alloc_probe::thread_counts().bytes_freed;
                drop(chunk);
                migo_alloc_probe::thread_counts().bytes_freed - before
            },
        )
        .unwrap()
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        watch.freed_bytes(),
        Some(n),
        "actual HTTP spool Box was not really freed"
    );
    assert!(before >= n as u64);
    assert!(host.pools().reserve_bytes(n as u64).is_ok());
    drop(rest);
}
