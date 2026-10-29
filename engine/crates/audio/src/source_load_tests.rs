//! Source switches used to be identified only by the numeric player ID.
//! Exercise the actual preparation, worker admission, publication and install
//! boundaries, including results published before a newer command is drained.
use super::*;
use crate::inner_audio::InnerAudioLoadToken;
use crate::streaming::{StreamMsg, StreamingState};

fn pcm(value: f32) -> DecodedAudio {
    DecodedAudio {
        samples: vec![value; 32],
        sample_rate: 48_000,
        channels: 1,
    }
}

fn response() -> (
    AudioResp<InnerAudioInfo>,
    tokio::sync::oneshot::Receiver<EngineResult<InnerAudioInfo>>,
) {
    tokio::sync::oneshot::channel()
}

fn install(
    players: &mut HashMap<InnerAudioId, InnerAudioPlayer>,
    load: InnerAudioLoadToken,
    result: EngineResult<DecodedAudio>,
) -> EngineResult<InnerAudioInfo> {
    let (resp, mut rx) = response();
    integrate_inner_audio_decode_result(players, 1, load, result, resp);
    rx.try_recv().unwrap()
}

fn audible(player: &mut InnerAudioPlayer, value: f32) {
    player.play();
    let mut output = [0.0; 2];
    assert!(player.process(&mut output));
    assert_eq!(output, [value; 2]);
}

#[test]
fn source_switch_retires_old_pcm_stream_seek_and_events() {
    let mut player = InnerAudioPlayer::new(1, 1);
    player.shared.set_autoplay(true);
    let state = StreamingState::new();
    let (tx, rx) = tokio::sync::mpsc::channel(4);
    player
        .start_streaming("old".into(), rx, state.clone())
        .unwrap();
    assert!(
        tx.try_send(StreamMsg::Ready {
            sample_rate: 8000,
            channels: 1
        })
        .is_ok()
    );
    let mut pool = crate::streaming::PcmPool::new();
    let mut samples = pool.take();
    samples.buffer_mut().extend_from_slice(&[0.75; 4000]);
    assert!(tx.try_send(StreamMsg::Samples(samples)).is_ok());
    player.poll_stream();
    assert!(player.is_active());
    player.seek(0.001);
    assert_eq!(player.shared.position_frames(), 8);
    let load = player.begin_load().unwrap();
    assert!(state.is_cancelled());
    assert!(tx.is_closed());
    assert_eq!(player.physical_pcm_bytes(), 0);
    assert!(!player.is_streaming());
    assert!(!player.shared.is_loaded());
    assert_eq!(player.shared.state(), PlaybackState::Stopped);
    assert_eq!(player.shared.position_frames(), 0);
    assert_eq!(player.shared.duration_frames(), 0);
    assert_eq!(player.shared.sample_rate(), 0);
    assert_eq!(player.shared.channels(), 0);
    assert!(player.loading_url().is_none());
    assert_eq!(player.drain_events().count(), 0);
    let mut other = [0.5; 8];
    assert!(!player.process(&mut other));
    assert_eq!(other, [0.5; 8]);
    player.complete_load(load, pcm(0.25)).unwrap();
    audible(&mut player, 0.25);
}

#[test]
fn out_of_order_owned_results_cannot_replace_the_new_source() {
    let mut player = InnerAudioPlayer::new(1, 1);
    let old = player.begin_load().unwrap();
    let new = player.begin_load().unwrap();
    let mut players = HashMap::from([(1, player)]);
    install(&mut players, new, Ok(pcm(0.25))).unwrap();
    players.get_mut(&1).unwrap().drain_events().for_each(drop);
    assert_eq!(
        install(&mut players, old, Ok(pcm(0.75))).unwrap_err().code,
        ErrorCode::Cancelled
    );
    let player = players.get_mut(&1).unwrap();
    assert_eq!(player.drain_events().count(), 0);
    audible(player, 0.25);
}

#[test]
fn stale_errors_are_cancelled_without_changing_the_new_source() {
    let mut player = InnerAudioPlayer::new(1, 1);
    let old = player.begin_load().unwrap();
    let new = player.begin_load().unwrap();
    let mut players = HashMap::from([(1, player)]);
    assert_eq!(
        install(
            &mut players,
            old,
            Err(EngineError::new(ErrorCode::InvalidArgument))
        )
        .unwrap_err()
        .code,
        ErrorCode::Cancelled
    );
    install(&mut players, new, Ok(pcm(0.25))).unwrap();
    audible(players.get_mut(&1).unwrap(), 0.25);
}

#[test]
fn equal_generations_from_different_live_players_do_not_match() {
    let mut first = InnerAudioPlayer::new(1, 1);
    let wrong = first.begin_load().unwrap();
    let mut replacement = InnerAudioPlayer::new(1, 1);
    let current = replacement.begin_load().unwrap();
    assert!(wrong.is_current());
    let mut players = HashMap::from([(1, replacement)]);
    assert_eq!(
        install(&mut players, wrong, Ok(pcm(0.75)))
            .unwrap_err()
            .code,
        ErrorCode::Cancelled
    );
    install(&mut players, current, Ok(pcm(0.25))).unwrap();
    audible(players.get_mut(&1).unwrap(), 0.25);
}

#[test]
fn destruction_invalidates_tokens_even_if_atomic_state_is_retained() {
    let mut player = InnerAudioPlayer::new(1, 1);
    let load = player.begin_load().unwrap();
    let retained = player.shared.clone();
    assert!(load.is_current());
    drop(player);
    assert!(!load.is_current());
    assert_eq!(retained.state(), PlaybackState::Stopped);
    let mut replacement = InnerAudioPlayer::new(1, 1);
    let current = replacement.begin_load().unwrap();
    let mut players = HashMap::from([(1, replacement)]);
    assert_eq!(
        install(&mut players, load, Ok(pcm(0.75))).unwrap_err().code,
        ErrorCode::Cancelled
    );
    install(&mut players, current, Ok(pcm(0.25))).unwrap();
}

#[test]
fn superseded_queued_jobs_are_cancelled_before_peak_admission() {
    let mut player = InnerAudioPlayer::new(1, 1);
    let old = player.begin_load().unwrap();
    let _new = player.begin_load().unwrap();
    let (resp, mut rx) = response();
    let (jobs, input) = decode_queue(1, 1024);
    jobs.try_send(DecodeJob::InnerAudio {
        id: 1,
        load: old,
        data: super::tests::wav(48_000, 32),
        resp,
    })
    .unwrap();
    drop(jobs);
    let (results, output) = decode_queue(1, 1024);
    let budget = Arc::new(DecodeInFlightUsage::new(0, 1));
    decode_worker(
        Arc::new(std::sync::Mutex::new(input)),
        results,
        budget.clone(),
        48_000,
        ThreadWakeup::new(),
    );
    assert_eq!(
        rx.try_recv().unwrap().unwrap_err().code,
        ErrorCode::Cancelled
    );
    assert!(output.try_recv().is_err());
    assert_eq!(budget.bytes.load(std::sync::atomic::Ordering::Acquire), 0);
}

#[test]
fn abandoned_replies_do_not_start_a_decode_pool() {
    let mut player = InnerAudioPlayer::new(1, 1);
    let load = player.begin_load().unwrap();
    let (resp, rx) = response();
    drop(rx);
    let (results, _output) = decode_queue(1, 1024);
    let mut pool = LazyDecodePool::new(results, 48_000, ThreadWakeup::new());
    pool.submit(DecodeJob::InnerAudio {
        id: 1,
        load,
        data: vec![0],
        resp,
    });
    assert!(!pool.is_started());
    let (resp, rx) = tokio::sync::oneshot::channel();
    drop(rx);
    pool.submit(DecodeJob::AudioBuffer {
        ctx_id: 1,
        target_rate: 48_000,
        data: Arc::new(vec![0]),
        resp,
    });
    assert!(!pool.is_started());
}

#[test]
fn superseded_decoded_pcm_never_enters_the_result_queue() {
    let mut player = InnerAudioPlayer::new(1, 1);
    let old = player.begin_load().unwrap();
    let _new = player.begin_load().unwrap();
    let (resp, mut rx) = response();
    let (results, output) = decode_queue(1, 1024);
    publish_decode_result(
        &results,
        DecodeResult::InnerAudio {
            id: 1,
            load: old,
            result: Ok(pcm(0.75)),
            resp,
        },
    );
    assert_eq!(
        rx.try_recv().unwrap().unwrap_err().code,
        ErrorCode::Cancelled
    );
    assert!(output.try_recv().is_err());
    assert_eq!(
        results
            .usage
            .bytes
            .load(std::sync::atomic::Ordering::Acquire),
        0
    );
}

#[test]
fn a_source_switch_after_publication_still_rejects_the_old_result() {
    let mut player = InnerAudioPlayer::new(1, 1);
    let old = player.begin_load().unwrap();
    let (resp, mut rx) = response();
    let (results, output) = decode_queue(1, 1024);
    publish_decode_result(
        &results,
        DecodeResult::InnerAudio {
            id: 1,
            load: old,
            result: Ok(pcm(0.75)),
            resp,
        },
    );
    assert_eq!(
        results
            .usage
            .bytes
            .load(std::sync::atomic::Ordering::Acquire),
        128
    );
    let new = player.begin_load().unwrap();
    let mut players = HashMap::from([(1, player)]);
    match output.try_recv().unwrap() {
        DecodeResult::InnerAudio {
            id,
            load,
            result,
            resp,
        } => integrate_inner_audio_decode_result(&mut players, id, load, result, resp),
        _ => panic!("unexpected result type"),
    }
    assert_eq!(
        rx.try_recv().unwrap().unwrap_err().code,
        ErrorCode::Cancelled
    );
    assert_eq!(
        results
            .usage
            .bytes
            .load(std::sync::atomic::Ordering::Acquire),
        0
    );
    install(&mut players, new, Ok(pcm(0.25))).unwrap();
}

#[test]
fn a_current_decode_failure_leaves_the_old_source_retired() {
    let mut player = InnerAudioPlayer::new(1, 1);
    player.load_audio(pcm(0.75)).unwrap();
    player.play();
    let load = player.begin_load().unwrap();
    let mut players = HashMap::from([(1, player)]);
    assert_eq!(
        install(
            &mut players,
            load,
            Err(EngineError::new(ErrorCode::InvalidArgument))
        )
        .unwrap_err()
        .code,
        ErrorCode::InvalidArgument
    );
    let player = players.get_mut(&1).unwrap();
    assert!(!player.is_active());
    assert!(!player.shared.is_loaded());
    assert_eq!(player.physical_pcm_bytes(), 0);
    assert_eq!(player.drain_events().count(), 0);
}

#[test]
fn cached_and_streaming_loads_supersede_owned_decode_results() {
    for cached in [false, true] {
        let mut player = InnerAudioPlayer::new(1, 1);
        let old = player.begin_load().unwrap();
        if cached {
            let mut cache = crate::cache::AudioCache::with_max_size_and_aggregate(
                1024,
                AudioAggregateLedger::process_global(),
            );
            let audio = cache.insert("new".into(), pcm(0.25)).unwrap();
            let load = player.begin_load().unwrap();
            player.complete_cached_load(load, audio).unwrap();
        } else {
            let (_tx, rx) = tokio::sync::mpsc::channel(1);
            player
                .start_streaming("new".into(), rx, StreamingState::new())
                .unwrap();
        }
        let mut players = HashMap::from([(1, player)]);
        assert_eq!(
            install(&mut players, old, Ok(pcm(0.75))).unwrap_err().code,
            ErrorCode::Cancelled
        );
        if cached {
            audible(players.get_mut(&1).unwrap(), 0.25);
        } else {
            assert_eq!(players[&1].loading_url(), Some("new"));
        }
    }
}

#[test]
fn source_replacement_returns_old_pcm_credit_before_new_admission() {
    let ledger = Arc::new(AudioAggregateLedger::new(128));
    let mut player = InnerAudioPlayer::new_with_aggregate(1, 1, ledger);
    player.load_audio(pcm(0.75)).unwrap();
    player.load_audio(pcm(0.25)).unwrap();
    audible(&mut player, 0.25);
    let error = player
        .load_audio(DecodedAudio {
            samples: vec![0.0; 64],
            sample_rate: 48_000,
            channels: 1,
        })
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::InputSaturated);
    assert!(!player.shared.is_loaded());
    assert_eq!(player.physical_pcm_bytes(), 0);
}

#[test]
fn rejected_stream_preparation_cancels_its_incoming_producer() {
    let mut player =
        InnerAudioPlayer::new_with_aggregate(1, 1, Arc::new(AudioAggregateLedger::new(128)));
    let (tx, rx) = tokio::sync::mpsc::channel(1);
    let state = StreamingState::new();
    assert!(
        player
            .start_streaming("large".into(), rx, state.clone())
            .is_err()
    );
    assert!(state.is_cancelled());
    assert!(tx.is_closed());
    assert!(!player.is_streaming());
    assert!(!player.shared.is_loaded());
}

#[test]
fn stale_preparation_tokens_are_rejected_for_all_backings() {
    for backing in 0..3 {
        let mut player = InnerAudioPlayer::new(1, 1);
        let old = player.begin_load().unwrap();
        let current = player.begin_load().unwrap();
        let result = match backing {
            0 => player.complete_load(old, pcm(0.75)),
            1 => {
                let mut cache = crate::cache::AudioCache::with_max_size_and_aggregate(
                    1024,
                    AudioAggregateLedger::process_global(),
                );
                let cached = cache.insert("stale".into(), pcm(0.75)).unwrap();
                player.complete_cached_load(old, cached)
            }
            _ => {
                let (tx, rx) = tokio::sync::mpsc::channel(1);
                let state = StreamingState::new();
                let result = player.complete_stream_load(old, "stale".into(), rx, state.clone());
                assert!(state.is_cancelled());
                assert!(tx.is_closed());
                result
            }
        };
        assert_eq!(result.unwrap_err().code, ErrorCode::Cancelled);
        assert!(!player.shared.is_loaded());
        assert!(!player.is_streaming());
        player.complete_load(current, pcm(0.25)).unwrap();
        audible(&mut player, 0.25);
    }
}

#[test]
fn source_epoch_changes_reuse_the_existing_atomic_allocation() {
    use migo_alloc_probe::{Burst, assert_no_steady_state_allocation};
    let mut player = InnerAudioPlayer::new(1, 1);
    assert_no_steady_state_allocation(
        Burst {
            path: "InnerAudio source epoch change",
            warmup: 4,
            measured: 128,
        },
        |_| {
            let load = player.begin_load().unwrap();
            assert!(load.is_current());
            std::hint::black_box(load);
        },
    );
}

#[test]
fn preparation_observers_wake_on_native_replacement_and_destruction() {
    use std::{
        future::Future,
        sync::atomic::{AtomicUsize, Ordering},
        task::{Context, Wake, Waker},
    };
    struct Wakes(AtomicUsize);
    impl Wake for Wakes {
        fn wake(self: Arc<Self>) {
            self.0.fetch_add(1, Ordering::AcqRel);
        }
    }
    let mut player = InnerAudioPlayer::new(1, 1);
    let first = player.begin_load_for(501).unwrap();
    assert_eq!(first.source_id(), 501);
    let watch = first.watch();
    let wakes = Arc::new(Wakes(AtomicUsize::new(0)));
    let waker = Waker::from(wakes.clone());
    let mut waiting = std::pin::pin!(watch.while_current(std::future::pending::<()>()));
    assert!(
        waiting
            .as_mut()
            .poll(&mut Context::from_waker(&waker))
            .is_pending()
    );
    let next = player.begin_load_for(502).unwrap();
    assert_eq!(wakes.0.load(Ordering::Acquire), 1);
    assert!(matches!(
        waiting.as_mut().poll(&mut Context::from_waker(&waker)),
        std::task::Poll::Ready(Err(_))
    ));
    let watch = next.watch();
    let mut waiting = std::pin::pin!(watch.while_current(std::future::pending::<()>()));
    assert!(
        waiting
            .as_mut()
            .poll(&mut Context::from_waker(&waker))
            .is_pending()
    );
    let _retained = player.shared.clone();
    drop(player);
    assert_eq!(wakes.0.load(Ordering::Acquire), 2);
    assert!(matches!(
        waiting.as_mut().poll(&mut Context::from_waker(&waker)),
        std::task::Poll::Ready(Err(_))
    ));
}

#[test]
fn native_events_capture_source_identity_and_playback_state_at_emission() {
    let mut player = InnerAudioPlayer::new(1, 1);
    let load = player.begin_load_for(501).unwrap();
    player.complete_load(load, pcm(0.25)).unwrap();
    player.play();
    player.process(&mut [0.0; 8]);
    player.pause();
    let events: Vec<_> = player.drain_events().collect();
    assert_eq!(events.len(), 3);
    assert!(events.iter().all(|event| event.source_id == 501
        && event.buffered
        && event.duration == 32.0 / 48_000.0));
    assert!(events[0].paused);
    assert!(!events[1].paused);
    assert!(events[2].paused);
    assert_eq!(events[2].current_time, 8.0 / 48_000.0);
    let load = player.begin_load_for(502).unwrap();
    player.complete_load(load, pcm(0.75)).unwrap();
    assert!(player.drain_events().all(|event| event.source_id == 502));
    assert!(events.iter().all(|event| event.source_id == 501));
}

#[test]
fn prepared_local_decode_uses_its_original_token_through_the_actual_worker() {
    let mut player = InnerAudioPlayer::new(1, 1);
    let load = player.begin_load_for(501).unwrap();
    let watch = load.watch();
    let mut players = HashMap::from([(1, player)]);
    let (tx, rx) = decode_queue(1, 1024);
    let wake = ThreadWakeup::new();
    let actual_pool = DecodePool::new_with_in_flight(
        1,
        tx.clone(),
        Arc::new(DecodeInFlightUsage::new(1, 1)),
        48_000,
        wake.clone(),
    );
    let mut pool = LazyDecodePool::new(tx, 48_000, wake);
    pool.pool = Some(actual_pool);
    let (resp, mut response) = response();
    submit_prepared_inner_audio_load(
        &players,
        &mut pool,
        1,
        load,
        super::tests::wav(48_000, 32),
        resp,
    );
    match rx.recv_timeout(Duration::from_secs(1)).unwrap() {
        DecodeResult::InnerAudio {
            id,
            load,
            result,
            resp,
        } => integrate_inner_audio_decode_result(&mut players, id, load, result, resp),
        _ => panic!("prepared local decode result"),
    }
    assert_eq!(response.try_recv().unwrap().unwrap().sample_rate, 48_000);
    assert!(
        watch.is_current(),
        "completion must not start another generation"
    );
    assert!(
        players
            .get_mut(&1)
            .unwrap()
            .drain_events()
            .all(|event| event.source_id == 501)
    );
}

#[test]
fn older_source_tags_cannot_retire_a_newer_preparation_or_loaded_source() {
    let mut player = InnerAudioPlayer::new(1, 1);
    let current = player.begin_load_for(702).unwrap();
    let watch = current.watch();
    assert_eq!(
        player.begin_load_for(701).err().unwrap().code,
        ErrorCode::Cancelled
    );
    assert!(watch.is_current());
    player.complete_load(current, pcm(0.25)).unwrap();
    player.play();
    assert_eq!(
        player.begin_load_for(702).err().unwrap().code,
        ErrorCode::Cancelled
    );
    assert!(watch.is_current());
    audible(&mut player, 0.25);
    // Legacy Rust callers use tag 0 without erasing the tagged request history.
    let _legacy = player.begin_load().unwrap();
    assert_eq!(
        player.begin_load_for(701).err().unwrap().code,
        ErrorCode::Cancelled
    );
    assert!(player.begin_load_for(703).is_ok());
}
