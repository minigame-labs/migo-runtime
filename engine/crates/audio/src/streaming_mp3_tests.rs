use super::*;
use crate::decoder::DecodedAudio;

const CBR: &[u8] = include_bytes!("decoder/mp3/fixtures/stereo-cbr.mp3");
const VBR: &[u8] = include_bytes!("decoder/mp3/fixtures/mono-vbr.mp3");
const CBR_PCM: &[u8] = include_bytes!("decoder/mp3/fixtures/stereo-cbr.f32le");
const VBR_PCM: &[u8] = include_bytes!("decoder/mp3/fixtures/mono-vbr.f32le");

pub(super) fn checked(
    decoder: &mut Mp3StreamDecoder,
    input: &[u8],
    eof: bool,
    out: &mut Vec<f32>,
    cancel: &AtomicBool,
) -> EngineResult<Step> {
    crate::decoder::mp3::take_stream_trace();
    bandlimited::take_trace();
    let before = out.len();
    let result = decoder.step(input, eof, out, cancel);
    let (calls, bytes, max_input, scan) = crate::decoder::mp3::take_stream_trace();
    let dsp = bandlimited::take_trace();
    assert!(
        calls + dsp.calls <= 1,
        "stream performed multiple kernels in one CPU turn"
    );
    assert!(
        bytes <= 8192 && max_input <= 8192 && scan <= 4096,
        "stream native work exceeded its budget"
    );
    assert!(dsp.input_samples <= 8192 && dsp.output_samples <= 8192);
    if let Ok(step) = &result {
        assert!(step.consumed <= input.len() && step.consumed <= 4096);
        assert!(
            out.len().saturating_sub(before) <= 8192,
            "stream published an oversized PCM batch"
        );
    }
    if let Some(owner) = &decoder.owner {
        assert!(owner.buffer.len() <= RECEIVE_BYTES);
        assert!(owner.scratch.len() <= MAX_SAMPLES_PER_FRAME);
        assert!(owner.output.len() < PCM_BATCH_SAMPLES || owner.dsp_finished);
    }
    result
}
fn collect(input: &[u8], target: u32, partition: usize) -> Vec<f32> {
    let mut decoder = Mp3StreamDecoder::new(target);
    let mut out = Vec::with_capacity(32768);
    let mut result = Vec::new();
    let mut position = 0;
    let mut end = partition.min(input.len());
    let mut eof = false;
    let cancel = AtomicBool::new(false);
    for _ in 0..1_000_000 {
        out.clear();
        let step = checked(&mut decoder, &input[position..end], eof, &mut out, &cancel)
            .expect("progressive MP3 did not complete its presentation");
        position += step.consumed;
        result.extend_from_slice(&out);
        if step.progress == Progress::Finished {
            assert!(eof && position == input.len());
            assert!(decoder.owner.is_none(), "EOF retained decoder/SRC storage");
            assert_eq!(
                decoder.step(&[], true, &mut out, &cancel).unwrap().progress,
                Progress::Finished
            );
            return result;
        }
        if step.progress == Progress::NeedInput && position == end {
            if position == input.len() {
                eof = true;
            } else {
                end = (end + partition).min(input.len());
            }
        }
    }
    panic!("stream failed to finish");
}
fn original_raw(data: &[u8]) -> Vec<f32> {
    use crate::decoder::mp3::first_frame_at_front;
    let mut frames = Mp3FrameDecoder::new();
    let mut probe = Mp3FrameDecoder::new();
    let mut position = 0;
    let mut previous = None;
    let mut samples = Vec::new();
    while let Some((skip, length)) = first_frame_at_front(&mut probe, &data[position..], previous) {
        position += skip;
        if let Mp3Step::Frame { pcm, .. } = frames.decode(&data[position..position + length]) {
            append_as_f32(pcm, &mut samples);
        }
        previous = Some(length);
        position += length;
    }
    samples
}

fn expected(encoded: &[u8], bytes: &[u8], source: u32, channels: u32, target: u32) -> Vec<f32> {
    let samples = original_raw(encoded);
    if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        let actual: Vec<_> = samples.iter().flat_map(|v| v.to_le_bytes()).collect();
        assert_eq!(
            actual, bytes,
            "original raw reference differs from frozen host PCM"
        );
    }
    let input = DecodedAudio {
        samples,
        sample_rate: source,
        channels,
    };
    if source == target {
        input.samples
    } else {
        crate::resample_job::tests::reference(&input, target)
    }
}

#[test]
fn complete_progressive_pcm_matches_frozen_raw_codec_and_independent_sinc() {
    for (encoded, raw, source, channels) in [(CBR, CBR_PCM, 44100, 2), (VBR, VBR_PCM, 22050, 1)] {
        for target in [source, 48000, 8000] {
            let reference = expected(encoded, raw, source, channels, target);
            for partition in [1, 137, 4096, 65536] {
                let actual = collect(encoded, target, partition);
                assert_eq!(
                    actual.len(),
                    reference.len(),
                    "progressive PCM duration changed"
                );
                assert!(
                    actual
                        .iter()
                        .zip(&reference)
                        .all(|(a, b)| a.to_bits() == b.to_bits()),
                    "progressive PCM differs from independent sinc"
                );
            }
        }
    }
}

#[test]
fn tagged_short_and_warm_eof_are_incremental_and_keep_every_raw_frame() {
    let mut tag = b"ID3\x03\x00\x00\x00\x00\x04\x58".to_vec();
    tag.resize(610, 0);
    tag[100..104].copy_from_slice(&[0xff, 0xfb, 0x90, 0x00]);
    let mut tail = b"TAG".to_vec();
    tail.resize(128, 0);
    for frames in [1, 2, 6, 40] {
        let original = crate::mp3_fixture::stream(frames);
        let source = [tag.as_slice(), &original, &tail].concat();
        let actual = collect(&source, 48000, 137);
        let raw = crate::decoder::mp3::decode(&original).unwrap();
        let reference = crate::resample_job::tests::reference(&raw, 48000);
        assert_eq!(actual, reference, "tagged EOF lost or repeated a frame");
    }
}

#[test]
fn cancellation_at_codec_setup_dsp_and_eof_releases_every_owner_and_stays_terminal() {
    let source = crate::mp3_fixture::stream(100);
    for stop in [0, 1, 5, 20, 100] {
        let mut decoder = Mp3StreamDecoder::new(48000);
        let mut out = Vec::with_capacity(32768);
        let mut position = 0;
        for _ in 0..stop {
            out.clear();
            let step = checked(
                &mut decoder,
                &source[position..],
                false,
                &mut out,
                &AtomicBool::new(false),
            )
            .unwrap();
            position += step.consumed;
        }
        out.push(0.25);
        assert_eq!(
            checked(&mut decoder, &[], false, &mut out, &AtomicBool::new(true))
                .unwrap()
                .progress,
            Progress::Cancelled
        );
        assert!(
            out.is_empty() && decoder.owner.is_none(),
            "cancel retained stream allocations or PCM"
        );
        assert_eq!(
            decoder
                .step(&[], false, &mut out, &AtomicBool::new(false))
                .err()
                .unwrap()
                .code,
            ErrorCode::Cancelled
        );
    }
    let mut decoder = Mp3StreamDecoder::new(48000);
    let mut out = Vec::with_capacity(32768);
    decoder
        .step(&[], true, &mut out, &AtomicBool::new(false))
        .unwrap();
    assert_eq!(
        decoder
            .step(&[], true, &mut out, &AtomicBool::new(true))
            .unwrap()
            .progress,
        Progress::Cancelled
    );
    assert!(decoder.owner.is_none());
}

#[test]
fn format_changes_are_errors_before_new_format_pcm_is_published() {
    let first = CBR.repeat(8);
    let other = VBR.repeat(8);
    let encoded = [first, other].concat();
    let mut decoder = Mp3StreamDecoder::new(48000);
    let mut out = Vec::with_capacity(32768);
    let mut position = 0;
    let mut eof = false;
    for _ in 0..100000 {
        out.clear();
        let result = checked(
            &mut decoder,
            &encoded[position..],
            eof,
            &mut out,
            &AtomicBool::new(false),
        );
        if let Err(error) = result {
            assert!(error.detail.as_deref().unwrap().contains("format changed"));
            assert!(out.is_empty() && decoder.owner.is_none());
            return;
        }
        let step = result.unwrap();
        position += step.consumed;
        assert_ne!(
            step.progress,
            Progress::Finished,
            "mixed-format stream was accepted"
        );
        if step.progress == Progress::NeedInput && position == encoded.len() {
            eof = true;
        }
    }
    panic!("mixed-format stream never terminated");
}

#[test]
fn invalid_target_nonempty_eof_and_insufficient_publication_capacity_are_sticky() {
    for (target, input, eof) in [(0, &[][..], false), (48000, &[1][..], true)] {
        let mut decoder = Mp3StreamDecoder::new(target);
        let mut out = Vec::with_capacity(32768);
        out.push(0.5);
        let code = decoder
            .step(input, eof, &mut out, &AtomicBool::new(false))
            .err()
            .unwrap()
            .code;
        assert!(decoder.owner.is_none() && out.is_empty());
        assert_eq!(
            decoder
                .step(&[], false, &mut out, &AtomicBool::new(false))
                .err()
                .unwrap()
                .code,
            code
        );
    }
    let encoded = crate::mp3_fixture::stream(40);
    let mut decoder = Mp3StreamDecoder::new(48000);
    let mut out = Vec::new();
    let mut position = 0;
    for _ in 0..100000 {
        match decoder.step(
            &encoded[position..],
            false,
            &mut out,
            &AtomicBool::new(false),
        ) {
            Ok(step) => position += step.consumed,
            Err(error) => {
                assert!(
                    error
                        .detail
                        .as_deref()
                        .unwrap()
                        .contains("reserved capacity")
                );
                assert!(out.is_empty() && decoder.owner.is_none());
                return;
            }
        }
    }
    panic!("undersized PCM publication capacity was accepted");
}

#[test]
fn running_codec_sinc_and_eof_reuse_preallocated_buffers_without_heap_growth() {
    let source = crate::mp3_fixture::stream(100);
    let mut decoder = Mp3StreamDecoder::new(48000);
    let mut out = Vec::with_capacity(32768);
    let mut position = 0;
    let mut eof = false;
    let mut measured = 0;
    for _ in 0..100000 {
        out.clear();
        let warm = decoder
            .owner
            .as_ref()
            .is_some_and(|o| o.pipeline.as_ref().is_some_and(|p| !p.is_preparing()));
        let before = migo_alloc_probe::thread_counts();
        let step = decoder
            .step(&source[position..], eof, &mut out, &AtomicBool::new(false))
            .unwrap();
        let after = migo_alloc_probe::thread_counts();
        if warm {
            assert_eq!(
                after.allocations, before.allocations,
                "stream processing allocated"
            );
            assert_eq!(
                after.reallocations, before.reallocations,
                "stream processing reallocated"
            );
            measured += 1;
        }
        position += step.consumed;
        if step.progress == Progress::Finished {
            assert!(measured > 100);
            return;
        }
        if step.progress == Progress::NeedInput && position == source.len() {
            eof = true;
        }
    }
    panic!("allocation-tested stream never finished");
}

#[test]
fn fixed_pcm_batching_preserves_a_useful_four_slot_decode_reserve() {
    let source = crate::mp3_fixture::stream(40);
    let mut decoder = Mp3StreamDecoder::new(48000);
    let mut out = Vec::with_capacity(32768);
    let mut position = 0;
    let mut publications = 0;
    for _ in 0..10000 {
        out.clear();
        let step = checked(
            &mut decoder,
            &source[position..],
            false,
            &mut out,
            &AtomicBool::new(false),
        )
        .unwrap();
        position += step.consumed;
        if !out.is_empty() {
            assert!(
                out.len() >= 4096,
                "small SRC quantum consumed a transport slot"
            );
            publications += 1;
        }
        if step.progress == Progress::NeedInput && position == source.len() {
            assert!(publications >= 8);
            return;
        }
    }
    panic!("body batching never requested EOF");
}

#[test]
fn garbage_is_bounded_and_eof_and_cancellation_retire_codec_storage() {
    let garbage = vec![0u8; 200000];
    assert!(collect(&garbage, 48000, 65536).is_empty());
    let mut decoder = Mp3StreamDecoder::new(48000);
    let mut out = Vec::with_capacity(32768);
    for _ in 0..4 {
        if decoder
            .step(&[], true, &mut out, &AtomicBool::new(false))
            .unwrap()
            .progress
            == Progress::Finished
        {
            break;
        }
    }
    assert!(decoder.owner.is_none(), "empty EOF retained codec storage");
    assert_eq!(
        decoder
            .step(&[], true, &mut out, &AtomicBool::new(true))
            .unwrap()
            .progress,
        Progress::Cancelled
    );
    assert!(
        decoder
            .step(&[], true, &mut out, &AtomicBool::new(false))
            .is_err()
    );
}

#[test]
fn long_music_matches_frozen_gapless_minimp3_and_independent_sinc() {
    let data = include_bytes!("decoder/mp3/fixtures/music.mp3");
    // Preserve the historical raw count as evidence of the corrected duration.
    let samples = original_raw(data);
    assert_eq!(
        samples.len() / 2,
        482688,
        "independent raw frame count changed"
    );
    let raw = DecodedAudio {
        samples: include_bytes!("decoder/mp3/fixtures/music.i16le")
            .chunks_exact(2)
            .map(|v| f32::from(i16::from_le_bytes(v.try_into().unwrap())) / 32768.0)
            .collect(),
        sample_rate: 48000,
        channels: 2,
    };
    for target in [48000, 44100] {
        let reference = if target == raw.sample_rate {
            raw.samples.clone()
        } else {
            crate::resample_job::tests::reference(&raw, target)
        };
        for partition in [137, 4096, 65536] {
            let actual = collect(data, target, partition);
            assert_eq!(
                actual.len(),
                reference.len(),
                "music stream duration changed"
            );
            assert!(
                actual
                    .iter()
                    .zip(&reference)
                    .all(|(a, b)| a.to_bits() == b.to_bits()),
                "music stream changed a frozen presentation or sinc sample"
            );
        }
    }
}

#[path = "streaming_mp3_gapless_tests.rs"]
mod gapless;
