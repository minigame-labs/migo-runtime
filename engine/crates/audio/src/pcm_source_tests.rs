use super::*;
use crate::media_source::{AudioFileSource, tests::MemoryFile};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

fn open(bytes: &[u8]) -> Symphonia {
    let container = Container::sniff(bytes).expect("a supported container");
    let source = AudioFileSource::new(
        MemoryFile::new(bytes.to_vec()),
        Arc::new(AtomicBool::new(false)),
    );
    Symphonia::open(Box::new(source), container).unwrap()
}

fn drain(source: &mut Symphonia) -> Vec<f32> {
    let mut out = Vec::new();
    while source.read(&mut out).unwrap() {}
    out
}

/// A canonical 16-bit PCM WAVE file.
fn wave(rate: u32, channels: u16, frames: usize) -> Vec<u8> {
    let samples: Vec<i16> = (0..frames * channels as usize)
        .map(|i| ((i as i64 * 7919 % 65_536) - 32_768) as i16)
        .collect();
    let data_len = (samples.len() * 2) as u32;
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
    for s in samples {
        out.extend(s.to_le_bytes());
    }
    out
}

fn fixtures() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        ("wave-stereo", wave(44_100, 2, 50_000)),
        ("wave-mono-short", wave(22_050, 1, 17)),
        (
            "0.ogg",
            include_bytes!("decoder/ogg-fixtures/0.ogg").to_vec(),
        ),
        (
            "1.ogg",
            include_bytes!("decoder/ogg-fixtures/1.ogg").to_vec(),
        ),
        (
            "2.ogg",
            include_bytes!("decoder/ogg-fixtures/2.ogg").to_vec(),
        ),
        (
            "cross.ogg",
            include_bytes!("decoder/ogg-fixtures/cross.ogg").to_vec(),
        ),
        (
            "adts-1.aac",
            include_bytes!("decoder/aac-fixtures/adts-1.aac").to_vec(),
        ),
        (
            "adts-2.aac",
            include_bytes!("decoder/aac-fixtures/adts-2.aac").to_vec(),
        ),
        (
            "1-front.m4a",
            include_bytes!("decoder/aac-fixtures/1-front.m4a").to_vec(),
        ),
        (
            "1-tail.m4a",
            include_bytes!("decoder/aac-fixtures/1-tail.m4a").to_vec(),
        ),
        (
            "2-co64.m4a",
            include_bytes!("decoder/aac-fixtures/2-co64.m4a").to_vec(),
        ),
        (
            "2-reorder.m4a",
            include_bytes!("decoder/aac-fixtures/2-reorder.m4a").to_vec(),
        ),
        (
            "edit-1537.m4a",
            include_bytes!("decoder/aac-fixtures/edit-1537.m4a").to_vec(),
        ),
        (
            "edit-211.m4a",
            include_bytes!("decoder/aac-fixtures/edit-211.m4a").to_vec(),
        ),
    ]
}

fn max_error(a: &[f32], b: &[f32]) -> f32 {
    a.iter()
        .zip(b)
        .map(|(x, y)| (x - y).abs())
        .fold(0.0, f32::max)
}

#[test]
fn every_container_decodes_the_same_presentation_as_the_product_decoder() {
    for (name, bytes) in fixtures() {
        let mut source = open(&bytes);
        let pcm = drain(&mut source);
        let oracle = crate::decoder::decode_for_rate(&bytes, source.rate()).unwrap();
        assert_eq!(
            (source.rate(), source.channels()),
            (oracle.sample_rate, oracle.channels),
            "{name}: format"
        );
        assert_eq!(
            pcm.len(),
            oracle.samples.len(),
            "{name}: presentation length"
        );
        if let Some(frames) = source.frames() {
            assert_eq!(
                frames * u64::from(source.channels()),
                pcm.len() as u64,
                "{name}: stated frames"
            );
        }
        let error = max_error(&pcm, &oracle.samples);
        assert!(error <= 1e-6, "{name}: max sample error {error}");
    }
}

#[test]
fn a_seek_publishes_exactly_the_frames_a_whole_decode_has_there() {
    for (name, bytes) in fixtures() {
        let reference = drain(&mut open(&bytes));
        let probe = open(&bytes);
        let channels = probe.channels() as usize;
        let frames = reference.len() / channels;
        for target in [0, 1, frames / 3, frames / 2 + 7, frames.saturating_sub(5)] {
            if target >= frames {
                continue;
            }
            let mut source = open(&bytes);
            // Read some first, so the seek repositions a decoder with state.
            let mut warm = Vec::new();
            source.read(&mut warm).unwrap();
            source
                .seek(target as u64)
                .unwrap_or_else(|e| panic!("{name}@{target}: {e:?}"));
            let mut out = Vec::new();
            while out.len() < (frames - target).min(4096) * channels
                && source.read(&mut out).unwrap()
            {}
            let n = out
                .len()
                .min((frames - target) * channels)
                .min(4096 * channels);
            assert!(n > 0, "{name}@{target}: nothing after seek");
            let expected = &reference[target * channels..target * channels + n];
            let error = max_error(&out[..n], expected);
            // AAC's perceptual noise substitution draws from a generator whose
            // state a seek cannot reproduce (the fixtures README: no arbitrary
            // AAC seek bit identity); every other codec reconverges exactly.
            let tolerance = if name.ends_with(".m4a") || name.ends_with(".aac") {
                1e-3
            } else {
                1e-6
            };
            assert!(
                error <= tolerance,
                "{name}@{target}: max sample error {error} after seek"
            );
        }
        // The end of the presentation is reachable and reads as the end.
        let mut source = open(&bytes);
        source.seek(frames as u64).unwrap();
        assert!(
            !source.read(&mut Vec::new()).unwrap(),
            "{name}: read past the end"
        );
    }
}

#[test]
fn unsupported_and_damaged_inputs_fail_cleanly() {
    assert_eq!(
        Container::sniff(b"ID3\x03\x00"),
        None,
        "MP3 is not a symphonia container here"
    );
    let mut truncated = include_bytes!("decoder/aac-fixtures/1-tail.m4a").to_vec();
    truncated.truncate(truncated.len() / 2);
    let source = AudioFileSource::new(MemoryFile::new(truncated), Arc::new(AtomicBool::new(false)));
    assert!(
        Symphonia::open(Box::new(source), Container::Mp4).is_err(),
        "a tail index cut off"
    );
    let source = AudioFileSource::new(
        MemoryFile::new(b"RIFF\0\0\0\0WAVE".to_vec()),
        Arc::new(AtomicBool::new(false)),
    );
    assert!(Symphonia::open(Box::new(source), Container::Wave).is_err());
}
