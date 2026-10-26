//! The common Migo input/work/PCM adapter with the typed AAC-LC backend.
use crate::streaming::{AdtsDecoder, Progress, SAMPLES_PER_STEP};
use std::{
    path::PathBuf,
    sync::atomic::{AtomicBool, Ordering},
};

fn fixture(i: u32) -> Vec<u8> {
    std::fs::read(
        PathBuf::from(std::env::var_os("W09_ADTS_FIXTURES").unwrap()).join(format!("{i}.aac")),
    )
    .unwrap()
}
fn decode(data: &[u8], fragment: usize, cap: Option<u64>) -> Result<Vec<f32>, String> {
    let mut decoder = AdtsDecoder::new(cap);
    let mut out = Vec::with_capacity(SAMPLES_PER_STEP);
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
            assert!(out.len() <= SAMPLES_PER_STEP);
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
    panic!("ADTS adapter stopped making bounded progress");
}

#[test]
fn adts_shared_partitions_equal_the_actual_whole_codec() {
    let mut count = 0;
    for i in 0..4 {
        let data = fixture(i);
        let expected =
            crate::candidate::decode(&data, crate::limits::MAX_AUDIO_PCM_SAMPLES as usize)
                .unwrap()
                .samples;
        for fragment in [1, 3, 137, 8192, 65536] {
            assert_eq!(
                decode(&data, fragment, None).unwrap(),
                expected,
                "{i}/{fragment}"
            );
            count += 1;
        }
    }
    assert_eq!(count, 20);
    println!("W09_ADTS_SHARED {count} whole/partition comparisons");
}

#[test]
fn adts_common_push_decode_convert_is_allocation_free_after_warmup() {
    use symphonia_codec_aac::streaming::{AdtsPacketizer, PacketStep};
    for i in 0..4 {
        let data = fixture(i);
        let mut parser = AdtsPacketizer::new(Default::default());
        let PacketStep::Packet { consumed, .. } = parser.next(&data, false).unwrap() else {
            panic!()
        };
        let frame = &data[..consumed];
        let mut decoder = AdtsDecoder::new(None);
        let mut out = Vec::with_capacity(SAMPLES_PER_STEP);
        let cancel = AtomicBool::new(false);
        let mut step = || {
            assert_eq!(decoder.push(frame).unwrap(), frame.len());
            out.clear();
            assert_eq!(
                decoder.drain(&mut out, &cancel).unwrap(),
                Progress::NeedInput
            );
            assert!(!out.is_empty() && out.len() <= SAMPLES_PER_STEP);
        };
        for _ in 0..32 {
            step();
        }
        let before = migo_alloc_probe::thread_counts().allocation_events();
        for _ in 0..256 {
            step();
        }
        let after = migo_alloc_probe::thread_counts().allocation_events();
        assert_eq!(after - before, 0, "fixture {i}");
    }
}

#[test]
fn adts_common_quota_error_and_cancellation_publish_no_partial_pcm() {
    let data = fixture(1);
    let cancel = AtomicBool::new(false);
    let mut decoder = AdtsDecoder::new(Some(1));
    let mut out = Vec::with_capacity(SAMPLES_PER_STEP);
    decoder.push(&data).unwrap();
    assert!(decoder.drain(&mut out, &cancel).is_err());
    assert!(out.is_empty());
    assert!(decoder.drain(&mut out, &cancel).is_err());
    assert!(decoder.push(&data).is_err());
    let mut decoder = AdtsDecoder::new(None);
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
    assert!(decoder.push(&data).is_err() && out.is_empty());
}
