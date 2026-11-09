use super::super::tests::{audio_buffer_decode_job, inner_decode_job, wav};
use super::*;
use std::sync::{Mutex, mpsc};

const WAIT: Duration = Duration::from_secs(5);
fn until(mut ready: impl FnMut() -> bool) {
    let end = Instant::now() + WAIT;
    while !ready() {
        assert!(Instant::now() < end, "resource did not retire");
        thread::yield_now();
    }
}
fn pool(
    host: &migo_io::scheduler::IoScheduler,
    slots: usize,
) -> (DecodePool, DecodeQueueReceiver<DecodeResult>) {
    let (tx, rx) = decode_queue(8, MAX_DECODE_RESULT_QUEUED_BYTES);
    (
        DecodePool::new_with_in_flight(
            slots,
            tx,
            Arc::new(DecodeInFlightUsage::new(2, 1)),
            ThreadWakeup::new(),
            host.audio_cpu(),
        ),
        rx,
    )
}
fn response<T>(rx: tokio::sync::oneshot::Receiver<T>) -> T {
    crate::streaming::get_stream_runtime()
        .block_on(async { tokio::time::timeout(WAIT, rx).await.unwrap().unwrap() })
}
struct Release(Option<mpsc::Sender<()>>);
impl Drop for Release {
    fn drop(&mut self) {
        self.0.take();
    }
}
struct Observation {
    name: Mutex<Option<String>>,
    cancelled: AtomicBool,
    flag: Mutex<Option<Arc<AtomicBool>>>,
}
fn hold_first_step(pool: &mut DecodePool) -> (mpsc::Receiver<()>, Release, Arc<Observation>) {
    let (start, started) = mpsc::channel();
    let (release, waiting) = mpsc::channel();
    let waiting = Mutex::new(Some(waiting));
    let name = Arc::new(Observation {
        name: Mutex::new(None),
        cancelled: AtomicBool::new(false),
        flag: Mutex::new(None),
    });
    let observed = name.clone();
    pool.step_hook = Some(Arc::new(move |cancel| {
        if let Some(waiting) = waiting.lock().unwrap().take() {
            *observed.flag.lock().unwrap() = Some(cancel.clone());
            *observed.name.lock().unwrap() = thread::current().name().map(str::to_owned);
            start.send(()).unwrap();
            let _ = waiting.recv_timeout(WAIT);
            observed
                .cancelled
                .store(cancel.load(Ordering::Acquire), Ordering::Release);
        }
    }));
    (started, Release(Some(release)), name)
}

#[test]
fn waiting_reply_cancel_returns_encoded_credit_without_an_active_slot() {
    let host = migo_io::scheduler::IoScheduler::new(0);
    let (mut pool, results) = pool(&host, 0);
    let data = Arc::new(wav(8000, 4000));
    let (job, reply) = audio_buffer_decode_job(1, data.clone());
    pool.submit(job);
    assert_eq!(pool.pending.items.load(Ordering::Acquire), 1);
    drop(reply);
    until(|| Arc::strong_count(&data) == 1 && pool.pending.items.load(Ordering::Acquire) == 0);
    assert_eq!(pool.pending.bytes.load(Ordering::Acquire), 0);
    assert_eq!(pool.in_flight.bytes.load(Ordering::Acquire), 0);
    assert!(results.try_recv().is_err());
}

#[test]
fn queued_source_change_wakes_without_waiting_for_cpu_or_local_admission() {
    let host = migo_io::scheduler::IoScheduler::new(0);
    let (mut pool, _results) = pool(&host, 0);
    let state = Arc::new(crate::inner_audio::InnerAudioSharedState::new());
    let load = state.begin_source(7).unwrap();
    let (resp, reply) = tokio::sync::oneshot::channel();
    pool.submit(DecodeJob::InnerAudio {
        id: 1,
        load,
        target_rate: 8000,
        data: wav(8000, 4000),
        resp,
    });
    let _next = state.begin_source(8).unwrap();
    assert_eq!(response(reply).unwrap_err().code, ErrorCode::Cancelled);
    until(|| pool.pending.items.load(Ordering::Acquire) == 0);
    assert_eq!(pool.pending.items.load(Ordering::Acquire), 0);
    assert_eq!(pool.in_flight.bytes.load(Ordering::Acquire), 0);
}

#[test]
fn running_cancel_keeps_memory_and_slot_until_the_cpu_owner_actually_retires() {
    let host = migo_io::scheduler::IoScheduler::new(0);
    let (mut pool, results) = pool(&host, 1);
    let (started, release, name) = hold_first_step(&mut pool);
    let data = Arc::new(wav(8000, 4000));
    let (job, reply) = audio_buffer_decode_job(1, data.clone());
    pool.submit(job);
    started.recv_timeout(WAIT).unwrap();
    drop(reply);
    until(|| {
        name.flag
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|flag| flag.load(Ordering::Acquire))
    });
    assert_eq!(pool.in_flight.bytes.load(Ordering::Acquire), 1);
    assert_eq!(pool.slots.available_permits(), 0);
    assert_eq!(Arc::strong_count(&data), 2);
    // A sentinel on the exact single async worker must run while native work is held.
    let (sent, observed) = mpsc::channel();
    crate::streaming::get_stream_runtime().spawn(async move {
        sent.send(()).unwrap();
    });
    observed.recv_timeout(WAIT).unwrap();
    assert_ne!(name.name.lock().unwrap().as_deref(), Some("audio-stream"));
    drop(release);
    until(|| {
        pool.in_flight.bytes.load(Ordering::Acquire) == 0
            && Arc::strong_count(&data) == 1
            && pool.slots.available_permits() == 1
    });
    assert_eq!(pool.slots.available_permits(), 1);
    assert!(name.cancelled.load(Ordering::Acquire));
    assert!(results.try_recv().is_err());
}

#[test]
fn host_close_waits_for_late_native_retirement_before_publishing_error() {
    let host = migo_io::scheduler::IoScheduler::new(0);
    let (mut pool, results) = pool(&host, 1);
    let (started, release, observation) = hold_first_step(&mut pool);
    let data = Arc::new(wav(8000, 4000));
    let (job, _reply) = audio_buffer_decode_job(1, data.clone());
    pool.submit(job);
    started.recv_timeout(WAIT).unwrap();
    host.close();
    until(|| {
        observation
            .flag
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|flag| flag.load(Ordering::Acquire))
    });
    let (sent, polled) = mpsc::channel();
    crate::streaming::get_stream_runtime().spawn(async move {
        sent.send(()).unwrap();
    });
    polled.recv_timeout(WAIT).unwrap();
    assert_eq!(pool.in_flight.bytes.load(Ordering::Acquire), 1);
    assert!(results.try_recv().is_err());
    drop(release);
    let DecodeResult::AudioBuffer { result, .. } = results.recv_timeout(WAIT).unwrap() else {
        panic!("buffer");
    };
    assert_eq!(result.unwrap_err().code, ErrorCode::Disconnected);
    assert_eq!(Arc::strong_count(&data), 1);
    assert_eq!(pool.in_flight.bytes.load(Ordering::Acquire), 0);
    assert_eq!(pool.slots.available_permits(), 1);
}

#[test]
fn pool_drop_aborts_its_tasks_without_waiting_or_closing_peer_hosts() {
    // Three workers: the fewest with two CPU-heavy slots, so the peer's job runs
    // beside the held step on any machine. The shared process executor sizes
    // itself from the cores, and on two it has one such slot, where the peer
    // waits for the step this test only releases after the peer answers.
    let host = migo_io::scheduler::IoScheduler::isolated_for_admission_probe(0, 3, u64::MAX);
    let peer = host.peer_for_admission_probe(1);
    let (mut pool, _results) = pool(&host, 1);
    let (started, release, _) = hold_first_step(&mut pool);
    let data = Arc::new(wav(8000, 4000));
    let (job, _reply) = audio_buffer_decode_job(1, data.clone());
    pool.submit(job);
    started.recv_timeout(WAIT).unwrap();
    let budget = pool.in_flight.clone();
    drop(pool); // still-held native step makes any synchronous join fail
    assert_eq!(budget.bytes.load(Ordering::Acquire), 1);
    let (sent, observed) = mpsc::channel();
    crate::streaming::get_stream_runtime().spawn(async move {
        sent.send(peer.audio_cpu().run(|| 42).await.unwrap())
            .unwrap();
    });
    assert_eq!(observed.recv_timeout(WAIT).unwrap(), 42);
    drop(release);
    until(|| budget.bytes.load(Ordering::Acquire) == 0 && Arc::strong_count(&data) == 1);
}

#[test]
fn host_close_releases_waiting_input_without_a_local_slot() {
    let host = migo_io::scheduler::IoScheduler::new(0);
    let (mut pool, results) = pool(&host, 0);
    let data = Arc::new(wav(8000, 4000));
    let (job, _reply) = audio_buffer_decode_job(1, data.clone());
    pool.submit(job);
    host.close();
    let DecodeResult::AudioBuffer { result, .. } = results.recv_timeout(WAIT).unwrap() else {
        panic!("buffer");
    };
    assert_eq!(result.unwrap_err().code, ErrorCode::Disconnected);
    assert_eq!(Arc::strong_count(&data), 1);
    assert_eq!(pool.pending.items.load(Ordering::Acquire), 0);
    assert_eq!(pool.pending.bytes.load(Ordering::Acquire), 0);
    assert_eq!(pool.in_flight.bytes.load(Ordering::Acquire), 0);
}

#[test]
fn a_panicked_cpu_step_retires_its_state_and_the_next_decode_completes() {
    let host = migo_io::scheduler::IoScheduler::new(0);
    let (mut pool, results) = pool(&host, 1);
    pool.step_hook = Some(Arc::new(|_| panic!("injected codec step panic")));
    let data = Arc::new(wav(8000, 4000));
    let (job, _reply) = audio_buffer_decode_job(1, data.clone());
    pool.submit(job);
    let DecodeResult::AudioBuffer { result, .. } = results.recv_timeout(WAIT).unwrap() else {
        panic!("buffer");
    };
    assert_eq!(result.unwrap_err().code, ErrorCode::Internal);
    assert_eq!(Arc::strong_count(&data), 1);
    assert_eq!(pool.in_flight.bytes.load(Ordering::Acquire), 0);
    assert_eq!(pool.slots.available_permits(), 1);
    pool.step_hook = None;
    let (job, _next_reply) = audio_buffer_decode_job(2, data);
    pool.submit(job);
    let DecodeResult::AudioBuffer { result, .. } = results.recv_timeout(WAIT).unwrap() else {
        panic!("buffer");
    };
    assert_eq!(result.unwrap().frame_count(), 24000);
}

#[test]
fn scheduled_formats_preserve_every_pcm_sample_while_refill_and_async_work_progress() {
    let host = migo_io::scheduler::IoScheduler::new(0);
    let peer = migo_io::scheduler::IoScheduler::new(0);
    let (mut pool, results) = pool(&host, 1);
    let turns = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let observed = turns.clone();
    pool.step_hook = Some(Arc::new(move |_| {
        observed.fetch_add(1, Ordering::AcqRel);
    }));
    for bytes in [
        wav(8000, 32000),
        include_bytes!("decoder/ogg-fixtures/cross.ogg").to_vec(),
        crate::mp3_fixture::stream(256),
        include_bytes!("decoder/aac-fixtures/adts-1.aac").to_vec(),
        include_bytes!("decoder/aac-fixtures/2-tail-reverse.m4a").to_vec(),
    ] {
        let native = crate::decoder::decode(&bytes).unwrap();
        let expected = crate::resampler::resample_if_needed(native, 48000).unwrap();
        let data = Arc::new(bytes);
        let (job, _reply) = audio_buffer_decode_job(1, data.clone());
        let before = turns.load(Ordering::Acquire);
        pool.submit(job);
        let (sent, progress) = mpsc::channel();
        let foreground = host.audio_cpu();
        let other = peer.audio_cpu();
        crate::streaming::get_stream_runtime().spawn(async move {
            for _ in 0..8 {
                assert_eq!(foreground.run(|| 17).await.unwrap(), 17);
                assert_eq!(other.run(|| 23).await.unwrap(), 23);
            }
            sent.send(()).unwrap();
        });
        progress.recv_timeout(WAIT).unwrap();
        let DecodeResult::AudioBuffer { result, .. } =
            results.recv_timeout(Duration::from_secs(60)).unwrap()
        else {
            panic!("buffer");
        };
        let actual = result.unwrap();
        assert_eq!(
            (actual.sample_rate, actual.channels),
            (expected.sample_rate, expected.channels)
        );
        assert_eq!(actual.samples, expected.samples);
        assert!(
            turns.load(Ordering::Acquire) > before + 1,
            "work was drained in one CPU step"
        );
        assert_eq!(Arc::strong_count(&data), 1);
        assert_eq!(pool.in_flight.bytes.load(Ordering::Acquire), 0);
    }
}

#[test]
fn aac_cancel_retains_original_allocation_and_credit_until_the_original_cpu_returns() {
    for bytes in [
        include_bytes!("decoder/aac-fixtures/adts-1.aac").to_vec(),
        include_bytes!("decoder/aac-fixtures/2-tail-reverse.m4a").to_vec(),
    ] {
        let host = migo_io::scheduler::IoScheduler::new(0);
        let (mut pool, results) = pool(&host, 1);
        let (started, release, observation) = hold_first_step(&mut pool);
        let capacity = bytes.capacity();
        let watch = migo_alloc_probe::DeallocationWatch::new(bytes.as_ptr()).unwrap();
        let (job, reply) = audio_buffer_decode_job(1, Arc::new(bytes));
        pool.submit(job);
        started.recv_timeout(WAIT).unwrap();
        drop(reply);
        until(|| {
            observation
                .flag
                .lock()
                .unwrap()
                .as_ref()
                .is_some_and(|f| f.load(Ordering::Acquire))
        });
        assert_eq!(
            watch.freed_bytes(),
            None,
            "AAC cancelled input freed before the original CPU returned"
        );
        assert_eq!(
            pool.in_flight.bytes.load(Ordering::Acquire),
            1,
            "AAC cancelled input refunded its credit early"
        );
        assert_eq!(pool.slots.available_permits(), 0);
        drop(release);
        until(|| {
            pool.in_flight.bytes.load(Ordering::Acquire) == 0 && watch.freed_bytes().is_some()
        });
        assert_eq!(
            watch.freed_bytes(),
            Some(capacity),
            "AAC original allocation did not actually retire"
        );
        assert_ne!(
            observation.name.lock().unwrap().as_deref(),
            Some("audio-stream")
        );
        assert!(observation.cancelled.load(Ordering::Acquire));
        assert!(results.try_recv().is_err());
    }
}

#[test]
fn aac_inner_load_uses_the_submitted_target_and_preserves_source_identity() {
    for bytes in [
        include_bytes!("decoder/aac-fixtures/adts-1.aac").to_vec(),
        include_bytes!("decoder/aac-fixtures/2-tail-reverse.m4a").to_vec(),
    ] {
        let expected = crate::decoder::decode_for_rate(&bytes, 16000).unwrap();
        let host = migo_io::scheduler::IoScheduler::new(0);
        let (mut pool, results) = pool(&host, 1);
        let shared = Arc::new(crate::inner_audio::InnerAudioSharedState::new());
        let load = shared.begin_source(37).unwrap();
        let (resp, _reply) = tokio::sync::oneshot::channel();
        pool.submit(DecodeJob::InnerAudio {
            id: 91,
            load,
            target_rate: 16000,
            data: bytes,
            resp,
        });
        let DecodeResult::InnerAudio {
            id, load, result, ..
        } = results.recv_timeout(WAIT).unwrap()
        else {
            panic!("AAC InnerAudio result lost its source kind");
        };
        assert_eq!(id, 91);
        let actual = result.unwrap();
        assert_eq!(
            (actual.sample_rate, actual.channels),
            (16000, expected.channels),
            "AAC InnerAudio submitted target changed"
        );
        assert_eq!(
            actual.samples, expected.samples,
            "AAC InnerAudio complete PCM changed"
        );
        assert_eq!(pool.in_flight.bytes.load(Ordering::Acquire), 0);
        assert!(shared.owns_load(&load));
        shared.begin_source(38).unwrap();
        assert!(
            !shared.owns_load(&load),
            "AAC result lost the submitted source generation"
        );
    }
}

fn bounded_state() -> State {
    State {
        refill: RefillWindow::new(0),
        input: Some(Input::Owned(wav(48000, 4_000_000))),
        job: None,
        resources: Some(Resources {
            waiting: None,
            in_flight: None,
            slot: None,
            _retirement: Retirement(Arc::new(Retired::default())),
        }),
        target: 48000,
        cancel: Arc::new(AtomicBool::new(false)),
        step_hook: None,
        last_turn_steps: 0,
    }
}

#[test]
fn refill_window_observes_short_requests_and_expires_at_the_boundary() {
    let start = Instant::now();
    let mut window = RefillWindow::new(17);
    assert_eq!(window.step_limit(17, start), 128);
    assert_eq!(window.step_limit(18, start), 32);
    assert_eq!(window.step_limit(18, start + REFILL_GRACE / 2), 32);
    assert_eq!(window.step_limit(18, start + REFILL_GRACE), 128);
    let next = start + REFILL_GRACE * 2;
    assert_eq!(window.step_limit(19, next), 32);
    assert_eq!(window.step_limit(19, next + REFILL_GRACE), 128);
    let mut wrapping = RefillWindow::new(usize::MAX);
    assert_eq!(wrapping.step_limit(0, start), 32);
}

#[test]
fn refill_burst_uses_short_steps_then_restores_long_turns() {
    let host = migo_io::scheduler::IoScheduler::new(0);
    let foreground = host.audio_cpu();
    let cpu = host.background_audio_cpu();
    let mut state = bounded_state();
    state.refill = RefillWindow::new(foreground.refill_generation());
    let runtime = crate::streaming::get_stream_runtime();
    assert_eq!(runtime.block_on(foreground.run(|| 17)).unwrap(), 17);
    let (_, steps) = runtime
        .block_on(OffWorker::new(state, cpu.clone()).with(move |state| {
            let start = Instant::now();
            assert!(
                state
                    .turn_with_refill_budget(&cpu, Duration::MAX, start)
                    .unwrap()
                    .is_none()
            );
            assert!((1..=32).contains(&state.last_turn_steps));
            state.refill = RefillWindow::new(0);
            let mut counts = Vec::new();
            for (generation, now) in [(0, start), (1, start), (1, start + REFILL_GRACE)] {
                let limit = state.refill.step_limit(generation, now);
                assert!(
                    state
                        .turn_with_budget(Duration::MAX, limit, || false)
                        .unwrap()
                        .is_none()
                );
                counts.push(state.last_turn_steps);
            }
            counts
        }))
        .unwrap();
    assert_eq!(steps, [128, 32, 128]);
}

#[test]
fn cpu_turn_has_a_hard_logical_step_cap_even_without_clock_pressure() {
    let host = migo_io::scheduler::IoScheduler::new(0);
    let (_, steps) = crate::streaming::get_stream_runtime()
        .block_on(
            OffWorker::new(bounded_state(), host.background_audio_cpu()).with(|state| {
                assert!(
                    state
                        .turn_with_budget(Duration::MAX, usize::MAX, || false)
                        .unwrap()
                        .is_none()
                );
                state.last_turn_steps
            }),
        )
        .unwrap();
    assert_eq!(steps, 128);
}
#[test]
fn cpu_turn_checks_elapsed_budget_after_each_bounded_library_step() {
    let host = migo_io::scheduler::IoScheduler::new(0);
    let (_, steps) = crate::streaming::get_stream_runtime()
        .block_on(
            OffWorker::new(bounded_state(), host.background_audio_cpu()).with(|state| {
                assert!(
                    state
                        .turn_with_budget(Duration::ZERO, usize::MAX, || false)
                        .unwrap()
                        .is_none()
                );
                state.last_turn_steps
            }),
        )
        .unwrap();
    assert_eq!(steps, 1);
}

#[test]
fn queued_refill_interrupts_a_turn_after_one_more_bounded_step() {
    let host = migo_io::scheduler::IoScheduler::new(0);
    let (_, steps) = crate::streaming::get_stream_runtime()
        .block_on(
            OffWorker::new(bounded_state(), host.background_audio_cpu()).with(|state| {
                let mut checks = 0;
                assert!(
                    state
                        .turn_with_budget(Duration::MAX, usize::MAX, || {
                            checks += 1;
                            checks == 2
                        })
                        .unwrap()
                        .is_none()
                );
                assert_eq!(checks, 2);
                state.last_turn_steps
            }),
        )
        .unwrap();
    assert_eq!(steps, 2);
}
#[test]
fn pre_cancelled_turn_never_constructs_the_codec() {
    let host = migo_io::scheduler::IoScheduler::new(0);
    let state = bounded_state();
    state.cancel.store(true, Ordering::Release);
    let (_, (code, input_kept, steps)) = crate::streaming::get_stream_runtime()
        .block_on(
            OffWorker::new(state, host.background_audio_cpu()).with(|state| {
                let error = state
                    .turn_with_budget(Duration::MAX, usize::MAX, || false)
                    .err()
                    .expect("pre-cancelled turn");
                (error.code, state.input.is_some(), state.last_turn_steps)
            }),
        )
        .unwrap();
    assert_eq!(code, ErrorCode::Cancelled);
    assert!(input_kept, "cancelled turn constructed a codec");
    assert_eq!(steps, 0);
}

#[test]
fn sandbox_file_service_reaches_the_actual_decode_pool_with_complete_pcm_and_identity() {
    local_file_product(false);
}

#[test]
fn package_file_service_reaches_the_actual_decode_pool_with_complete_pcm_and_identity() {
    local_file_product(true);
}

fn local_file_product(packaged: bool) {
    let root = std::env::temp_dir().join(format!(
        "migo-local-file-product-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let paths =
        shared::vfs::GamePaths::new(root.join("files"), root.join("cache"), "audio", 1).unwrap();
    paths.ensure_directories().unwrap();
    std::fs::create_dir_all(paths.code_dir()).unwrap();
    let vfs = Arc::new(shared::vfs::VirtualFS::try_from_game_paths(&paths).unwrap());
    let source = paths.user_data_dir().join("deliberately-wrong.mp3");
    let media = [
        ("wave", wav(8000, 32000)),
        (
            "vorbis",
            include_bytes!("decoder/ogg-fixtures/cross.ogg").to_vec(),
        ),
        ("mp3", crate::mp3_fixture::stream(32)),
        (
            "adts",
            include_bytes!("decoder/aac-fixtures/adts-1.aac").to_vec(),
        ),
        (
            "m4a-front",
            include_bytes!("decoder/aac-fixtures/1-front.m4a").to_vec(),
        ),
        (
            "m4a-reverse",
            include_bytes!("decoder/aac-fixtures/2-tail-reverse.m4a").to_vec(),
        ),
    ];
    for (name, bytes) in media {
        for target in [16000, 48000] {
            std::fs::write(&source, &bytes).unwrap();
            let mounts = if packaged {
                let path = root.join("clip.mpkg");
                let mut writer =
                    shared::vfs::PackageWriter::new(std::fs::File::create(&path).unwrap()).unwrap();
                writer.add_entry("deliberately-wrong.mp3", &bytes).unwrap();
                writer.finish("audio", "1").unwrap();
                let mounts = Arc::new(shared::vfs::MountTable::new(paths.code_dir().to_path_buf()));
                mounts.swap_base(Arc::new(
                    shared::vfs::PackSource::open(&path, "audio", "1").unwrap(),
                ));
                Some(mounts)
            } else {
                None
            };
            let expected = crate::decoder::decode_for_rate(&bytes, target).unwrap();
            let host = Arc::new(migo_io::scheduler::IoScheduler::new(997));
            let (mut pool, results) = pool(&host, 1);
            let (tx, rx) = shared::audio_channel::channel();
            let tx = shared::op_state::AudioSender::new(tx, ThreadWakeup::new());
            let work=migo_services::audio::inner_audio_load_url(tx,migo_services::audio::InnerAudioSources{
                code_dir:None,vfs:Some(vfs.clone()),
                local_file:Some(migo_services::audio::LocalAudioFile::session(host.clone(),&paths).with_mounts(mounts.clone())),
                admit_remote:|_:&migo_services::network::url::Url|->Result<(),migo_services::ServiceError>{panic!("local source entered remote admission")},
            },91,3701,if packaged { "deliberately-wrong.mp3" } else { "/user/deliberately-wrong.mp3" }.into()).unwrap();
            let AudioCmd::InnerAudioBeginLoad {
                id,
                source_id,
                resp,
            } = rx.try_recv().unwrap()
            else {
                panic!("local source lost synchronous begin")
            };
            let mut player = crate::inner_audio::InnerAudioPlayer::new(91, expected.channels);
            resp.send(player.begin_load_for(source_id)).unwrap();
            assert_eq!((id, source_id), (91, 3701));
            let (done, answer) = mpsc::channel();
            crate::streaming::get_stream_runtime().spawn(async move {
                done.send(work.await).unwrap();
            });
            let mut delivery = None;
            until(|| {
                delivery = rx.try_recv().ok();
                delivery.is_some()
            });
            let AudioCmd::InnerAudioCompleteLoad {
                id,
                load,
                data,
                resp,
            } = delivery.unwrap()
            else {
                panic!("local source did not preserve whole-file M4A/decode dispatch")
            };
            assert_eq!(data, bytes, "local service changed complete encoded bytes");
            assert_eq!(
                load.source_id(),
                3701,
                "local service changed source identity"
            );
            // Later host/source writes cannot retarget the accepted input.
            std::fs::write(&source, b"replacement after source publication").unwrap();
            pool.submit(DecodeJob::InnerAudio {
                id,
                load,
                target_rate: target,
                data,
                resp,
            });
            let DecodeResult::InnerAudio {
                id,
                load,
                result,
                resp,
            } = results.recv_timeout(WAIT).unwrap()
            else {
                panic!("local source lost its actual DecodePool result")
            };
            let actual = result.expect("actual pool refused sandboxed local source");
            assert_eq!(
                (id, actual.sample_rate, actual.channels),
                (91, target, expected.channels),
                "local submitted format changed"
            );
            assert!(
                actual
                    .samples
                    .iter()
                    .map(|s| s.to_bits())
                    .eq(expected.samples.iter().map(|s| s.to_bits())),
                "local actual DecodePool complete PCM changed"
            );
            if std::env::var_os(if packaged {
                "MIGO_DUMP_PACKAGE_FILE_PCM"
            } else {
                "MIGO_DUMP_LOCAL_FILE_PCM"
            })
            .is_some()
            {
                println!(
                    "{} {name}/{target} {:?}",
                    if packaged {
                        "MIGO_PACKAGE_FILE_PCM"
                    } else {
                        "MIGO_LOCAL_FILE_PCM"
                    },
                    actual
                        .samples
                        .iter()
                        .map(|s| s.to_bits())
                        .collect::<Vec<_>>()
                );
            }
            let info = InnerAudioInfo {
                duration: actual.duration(),
                sample_rate: actual.sample_rate,
                channels: actual.channels,
            };
            player.complete_load(load, actual).unwrap();
            resp.send(Ok(info)).unwrap();
            answer.recv_timeout(WAIT).unwrap().unwrap();
            assert_eq!(pool.in_flight.bytes.load(Ordering::Acquire), 0);
            assert!(
                host.metrics().delegated_runs > 0,
                "actual local product did not enter session Fs scheduling"
            );
        }
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn actual_decode_pools_share_admission_and_release_before_result_visibility() {
    let first_host = migo_io::scheduler::IoScheduler::new(0);
    let second_host = migo_io::scheduler::IoScheduler::new(0);
    let (mut first, first_results) = pool(&first_host, 1);
    let (mut second, second_results) = pool(&second_host, 1);
    second.in_flight = first.in_flight.clone();
    let (started, release, _) = hold_first_step(&mut first);
    let (job, _reply) = inner_decode_job(1);
    first.submit(job);
    started.recv_timeout(WAIT).unwrap();
    let held = second.in_flight.try_reserve().unwrap();
    let (job, _reply2) = inner_decode_job(2);
    second.submit(job);
    // The second pool's refusal is published while the first step is still held.
    let DecodeResult::InnerAudio { result, .. } = second_results.recv_timeout(WAIT).unwrap() else {
        panic!("inner");
    };
    assert_eq!(result.unwrap_err().code, ErrorCode::InputSaturated);
    drop(release);
    drop(held);
    let _ = first_results.recv_timeout(WAIT).unwrap();
    assert_eq!(first.in_flight.bytes.load(Ordering::Acquire), 0);
    assert_eq!(first.slots.available_permits(), 1);
}
