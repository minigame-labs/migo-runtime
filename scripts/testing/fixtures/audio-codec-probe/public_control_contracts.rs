//! Exercise public seek/stop/play through actual source actors, not manual ACKs.
use crate::{
    candidate,
    decoder_seek_contracts::{assert_pcm, resample},
    indexed_seek::SeekSource,
    inner_audio::{InnerAudioPlayer, PlaybackState},
    isomp4_file::IndexedFile,
    isomp4_source_contracts::{Config, client, fixture, start_server},
    isomp4_transport::IndexedHttp,
    producer_control::Controls,
    streaming::{StreamMsg, StreamRevision, StreamingState, stream_channel},
    streaming_transport::{self, HttpSource},
    timed_m4a::Timed,
};
use shared::protocol::audio_cmd::InnerAudioEventType;
use std::{
    sync::{Arc, atomic::Ordering},
    time::Duration,
};
use tokio::{sync::mpsc, time::Instant};
struct Cancel(Arc<StreamingState>);
impl Drop for Cancel {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

async fn collect(
    rx: &mut mpsc::Receiver<StreamMsg>,
    player_tx: &mpsc::Sender<StreamMsg>,
    player: &mut InnerAudioPlayer,
    revision: StreamRevision,
) -> Vec<f32> {
    tokio::time::timeout(Duration::from_secs(5), async {
        let (mut samples, mut ready) = (Vec::new(), 0);
        loop {
            player.poll_stream(); // Also accepts a source receipt on this thread.
            if let Ok(message) = rx.try_recv() {
                assert_eq!(message.revision(), revision);
                let done = match &message {
                    StreamMsg::Ready { .. } => {
                        ready += 1;
                        assert_eq!(ready, 1);
                        false
                    }
                    StreamMsg::Samples { pcm, .. } => {
                        assert_eq!(ready, 1);
                        samples.extend_from_slice(pcm);
                        false
                    }
                    StreamMsg::Done { .. } => {
                        assert_eq!(ready, 1);
                        true
                    }
                    StreamMsg::Error { message, .. } => panic!("public source error: {message}"),
                };
                assert!(player_tx.try_send(message).is_ok());
                player.poll_stream();
                if done {
                    return samples;
                }
            } else if rx.is_closed() {
                panic!("source actor ended before completing the requested revision");
            } else {
                tokio::task::yield_now().await;
            }
        }
    })
    .await
    .unwrap()
}
async fn public_sequence<const B: bool, S: SeekSource<Timed<B>> + 'static>(
    owner: migo_io::scheduler::IoScheduler,
    source: S,
    data: &[u8],
    rate: u32,
) -> usize {
    let full = candidate::decode(data, 16 * 1024 * 1024).unwrap();
    let reference = resample(&full.samples, full.sample_rate, rate, full.channels);
    let duration = (full.samples.len() / full.channels as usize) as f64 / full.sample_rate as f64;
    let state = StreamingState::new_reusable();
    let _cancel = Cancel(state.clone());
    let controls = Controls::for_source(&state).unwrap();
    let (tx, mut rx) = stream_channel();
    let (player_tx, player_rx) = stream_channel();
    let mut player = InnerAudioPlayer::new(913, full.channels);
    player
        .start_streaming("public clocked source".into(), player_rx, state.clone())
        .unwrap();
    let task = tokio::spawn(streaming_transport::run_source_controlled(
        owner.audio_cpu(),
        source,
        None,
        tx,
        state.clone(),
        move |len| Timed::<B>::new(rate, len),
        controls,
    ));
    let mut revision = StreamRevision::INITIAL;
    assert_pcm(
        &collect(&mut rx, &player_tx, &mut player, revision).await,
        &reference,
    );
    for (index, time) in [
        0.125,
        duration - 0.5 / full.sample_rate as f64,
        0.0,
        duration / 2.0,
    ]
    .into_iter()
    .enumerate()
    {
        let play_state = [
            PlaybackState::Playing,
            PlaybackState::Paused,
            PlaybackState::Stopped,
        ][index % 3];
        player.shared.set_state(play_state);
        player.drain_events().for_each(drop);
        player.seek(time);
        assert!(
            player.needs_stream_poll(),
            "public seek never enqueued a source command"
        );
        revision = revision.checked_next().unwrap();
        let pcm = collect(&mut rx, &player_tx, &mut player, revision).await;
        let input = (time * full.sample_rate as f64) as u128;
        let output = (input * rate as u128).div_ceil(full.sample_rate as u128) as usize;
        assert_pcm(&pcm, &reference[output * full.channels as usize..]);
        assert_eq!(player.shared.position_frames(), output as u64);
        assert_eq!(player.shared.duration(), duration);
        assert_eq!(player.shared.state(), play_state);
        let events: Vec<_> = player.drain_events().map(|e| e.event_type).collect();
        assert_eq!(
            events
                .iter()
                .filter(|e| **e == InnerAudioEventType::Seeking)
                .count(),
            1
        );
        assert_eq!(
            events
                .iter()
                .filter(|e| **e == InnerAudioEventType::Seeked)
                .count(),
            1
        );
        assert!(!events.contains(&InnerAudioEventType::Error));
        assert!(!events.contains(&InnerAudioEventType::Ended));
    }
    // Public clamping reaches a legal empty endpoint, not an invented sample.
    player.seek(duration * 2.0);
    revision = revision.checked_next().unwrap();
    assert!(
        collect(&mut rx, &player_tx, &mut player, revision)
            .await
            .is_empty()
    );
    assert_eq!(
        player.shared.position_frames(),
        (reference.len() / full.channels as usize) as u64
    );
    let events: Vec<_> = player.drain_events().map(|e| e.event_type).collect();
    assert_eq!(
        events
            .iter()
            .filter(|e| **e == InnerAudioEventType::Seeked)
            .count(),
        1
    );
    assert!(!events.contains(&InnerAudioEventType::Error));
    assert!(!state.is_cancelled());
    player.stop();
    player.play();
    revision = revision.checked_next().unwrap();
    assert_pcm(
        &collect(&mut rx, &player_tx, &mut player, revision).await,
        &reference,
    );
    assert!(!player.drain_events().any(|e| matches!(
        e.event_type,
        InnerAudioEventType::Seeking
            | InnerAudioEventType::Seeked
            | InnerAudioEventType::Ended
            | InnerAudioEventType::Error
    )));
    assert_eq!(player.shared.state(), PlaybackState::Playing);
    assert!(!task.is_finished());
    state.cancel();
    tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    drop(owner);
    6
}

async fn http(data: Vec<u8>) -> (crate::isomp4_source_contracts::Server, IndexedHttp) {
    let server = start_server(data, Config::default()).await;
    let client = client();
    let response = <IndexedHttp as HttpSource<Timed<false>>>::request(client.get(
        crate::network::gate::GateKind::AudioStream,
        server.url.clone(),
    ))
    .send()
    .await
    .unwrap();
    let source = <IndexedHttp as HttpSource<Timed<false>>>::new(client, response).unwrap();
    (server, source)
}
async fn file(owner: &migo_io::scheduler::IoScheduler, name: &str) -> IndexedFile {
    let path = std::path::PathBuf::from(std::env::var_os("W09_M4A_FIXTURES").unwrap()).join(name);
    IndexedFile::open(std::fs::File::open(path).unwrap(), owner)
        .await
        .unwrap()
}
#[tokio::test]
async fn public_file_http_seek_stop_and_play_preserve_exact_pcm_and_source_clock() {
    let mut count = 0;
    for layout in ["front", "tail", "scaled", "edit"] {
        let name = format!("1-seek-{layout}.m4a");
        let data = fixture(&name);
        for rate in [16000, 48000] {
            for buffered in [false, true] {
                let owner = migo_io::scheduler::IoScheduler::new(0);
                let source = file(&owner, &name).await;
                count += if buffered {
                    public_sequence::<true, _>(owner, source, &data, rate).await
                } else {
                    public_sequence::<false, _>(owner, source, &data, rate).await
                };
                let (server, source) = http(data.clone()).await;
                let owner = migo_io::scheduler::IoScheduler::new(0);
                count += if buffered {
                    public_sequence::<true, _>(owner, source, &data, rate).await
                } else {
                    public_sequence::<false, _>(owner, source, &data, rate).await
                };
                drop(server);
            }
        }
    }
    assert_eq!(count, 192);
    println!("W09_PUBLIC_CONTROL192 public seek/restart sequences;32 File/HTTP actors");
}

async fn render_pass(player: &mut InnerAudioPlayer, expected: &[f32], channels: usize) -> u64 {
    let frames = expected.len() / channels;
    let mut rendered = 0;
    let mut ended = 0;
    let start = Instant::now();
    let mut reclaimed = 0;
    let mut output = vec![0.0; 512 * channels];
    while ended == 0 {
        assert!(
            start.elapsed() < Duration::from_secs(60),
            "public playback stalled"
        );
        player.poll_stream();
        reclaimed = reclaimed.max(player.retained_stream_start());
        let before = player.shared.position_frames();
        output.fill(0.0);
        let active = player.process(&mut output);
        let after = player.shared.position_frames();
        let count = if active {
            after.checked_sub(before).unwrap() as usize
        } else if player.shared.is_loaded() && player.shared.state() == PlaybackState::Stopped {
            frames.checked_sub(rendered).unwrap()
        } else {
            0
        };
        assert!(count <= 512);
        assert_pcm(
            &output[..count * channels],
            &expected[rendered * channels..(rendered + count) * channels],
        );
        assert!(output[count * channels..].iter().all(|s| *s == 0.0));
        rendered += count;
        for event in player.drain_events() {
            match event.event_type {
                InnerAudioEventType::Ended => ended += 1,
                InnerAudioEventType::Error => panic!("public player error at {rendered}"),
                _ => {}
            }
        }
        assert!(player.physical_pcm_bytes() < 4 * 1024 * 1024);
        if count == 0 {
            tokio::time::sleep(Duration::from_millis(1)).await;
        } else {
            tokio::task::yield_now().await;
        }
    }
    assert_eq!((rendered, ended), (frames, 1));
    assert!(player.is_stream_complete());
    reclaimed
}
async fn real_playback<S: SeekSource<Timed<true>> + 'static>(
    owner: migo_io::scheduler::IoScheduler,
    source: S,
    data: &[u8],
    backpressure: bool,
) {
    let full = candidate::decode(data, 16 * 1024 * 1024).unwrap();
    let rate = if backpressure { 48000 } else { 96000 };
    let expected = resample(&full.samples, full.sample_rate, rate, full.channels);
    let state = StreamingState::new_reusable();
    let _cancel = Cancel(state.clone());
    let controls = Controls::for_source(&state).unwrap();
    let (tx, rx) = stream_channel();
    let mut player = InnerAudioPlayer::new(914, full.channels);
    player.shared.set_autoplay(true);
    player
        .start_streaming("public playback".into(), rx, state.clone())
        .unwrap();
    let task = tokio::spawn(streaming_transport::run_source_controlled(
        owner.audio_cpu(),
        source,
        None,
        tx,
        state.clone(),
        move |len| Timed::<true>::new(rate, len),
        controls,
    ));
    if backpressure {
        tokio::time::timeout(Duration::from_secs(10), async {
            while !player.shared.is_loaded() {
                player.poll_stream();
                tokio::task::yield_now().await;
            }
            player.pause();
            while player.queued_stream_messages() != crate::streaming::STREAM_CHANNEL_CAPACITY
                || !state.publication_waiting.load(Ordering::Acquire)
            {
                player.poll_stream();
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        player.seek(0.125);
        player.stop();
        player.play();
        render_pass(&mut player, &expected, full.channels as usize).await;
    } else {
        assert!(
            render_pass(&mut player, &expected, full.channels as usize).await > 0,
            "fixture never reclaimed a real PCM prefix"
        );
        assert!(!task.is_finished());
        player.play();
        assert_eq!(player.shared.position_frames(), 0);
        render_pass(&mut player, &expected, full.channels as usize).await;
    }
    state.cancel();
    tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    drop(owner);
}
#[tokio::test]
async fn real_file_http_public_play_restarts_after_actual_prefix_reclamation() {
    let name = "2-seek-front.m4a";
    let data = fixture(name);
    let owner = migo_io::scheduler::IoScheduler::new(0);
    let source = file(&owner, name).await;
    real_playback(owner, source, &data, false).await;
    let (server, source) = http(data.clone()).await;
    real_playback(
        migo_io::scheduler::IoScheduler::new(0),
        source,
        &data,
        false,
    )
    .await;
    drop(server);
    println!("W09_PUBLIC_REPLAY2 reclaimed File/HTTP sources;4 complete player renders");
}
#[tokio::test]
async fn public_stop_orders_after_seek_while_real_file_http_publication_is_blocked() {
    let name = "2-seek-front.m4a";
    let data = fixture(name);
    let owner = migo_io::scheduler::IoScheduler::new(0);
    let source = file(&owner, name).await;
    real_playback(owner, source, &data, true).await;
    let (server, source) = http(data.clone()).await;
    real_playback(migo_io::scheduler::IoScheduler::new(0), source, &data, true).await;
    drop(server);
    println!("W09_PUBLIC_BACKPRESSURE2 real full-queue seek/stop/play sequences");
}
