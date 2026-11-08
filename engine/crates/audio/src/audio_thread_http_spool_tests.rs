use super::*;
use crate::streaming_http::tests::{Mode, server};
use shared::protocol::audio_cmd::InnerAudioEventType;
struct Actor {
    sender: shared::op_state::AudioSender,
    thread: Option<thread::JoinHandle<()>>,
    root: std::path::PathBuf,
}
impl Drop for Actor {
    fn drop(&mut self) {
        let _ = self.sender.send(AudioCmd::Shutdown);
        if let Some(t) = self.thread.take() {
            t.join().unwrap();
        }
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[tokio::test]
async fn admitted_url_carries_session_spool_through_the_actual_audio_actor_and_source_identity() {
    let io = Arc::new(migo_io::scheduler::IoScheduler::new(1082));
    let source = server(
        include_bytes!("decoder/aac-fixtures/1-tail.m4a").to_vec(),
        Mode::NoTag,
        false,
        false,
    )
    .await;
    let client = source.client(io.pools().clone());
    let factory: StreamingHttpClientFactory = Arc::new(move || Ok(client.clone()));
    let (tx, rx) = shared::audio_channel::channel();
    let wakeup = ThreadWakeup::new();
    let sender = shared::op_state::AudioSender::new(tx, wakeup.clone());
    let (host_tx, _critical, mut events) = shared::host_channel::channel(64);
    let root = std::env::temp_dir().join(format!(
        "migo-spool-actor-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let paths =
        shared::vfs::GamePaths::new(root.join("files"), root.join("cache"), "audio", 1).unwrap();
    paths.ensure_directories().unwrap();
    let mut silent = crate::output::SilentOutput::new();
    silent.resume_stream();
    let cpu = io.audio_cpu();
    let thread = thread::spawn(move || {
        run_audio_thread(
            rx,
            Vec::new(),
            AudioOutput::Silent(silent),
            host_tx,
            wakeup,
            factory,
            cpu,
        )
    });
    let actor = Actor {
        sender,
        thread: Some(thread),
        root,
    };
    actor
        .sender
        .send(AudioCmd::CreateInnerAudio { id: 811 })
        .unwrap();
    actor
        .sender
        .send(AudioCmd::InnerAudioSetAutoplay {
            id: 811,
            autoplay: true,
        })
        .unwrap();
    let sources = migo_services::audio::InnerAudioSources {
        code_dir: None,
        vfs: None,
        local_file: Some(migo_services::audio::LocalAudioFile::session(io, &paths)),
        admit_remote: |_: &migo_services::audio::Url| Ok(()),
    };
    let load = migo_services::audio::inner_audio_load_url(
        actor.sender.clone(),
        sources,
        811,
        818,
        "http://indexed-product-fixture.invalid/wrong.mp3".into(),
    )
    .unwrap();
    tokio::time::timeout(Duration::from_secs(20), load)
        .await
        .unwrap()
        .unwrap();
    let end = Instant::now() + Duration::from_secs(20);
    let mut canplay = false;
    loop {
        assert!(
            Instant::now() < end,
            "session spool never reached actual actor completion"
        );
        if let Ok(HostCommand::InnerAudioEvent { event }) = events.try_recv() {
            assert_eq!((event.id, event.source_id), (811, 818));
            assert_ne!(
                event.event_type,
                InnerAudioEventType::Error,
                "actual actor lost session HTTP spool capability"
            );
            canplay |= event.event_type == InnerAudioEventType::CanPlay;
            if event.event_type == InnerAudioEventType::Ended {
                break;
            }
        } else {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    }
    assert!(canplay, "actual spool source never published CanPlay");
    drop(actor);
}

#[tokio::test]
async fn actual_url_actor_keeps_the_original_sealed_source_for_public_seek_and_restart() {
    let io = Arc::new(migo_io::scheduler::IoScheduler::new(1082));
    let source = server(
        include_bytes!("decoder/aac-fixtures/1-tail.m4a").to_vec(),
        Mode::NoTag,
        false,
        false,
    )
    .await;
    let client = source.client(io.pools().clone());
    let factory: StreamingHttpClientFactory = Arc::new(move || Ok(client.clone()));
    let (tx, rx) = shared::audio_channel::channel();
    let wakeup = ThreadWakeup::new();
    let sender = shared::op_state::AudioSender::new(tx, wakeup.clone());
    let (host_tx, _critical, mut events) = shared::host_channel::channel(64);
    let root = std::env::temp_dir().join(format!(
        "migo-spool-actor-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let paths =
        shared::vfs::GamePaths::new(root.join("files"), root.join("cache"), "audio", 1).unwrap();
    paths.ensure_directories().unwrap();
    let mut silent = crate::output::SilentOutput::new();
    silent.resume_stream();
    let cpu = io.audio_cpu();
    let thread = thread::spawn(move || {
        run_audio_thread(
            rx,
            Vec::new(),
            AudioOutput::Silent(silent),
            host_tx,
            wakeup,
            factory,
            cpu,
        )
    });
    let actor = Actor {
        sender,
        thread: Some(thread),
        root,
    };
    actor
        .sender
        .send(AudioCmd::CreateInnerAudio { id: 811 })
        .unwrap();
    actor
        .sender
        .send(AudioCmd::InnerAudioSetAutoplay {
            id: 811,
            autoplay: true,
        })
        .unwrap();
    let (spool, seen, freed) = crate::streaming_http::tests::controls::observe(
        migo_services::audio::LocalAudioFile::session(io.clone(), &paths).http_spool(),
    );
    let (reply, result) = tokio::sync::oneshot::channel();
    actor
        .sender
        .send(AudioCmd::InnerAudioBeginLoad {
            id: 811,
            source_id: 818,
            resp: reply,
        })
        .unwrap();
    let load = result.await.unwrap().unwrap();
    let (reply, result) = tokio::sync::oneshot::channel();
    actor
        .sender
        .send(AudioCmd::InnerAudioLoadUrl {
            id: 811,
            load,
            url: "http://indexed-product-fixture.invalid/wrong.mp3".into(),
            spool: Some(spool),
            resp: reply,
        })
        .unwrap();
    result.await.unwrap().unwrap();
    let seen = tokio::time::timeout(Duration::from_secs(20), seen)
        .await
        .unwrap()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut ended = 0;
    let mut seeked = false;
    while ended < 2 {
        assert!(Instant::now() < deadline, "actual URL controls stalled");
        if let Ok(HostCommand::InnerAudioEvent { event }) = events.try_recv() {
            assert_eq!((event.id, event.source_id), (811, 818));
            assert_ne!(
                event.event_type,
                InnerAudioEventType::Error,
                "actual URL control emitted Error"
            );
            seeked |= event.event_type == InnerAudioEventType::Seeked;
            if event.event_type == InnerAudioEventType::Ended {
                ended += 1;
                assert!(
                    seen.file.upgrade().is_some() && seen.watch.freed_bytes().is_none(),
                    "actual URL actor did not retain the sealed source at EOF"
                );
                if ended == 1 {
                    actor
                        .sender
                        .send(AudioCmd::InnerAudioSeek {
                            id: 811,
                            position: 0.0,
                        })
                        .unwrap();
                    actor
                        .sender
                        .send(AudioCmd::InnerAudioPlay { id: 811 })
                        .unwrap();
                }
            }
        } else {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    }
    assert!(
        seeked,
        "actual URL actor did not complete public source seek"
    );
    drop(actor);
    let freed = tokio::time::timeout(Duration::from_secs(20), freed)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(freed.allocation, Some(65536));
    assert!(freed.bytes >= 131072 && freed.off_async_thread);
}

#[tokio::test]
async fn actual_url_actor_reopens_the_same_pinned_range_source_after_eof() {
    let io = Arc::new(migo_io::scheduler::IoScheduler::new(1082));
    let source = server(
        crate::streaming_http::tests::padded(
            include_bytes!("decoder/aac-fixtures/1-tail.m4a"),
            1024 * 1024,
        ),
        Mode::Normal,
        false,
        false,
    )
    .await;
    let client = source.client(io.pools().clone());
    let factory: StreamingHttpClientFactory = Arc::new(move || Ok(client.clone()));
    let (tx, rx) = shared::audio_channel::channel();
    let wakeup = ThreadWakeup::new();
    let sender = shared::op_state::AudioSender::new(tx, wakeup.clone());
    let (host_tx, _critical, mut events) = shared::host_channel::channel(64);
    let root = std::env::temp_dir().join(format!(
        "migo-spool-actor-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let paths =
        shared::vfs::GamePaths::new(root.join("files"), root.join("cache"), "audio", 1).unwrap();
    paths.ensure_directories().unwrap();
    let mut silent = crate::output::SilentOutput::new();
    silent.resume_stream();
    let cpu = io.audio_cpu();
    let thread = thread::spawn(move || {
        run_audio_thread(
            rx,
            Vec::new(),
            AudioOutput::Silent(silent),
            host_tx,
            wakeup,
            factory,
            cpu,
        )
    });
    let actor = Actor {
        sender,
        thread: Some(thread),
        root,
    };
    actor
        .sender
        .send(AudioCmd::CreateInnerAudio { id: 811 })
        .unwrap();
    actor
        .sender
        .send(AudioCmd::InnerAudioSetAutoplay {
            id: 811,
            autoplay: true,
        })
        .unwrap();
    let spool = migo_services::audio::LocalAudioFile::session(io.clone(), &paths).http_spool();
    let (reply, result) = tokio::sync::oneshot::channel();
    actor
        .sender
        .send(AudioCmd::InnerAudioBeginLoad {
            id: 811,
            source_id: 818,
            resp: reply,
        })
        .unwrap();
    let load = result.await.unwrap().unwrap();
    let (reply, result) = tokio::sync::oneshot::channel();
    actor
        .sender
        .send(AudioCmd::InnerAudioLoadUrl {
            id: 811,
            load,
            url: "http://indexed-product-fixture.invalid/wrong.mp3".into(),
            spool: Some(spool),
            resp: reply,
        })
        .unwrap();
    result.await.unwrap().unwrap();
    let mut ranges_at_eof = 0;
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut ended = 0;
    let mut seeked = false;
    while ended < 2 {
        assert!(Instant::now() < deadline, "actual URL controls stalled");
        if let Ok(HostCommand::InnerAudioEvent { event }) = events.try_recv() {
            assert_eq!((event.id, event.source_id), (811, 818));
            assert_ne!(
                event.event_type,
                InnerAudioEventType::Error,
                "actual URL control emitted Error"
            );
            seeked |= event.event_type == InnerAudioEventType::Seeked;
            if event.event_type == InnerAudioEventType::Ended {
                ended += 1;
                if ended == 2 {
                    assert!(
                        source.range_requests() > ranges_at_eof,
                        "actual URL actor did not reopen the retained Range source"
                    );
                }
                if ended == 1 {
                    ranges_at_eof = source.range_requests();
                    actor
                        .sender
                        .send(AudioCmd::InnerAudioSeek {
                            id: 811,
                            position: 0.0,
                        })
                        .unwrap();
                    actor
                        .sender
                        .send(AudioCmd::InnerAudioPlay { id: 811 })
                        .unwrap();
                }
            }
        } else {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    }
    assert!(
        seeked,
        "actual URL actor did not complete public source seek"
    );
    drop(actor);
}

#[tokio::test]
async fn actual_url_actor_reopens_the_same_pinned_wave_source_after_eof() {
    let io = Arc::new(migo_io::scheduler::IoScheduler::new(1107));
    let source = server(
        crate::streaming_codecs::tests::wav(48000, 2, 120137),
        Mode::Normal,
        false,
        false,
    )
    .await;
    let client = source.client(io.pools().clone());
    let factory: StreamingHttpClientFactory = Arc::new(move || Ok(client.clone()));
    let (tx, rx) = shared::audio_channel::channel();
    let wakeup = ThreadWakeup::new();
    let sender = shared::op_state::AudioSender::new(tx, wakeup.clone());
    let (host_tx, _critical, mut events) = shared::host_channel::channel(64);
    let root = std::env::temp_dir().join(format!(
        "migo-spool-actor-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let paths =
        shared::vfs::GamePaths::new(root.join("files"), root.join("cache"), "audio", 1).unwrap();
    paths.ensure_directories().unwrap();
    let mut silent = crate::output::SilentOutput::new();
    silent.resume_stream();
    let cpu = io.audio_cpu();
    let thread = thread::spawn(move || {
        run_audio_thread(
            rx,
            Vec::new(),
            AudioOutput::Silent(silent),
            host_tx,
            wakeup,
            factory,
            cpu,
        )
    });
    let actor = Actor {
        sender,
        thread: Some(thread),
        root,
    };
    actor
        .sender
        .send(AudioCmd::CreateInnerAudio { id: 811 })
        .unwrap();
    actor
        .sender
        .send(AudioCmd::InnerAudioSetAutoplay {
            id: 811,
            autoplay: true,
        })
        .unwrap();
    actor
        .sender
        .send(AudioCmd::InnerAudioSetPlaybackRate {
            id: 811,
            rate: 1.333,
        })
        .unwrap();
    let spool = migo_services::audio::LocalAudioFile::session(io.clone(), &paths).http_spool();
    let (reply, result) = tokio::sync::oneshot::channel();
    actor
        .sender
        .send(AudioCmd::InnerAudioBeginLoad {
            id: 811,
            source_id: 818,
            resp: reply,
        })
        .unwrap();
    let load = result.await.unwrap().unwrap();
    let (reply, result) = tokio::sync::oneshot::channel();
    actor
        .sender
        .send(AudioCmd::InnerAudioLoadUrl {
            id: 811,
            load,
            url: "http://indexed-product-fixture.invalid/wrong.mp3".into(),
            spool: Some(spool),
            resp: reply,
        })
        .unwrap();
    result.await.unwrap().unwrap();
    let (resp, reply) = tokio::sync::oneshot::channel();
    actor
        .sender
        .send(AudioCmd::InnerAudioGetState { id: 811, resp })
        .unwrap();
    assert_eq!(reply.await.unwrap().unwrap().playback_rate, 1.333);
    let mut ranges_at_eof = 0;
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut ended = 0;
    let mut seeked = false;
    while ended < 2 {
        assert!(Instant::now() < deadline, "actual URL controls stalled");
        if let Ok(HostCommand::InnerAudioEvent { event }) = events.try_recv() {
            assert_eq!((event.id, event.source_id), (811, 818));
            assert_ne!(
                event.event_type,
                InnerAudioEventType::Error,
                "actual URL control emitted Error"
            );
            seeked |= event.event_type == InnerAudioEventType::Seeked;
            if event.event_type == InnerAudioEventType::Ended {
                ended += 1;
                if ended == 2 {
                    assert!(
                        source.range_requests() > ranges_at_eof,
                        "actual URL WAVE actor did not reopen the pinned source"
                    );
                }
                if ended == 1 {
                    ranges_at_eof = source.range_requests();
                    actor
                        .sender
                        .send(AudioCmd::InnerAudioSeek {
                            id: 811,
                            position: 0.0,
                        })
                        .unwrap();
                    actor
                        .sender
                        .send(AudioCmd::InnerAudioPlay { id: 811 })
                        .unwrap();
                }
            }
        } else {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    }
    assert!(
        seeked,
        "actual URL actor did not complete public source seek"
    );
    drop(actor);
}

#[tokio::test]
async fn actual_mp3_url_actor_reopens_the_same_pinned_source_after_eof() {
    let io = Arc::new(migo_io::scheduler::IoScheduler::new(1082));
    let source = server(
        {
            // A valid declared stream outlives the original 64KiB encoded window.
            use crate::mp3_fixture as m;
            let count = 180u32;
            let mut info = m::stream(1);
            info[36..40].copy_from_slice(b"Info");
            info[40..44].copy_from_slice(&1u32.to_be_bytes());
            info[44..48].copy_from_slice(&count.to_be_bytes());
            [info, m::stream(count as usize)].concat()
        },
        Mode::Normal,
        false,
        false,
    )
    .await;
    let client = source.client(io.pools().clone());
    let factory: StreamingHttpClientFactory = Arc::new(move || Ok(client.clone()));
    let (tx, rx) = shared::audio_channel::channel();
    let wakeup = ThreadWakeup::new();
    let sender = shared::op_state::AudioSender::new(tx, wakeup.clone());
    let (host_tx, _critical, mut events) = shared::host_channel::channel(64);
    let root = std::env::temp_dir().join(format!(
        "migo-spool-actor-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let paths =
        shared::vfs::GamePaths::new(root.join("files"), root.join("cache"), "audio", 1).unwrap();
    paths.ensure_directories().unwrap();
    let mut silent = crate::output::SilentOutput::new();
    silent.resume_stream();
    let cpu = io.audio_cpu();
    let thread = thread::spawn(move || {
        run_audio_thread(
            rx,
            Vec::new(),
            AudioOutput::Silent(silent),
            host_tx,
            wakeup,
            factory,
            cpu,
        )
    });
    let actor = Actor {
        sender,
        thread: Some(thread),
        root,
    };
    actor
        .sender
        .send(AudioCmd::CreateInnerAudio { id: 811 })
        .unwrap();
    actor
        .sender
        .send(AudioCmd::InnerAudioSetAutoplay {
            id: 811,
            autoplay: true,
        })
        .unwrap();
    let spool = migo_services::audio::LocalAudioFile::session(io.clone(), &paths).http_spool();
    let (reply, result) = tokio::sync::oneshot::channel();
    actor
        .sender
        .send(AudioCmd::InnerAudioBeginLoad {
            id: 811,
            source_id: 818,
            resp: reply,
        })
        .unwrap();
    let load = result.await.unwrap().unwrap();
    let (reply, result) = tokio::sync::oneshot::channel();
    actor
        .sender
        .send(AudioCmd::InnerAudioLoadUrl {
            id: 811,
            load,
            url: "http://indexed-product-fixture.invalid/wrong.wav".into(),
            spool: Some(spool),
            resp: reply,
        })
        .unwrap();
    result.await.unwrap().unwrap();
    let mut ranges_at_eof = 0;
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut ended = 0;
    let mut seeked = false;
    while ended < 2 {
        assert!(Instant::now() < deadline, "actual URL controls stalled");
        if let Ok(HostCommand::InnerAudioEvent { event }) = events.try_recv() {
            assert_eq!((event.id, event.source_id), (811, 818));
            assert_ne!(
                event.event_type,
                InnerAudioEventType::Error,
                "actual URL control emitted Error"
            );
            seeked |= event.event_type == InnerAudioEventType::Seeked;
            if event.event_type == InnerAudioEventType::Ended {
                ended += 1;
                if ended == 2 {
                    assert!(
                        source.range_requests() > ranges_at_eof,
                        "actual URL actor did not reopen the retained Range source"
                    );
                }
                if ended == 1 {
                    ranges_at_eof = source.range_requests();
                    actor
                        .sender
                        .send(AudioCmd::InnerAudioSeek {
                            id: 811,
                            position: 0.0,
                        })
                        .unwrap();
                    actor
                        .sender
                        .send(AudioCmd::InnerAudioPlay { id: 811 })
                        .unwrap();
                }
            }
        } else {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    }
    assert!(
        seeked,
        "actual URL actor did not complete public source seek"
    );
    drop(actor);
}

#[tokio::test]
async fn actual_vorbis_url_actor_reopens_the_same_pinned_source_after_eof() {
    let io = Arc::new(migo_io::scheduler::IoScheduler::new(1143));
    let source = server(
        include_bytes!("decoder/ogg-fixtures/cross.ogg").to_vec(),
        Mode::Normal,
        false,
        false,
    )
    .await;
    let client = source.client(io.pools().clone());
    let factory: StreamingHttpClientFactory = Arc::new(move || Ok(client.clone()));
    let (tx, rx) = shared::audio_channel::channel();
    let wakeup = ThreadWakeup::new();
    let sender = shared::op_state::AudioSender::new(tx, wakeup.clone());
    let (host_tx, _critical, mut events) = shared::host_channel::channel(64);
    let root = std::env::temp_dir().join(format!(
        "migo-spool-actor-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let paths =
        shared::vfs::GamePaths::new(root.join("files"), root.join("cache"), "audio", 1).unwrap();
    paths.ensure_directories().unwrap();
    let mut silent = crate::output::SilentOutput::new();
    silent.resume_stream();
    let cpu = io.audio_cpu();
    let thread = thread::spawn(move || {
        run_audio_thread(
            rx,
            Vec::new(),
            AudioOutput::Silent(silent),
            host_tx,
            wakeup,
            factory,
            cpu,
        )
    });
    let actor = Actor {
        sender,
        thread: Some(thread),
        root,
    };
    actor
        .sender
        .send(AudioCmd::CreateInnerAudio { id: 811 })
        .unwrap();
    actor
        .sender
        .send(AudioCmd::InnerAudioSetAutoplay {
            id: 811,
            autoplay: true,
        })
        .unwrap();
    let spool = migo_services::audio::LocalAudioFile::session(io.clone(), &paths).http_spool();
    let (reply, result) = tokio::sync::oneshot::channel();
    actor
        .sender
        .send(AudioCmd::InnerAudioBeginLoad {
            id: 811,
            source_id: 818,
            resp: reply,
        })
        .unwrap();
    let load = result.await.unwrap().unwrap();
    let (reply, result) = tokio::sync::oneshot::channel();
    actor
        .sender
        .send(AudioCmd::InnerAudioLoadUrl {
            id: 811,
            load,
            url: "http://indexed-product-fixture.invalid/wrong.wav".into(),
            spool: Some(spool),
            resp: reply,
        })
        .unwrap();
    result.await.unwrap().unwrap();
    let mut ranges_at_eof = 0;
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut ended = 0;
    let mut seeked = false;
    while ended < 2 {
        assert!(Instant::now() < deadline, "actual URL controls stalled");
        if let Ok(HostCommand::InnerAudioEvent { event }) = events.try_recv() {
            assert_eq!((event.id, event.source_id), (811, 818));
            assert_ne!(
                event.event_type,
                InnerAudioEventType::Error,
                "actual URL control emitted Error"
            );
            seeked |= event.event_type == InnerAudioEventType::Seeked;
            if event.event_type == InnerAudioEventType::Ended {
                ended += 1;
                if ended == 2 {
                    assert!(
                        source.range_requests() > ranges_at_eof,
                        "actual URL actor did not reopen the retained Range source"
                    );
                }
                if ended == 1 {
                    ranges_at_eof = source.range_requests();
                    actor
                        .sender
                        .send(AudioCmd::InnerAudioSeek {
                            id: 811,
                            position: 0.0,
                        })
                        .unwrap();
                    actor
                        .sender
                        .send(AudioCmd::InnerAudioPlay { id: 811 })
                        .unwrap();
                }
            }
        } else {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    }
    assert!(
        seeked,
        "actual URL actor did not complete public source seek"
    );
    drop(actor);
}
