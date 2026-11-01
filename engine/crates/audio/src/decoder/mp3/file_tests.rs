use super::*;
use crate::{decoder::mp3::take_native_trace, mp3_fixture};

const CBR: &[u8] = include_bytes!("fixtures/stereo-cbr.mp3");
const VBR: &[u8] = include_bytes!("fixtures/mono-vbr.mp3");
const CBR_PCM: &[u8] = include_bytes!("fixtures/stereo-cbr.f32le");
const VBR_PCM: &[u8] = include_bytes!("fixtures/mono-vbr.f32le");

fn bounded_step<'a, B: AsRef<[u8]>>(
    decoder: &'a mut Mp3FileDecoder<B>,
    cancel: &AtomicBool,
) -> EngineResult<FileStep<'a>> {
    take_native_trace();
    let result = decoder.step(cancel);
    checked_trace();
    result
}

fn checked_trace() -> super::super::NativeTrace {
    let trace = take_native_trace();
    assert!(trace.calls <= 4, "actual frame FFI calls: {}", trace.calls);
    assert!(trace.bytes <= 8192, "actual FFI bytes: {}", trace.bytes);
    assert!(
        trace.max_input <= 8192,
        "actual FFI slice: {}",
        trace.max_input
    );
    assert!(
        trace.max_scan <= 4096,
        "actual Rust scan: {}",
        trace.max_scan
    );
    trace
}

fn collect<B: AsRef<[u8]>>(input: B) -> (Vec<u8>, u32, u32) {
    drain(Mp3FileDecoder::new(input))
}

fn drain<B: AsRef<[u8]>>(mut decoder: Mp3FileDecoder<B>) -> (Vec<u8>, u32, u32) {
    let mut samples = Vec::new();
    let mut rate = 0;
    let mut channels = 0;
    let cancel = AtomicBool::new(false);
    for _ in 0..1_000_000 {
        match bounded_step(&mut decoder, &cancel).unwrap() {
            FileStep::Frame {
                pcm,
                sample_rate,
                channels: ch,
            } => {
                assert!(pcm.len() <= super::super::MAX_SAMPLES_PER_FRAME);
                if rate == 0 {
                    rate = sample_rate;
                    channels = ch;
                }
                for sample in pcm {
                    samples.extend_from_slice(&(f32::from(*sample) / 32768.0).to_le_bytes());
                }
            }
            FileStep::Yield => {}
            FileStep::Finished => {
                assert!(
                    decoder.state.is_none(),
                    "EOF releases the encoded input and codecs"
                );
                assert!(matches!(
                    bounded_step(&mut decoder, &cancel).unwrap(),
                    FileStep::Finished
                ));
                return (samples, rate, channels);
            }
        }
    }
    panic!("bounded cursor failed to make progress");
}

// Original synchronous framing control, using the already-shared mature
// minimp3 helpers. It is test-only and never calls the new file cursor. Native
// SIMD/scalar rounding can differ across architectures: keep exact comparison
// to the old algorithm on each target, plus the frozen Linux/x86_64 bytes here.
fn original_pcm(data: &[u8]) -> Vec<u8> {
    let mut frames = Mp3FrameDecoder::new();
    let mut probe = Mp3FrameDecoder::new();
    let mut hint = None;
    let mut position = 0;
    let mut output = Vec::new();
    while position < data.len() {
        let remaining = &data[position..];
        let (length, measured) =
            match super::super::first_frame_at_front(&mut probe, remaining, hint) {
                Some((skip, _)) if skip != 0 => {
                    position += skip;
                    continue;
                }
                Some((_, length)) => {
                    hint = Some(length);
                    (length, true)
                }
                None => (remaining.len(), false),
            };
        match frames.decode(&remaining[..length]) {
            Mp3Step::Frame { pcm, consumed, .. } => {
                position += consumed;
                for sample in pcm {
                    output.extend_from_slice(&(f32::from(*sample) / 32768.0).to_le_bytes());
                }
            }
            Mp3Step::Skipped(consumed) => position += consumed,
            Mp3Step::NeedMoreData if measured => position += length,
            Mp3Step::NeedMoreData => break,
        }
    }
    output
}

pub(crate) fn expected_fixture(encoded: &[u8], frozen: &[u8]) -> Vec<u8> {
    let expected = original_pcm(encoded);
    if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        assert_eq!(
            expected, frozen,
            "original control must match frozen host PCM"
        );
    }
    expected
}

fn id3(body: &[u8]) -> Vec<u8> {
    let n = body.len();
    let mut result = b"ID3\x03\0\0".to_vec();
    result.extend([
        (n >> 21) as u8 & 127,
        (n >> 14) as u8 & 127,
        (n >> 7) as u8 & 127,
        n as u8 & 127,
    ]);
    result.extend_from_slice(body);
    result
}

#[test]
fn nonzero_cbr_and_vbr_match_original_complete_pcm_with_owned_or_borrowed_input() {
    for (encoded, expected, rate, channels) in
        [(CBR, CBR_PCM, 44_100, 2), (VBR, VBR_PCM, 22_050, 1)]
    {
        assert!(
            expected
                .chunks_exact(4)
                .any(|s| f32::from_le_bytes(s.try_into().unwrap()).abs() > 0.01)
        );
        let expected = expected_fixture(encoded, expected);
        let full = super::super::decode(encoded).unwrap();
        let complete: Vec<_> = full
            .samples
            .iter()
            .flat_map(|sample| sample.to_le_bytes())
            .collect();
        assert_eq!(complete, expected, "actual whole-file product entry");
        for owned in [false, true] {
            let (actual, got_rate, got_channels) = if owned {
                {
                    let owned: Box<[u8]> = encoded.into();
                    collect(owned)
                }
            } else {
                collect(encoded)
            };
            assert_eq!(
                actual, expected,
                "every sample including encoder delay and final tail"
            );
            assert_eq!((got_rate, got_channels), (rate, channels));
        }
    }
}

#[test]
fn metadata_and_scan_window_boundaries_keep_exact_nonzero_pcm() {
    for (encoded, expected) in [(CBR, CBR_PCM), (VBR, VBR_PCM)] {
        let expected = expected_fixture(encoded, expected);
        for padding in [
            1,
            SCAN_BYTES - 1,
            SCAN_BYTES,
            SCAN_BYTES + 1,
            2 * SCAN_BYTES - 1,
        ] {
            let mut bytes = vec![0; padding];
            bytes.extend_from_slice(encoded);
            bytes.extend_from_slice(b"TAG");
            bytes.extend_from_slice(&[0; 125]);
            assert_eq!(collect(bytes).0, expected);
        }
        let mut body = mp3_fixture::stream(2);
        body.resize(64 * 1024, 0xFF);
        let mut bytes = id3(&body);
        bytes.extend_from_slice(&id3(&body));
        bytes.extend_from_slice(encoded);
        bytes.extend_from_slice(b"TAG");
        bytes.extend_from_slice(&[0; 125]);
        assert_eq!(
            collect(bytes).0,
            expected,
            "consecutive declared tags contain no audio"
        );
    }
}

#[test]
fn a_lone_tagged_frame_and_an_unavailable_reservoir_do_not_lose_later_frames() {
    for frames in [1, 2, 7] {
        let mut input = mp3_fixture::stream(frames);
        input.extend_from_slice(b"TAG");
        input.resize(input.len() + 125, 0);
        let samples = collect(input).0;
        assert_eq!(samples.len(), frames * mp3_fixture::SAMPLES_PER_FRAME * 4);
    }
    let all = mp3_fixture::stream(6);
    assert_eq!(
        collect(&all[mp3_fixture::FRAME_BYTES..]).0.len(),
        4 * mp3_fixture::SAMPLES_PER_FRAME * 4
    );
}

fn free_format(frames: usize, length: usize) -> Vec<u8> {
    let mut result = Vec::new();
    for original in mp3_fixture::stream(frames).chunks_exact(mp3_fixture::FRAME_BYTES) {
        let mut frame = original.to_vec();
        frame[2] &= 0x0F;
        frame.resize(length, 0);
        result.extend_from_slice(&frame);
    }
    result
}

#[test]
fn free_format_uses_native_boundaries_bootstrap_and_isolated_tagged_tail() {
    for length in [mp3_fixture::FRAME_BYTES, FRAME_BYTES - 1] {
        let mut input = free_format(7, length);
        input.extend_from_slice(b"TAG");
        input.resize(input.len() + 125, 0);
        let (pcm, rate, channels) = collect(input);
        assert_eq!(
            pcm.len(),
            7 * mp3_fixture::SAMPLES_PER_FRAME * 4,
            "free-format frame length {length}"
        );
        assert_eq!((rate, channels), (44_100, 2));
        assert!(pcm.iter().all(|b| *b == 0));
    }
    // The native inference loop's bound is exclusive. Infer a 2303-byte
    // unpadded base first, then exercise the 2304-byte padded frame and tail.
    let mut mixed = Vec::new();
    for (i, original) in mp3_fixture::stream(7)
        .chunks_exact(mp3_fixture::FRAME_BYTES)
        .enumerate()
    {
        let mut frame = original.to_vec();
        frame[2] &= 0x0F;
        if i != 0 {
            frame[2] |= 2;
        }
        frame.resize(FRAME_BYTES - usize::from(i == 0), 0);
        mixed.extend(frame);
    }
    mixed.extend_from_slice(b"TAG");
    mixed.resize(mixed.len() + 125, 0);
    assert_eq!(
        collect(mixed).0.len(),
        7 * mp3_fixture::SAMPLES_PER_FRAME * 4
    );
}

#[test]
fn native_probes_and_fallback_obey_each_step_budget_on_invalid_input() {
    let mut long = vec![0; 64 * 1024];
    long[..4].copy_from_slice(&[0xFF, 0xFB, 0xF0, 0]);
    let inputs = [
        long.clone(),
        b"ID3\xff\xff\0\x7f\x7f\x7f\x7f"
            .iter()
            .copied()
            .chain(std::iter::repeat_n(0, 65_536))
            .collect(),
        [0xFF, 0xFB, 0xF0, 0].repeat(100),
    ];
    let mut saw_fallback = false;
    for input in inputs {
        let mut decoder = Mp3FileDecoder::new(input);
        let mut ended = false;
        for _ in 0..100_000 {
            take_native_trace();
            let result = decoder.step(&AtomicBool::new(false));
            saw_fallback |= checked_trace().fallbacks != 0;
            match result {
                Ok(FileStep::Finished) | Err(_) => {
                    ended = true;
                    break;
                }
                Ok(FileStep::Frame { .. }) => panic!("invalid bitrate bytes must not become audio"),
                Ok(FileStep::Yield) => {}
            }
        }
        assert!(ended);
    }
    assert!(
        saw_fallback,
        "real malformed candidates reach bounded native fallback"
    );
    let mut decoder = Mp3FileDecoder::new(long);
    decoder.state.as_mut().unwrap().phase = Phase::Fallback;
    assert!(matches!(
        bounded_step(&mut decoder, &AtomicBool::new(false)).unwrap(),
        FileStep::Yield
    ));
    assert!(decoder.state.as_ref().unwrap().position > 0);
    let mut prefixed = vec![0; 1000];
    prefixed.extend_from_slice(CBR);
    let mut decoder = Mp3FileDecoder::new(prefixed);
    decoder.state.as_mut().unwrap().phase = Phase::Fallback;
    assert_eq!(
        drain(decoder).0,
        expected_fixture(CBR, CBR_PCM),
        "fallback includes skipped bytes in its cursor"
    );
}

#[test]
fn cancellation_retires_owned_input_at_every_suspended_phase_without_native_work() {
    for mode in 0..6 {
        let (input, steps, fallback) = match mode {
            0 => (CBR.to_vec(), 0, false),
            1 => (free_format(7, FRAME_BYTES), 1, false),
            2 => (free_format(7, FRAME_BYTES), 2, false),
            3 => (mp3_fixture::stream(1000), 10, false),
            4 => (vec![0; 64 * 1024], 1, false),
            _ => (vec![0; 64 * 1024], 0, true),
        };
        let input: std::sync::Arc<[u8]> = input.into();
        let weak = std::sync::Arc::downgrade(&input);
        let mut decoder = Mp3FileDecoder::new(input);
        if fallback {
            decoder.state.as_mut().unwrap().phase = Phase::Fallback;
        }
        for _ in 0..steps {
            bounded_step(&mut decoder, &AtomicBool::new(false)).unwrap();
        }
        let before = migo_alloc_probe::thread_counts();
        assert!(
            matches!(bounded_step(&mut decoder, &AtomicBool::new(true)), Err(e) if e.code == ErrorCode::Cancelled)
        );
        assert!(migo_alloc_probe::thread_counts().bytes_freed > before.bytes_freed);
        assert!(weak.upgrade().is_none());
        assert!(decoder.state.is_none());
        assert!(
            matches!(bounded_step(&mut decoder, &AtomicBool::new(false)), Err(e) if e.code == ErrorCode::Cancelled)
        );
    }
}

#[test]
fn native_call_quota_is_terminal_and_cannot_publish_complete_audio() {
    let mut decoder = Mp3FileDecoder::new(mp3_fixture::stream(10));
    decoder.state.as_mut().unwrap().calls_left = 3;
    let mut calls = 0;
    let mut error = false;
    for _ in 0..20 {
        take_native_trace();
        let step = decoder.step(&AtomicBool::new(false));
        calls += take_native_trace().calls;
        assert!(
            calls <= 3,
            "global work quota includes probes and real decoding"
        );
        match step {
            Err(e) => {
                assert_eq!(e.code, ErrorCode::InvalidArgument);
                error = true;
                break;
            }
            Ok(FileStep::Finished) => panic!("quota cannot turn partial decode into success"),
            _ => {}
        }
    }
    assert!(error);
    assert!(bounded_step(&mut decoder, &AtomicBool::new(false)).is_err());
}

#[test]
fn productive_frame_steps_preserve_decoder_state_without_heap_activity() {
    let mut decoder = Mp3FileDecoder::new(mp3_fixture::stream(1000));
    let mut ready = false;
    for _ in 0..100 {
        if matches!(
            bounded_step(&mut decoder, &AtomicBool::new(false)).unwrap(),
            FileStep::Frame { .. }
        ) {
            ready = true;
            break;
        }
    }
    assert!(
        ready,
        "metadata/bootstrap must reach a real productive frame"
    );
    migo_alloc_probe::assert_no_steady_state_allocation(
        migo_alloc_probe::Burst {
            path: "bounded MP3 frame cursor",
            warmup: 1,
            measured: 256,
        },
        |iteration| {
            take_native_trace();
            let frame = decoder.step(&AtomicBool::new(false)).unwrap();
            let trace = checked_trace();
            if iteration > 0 {
                assert!(
                    trace.calls <= 2 && trace.bytes <= 2 * mp3_fixture::FRAME_BYTES,
                    "steady frames reuse the known native boundary"
                );
            }
            let FileStep::Frame { pcm, .. } = frame else {
                panic!("productive frame");
            };
            assert_eq!(pcm.len(), mp3_fixture::SAMPLES_PER_FRAME);
        },
    );
}

#[test]
fn owned_codec_survives_real_audio_cpu_handoffs_and_releases_input_at_eof() {
    let input: std::sync::Arc<[u8]> = CBR.into();
    let weak = std::sync::Arc::downgrade(&input);
    let host = migo_io::scheduler::IoScheduler::new(0);
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .build()
        .unwrap();
    runtime.block_on(async {
        let mut owner = crate::off_worker::OffWorker::new(
            (
                Mp3FileDecoder::new(input),
                Vec::with_capacity(CBR_PCM.len()),
            ),
            host.audio_cpu(),
        );
        let mut steps = 0;
        loop {
            let (next, result) = owner
                .with(|(decoder, out)| -> EngineResult<bool> {
                    match bounded_step(decoder, &AtomicBool::new(false))? {
                        FileStep::Frame { pcm, .. } => {
                            for value in pcm {
                                out.extend_from_slice(&(f32::from(*value) / 32768.0).to_le_bytes());
                            }
                            Ok(false)
                        }
                        FileStep::Yield => Ok(false),
                        FileStep::Finished => Ok(true),
                    }
                })
                .await
                .unwrap();
            owner = next;
            steps += 1;
            if result.unwrap() {
                break;
            }
            assert_eq!(host.audio_cpu().run(|| 321).await.unwrap(), 321);
        }
        assert!(steps > 2);
        assert!(weak.upgrade().is_none());
        let (_, pcm) = owner.with(|(_, pcm)| std::mem::take(pcm)).await.unwrap();
        assert_eq!(pcm, expected_fixture(CBR, CBR_PCM));
    });
}

#[test]
fn whole_mp3_pcm_growth_respects_the_capacity_and_sample_ceiling() {
    let frames = crate::limits::MAX_AUDIO_PCM_SAMPLES as usize / mp3_fixture::SAMPLES_PER_FRAME;
    let input = mp3_fixture::stream(frames + 1);
    {
        let decoded = super::super::decode(&input[..frames * mp3_fixture::FRAME_BYTES]).unwrap();
        assert_eq!(
            decoded.samples.len(),
            frames * mp3_fixture::SAMPLES_PER_FRAME
        );
        assert!(decoded.samples.capacity() as u64 <= crate::limits::MAX_AUDIO_PCM_SAMPLES);
    }
    let Err(error) = super::super::decode(&input) else {
        panic!("PCM overrun must be rejected");
    };
    assert_eq!(error.code, ErrorCode::InvalidArgument);
}

#[test]
fn native_fallback_bootstraps_gapless_before_publishing_any_pcm() {
    let music = include_bytes!("fixtures/music.mp3");
    let reference = include_bytes!("fixtures/music.i16le");
    let mut input = vec![0; 1000];
    input.extend_from_slice(music);
    let mut cursor = Mp3FileDecoder::new(input);
    cursor.state.as_mut().unwrap().phase = Phase::Fallback;
    let (actual, rate, channels) = drain(cursor);
    let expected: Vec<u8> = reference
        .chunks_exact(2)
        .flat_map(|v| {
            (f32::from(i16::from_le_bytes(v.try_into().unwrap())) / 32768.0).to_le_bytes()
        })
        .collect();
    assert_eq!((rate, channels), (48000, 2));
    assert!(
        actual == expected,
        "fallback must trim the first native tag frame before any PCM escapes"
    );
}

#[test]
fn declared_gapless_cursor_eof_rejects_partial_pcm_and_releases_input() {
    let music = include_bytes!("fixtures/music.mp3");
    let input: std::sync::Arc<[u8]> = music[..music.len() / 2].into();
    let weak = std::sync::Arc::downgrade(&input);
    let mut cursor = Mp3FileDecoder::new(input);
    let mut frames = 0;
    for _ in 0..10000 {
        match bounded_step(&mut cursor, &AtomicBool::new(false)) {
            Ok(FileStep::Frame { .. }) => frames += 1,
            Ok(FileStep::Yield) => {}
            Ok(FileStep::Finished) => {
                panic!("declared presentation cannot finish with a partial prefix")
            }
            Err(error) => {
                assert_eq!(error.code, ErrorCode::InvalidArgument);
                assert!(frames > 0 && weak.upgrade().is_none());
                assert!(bounded_step(&mut cursor, &AtomicBool::new(false)).is_err());
                return;
            }
        }
    }
    panic!("declared partial input did not retire");
}

#[test]
fn native_metadata_quota_rejects_before_any_borrowed_pcm_can_escape() {
    let mut encoded = include_bytes!("fixtures/music.mp3").to_vec();
    let tag = encoded
        .windows(4)
        .position(|v| v == b"Info" || v == b"Xing")
        .unwrap();
    encoded[tag + 8..tag + 12].copy_from_slice(&u32::MAX.to_be_bytes());
    let input: std::sync::Arc<[u8]> = encoded.into();
    let weak = std::sync::Arc::downgrade(&input);
    let mut cursor = Mp3FileDecoder::new(input);
    for _ in 0..1000 {
        match bounded_step(&mut cursor, &AtomicBool::new(false)) {
            Ok(FileStep::Yield) => {}
            Ok(FileStep::Frame { .. }) => {
                panic!("oversized native presentation must be rejected before any PCM escapes")
            }
            Ok(FileStep::Finished) => panic!("oversized native presentation cannot finish"),
            Err(e) => {
                assert_eq!(e.code, ErrorCode::InvalidArgument);
                assert!(bounded_step(&mut cursor, &AtomicBool::new(false)).is_err());
                assert!(weak.upgrade().is_none());
                return;
            }
        }
    }
    panic!("native quota did not retire");
}
