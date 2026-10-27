//! Migo's shared streaming adapter, using the actual typed WAVE/PCM library.
use symphonia::core as symphonia_core;
include!("wave_pcm_contracts.rs");
use crate::streaming::{Progress, SAMPLES_PER_STEP, WavDecoder};
use std::sync::atomic::AtomicBool;

fn adapted(file: &[u8], fragment: usize, cap: Option<u64>) -> Result<Vec<f32>, String> {
    let mut decoder = WavDecoder::new(cap);
    let mut out = Vec::with_capacity(SAMPLES_PER_STEP);
    let mut actual = Vec::new();
    let mut position = 0;
    let cancel = AtomicBool::new(false);
    for _ in 0..file.len() * 3 + 1024 {
        if position == file.len() {
            decoder.finish_input();
        } else {
            let end = (position + fragment).min(file.len());
            let accepted = decoder.push(&file[position..end])?;
            assert!(accepted <= 64 * 1024 && accepted <= end - position);
            position += accepted;
        }
        loop {
            out.clear();
            let progress = decoder.drain(&mut out, &cancel)?;
            assert!(out.len() <= SAMPLES_PER_STEP);
            actual.extend_from_slice(&out);
            match progress {
                Progress::Yield => continue,
                Progress::Finished => {
                    assert_eq!(decoder.rate, 44100);
                    return Ok(actual);
                }
                Progress::NeedInput => break,
                Progress::Cancelled => panic!("unexpected cancellation"),
                #[cfg(feature = "m4a-streaming-probe")]
                Progress::NeedSource => panic!("WAVE requested an indexed source"),
            }
        }
    }
    panic!("adapter made no bounded progress");
}

#[test]
fn wave_shared_adapter_partitions_match_numeric_and_whole_pcm() {
    let mut count = 0;
    for channels in [1, 2, 6, 26] {
        for (bits, floating) in [
            (8, false),
            (16, false),
            (24, false),
            (32, false),
            (32, true),
        ] {
            for frames in [17, 2317] {
                let (file, expected) = fixture(bits, floating, channels, frames);
                for fragment in [1, 137, 8192, 100000] {
                    assert_eq!(
                        adapted(&file, fragment, None).unwrap(),
                        expected,
                        "{channels}/{bits}/{floating}/{frames}/{fragment}"
                    );
                    count += 1;
                }
            }
        }
    }
    println!("W09_WAVE_ADAPTER {count} comparisons");
    assert_eq!(count, 160);
}

fn zero_header(frames: usize) -> Vec<u8> {
    let (mut header, _) = fixture(16, false, 1, 1);
    header.truncate(header.len() - 2);
    let data_bytes = u32::try_from(frames * 2).unwrap();
    let size = header.len() as u32 - 8 + data_bytes;
    header[4..8].copy_from_slice(&size.to_le_bytes());
    let end = header.len();
    header[end - 4..].copy_from_slice(&data_bytes.to_le_bytes());
    let fmt = header.windows(4).position(|p| p == b"fmt ").unwrap() + 8;
    header[fmt + 4..fmt + 8].copy_from_slice(&8000_u32.to_le_bytes());
    header[fmt + 8..fmt + 12].copy_from_slice(&16000_u32.to_le_bytes());
    header
}

#[test]
fn long_wave_uses_bounded_buffers_beyond_the_whole_decode_quota() {
    const FRAMES: usize = 17_600_000;
    let header = zero_header(FRAMES);
    let mut decoder = WavDecoder::new(None);
    assert_eq!(decoder.push(&header).unwrap(), header.len());
    let block = [0_u8; 8192];
    let mut out = Vec::with_capacity(SAMPLES_PER_STEP);
    let (mut sent, mut samples) = (0, 0);
    let cancel = AtomicBool::new(false);
    loop {
        if sent == FRAMES * 2 {
            decoder.finish_input();
        } else {
            let count = block.len().min(FRAMES * 2 - sent);
            sent += decoder.push(&block[..count]).unwrap();
        }
        loop {
            out.clear();
            let progress = decoder.drain(&mut out, &cancel).unwrap();
            assert!(out.len() <= SAMPLES_PER_STEP && out.iter().all(|v| *v == 0.0));
            samples += out.len();
            match progress {
                Progress::Yield => continue,
                Progress::Finished => {
                    assert_eq!(sent, FRAMES * 2);
                    assert_eq!(samples, FRAMES);
                    assert_eq!(decoder.rate, 8000);
                    assert!(samples as u64 > crate::limits::MAX_AUDIO_PCM_SAMPLES);
                    println!(
                        "W09_WAVE_LONG {samples} frames, streamed zero input and one reusable output buffer"
                    );
                    let mut limited = WavDecoder::new(Some(crate::limits::MAX_AUDIO_PCM_SAMPLES));
                    limited.push(&header).unwrap();
                    limited.push(&block).unwrap();
                    out.clear();
                    assert!(limited.drain(&mut out, &cancel).is_err());
                    assert!(out.is_empty());
                    return;
                }
                Progress::NeedInput => break,
                Progress::Cancelled => panic!("unexpected cancellation"),
                #[cfg(feature = "m4a-streaming-probe")]
                Progress::NeedSource => panic!("WAVE requested an indexed source"),
            }
        }
    }
}

#[test]
fn steady_wave_push_decode_convert_has_no_rust_heap_allocation() {
    use migo_alloc_probe::{Burst, assert_no_steady_state_allocation};
    let mut decoder = WavDecoder::new(None);
    let header = zero_header(2048 * 400);
    decoder.push(&header).unwrap();
    let mut out = Vec::with_capacity(SAMPLES_PER_STEP);
    let cancel = AtomicBool::new(false);
    assert_no_steady_state_allocation(
        Burst {
            path: "WAVE typed PCM shared push/decode/convert",
            warmup: 32,
            measured: 256,
        },
        |_| {
            assert_eq!(decoder.push(&[0_u8; 4096]).unwrap(), 4096);
            let mut samples = 0;
            loop {
                out.clear();
                let progress = decoder.drain(&mut out, &cancel).unwrap();
                samples += out.len();
                if progress != Progress::Yield {
                    assert_eq!(progress, Progress::NeedInput);
                    break;
                }
            }
            assert_eq!(samples, 2048);
        },
    );
}

#[test]
fn wave_failure_clears_partial_pcm_and_cancellation_is_sticky() {
    let (mut file, _) = fixture(16, false, 1, 2317);
    file[4..8].copy_from_slice(&u32::MAX.to_le_bytes());
    // This fixture has no trailing pad: data header is immediately before PCM.
    let data_len = file.len() - 2317 * 2 - 4;
    file[data_len..data_len + 4].copy_from_slice(&u32::MAX.to_le_bytes());
    let mut limited = WavDecoder::new(Some(1153));
    limited.push(&file).unwrap();
    limited.finish_input();
    let mut out = Vec::with_capacity(SAMPLES_PER_STEP);
    assert!(limited.drain(&mut out, &AtomicBool::new(false)).is_err());
    assert!(out.is_empty(), "failed step exposed its earlier valid PCM");
    assert!(limited.drain(&mut out, &AtomicBool::new(false)).is_err());
    let mut cancelled = WavDecoder::new(None);
    cancelled.push(&file).unwrap();
    assert_eq!(
        cancelled.drain(&mut out, &AtomicBool::new(true)).unwrap(),
        Progress::Cancelled
    );
    assert!(out.is_empty() && cancelled.push(&[0; 4]).is_err());
    assert_eq!(
        cancelled.drain(&mut out, &AtomicBool::new(false)).unwrap(),
        Progress::Cancelled
    );
}
