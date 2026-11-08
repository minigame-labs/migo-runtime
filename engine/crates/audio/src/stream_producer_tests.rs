use super::*;
use crate::media_source::{AudioFileSource, tests::MemoryFile};
use crate::pcm_source::{Container, Symphonia};
use std::time::Duration;

/// A canonical 16-bit PCM WAVE file of a non-constant signal.
fn wave(rate: u32, channels: u16, frames: usize) -> Vec<u8> {
    let data_len = (frames * channels as usize * 2) as u32;
    let mut out = Vec::new();
    out.extend(b"RIFF");
    out.extend((36 + data_len).to_le_bytes());
    out.extend(b"WAVEfmt ");
    out.extend(16u32.to_le_bytes());
    out.extend(1u16.to_le_bytes());
    out.extend(channels.to_le_bytes());
    out.extend(rate.to_le_bytes());
    out.extend((rate * u32::from(channels) * 2).to_le_bytes());
    out.extend((channels * 2).to_le_bytes());
    out.extend(16u16.to_le_bytes());
    out.extend(b"data");
    out.extend(data_len.to_le_bytes());
    for i in 0..frames * channels as usize {
        let s = ((i as f64 * 0.013).sin() * 20_000.0) as i16;
        out.extend(s.to_le_bytes());
    }
    out
}

fn start(bytes: Vec<u8>, target: u32, state: &Arc<StreamingState>) -> mpsc::Receiver<StreamMsg> {
    let (tx, rx) = crate::streaming::stream_channel();
    let open: Open = Box::new(move |_state| {
        let container = Container::sniff(&bytes).unwrap();
        let source = AudioFileSource::new(MemoryFile::new(bytes), Arc::new(AtomicBool::new(false)));
        Ok(Box::new(Symphonia::open(Box::new(source), container)?) as Box<dyn PcmSource>)
    });
    spawn(open, target, tx, state.clone(), None).unwrap();
    rx
}

/// Everything one revision publishes, until Done (or Error).
fn collect(
    rx: &mut mpsc::Receiver<StreamMsg>,
    revision: StreamRevision,
) -> (Vec<f32>, Option<String>) {
    let mut pcm = Vec::new();
    loop {
        match rx.blocking_recv().expect("producer hung up before Done") {
            StreamMsg::Ready { .. } => {}
            StreamMsg::Samples {
                revision: r,
                pcm: chunk,
            } => {
                if r == revision {
                    pcm.extend_from_slice(&chunk);
                }
            }
            StreamMsg::Done { revision: r } if r == revision => return (pcm, None),
            StreamMsg::Done { .. } => {}
            StreamMsg::Error { message, .. } => return (pcm, Some(message)),
        }
    }
}

fn max_error(a: &[f32], b: &[f32]) -> f32 {
    assert_eq!(a.len(), b.len(), "sample count");
    a.iter()
        .zip(b)
        .map(|(x, y)| (x - y).abs())
        .fold(0.0, f32::max)
}

#[test]
fn a_stream_publishes_the_whole_presentation_converted_once() {
    for (rate, target, channels) in [
        (44_100, 48_000, 2),
        (48_000, 48_000, 1),
        (22_050, 16_000, 2),
    ] {
        let bytes = wave(rate, channels, 30_011);
        let reference = crate::decoder::decode_for_rate(&bytes, target).unwrap();
        let state = StreamingState::new();
        let mut rx = start(bytes, target, &state);
        let (pcm, error) = collect(&mut rx, StreamRevision::INITIAL);
        assert_eq!(error, None);
        let clock = state.timeline().expect("a WAVE states its length");
        assert_eq!(
            pcm.len() as u64,
            clock.output_frames() * u64::from(channels)
        );
        let error = max_error(&pcm, &reference.samples);
        assert!(error <= 1e-6, "{rate}->{target}x{channels}: {error}");
    }
}

#[test]
fn a_seek_offers_a_revision_and_publishes_the_continuous_samples_from_there() {
    let (rate, target, channels) = (44_100, 48_000, 2usize);
    let bytes = wave(rate, channels as u16, 40_000);
    let reference = crate::decoder::decode_for_rate(&bytes, target).unwrap();
    let state = StreamingState::new_url();
    let mut rx = start(bytes, target, &state);
    // Leave the channel full: a paused consumer must still be able to seek.
    std::thread::sleep(Duration::from_millis(200));
    let controls = state
        .control_sender()
        .expect("a known clock installs controls")
        .clone();
    let frame = 25_000i64;
    let runtime = crate::streaming::get_stream_runtime();
    let reply =
        crate::stream_control::request(&controls, frame, Instant::now() + Duration::from_secs(10))
            .unwrap();
    let Reply::Prepared(prepared) = runtime.block_on(reply).unwrap() else {
        panic!("seek was not prepared");
    };
    let start_frame = prepared.mapped_output().unwrap() as usize;
    let revision = prepared.revision;
    prepared.accept.send(()).unwrap();
    let (pcm, error) = collect(&mut rx, revision);
    assert_eq!(error, None);
    let want = &reference.samples[start_frame * channels..];
    let error = max_error(&pcm, want);
    assert!(error <= 1e-6, "seek suffix error {error}");
}

#[test]
fn cancelling_ends_a_producer_blocked_on_a_full_channel() {
    let state = StreamingState::new();
    let rx = start(wave(44_100, 2, 400_000), 48_000, &state);
    std::thread::sleep(Duration::from_millis(100));
    state.cancel();
    // The channel closes once the producer thread has exited.
    let started = std::time::Instant::now();
    let mut rx = rx;
    while rx.blocking_recv().is_some() {}
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[test]
fn a_steady_state_mp3_traversal_never_reaches_the_heap() {
    use migo_alloc_probe::{Burst, assert_no_steady_state_allocation};
    // Decoding, conversion and chunking reuse their buffers once warm. (The
    // Symphonia readers box each packet upstream; that is a decode-thread cost,
    // never the audio callback's.)
    let bytes = include_bytes!("decoder/mp3/fixtures/music.mp3").to_vec();
    let source = AudioFileSource::new(MemoryFile::new(bytes), Arc::new(AtomicBool::new(false)));
    let mut source = crate::mp3_source::Minimp3::open(Box::new(source)).unwrap();
    let (rate, channels) = (source.rate(), source.channels());
    let sinc = Sinc::new_at(rate, 44_100, channels, source.frames(), 0, 0).unwrap();
    let mut traversal = Traversal::new(sinc, channels, source.frames(), 0, Some(1));
    let cancel = AtomicBool::new(false);
    let mut pcm = Vec::with_capacity(1024);
    assert_no_steady_state_allocation(
        Burst {
            path: "MP3 decode, SRC and chunk fill",
            warmup: 64,
            measured: 256,
        },
        |_| {
            pcm.clear();
            let done = traversal
                .fill(&mut source, &mut pcm, 1024, &cancel)
                .unwrap();
            assert!(!done && pcm.len() == 1024);
        },
    );
}
