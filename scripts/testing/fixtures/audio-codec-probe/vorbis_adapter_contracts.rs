//! Common bounded decoder with real borrowed and cross-page Vorbis packets.
use crate::streaming::{Progress, VorbisDecoder};
use std::{
    path::PathBuf,
    sync::atomic::{AtomicBool, Ordering},
};
const SCRATCH: usize = 32 * 1024;
fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(PathBuf::from(std::env::var_os("W09_OGG_FIXTURES").unwrap()).join(name)).unwrap()
}
fn decode(data: &[u8], fragment: usize, cap: Option<u64>) -> Result<Vec<f32>, String> {
    let mut decoder = VorbisDecoder::new(cap);
    let mut out = Vec::with_capacity(SCRATCH);
    let mut actual = Vec::new();
    let cancel = AtomicBool::new(false);
    let mut position = 0;
    for _ in 0..data.len() * 3 + 1024 {
        if position == data.len() {
            decoder.finish_input();
        } else {
            let end = (position + fragment).min(data.len());
            position += decoder.push(&data[position..end])?;
        }
        loop {
            out.clear();
            let progress = decoder.drain(&mut out, &cancel)?;
            assert!(out.len() <= SCRATCH);
            actual.extend_from_slice(&out);
            match progress {
                Progress::Yield => continue,
                Progress::NeedInput => break,
                Progress::Finished => {
                    assert_eq!(position, data.len());
                    return Ok(actual);
                }
                Progress::Cancelled => panic!("unexpected cancellation"),
            }
        }
    }
    panic!("Vorbis made no bounded progress");
}
#[test]
fn vorbis_shared_partitions_match_whole_codec_and_source_lengths() {
    let mut count = 0;
    for (name, frames, channels) in [
        ("0.ogg", 17, 1),
        ("1.ogg", 9737, 2),
        ("2.ogg", 9737, 1),
        ("3.ogg", 240137, 6),
        ("large.ogg", 9737, 2),
        ("cross.ogg", 240137, 6),
    ] {
        let data = fixture(name);
        let expected =
            crate::candidate::decode(&data, crate::limits::MAX_AUDIO_PCM_SAMPLES as usize)
                .unwrap()
                .samples;
        assert_eq!(expected.len(), frames * channels);
        for n in [1, 3, 137, 8192, 65536] {
            assert_eq!(decode(&data, n, None).unwrap(), expected, "{name}/{n}");
            count += 1;
        }
    }
    assert_eq!(count, 30);
    println!("W09_VORBIS_SHARED {count} comparisons");
}
#[test]
fn maximum_legal_vorbis_window_and_eight_channels_fit_publication_scratch() {
    let data = fixture("max-window-8.ogg");
    let expected =
        crate::candidate::decode(&data, crate::limits::MAX_AUDIO_PCM_SAMPLES as usize).unwrap();
    assert_eq!(
        (
            expected.sample_rate,
            expected.channels,
            expected.samples.len()
        ),
        (48000, 8, 8192 * 8)
    );
    assert!(expected.samples.iter().all(|v| *v == 0.0));
    for n in [1, 3, 137, 8192, 65536] {
        assert_eq!(decode(&data, n, None).unwrap(), expected.samples);
    }
}
#[test]
fn vorbis_common_steps_have_no_steady_rust_allocation() {
    let data = fixture("3.ogg");
    let mut decoder = VorbisDecoder::new(None);
    let mut out = Vec::with_capacity(SCRATCH);
    let cancel = AtomicBool::new(false);
    let mut offset = 0;
    let mut step = || {
        offset += decoder.push(&data[offset..]).unwrap();
        out.clear();
        let progress = decoder.drain(&mut out, &cancel).unwrap();
        assert_ne!(progress, Progress::Finished);
        out.len()
    };
    for _ in 0..8 {
        step();
    }
    let before = migo_alloc_probe::thread_counts().allocation_events();
    let mut produced = 0;
    for _ in 0..16 {
        let count = step();
        assert!(count > 0, "measured an idle decoder");
        produced += count;
    }
    let after = migo_alloc_probe::thread_counts().allocation_events();
    assert_eq!(after - before, 0);
    assert!(produced > 0);
}
#[test]
fn vorbis_common_error_quota_and_cancel_do_not_publish_partial_pcm() {
    let data = fixture("1.ogg");
    let cancel = AtomicBool::new(false);
    let mut out = Vec::with_capacity(SCRATCH);
    let mut decoder = VorbisDecoder::new(Some(1));
    decoder.push(&data).unwrap();
    loop {
        out.clear();
        match decoder.drain(&mut out, &cancel) {
            Err(_) => break,
            Ok(Progress::Yield) => {}
            _ => panic!("quota did not fail"),
        }
    }
    assert!(out.is_empty());
    assert!(decoder.push(&data).is_err());
    assert!(decoder.drain(&mut out, &cancel).is_err());
    let mut decoder = VorbisDecoder::new(None);
    decoder.push(&data).unwrap();
    cancel.store(true, Ordering::Release);
    assert_eq!(
        decoder.drain(&mut out, &cancel).unwrap(),
        Progress::Cancelled
    );
    cancel.store(false, Ordering::Release);
    assert_eq!(
        decoder.drain(&mut out, &cancel).unwrap(),
        Progress::Cancelled
    );
    assert!(out.is_empty());
    assert!(decoder.push(&data).is_err());
}
