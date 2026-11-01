//! Carry the same clock through actual sources, producer receipts and player.
use super::*;
use crate::{
    indexed_seek::SeekSource,
    inner_audio::InnerAudioPlayer,
    isomp4_file::IndexedFile,
    isomp4_source_contracts::{Config, client, start_server},
    isomp4_transport::IndexedHttp,
    producer_control::{self, Reply},
    streaming::{StreamMsg, StreamRevision, StreamingState, stream_channel},
    streaming_transport::{self, HttpSource},
};
use shared::protocol::audio_cmd::InnerAudioEventType;
use std::{sync::Arc, time::Duration};
use tokio::{sync::mpsc, time::Instant};

struct Cancel(Arc<StreamingState>);
impl Drop for Cancel {
    fn drop(&mut self) {
        self.0.cancel();
    }
}
async fn collect(
    rx: &mut mpsc::Receiver<StreamMsg>,
    tx: &mpsc::Sender<StreamMsg>,
    player: &mut InnerAudioPlayer,
    revision: StreamRevision,
) -> Vec<f32> {
    let mut pcm = Vec::new();
    let mut ready = 0;
    loop {
        let message = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(message.revision(), revision);
        let done = match &message {
            StreamMsg::Ready { .. } => {
                ready += 1;
                assert_eq!(ready, 1);
                false
            }
            StreamMsg::Samples { pcm: chunk, .. } => {
                assert_eq!(ready, 1);
                pcm.extend_from_slice(chunk);
                false
            }
            StreamMsg::Done { .. } => {
                assert_eq!(ready, 1);
                true
            }
            StreamMsg::Error { message, .. } => panic!("producer error: {message}"),
        };
        assert!(tx.try_send(message).is_ok());
        player.poll_stream();
        if done {
            return pcm;
        }
    }
}
async fn actor<const B: bool, S: SeekSource<Timed<B>> + 'static>(
    owner: migo_io::scheduler::IoScheduler,
    source: S,
    data: &[u8],
    rate: u32,
) -> usize {
    let full = candidate::decode(data, 16 * 1024 * 1024).unwrap();
    let reference = resample(&full.samples, full.sample_rate, rate, full.channels);
    let (tx, mut rx) = stream_channel();
    let (player_tx, player_rx) = stream_channel();
    let (commands, controls) = producer_control::channel();
    let state = StreamingState::new_reusable();
    let _cancel = Cancel(state.clone());
    let mut player = InnerAudioPlayer::new(432, full.channels);
    player
        .start_streaming("clocked source".into(), player_rx, state.clone())
        .unwrap();
    let task = tokio::spawn(streaming_transport::run_source_controlled(
        owner.audio_cpu(),
        source,
        Some(data.len() as u64),
        tx,
        state.clone(),
        move |len| Timed::<B>::new(rate, len),
        controls,
    ));
    assert_pcm(
        &collect(&mut rx, &player_tx, &mut player, StreamRevision::INITIAL).await,
        &reference,
    );
    let duration = full.samples.len() as f64 / full.channels as f64 / full.sample_rate as f64;
    assert_eq!(player.shared.duration(), duration);
    let frames = full.samples.len() / full.channels as usize;
    let mut revision = StreamRevision::INITIAL;
    for target in [1, 211, frames - 1, 0, frames / 2] {
        player.drain_events().for_each(drop);
        let reply = producer_control::request(
            &commands,
            Timestamp::new(target as i64),
            Instant::now() + Duration::from_secs(5),
        )
        .unwrap();
        let value = match tokio::time::timeout(Duration::from_secs(5), reply)
            .await
            .unwrap()
            .unwrap()
        {
            Reply::Prepared(value) => value,
            _ => panic!("clocked seek not prepared"),
        };
        assert_eq!(value.target, target as i64);
        revision = revision.checked_next().unwrap();
        assert_eq!(value.revision, revision);
        assert!(
            rx.try_recv().is_err(),
            "producer published before clocked acceptance"
        );
        value.accept_clocked(&mut player).unwrap();
        let start = (target as u128 * rate as u128).div_ceil(full.sample_rate as u128) as usize;
        assert_eq!(player.shared.position_frames(), start as u64);
        assert_eq!(player.shared.duration(), duration);
        assert!(
            player
                .drain_events()
                .all(|e| e.event_type != InnerAudioEventType::Seeked)
        );
        assert_pcm(
            &collect(&mut rx, &player_tx, &mut player, revision).await,
            &reference[start * full.channels as usize..],
        );
        assert_eq!(
            player
                .drain_events()
                .filter(|e| e.event_type == InnerAudioEventType::Seeked)
                .count(),
            1
        );
        assert_eq!(player.shared.duration(), duration);
        assert!(!state.is_cancelled());
        assert!(!task.is_finished(), "clocked EOF retired the producer");
    }
    drop(commands);
    tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    drop(owner);
    5
}
#[tokio::test]
async fn file_http_clocked_receipts_keep_player_and_pcm_on_one_output_grid() {
    let mut count = 0;
    for layout in ["front", "tail", "scaled", "edit"] {
        let name = format!("1-seek-{layout}.m4a");
        let data = fixture(&name);
        for rate in [16000, 44100, 48000] {
            let owner = migo_io::scheduler::IoScheduler::new(0);
            let path =
                std::path::PathBuf::from(std::env::var_os("W09_M4A_FIXTURES").unwrap()).join(&name);
            let source = IndexedFile::open(std::fs::File::open(&path).unwrap(), &owner)
                .await
                .unwrap();
            count += actor::<false, _>(owner, source, &data, rate).await;
            let owner = migo_io::scheduler::IoScheduler::new(0);
            let source = IndexedFile::open(std::fs::File::open(&path).unwrap(), &owner)
                .await
                .unwrap();
            count += actor::<true, _>(owner, source, &data, rate).await;
            for buffered in [false, true] {
                let server = start_server(data.clone(), Config::default()).await;
                let client = client();
                let response = <IndexedHttp as HttpSource<Timed<false>>>::request(client.get(
                    crate::network::gate::GateKind::AudioStream,
                    server.url.clone(),
                ))
                .send()
                .await
                .unwrap();
                let source =
                    <IndexedHttp as HttpSource<Timed<false>>>::new(client, response).unwrap();
                let owner = migo_io::scheduler::IoScheduler::new(0);
                count += if buffered {
                    actor::<true, _>(owner, source, &data, rate).await
                } else {
                    actor::<false, _>(owner, source, &data, rate).await
                };
                drop(server);
            }
        }
    }
    assert_eq!(count, 240);
    println!("W09_PHASE_SOURCE {count} File/HTTP clocked receipt/PCM/player suffixes");
}
