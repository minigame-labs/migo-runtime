use super::*;
use crate::media_source::{AudioFileSource, tests::MemoryFile};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

fn open(bytes: &[u8]) -> Minimp3 {
    let source = AudioFileSource::new(
        MemoryFile::new(bytes.to_vec()),
        Arc::new(AtomicBool::new(false)),
    );
    Minimp3::open(Box::new(source)).unwrap()
}

fn drain(source: &mut Minimp3) -> Vec<f32> {
    let mut out = Vec::new();
    while source.read(&mut out).unwrap() {}
    out
}

fn fixtures() -> Vec<(&'static str, &'static [u8])> {
    vec![
        (
            "music.mp3",
            include_bytes!("decoder/mp3/fixtures/music.mp3"),
        ),
        (
            "stereo-cbr.mp3",
            include_bytes!("decoder/mp3/fixtures/stereo-cbr.mp3"),
        ),
        (
            "mono-vbr.mp3",
            include_bytes!("decoder/mp3/fixtures/mono-vbr.mp3"),
        ),
        (
            "tiny-47-mono.mp3",
            include_bytes!("decoder/mp3/fixtures/tiny-47-mono.mp3"),
        ),
        (
            "tiny-47-stereo.mp3",
            include_bytes!("decoder/mp3/fixtures/tiny-47-stereo.mp3"),
        ),
        (
            "gapless-mpeg2.mp3",
            include_bytes!("decoder/mp3/fixtures/gapless-mpeg2.mp3"),
        ),
    ]
}

#[test]
fn every_mp3_decodes_the_product_presentation_bit_for_bit() {
    for (name, bytes) in fixtures() {
        let mut source = open(bytes);
        let pcm = drain(&mut source);
        let oracle = crate::decoder::decode_for_rate(bytes, source.rate()).unwrap();
        assert_eq!(
            (source.rate(), source.channels()),
            (oracle.sample_rate, oracle.channels),
            "{name}"
        );
        assert_eq!(
            pcm.len(),
            oracle.samples.len(),
            "{name}: presentation length"
        );
        assert!(
            pcm == oracle.samples,
            "{name}: PCM differs from the product decoder"
        );
        if let Some(frames) = source.frames() {
            assert_eq!(
                frames * u64::from(source.channels()),
                pcm.len() as u64,
                "{name}: tagged length"
            );
        }
    }
}

#[test]
fn a_seek_lands_on_the_exact_sample_of_a_whole_decode() {
    for (name, bytes) in fixtures() {
        let reference = drain(&mut open(bytes));
        let channels = open(bytes).channels() as usize;
        let frames = reference.len() / channels;
        for target in [
            0,
            1,
            1151,
            1152,
            frames / 3,
            frames / 2 + 5,
            frames.saturating_sub(3),
        ] {
            if target >= frames {
                continue;
            }
            let mut source = open(bytes);
            source.read(&mut Vec::new()).unwrap();
            source
                .seek(target as u64)
                .unwrap_or_else(|e| panic!("{name}@{target}: {e:?}"));
            let mut out = Vec::new();
            let want = (frames - target).min(3000) * channels;
            while out.len() < want && source.read(&mut out).unwrap() {}
            assert!(out.len() >= want, "{name}@{target}: short read after seek");
            assert!(
                out[..want] == reference[target * channels..target * channels + want],
                "{name}@{target}: PCM after seek differs"
            );
        }
    }
}

#[test]
fn a_cancelled_source_reports_cancellation_not_end_of_stream() {
    let cancel = Arc::new(AtomicBool::new(false));
    let bytes = include_bytes!("decoder/mp3/fixtures/music.mp3");
    let source = AudioFileSource::new(MemoryFile::new(bytes.to_vec()), cancel.clone());
    let mut mp3 = Minimp3::open(Box::new(source)).unwrap();
    cancel.store(true, std::sync::atomic::Ordering::Release);
    let mut out = Vec::new();
    let error = loop {
        match mp3.read(&mut out) {
            Ok(true) => continue,
            Ok(false) => panic!("cancellation read as end of stream"),
            Err(error) => break error,
        }
    };
    assert_eq!(error.code, ErrorCode::Cancelled);
    let junk = AudioFileSource::new(
        MemoryFile::new(vec![0x55; 4096]),
        Arc::new(AtomicBool::new(false)),
    );
    assert!(Minimp3::open(Box::new(junk)).is_err());
}
