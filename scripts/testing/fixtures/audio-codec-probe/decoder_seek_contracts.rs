//! Real AAC/preroll and Rubato discontinuities over the existing source protocol.
use crate::{
    push_decoder::{self, M4aSourceDecoder, M4aSourceRequest, incremental},
    resampled_streaming::{Batched, core},
    streaming_transport::{Decoder as TransportDecoder, Progress},
};
use std::sync::atomic::AtomicBool;
use symphonia::core::{
    errors::{Error, Result as FormatResult},
    formats::SeekedTo,
    units::Timestamp,
};
use symphonia_format_isomp4::streaming::{AtomKind, AtomSpan, AudioIndex, IndexOptions};

fn file(name: &str) -> Vec<u8> {
    std::fs::read(
        std::path::PathBuf::from(std::env::var_os("W09_M4A_FIXTURES").unwrap()).join(name),
    )
    .unwrap()
}
fn atom(data: &[u8], kind: AtomKind, byte: Option<u64>) -> AtomSpan {
    let mut at = 0;
    while at < data.len() {
        let span = AtomSpan::read(&data[at..], at as u64, Some(data.len() as u64))
            .unwrap()
            .unwrap();
        if span.kind == kind && byte.is_none_or(|p| p >= span.body_offset && p < span.end.unwrap())
        {
            return span;
        }
        at = span.end.unwrap() as usize;
    }
    panic!("fixture atom missing");
}
fn index(data: &[u8]) -> AudioIndex {
    let span = atom(data, AtomKind::Movie, None);
    AudioIndex::from_moov(
        &data[span.offset as usize..span.end.unwrap() as usize],
        IndexOptions {
            object_len: Some(data.len() as u64),
            ..Default::default()
        },
    )
    .unwrap()
}
trait Subject: M4aSourceDecoder {
    fn seek(&mut self, target: usize) -> FormatResult<SeekedTo>;
    fn tick(
        &mut self,
        data: &[u8],
        eof: bool,
        out: &mut Vec<f32>,
        cancel: &AtomicBool,
    ) -> Result<(usize, Progress), String>;
}
impl<const B: bool> Subject for push_decoder::M4aDecoderMode<B> {
    fn seek(&mut self, target: usize) -> FormatResult<SeekedTo> {
        self.seek(Timestamp::new(target as i64))
    }
    fn tick(
        &mut self,
        data: &[u8],
        eof: bool,
        out: &mut Vec<f32>,
        cancel: &AtomicBool,
    ) -> Result<(usize, Progress), String> {
        let consumed = if self.source_request().is_some() {
            0
        } else if eof {
            self.finish_input();
            0
        } else {
            self.push(data)?
        };
        let p = self.drain(out, cancel)?;
        Ok((
            consumed,
            match p {
                push_decoder::Progress::NeedInput => Progress::NeedInput,
                push_decoder::Progress::NeedSource => Progress::NeedSource,
                push_decoder::Progress::Yield => Progress::Yield,
                push_decoder::Progress::Finished => Progress::Finished,
                push_decoder::Progress::Cancelled => Progress::Cancelled,
            },
        ))
    }
}
impl<const Q: usize, const B: bool> Subject for Batched<Q, push_decoder::M4a<B>> {
    fn seek(&mut self, target: usize) -> FormatResult<SeekedTo> {
        self.seek(Timestamp::new(target as i64))
    }
    fn tick(
        &mut self,
        data: &[u8],
        eof: bool,
        out: &mut Vec<f32>,
        cancel: &AtomicBool,
    ) -> Result<(usize, Progress), String> {
        let step = self
            .step(data, eof, out, cancel)
            .map_err(|e| e.to_string())?;
        Ok((step.consumed, step.progress))
    }
}
fn run<D: Subject>(
    decoder: &mut D,
    data: &[u8],
    mut at: usize,
    fragment: usize,
) -> Result<(Vec<f32>, usize), String> {
    let mut out = Vec::with_capacity(crate::bandlimited::MAX_SCRATCH_SAMPLES);
    let mut pcm = Vec::new();
    let mut resumes = 0;
    for _ in 0..data.len() * 4 + 1024 {
        out.clear();
        let end = (at + fragment).min(data.len());
        let (consumed, progress) = decoder.tick(
            &data[at..end],
            at == data.len(),
            &mut out,
            &AtomicBool::new(false),
        )?;
        assert!(consumed <= end - at);
        at += consumed;
        pcm.extend_from_slice(&out);
        match progress {
            Progress::NeedSource => match decoder.source_request().expect("active request") {
                M4aSourceRequest::Index(_) => {
                    decoder.install_index_with_length(index(data), Some(data.len() as u64))?
                }
                M4aSourceRequest::ReadAt(p) => {
                    decoder.resume_at(atom(data, AtomKind::MediaData, Some(p.offset)), p.offset)?;
                    at = p.offset as usize;
                    resumes += 1;
                }
            },
            Progress::Finished => return Ok((pcm, resumes)),
            Progress::Cancelled => return Err("unexpected cancellation".into()),
            Progress::NeedInput | Progress::Yield => {}
        }
    }
    Err("decoder failed to make bounded progress".into())
}
pub(crate) fn assert_pcm(actual: &[f32], expected: &[f32]) {
    assert_eq!(actual.len(), expected.len());
    let error = actual
        .iter()
        .zip(expected)
        .map(|(a, b)| (a - b).abs())
        .fold(0.0_f32, f32::max);
    assert!(error < 0.000_01, "PCM error {error}");
}
fn targets(frames: usize) -> Vec<usize> {
    let mut v = vec![frames - 1, 0, 1, frames / 2];
    v.extend([211, 1023, 1024, 2100].into_iter().filter(|&n| n < frames));
    v
}
fn matrix<const B: bool>() -> usize {
    let mut count = 0;
    let manifest: Vec<serde_json::Value> =
        serde_json::from_slice(&file("seek-manifest.json")).unwrap();
    for entry in manifest {
        let data = file(entry["file"].as_str().unwrap());
        let idx = index(&data);
        let frames = idx.track().num_frames.unwrap() as usize;
        let full = crate::candidate::decode(&data, 16_777_216).unwrap();
        for fragment in [137, 100000] {
            let mut decoder =
                push_decoder::M4aDecoderMode::<B>::with_object_len(None, Some(data.len() as u64));
            assert_pcm(
                &run(&mut decoder, &data, 0, fragment).unwrap().0,
                &full.samples,
            );
            for target in targets(frames) {
                let seeked = decoder.seek(Timestamp::new(target as i64)).unwrap();
                assert_eq!(seeked.required_ts.get(), target as i64);
                assert!(decoder.source_request().is_some());
                assert!(decoder.push(b"old-source-position").is_err());
                let (pcm, _) = run(&mut decoder, &data, data.len(), fragment).unwrap();
                assert_pcm(&pcm, &full.samples[target * full.channels as usize..]);
                count += 1;
            }
        }
    }
    count
}
#[test]
fn common_seek_resets_and_discards_preroll_across_eof_and_partitions() {
    assert_eq!(matrix::<false>(), 208);
    assert_eq!(matrix::<true>(), 208);
    println!("W09_DECODER_SEEK_PCM 416 deterministic AAC suffixes, error < 0.00001");
}
#[test]
fn invalid_seek_preserves_a_live_request_and_preindex_parser() {
    let data = file("1-seek-front.m4a");
    let mut decoder = push_decoder::M4aDecoder::with_object_len(None, Some(data.len() as u64));
    assert!(matches!(
        decoder.seek(Timestamp::ZERO),
        Err(Error::SeekError(_))
    ));
    run(&mut decoder, &data, 0, 137).unwrap();
    decoder.seek(Timestamp::new(211)).unwrap();
    let request = decoder.source_request();
    for t in [
        -1,
        index(&data).track().num_frames.unwrap() as i64,
        i64::MAX,
    ] {
        assert!(matches!(
            decoder.seek(Timestamp::new(t)),
            Err(Error::SeekError(_))
        ));
        assert_eq!(decoder.source_request(), request);
    }
    let full = crate::candidate::decode(&data, 16_777_216).unwrap();
    assert_pcm(
        &run(&mut decoder, &data, data.len(), 137).unwrap().0,
        &full.samples[422..],
    );
}
#[test]
fn seek_wait_cancellation_and_invalid_resume_retire_without_pcm() {
    let data = file("1-seek-front.m4a");
    for bad in [false, true] {
        let mut d = push_decoder::M4aDecoder::with_object_len(None, Some(data.len() as u64));
        run(&mut d, &data, 0, 100000).unwrap();
        d.seek(Timestamp::new(211)).unwrap();
        let mut out = Vec::with_capacity(incremental::SAMPLES_PER_STEP);
        if bad {
            let Some(M4aSourceRequest::ReadAt(p)) = d.source_request() else {
                panic!("seek request missing")
            };
            assert!(
                d.resume_at(
                    atom(&data, AtomKind::MediaData, Some(p.offset)),
                    p.offset + 1
                )
                .is_err()
            );
            assert!(d.drain(&mut out, &AtomicBool::new(false)).is_err());
        } else {
            assert_eq!(
                d.drain(&mut out, &AtomicBool::new(true)).unwrap(),
                push_decoder::Progress::Cancelled
            );
        }
        assert!(out.is_empty());
        assert!(d.seek(Timestamp::ZERO).is_err());
        assert!(d.push(&data).is_err());
        assert!(d.source_request().is_none());
    }
}
#[test]
fn complete_cache_seek_reuses_bytes_and_seek_does_not_bypass_pcm_quota() {
    let data = file("1-seek-tail.m4a");
    let full = crate::candidate::decode(&data, 16_777_216).unwrap();
    let mut d = push_decoder::BufferedM4aDecoder::with_object_len(
        Some(full.samples.len() as u64),
        Some(data.len() as u64),
    );
    run(&mut d, &data, 0, 100000).unwrap();
    for _ in 0..4 {
        d.seek(Timestamp::new(211)).unwrap();
        let (actual, resumes) = run(&mut d, &data, data.len(), 137).unwrap();
        assert_eq!(resumes, 0);
        assert_pcm(&actual, &full.samples[422..]);
    }
    let mut small =
        push_decoder::BufferedM4aDecoder::with_object_len(Some(1), Some(data.len() as u64));
    assert!(run(&mut small, &data, 0, 137).is_err());
    assert!(small.seek(Timestamp::ZERO).is_err());
    let mut mp3 = push_decoder::Decoder::new(None);
    assert!(matches!(
        mp3.seek(Timestamp::ZERO),
        Err(Error::SeekError(_))
    ));
    assert_eq!(mp3.push(b"ID3").unwrap(), 3);
}
pub(crate) fn resample(samples: &[f32], rate: u32, target: u32, channels: u32) -> Vec<f32> {
    use rubato::{
        Resampler, audioadapter::Adapter, audioadapter_buffers::direct::InterleavedSlice,
    };
    if rate == target {
        return samples.to_vec();
    }
    let n = samples.len() / channels as usize;
    let frames = (n as u128 * target as u128).div_ceil(rate as u128) as usize;
    let src = InterleavedSlice::new(samples, channels as usize, n).unwrap();
    let out = core(rate, target, channels)
        .unwrap()
        .process_all(&src, n, None)
        .unwrap();
    (0..frames)
        .flat_map(|i| (0..channels as usize).map(move |ch| (i, ch)))
        .map(|(i, ch)| out.read_sample(ch, i).unwrap())
        .collect()
}
#[test]
fn dsp_seek_matches_fresh_library_resampling_and_bypass() {
    let mut count = 0;
    for name in ["front", "tail", "reorder", "scaled", "edit"] {
        let data = file(&format!("1-seek-{name}.m4a"));
        let full = crate::candidate::decode(&data, 16_777_216).unwrap();
        let frames = full.samples.len() / 2;
        for target_rate in [44100, 48000] {
            for fragment in [137, 100000] {
                let mut d = Batched::<1, push_decoder::M4a<true>>::with_object_len(
                    target_rate,
                    Some(data.len() as u64),
                );
                run(&mut d, &data, 0, fragment).unwrap();
                for target in [frames - 1, 0, 211] {
                    Subject::seek(&mut d, target).unwrap();
                    let actual = run(&mut d, &data, data.len(), fragment).unwrap().0;
                    let expected = resample(
                        &full.samples[target * 2..],
                        full.sample_rate,
                        target_rate,
                        2,
                    );
                    assert_pcm(&actual, &expected);
                    count += 1;
                }
            }
        }
    }
    assert_eq!(count, 60);
    println!("W09_DSP_SEEK_PCM {count} fresh-library suffix comparisons");
}
#[test]
fn dsp_seek_discards_pending_pcm_and_invalid_seek_keeps_it() {
    let data = file("1-seek-front.m4a");
    let full = crate::candidate::decode(&data, 16_777_216).unwrap();
    for valid in [false, true] {
        let mut d =
            Batched::<1, push_decoder::M4a<true>>::with_object_len(48000, Some(data.len() as u64));
        let mut out = Vec::with_capacity(crate::bandlimited::MAX_SCRATCH_SAMPLES);
        let step = d
            .step(&data, false, &mut out, &AtomicBool::new(false))
            .unwrap();
        assert!(step.consumed > 0);
        assert!(out.is_empty());
        let target = if valid {
            d.seek(Timestamp::new(211)).unwrap();
            let stale = d
                .step(
                    b"old-source-position",
                    false,
                    &mut out,
                    &AtomicBool::new(false),
                )
                .unwrap();
            assert_eq!(stale.consumed, 0);
            assert!(out.is_empty());
            211
        } else {
            assert!(matches!(
                d.seek(Timestamp::new(-1)),
                Err(Error::SeekError(_))
            ));
            0
        };
        let actual = run(&mut d, &data, step.consumed, 137).unwrap().0;
        assert_pcm(
            &actual,
            &resample(&full.samples[target * 2..], 44100, 48000, 2),
        );
    }
}
#[test]
fn dsp_cancelled_and_failed_decoders_cannot_be_revived_by_seek() {
    let data = file("1-seek-front.m4a");
    for bad in [false, true] {
        let mut d =
            Batched::<1, push_decoder::M4a<false>>::with_object_len(48000, Some(data.len() as u64));
        run(&mut d, &data, 0, 137).unwrap();
        d.seek(Timestamp::new(211)).unwrap();
        let mut out = Vec::with_capacity(crate::bandlimited::MAX_SCRATCH_SAMPLES);
        let wait = d
            .step(
                b"old-source-position",
                false,
                &mut out,
                &AtomicBool::new(false),
            )
            .unwrap();
        assert_eq!(wait.consumed, 0);
        assert_eq!(wait.progress, Progress::NeedSource);
        assert!(out.is_empty());
        if bad {
            let Some(M4aSourceRequest::ReadAt(p)) = d.source_request() else {
                panic!("no request")
            };
            assert!(
                d.resume_at(
                    atom(&data, AtomKind::MediaData, Some(p.offset)),
                    p.offset + 1
                )
                .is_err()
            );
            assert!(
                d.step(&[], false, &mut out, &AtomicBool::new(false))
                    .is_err()
            );
        } else {
            assert_eq!(
                d.step(&[], false, &mut out, &AtomicBool::new(true))
                    .unwrap()
                    .progress,
                Progress::Cancelled
            );
        }
        assert!(out.is_empty());
        assert!(d.seek(Timestamp::ZERO).is_err());
        assert!(d.source_request().is_none());
    }
}
#[test]
fn seek_and_cached_codec_dsp_steps_have_no_steady_heap_activity() {
    use migo_alloc_probe::{Burst, assert_no_steady_state_allocation};
    let data = file("1-seek-front.m4a");
    let mut d =
        Batched::<1, push_decoder::M4a<true>>::with_object_len(48000, Some(data.len() as u64));
    run(&mut d, &data, 0, 100000).unwrap();
    let mut out = Vec::with_capacity(crate::bandlimited::MAX_SCRATCH_SAMPLES);
    assert_no_steady_state_allocation(
        Burst {
            path: "cached M4A seek + AAC/preroll + retained sinc reset",
            warmup: 8,
            measured: 32,
        },
        |_| {
            d.seek(Timestamp::new(211)).unwrap();
            let mut produced = false;
            for _ in 0..64 {
                out.clear();
                let step = d
                    .step(&[], true, &mut out, &AtomicBool::new(false))
                    .unwrap();
                assert_eq!(step.consumed, 0);
                assert_ne!(step.progress, Progress::NeedSource);
                if !out.is_empty() {
                    produced = true;
                    break;
                }
            }
            assert!(produced);
        },
    );
}

// Observe the actual codec boundary independently of PCM equivalence. AAC's
// preroll can erase old overlap, so a suffix test alone may miss a skipped reset.
use std::sync::atomic::{AtomicUsize, Ordering};
use symphonia::core::{
    audio::GenericAudioBufferRef,
    codecs::{
        CodecInfo,
        audio::{AudioCodecParameters, AudioDecoder, FinalizeResult},
    },
    formats::Track,
    packet::PacketRef,
};
static RESETS: AtomicUsize = AtomicUsize::new(0);
static CONSTRUCTIONS: AtomicUsize = AtomicUsize::new(0);
struct ObservedCodec(symphonia_codec_aac::AacDecoder);
impl AudioDecoder for ObservedCodec {
    fn reset(&mut self) {
        RESETS.fetch_add(1, Ordering::Relaxed);
        self.0.reset();
    }
    fn codec_info(&self) -> &CodecInfo {
        self.0.codec_info()
    }
    fn codec_params(&self) -> &AudioCodecParameters {
        self.0.codec_params()
    }
    fn decode_ref(&mut self, p: &PacketRef<'_>) -> FormatResult<GenericAudioBufferRef<'_>> {
        self.0.decode_ref(p)
    }
    fn finalize(&mut self) -> FinalizeResult {
        self.0.finalize()
    }
    fn last_decoded(&self) -> GenericAudioBufferRef<'_> {
        self.0.last_decoded()
    }
}
struct ObservedPackets(push_decoder::M4a<true>);
impl incremental::Packetizer for ObservedPackets {
    type Codec = ObservedCodec;
    const RETAIN_ENCODED_HISTORY: bool = true;
    fn new() -> Self {
        Self(<push_decoder::M4a<true> as incremental::Packetizer>::new())
    }
    fn track(&self) -> Option<&Track> {
        self.0.track()
    }
    fn next<'a>(
        &mut self,
        input: &'a [u8],
        eof: bool,
    ) -> FormatResult<incremental::PacketStep<'a>> {
        self.0.next(input, eof)
    }
    fn seek(&mut self, ts: Timestamp) -> FormatResult<SeekedTo> {
        self.0.seek(ts)
    }
    fn resume_buffered(
        &mut self,
        input: &[u8],
        head: usize,
        eof: bool,
    ) -> FormatResult<incremental::BufferedSource> {
        self.0.resume_buffered(input, head, eof)
    }
    fn validate_codec(p: &AudioCodecParameters) -> Result<(), String> {
        <push_decoder::M4a<true> as incremental::Packetizer>::validate_codec(p)
    }
    fn create_decoder(p: &AudioCodecParameters) -> Result<Self::Codec, String> {
        CONSTRUCTIONS.fetch_add(1, Ordering::Relaxed);
        <push_decoder::M4a<true> as incremental::Packetizer>::create_decoder(p).map(ObservedCodec)
    }
}
#[test]
fn explicit_seek_resets_the_retained_codec_once_and_container_rewinds_do_not() {
    let data = file("1-seek-reorder.m4a");
    let expected = crate::candidate::decode(&data, 16_777_216).unwrap();
    RESETS.store(0, Ordering::Relaxed);
    CONSTRUCTIONS.store(0, Ordering::Relaxed);
    let mut d = incremental::Decoder::<ObservedPackets>::new(None);
    assert_eq!(d.push(&data).unwrap(), data.len());
    d.finish_input();
    fn collect(d: &mut incremental::Decoder<ObservedPackets>) -> Vec<f32> {
        let mut pcm = Vec::new();
        let mut out = Vec::with_capacity(incremental::SAMPLES_PER_STEP);
        for _ in 0..1024 {
            out.clear();
            let p = d.drain(&mut out, &AtomicBool::new(false)).unwrap();
            pcm.extend_from_slice(&out);
            match p {
                push_decoder::Progress::Finished => return pcm,
                push_decoder::Progress::Yield => {}
                other => panic!("complete cache requested {other:?}"),
            }
        }
        panic!("codec observation made no progress")
    }
    assert_pcm(&collect(&mut d), &expected.samples);
    assert_eq!(RESETS.load(Ordering::Relaxed), 0);
    for (i, target) in [211, 0, 9000, 1].into_iter().enumerate() {
        d.seek(Timestamp::new(target as i64)).unwrap();
        assert_eq!(RESETS.load(Ordering::Relaxed), i + 1);
        assert_pcm(&collect(&mut d), &expected.samples[target * 2..]);
        assert_eq!(RESETS.load(Ordering::Relaxed), i + 1);
        assert_eq!(CONSTRUCTIONS.load(Ordering::Relaxed), 1);
    }
    assert!(d.seek(Timestamp::new(-1)).is_err());
    assert_eq!(RESETS.load(Ordering::Relaxed), 4);
}
