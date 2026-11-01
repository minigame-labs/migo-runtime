//! Public setter -> source-bound ACK -> actual File/HTTP cyclic PCM.
use super::*;
use crate::{
    candidate,
    decoder_seek_contracts::{assert_pcm, resample},
    indexed_seek::SeekSource,
    inner_audio::{InnerAudioPlayer, PlaybackState},
    isomp4_file::IndexedFile,
    isomp4_source_contracts::{Config, client, fixture, start_server},
    isomp4_transport::IndexedHttp,
    producer_control::Controls,
    streaming::{StreamPresentation, StreamingState, stream_channel},
    streaming_transport::{self, HttpSource, Progress},
};
use shared::{error::EngineResult, protocol::audio_cmd::InnerAudioEventType};
use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use symphonia::core::units::Timestamp;

struct Observed<const B: bool> {
    inner: Timed<B>,
    cutovers: Arc<AtomicUsize>,
    bad_metadata: Option<Arc<AtomicUsize>>,
}
impl<const B: bool> Decoder for Observed<B> {
    fn timeline(&self) -> EngineResult<Option<crate::streaming::StreamTimeline>> {
        self.inner.timeline()
    }
    fn presentation(&self) -> EngineResult<Option<StreamPresentation>> {
        if self.bad_metadata.is_some() && self.cutovers.load(Ordering::Acquire) > 0 {
            return self
                .inner
                .presentation()
                .map(|p| p.map(|p| StreamPresentation::single(p.timeline())));
        }
        self.inner.presentation()
    }
    fn step(
        &mut self,
        input: &[u8],
        eof: bool,
        out: &mut Vec<f32>,
        cancel: &AtomicBool,
    ) -> EngineResult<Step> {
        let result = self.inner.step(input, eof, out, cancel);
        if self.cutovers.load(Ordering::Acquire) > 0
            && let Some(steps) = &self.bad_metadata
        {
            steps.fetch_add(1, Ordering::Release);
        }
        result
    }
}
impl<const B: bool> M4aSourceDecoder for Observed<B> {
    fn source_request(&self) -> Option<M4aSourceRequest> {
        self.inner.source_request()
    }
    fn install_index_with_length(
        &mut self,
        index: AudioIndex,
        len: Option<u64>,
    ) -> Result<(), String> {
        self.inner.install_index_with_length(index, len)
    }
    fn resume_at(&mut self, span: AtomSpan, offset: u64) -> Result<(), String> {
        self.inner.resume_at(span, offset)
    }
}
impl<const B: bool> M4aSeekDecoder for Observed<B> {
    fn seek_loop(
        &mut self,
        target: Timestamp,
        enabled: bool,
    ) -> symphonia::core::errors::Result<SeekedTo> {
        let result = M4aSeekDecoder::seek_loop(&mut self.inner, target, enabled);
        if result.is_ok() {
            self.cutovers.fetch_add(1, Ordering::Release);
        }
        result
    }
    fn seek(&mut self, target: Timestamp) -> symphonia::core::errors::Result<SeekedTo> {
        self.inner.seek(target)
    }
    fn cutover(&mut self, at: u64, enabled: bool) -> symphonia::core::errors::Result<SeekedTo> {
        let result = M4aSeekDecoder::cutover(&mut self.inner, at, enabled);
        if result.is_ok() {
            self.cutovers.fetch_add(1, Ordering::Release);
        }
        result
    }
}

#[path = "cyclic_seek_contracts.rs"]
mod cyclic_seek;
#[path = "loading_controls_contracts.rs"]
mod loading_controls;
struct Cancel(Arc<StreamingState>);
impl Drop for Cancel {
    fn drop(&mut self) {
        self.0.cancel();
    }
}
fn no_errors(p: &mut InnerAudioPlayer) -> usize {
    let mut ended = 0;
    for e in p.drain_events() {
        assert_ne!(
            e.event_type,
            InnerAudioEventType::Error,
            "live loop source error"
        );
        assert!(!matches!(
            e.event_type,
            InnerAudioEventType::Seeking | InnerAudioEventType::Seeked
        ));
        if e.event_type == InnerAudioEventType::Ended {
            ended += 1;
            assert_eq!(e.current_time, e.duration);
        }
    }
    ended
}
async fn wait(p: &mut InnerAudioPlayer, condition: impl Fn(&InnerAudioPlayer) -> bool) {
    tokio::time::timeout(Duration::from_secs(20), async {
        while !condition(p) {
            p.poll_stream();
            assert_eq!(no_errors(p), 0);
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap();
}
async fn render(p: &mut InnerAudioPlayer, frames: usize, channels: u32, ending: bool) -> Vec<f32> {
    tokio::time::timeout(Duration::from_secs(20), async {
        let mut got = Vec::new();
        let mut ended = 0;
        let mut out = vec![0.0; 137 * channels as usize];
        while got.len() < frames * channels as usize || ending && ended == 0 {
            p.poll_stream();
            let before = p.shared.position_frames();
            let n = (frames - got.len() / channels as usize).min(137);
            out.fill(0.0);
            let active = p.process(&mut out[..n * channels as usize]);
            let after = p.shared.position_frames();
            let newly_ended = no_errors(p);
            let count = if newly_ended != 0 {
                assert!(ending && !active && p.shared.state() == PlaybackState::Stopped);
                assert_eq!(n, frames - got.len() / channels as usize);
                n
            } else if after >= before {
                (after - before) as usize
            } else {
                panic!("position moved backward")
            };
            assert!(count <= n);
            got.extend_from_slice(&out[..count * channels as usize]);
            assert!(out[count * channels as usize..].iter().all(|x| *x == 0.0));
            ended += newly_ended;
            assert!(ended <= usize::from(ending));
            if count == 0 {
                tokio::time::sleep(Duration::from_millis(1)).await;
            } else {
                tokio::task::yield_now().await;
            }
        }
        assert_eq!(ended, usize::from(ending));
        got
    })
    .await
    .unwrap()
}
async fn live<const B: bool, S: SeekSource<Observed<B>> + 'static>(
    owner: migo_io::scheduler::IoScheduler,
    source: S,
    data: &[u8],
    rate: u32,
    rapid: bool,
    loading: bool,
) {
    let full = candidate::decode(data, 16_777_216).unwrap();
    let n = full.samples.len() / full.channels as usize;
    let single = (n as u128 * rate as u128).div_ceil(full.sample_rate as u128) as usize;
    let state = StreamingState::new_reusable();
    let _cancel = Cancel(state.clone());
    let controls = Controls::for_source(&state).unwrap();
    let (tx, rx) = stream_channel();
    let trace = Arc::new(AtomicUsize::new(0));
    let counting = trace.clone();
    let task = tokio::spawn(streaming_transport::run_source_controlled(
        owner.audio_cpu(),
        source,
        None,
        tx,
        state.clone(),
        move |len| Observed {
            inner: Timed::<B>::new(rate, len),
            cutovers: counting,
            bad_metadata: None,
        },
        controls,
    ));
    let mut p = InnerAudioPlayer::new(940, full.channels);
    p.start_streaming("public live loop".into(), rx, state.clone())
        .unwrap();
    if loading {
        p.set_loop_enabled(true);
        wait(&mut p, |p| {
            p.shared.loop_enabled() && trace.load(Ordering::Acquire) >= 1
        })
        .await;
    } else {
        wait(&mut p, |p| p.shared.is_loaded() && p.is_stream_complete()).await;
    }
    assert_eq!(p.shared.state(), PlaybackState::Stopped);
    p.play();
    let start = if loading {
        0
    } else {
        single.min(137).saturating_sub(1).max(1)
    };
    let reference = resample(&full.samples, full.sample_rate, rate, full.channels);
    if !loading {
        assert_pcm(
            &render(&mut p, start, full.channels, false).await,
            &reference[..start * full.channels as usize],
        );
    }
    if rapid {
        p.set_loop_enabled(true);
        p.set_loop_enabled(false);
        p.stop();
        p.play();
        let got = render(&mut p, single, full.channels, true).await;
        assert_pcm(&got, &reference);
        assert!(!p.shared.loop_enabled());
        assert_eq!(trace.load(Ordering::Acquire), 3);
    } else {
        if !loading {
            p.set_loop_enabled(true);
            let at = p.shared.position_frames();
            let mut bus = [0.5; 8];
            p.process(&mut bus);
            assert_eq!(bus, [0.5; 8]);
            assert_eq!(p.shared.position_frames(), at);
            tokio::time::timeout(Duration::from_secs(10), async {
                while trace.load(Ordering::Acquire) == 0 {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            assert_eq!(
                p.queued_stream_messages(),
                0,
                "new revision published before audio-thread ACK"
            );
            wait(&mut p, |p| p.shared.loop_enabled()).await;
        }
        assert_eq!(p.shared.position_frames(), start as u64);
        let count = 2 * single + 137;
        let cycles = ((start + count) as u128 * full.sample_rate as u128
            / (rate as u128 * n as u128)) as usize
            + 64;
        let reference = resample(
            &full.samples.repeat(cycles),
            full.sample_rate,
            rate,
            full.channels,
        );
        let got = render(&mut p, count, full.channels, false).await;
        assert_pcm(
            &got,
            &reference[start * full.channels as usize..(start + count) * full.channels as usize],
        );
        let at = p.shared.position_frames();
        assert_eq!(at, (start + count) as u64);
        assert_eq!(p.shared.duration(), n as f64 / full.sample_rate as f64);
        assert_eq!(
            state.presentation().unwrap().timeline(),
            state.timeline().unwrap()
        );
        assert!(!state.presentation().unwrap().is_cyclic());
        p.set_loop_enabled(false);
        wait(&mut p, |p| !p.shared.loop_enabled()).await;
        assert_eq!(p.shared.position_frames(), at);
        let cycles =
            (at as u128 * full.sample_rate as u128 / (rate as u128 * n as u128)) as usize + 1;
        let finite = resample(
            &full.samples.repeat(cycles),
            full.sample_rate,
            rate,
            full.channels,
        );
        let remaining = finite.len() / full.channels as usize - at as usize;
        assert_pcm(
            &render(&mut p, remaining, full.channels, true).await,
            &finite[at as usize * full.channels as usize..],
        );
        assert_eq!(p.shared.current_time(), 0.0);
        assert_eq!(trace.load(Ordering::Acquire), 2);
    }
    assert!(state.download_complete.load(Ordering::Acquire));
    state.cancel();
    let _ = tokio::time::timeout(Duration::from_secs(3), task)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn file_http_public_loop_modes_keep_exact_pcm_clock_and_current_cycle_end() {
    let mut count = 0;
    for name in ["0-seek-front.m4a", "1-seek-front.m4a", "1-seek-tail.m4a"] {
        let data = fixture(name);
        let path =
            std::path::PathBuf::from(std::env::var_os("W09_M4A_FIXTURES").unwrap()).join(name);
        for rate in [16000, 44100, 48000] {
            for buffered in [false, true] {
                if name.starts_with('0') && !buffered {
                    continue;
                }
                let owner = migo_io::scheduler::IoScheduler::new(0);
                let source = IndexedFile::open(std::fs::File::open(&path).unwrap(), &owner)
                    .await
                    .unwrap();
                if buffered {
                    live::<true, _>(owner, source, &data, rate, false, false).await;
                } else {
                    live::<false, _>(owner, source, &data, rate, false, false).await;
                }
                let server = start_server(data.clone(), Config::default()).await;
                let client = client();
                let response = <IndexedHttp as HttpSource<Observed<false>>>::request(client.get(
                    crate::network::gate::GateKind::AudioStream,
                    server.url.clone(),
                ))
                .send()
                .await
                .unwrap();
                let source =
                    <IndexedHttp as HttpSource<Observed<false>>>::new(client, response).unwrap();
                let owner = migo_io::scheduler::IoScheduler::new(0);
                if buffered {
                    live::<true, _>(owner, source, &data, rate, false, false).await;
                } else {
                    live::<false, _>(owner, source, &data, rate, false, false).await;
                }
                assert_eq!(server.full_requests.load(Ordering::Acquire), 1);
                count += 2;
            }
        }
    }
    assert_eq!(count, 30);
    println!("W09_LIVE_LOOP30 actualFileHTTP public on/off exactPCM/currentcycle");
}
#[tokio::test]
async fn queued_loop_on_off_stop_play_cannot_restore_old_pcm_or_position() {
    let name = "1-seek-front.m4a";
    let data = fixture(name);
    let path = std::path::PathBuf::from(std::env::var_os("W09_M4A_FIXTURES").unwrap()).join(name);
    let owner = migo_io::scheduler::IoScheduler::new(0);
    let source = IndexedFile::open(std::fs::File::open(path).unwrap(), &owner)
        .await
        .unwrap();
    live::<true, _>(owner, source, &data, 44100, true, false).await;
    let server = start_server(data.clone(), Config::default()).await;
    let client = client();
    let response = <IndexedHttp as HttpSource<Observed<true>>>::request(client.get(
        crate::network::gate::GateKind::AudioStream,
        server.url.clone(),
    ))
    .send()
    .await
    .unwrap();
    let source = <IndexedHttp as HttpSource<Observed<true>>>::new(client, response).unwrap();
    live::<true, _>(
        migo_io::scheduler::IoScheduler::new(0),
        source,
        &data,
        44100,
        true,
        false,
    )
    .await;
    println!("W09_LIVE_LOOP_ORDER2 actualFileHTTP on/off/stop/play");
}
#[tokio::test]
async fn loading_loop_intent_reaches_real_actor_once_without_starting_playback() {
    let name = "0-seek-front.m4a";
    let data = fixture(name);
    let path = std::path::PathBuf::from(std::env::var_os("W09_M4A_FIXTURES").unwrap()).join(name);
    let owner = migo_io::scheduler::IoScheduler::new(0);
    let source = IndexedFile::open(std::fs::File::open(path).unwrap(), &owner)
        .await
        .unwrap();
    live::<true, _>(owner, source, &data, 44100, false, true).await;
    let server = start_server(
        data.clone(),
        Config {
            chunked: true,
            no_tag: true,
            ..Default::default()
        },
    )
    .await;
    let client = client();
    let response = <IndexedHttp as HttpSource<Observed<true>>>::request(client.get(
        crate::network::gate::GateKind::AudioStream,
        server.url.clone(),
    ))
    .send()
    .await
    .unwrap();
    let source = <IndexedHttp as HttpSource<Observed<true>>>::new(client, response).unwrap();
    live::<true, _>(
        migo_io::scheduler::IoScheduler::new(0),
        source,
        &data,
        44100,
        false,
        true,
    )
    .await;
    assert_eq!(server.full_requests.load(Ordering::Acquire), 1);
    assert!(server.ranges.lock().unwrap().is_empty());
    println!("W09_LIVE_LOOP_LOADING2 actualFileHTTP deferredloop");
}

#[tokio::test]
async fn unaccepted_decoder_metadata_change_is_fatal_after_loop_ack() {
    let data = fixture("0-seek-front.m4a");
    let path = std::path::PathBuf::from(std::env::var_os("W09_M4A_FIXTURES").unwrap())
        .join("0-seek-front.m4a");
    let owner = migo_io::scheduler::IoScheduler::new(0);
    let source = IndexedFile::open(std::fs::File::open(path).unwrap(), &owner)
        .await
        .unwrap();
    let state = StreamingState::new_reusable();
    let _cancel = Cancel(state.clone());
    let controls = Controls::for_source(&state).unwrap();
    let (tx, rx) = stream_channel();
    let cutovers = Arc::new(AtomicUsize::new(0));
    let steps = Arc::new(AtomicUsize::new(0));
    let watched = steps.clone();
    let task = tokio::spawn(streaming_transport::run_source_controlled(
        owner.audio_cpu(),
        source,
        None,
        tx,
        state.clone(),
        move |len| Observed {
            inner: Timed::<true>::new(44100, len),
            cutovers,
            bad_metadata: Some(watched),
        },
        controls,
    ));
    let mut p = InnerAudioPlayer::new(941, 1);
    p.start_streaming("malicious metadata".into(), rx, state.clone())
        .unwrap();
    wait(&mut p, |p| p.shared.is_loaded() && p.is_stream_complete()).await;
    p.set_loop_enabled(true);
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            p.poll_stream();
            if p.drain_events()
                .any(|e| e.event_type == InnerAudioEventType::Error)
            {
                break;
            }
            assert!(
                steps.load(Ordering::Acquire) < 32,
                "undeclared metadata change survived32 decoder steps"
            );
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(!p.shared.is_loaded());
    assert!(state.is_cancelled());
    let _ = tokio::time::timeout(Duration::from_secs(3), task)
        .await
        .unwrap()
        .unwrap();
}
