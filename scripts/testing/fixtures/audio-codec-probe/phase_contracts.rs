//! Compare discontinuities with the absolute grid of a complete library render.
use super::*;
use crate::{
    candidate,
    decoder_seek_contracts::{assert_pcm, resample},
    isomp4_source_contracts::fixture,
    streaming_transport::Progress,
};
use symphonia_format_isomp4::streaming::{AtomKind, IndexOptions};

fn movie(data: &[u8]) -> AudioIndex {
    let mut at = 0;
    loop {
        let span = AtomSpan::read(&data[at..], at as u64, Some(data.len() as u64))
            .unwrap()
            .unwrap();
        if span.kind == AtomKind::Movie {
            return AudioIndex::from_moov(
                &data[at..span.end.unwrap() as usize],
                IndexOptions {
                    object_len: Some(data.len() as u64),
                    ..Default::default()
                },
            )
            .unwrap();
        }
        at = span.end.unwrap() as usize;
    }
}
fn media(data: &[u8], offset: u64) -> AtomSpan {
    let mut at = 0;
    loop {
        let span = AtomSpan::read(&data[at..], at as u64, Some(data.len() as u64))
            .unwrap()
            .unwrap();
        if span.kind == AtomKind::MediaData
            && offset >= span.body_offset
            && offset < span.end.unwrap()
        {
            return span;
        }
        at = span.end.unwrap() as usize;
    }
}
pub(super) fn render<const B: bool>(
    d: &mut Timed<B>,
    data: &[u8],
    at: usize,
    fragment: usize,
) -> Vec<f32> {
    let mut cursor = at;
    render_cursor(d, data, &mut cursor, fragment, None)
}
pub(super) fn render_prefix<const B: bool>(
    d: &mut Timed<B>,
    data: &[u8],
    at: usize,
    fragment: usize,
    samples: usize,
) -> Vec<f32> {
    let mut cursor = at;
    render_cursor(d, data, &mut cursor, fragment, Some(samples))
}
pub(super) fn render_cursor<const B: bool>(
    d: &mut Timed<B>,
    data: &[u8],
    at: &mut usize,
    fragment: usize,
    limit: Option<usize>,
) -> Vec<f32> {
    let mut pcm = Vec::new();
    let mut out = Vec::with_capacity(crate::bandlimited::MAX_SCRATCH_SAMPLES);
    for _ in 0..data.len() * 8 + 4096 {
        out.clear();
        let end = (*at + fragment).min(data.len());
        let step = d
            .step(
                &data[*at..end],
                *at == data.len(),
                &mut out,
                &AtomicBool::new(false),
            )
            .unwrap();
        *at += step.consumed;
        pcm.extend_from_slice(&out);
        if let Some(limit) = limit.filter(|limit| pcm.len() >= *limit) {
            pcm.truncate(limit);
            return pcm;
        }
        match step.progress {
            Progress::NeedSource => match d.source_request().unwrap() {
                M4aSourceRequest::Index(_) => d
                    .install_index_with_length(movie(data), Some(data.len() as u64))
                    .unwrap(),
                M4aSourceRequest::ReadAt(packet) => {
                    d.resume_at(media(data, packet.offset), packet.offset)
                        .unwrap();
                    *at = packet.offset as usize;
                }
            },
            Progress::Finished => return pcm,
            Progress::Cancelled => panic!("unexpected cancellation"),
            Progress::NeedInput | Progress::Yield => {}
        }
    }
    panic!("clocked decoder stopped making bounded progress");
}
fn matrix<const B: bool>() -> usize {
    let manifest: Vec<serde_json::Value> =
        serde_json::from_slice(&fixture("seek-manifest.json")).unwrap();
    let mut count = 0;
    for entry in manifest {
        let data = fixture(entry["file"].as_str().unwrap());
        let full = candidate::decode(&data, 16 * 1024 * 1024).unwrap();
        let frames = full.samples.len() / full.channels as usize;
        for rate in [16000, 44100, 48000] {
            let reference = resample(&full.samples, full.sample_rate, rate, full.channels);
            for fragment in [137, 100000] {
                let mut d = Timed::<B>::new(rate, Some(data.len() as u64));
                assert_pcm(&render(&mut d, &data, 0, fragment), &reference);
                for target in [frames - 1, 0, 1, 211.min(frames - 1), frames / 2] {
                    let seeked = d.seek(Timestamp::new(target as i64)).unwrap();
                    assert_eq!(seeked.required_ts.get(), target as i64);
                    let pcm = render(&mut d, &data, data.len(), fragment);
                    let start =
                        (target as u128 * rate as u128).div_ceil(full.sample_rate as u128) as usize;
                    assert_pcm(&pcm, &reference[start * full.channels as usize..]);
                    count += 1;
                }
            }
        }
    }
    count
}
#[test]
fn clocked_seek_matches_whole_stream_output() {
    let comparisons = matrix::<false>() + matrix::<true>();
    assert_eq!(comparisons, 840);
    println!("W09_PHASE {comparisons} absolute-grid AAC/Rubato suffixes");
}

#[test]
fn integer_seek_plan_bounds_history_and_keeps_the_global_output_boundary() {
    let frames = 9_007_199_254_740_993;
    let mut fractional = 0;
    for (input, output) in [
        (44100, 48000),
        (48000, 44100),
        (48000, 16000),
        (3001, 768000),
        (768000, 3001),
        (48000, 48000),
    ] {
        let timeline = StreamTimeline::new(frames, input, output, 2).unwrap();
        for target in [0, 1, 211, 50_001, frames / 2, frames - 1] {
            let plan = seek_plan(timeline, target).unwrap();
            let scaled = u128::from(target) * u128::from(output);
            let quotient = scaled / u128::from(input);
            let remainder = scaled % u128::from(input);
            assert_eq!(
                u128::from(plan.output_start),
                quotient + u128::from(remainder != 0)
            );
            fractional += usize::from(remainder != 0);
            assert!(plan.input_origin <= target);
            assert!(target - plan.input_origin < u64::from(input) + 512);
            assert_eq!(
                u128::from(plan.input_origin) * u128::from(output) % u128::from(input),
                0
            );
            if input == output {
                assert_eq!(plan.input_origin, target);
            } else if plan.input_origin != 0 {
                assert!(plan.input_origin + 258 <= target);
            }
        }
        assert!(seek_plan(timeline, frames + 1).is_err());
    }
    assert!(fractional > 10);
}

fn raw_render(s: &mut crate::bandlimited::Stream<rubato::Async<f32>>, samples: &[f32]) -> Vec<f32> {
    let mut pcm = Vec::new();
    let mut out = Vec::with_capacity(s.output_capacity());
    let mut at = 0;
    for _ in 0..samples.len() + 4096 {
        out.clear();
        let step = s
            .step(&samples[at..], at == samples.len(), &mut out)
            .unwrap();
        at += step.consumed;
        pcm.extend_from_slice(&out);
        if step.progress == crate::bandlimited::Progress::Finished {
            return pcm;
        }
    }
    panic!("resampler did not finish");
}
#[test]
fn invalid_origins_preserve_the_live_resampler_and_absolute_eof_is_exact() {
    let samples: Vec<_> = (0..16001).map(|i| (i as f32 * 0.137).sin()).collect();
    let reference = resample(&samples, 44100, 48000, 1);
    let make = || {
        crate::bandlimited::Stream::new(
            crate::resampled_streaming::core(44100, 48000, 1).unwrap(),
            44100,
            48000,
            1,
        )
        .unwrap()
    };
    let mut s = make();
    let mut scratch = Vec::with_capacity(s.output_capacity());
    let first = s.step(&samples[..31], false, &mut scratch).unwrap();
    assert_eq!(first.consumed, 31);
    assert!(scratch.is_empty());
    assert!(s.reset_from(1, 211).is_err());
    assert!(s.reset_from(147, 159).is_err());
    assert!(s.reset_from(u64::MAX - u64::MAX % 147, u64::MAX).is_err());
    assert_pcm(&raw_render(&mut s, &samples[31..]), &reference);
    for target in [0, 1, 211, 10000, 16000] {
        let timeline = StreamTimeline::new(16001, 44100, 48000, 1).unwrap();
        let plan = seek_plan(timeline, target).unwrap();
        let mut s = make();
        s.reset_from(plan.input_origin, plan.output_start).unwrap();
        let pcm = raw_render(&mut s, &samples[plan.input_origin as usize..]);
        assert_pcm(&pcm, &reference[plan.output_start as usize..]);
    }
    // Exercise real filtering and EOF counters above f64's integer range,
    // using an integral grid shift of a small independent library render.
    let origin = 9_007_199_254_740_993_u64 / 147 * 147;
    let timeline = StreamTimeline::new(origin + 1024, 44100, 48000, 1).unwrap();
    let plan = seek_plan(timeline, origin + 500).unwrap();
    let mut s = make();
    s.reset_from(plan.input_origin, plan.output_start).unwrap();
    let pcm = raw_render(
        &mut s,
        &samples[(plan.input_origin - origin) as usize..1024],
    );
    let reference = resample(&samples[..1024], 44100, 48000, 1);
    let scaled = 500_u128 * 160;
    let (quotient, remainder) = (scaled / 147, scaled % 147);
    let start = (quotient + u128::from(remainder != 0)) as usize;
    assert_pcm(&pcm, &reference[start..]);
}

#[test]
fn invalid_targets_preserve_pending_pcm_and_retired_decoders_stay_retired() {
    use symphonia::core::errors::{Error, SeekErrorKind};
    let data = fixture("1-seek-front.m4a");
    let full = candidate::decode(&data, 16 * 1024 * 1024).unwrap();
    let reference = resample(&full.samples, full.sample_rate, 48000, full.channels);
    let mut d = Timed::<true>::new(48000, Some(data.len() as u64));
    let mut out = Vec::with_capacity(crate::bandlimited::MAX_SCRATCH_SAMPLES);
    let step = d
        .step(&data, false, &mut out, &AtomicBool::new(false))
        .unwrap();
    assert!(out.is_empty());
    assert!(step.consumed > 0);
    for target in [
        -1,
        full.samples.len() as i64 / i64::from(full.channels) + 1,
        i64::MAX,
    ] {
        assert!(matches!(
            d.seek(Timestamp::new(target)),
            Err(Error::SeekError(SeekErrorKind::OutOfRange))
        ));
    }
    assert_pcm(&render(&mut d, &data, step.consumed, 137), &reference);
    for bad_resume in [false, true] {
        let mut d = Timed::<false>::new(48000, Some(data.len() as u64));
        render(&mut d, &data, 0, 137);
        d.seek(Timestamp::new(211)).unwrap();
        let request = d.source_request();
        assert!(d.seek(Timestamp::new(-1)).is_err());
        assert_eq!(d.source_request(), request);
        if bad_resume {
            let Some(M4aSourceRequest::ReadAt(packet)) = request else {
                panic!("missing read request")
            };
            assert!(
                d.resume_at(media(&data, packet.offset), packet.offset + 1)
                    .is_err()
            );
            assert!(
                d.step(&[], true, &mut out, &AtomicBool::new(false))
                    .is_err()
            );
        } else {
            assert_eq!(
                d.step(&[], true, &mut out, &AtomicBool::new(true))
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
fn cached_clocked_seek_and_preroll_have_no_steady_allocation() {
    use migo_alloc_probe::{Burst, assert_no_steady_state_allocation};
    let data = fixture("1-seek-front.m4a");
    let mut d = Timed::<true>::new(48000, Some(data.len() as u64));
    render(&mut d, &data, 0, 100000);
    let mut out = Vec::with_capacity(crate::bandlimited::MAX_SCRATCH_SAMPLES);
    assert_no_steady_state_allocation(
        Burst {
            path: "clocked cached seek and bounded filter preroll",
            warmup: 8,
            measured: 32,
        },
        |_| {
            d.seek(Timestamp::new(211)).unwrap();
            let mut produced = false;
            for _ in 0..128 {
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

#[path = "phase_producer_contracts.rs"]
mod producer;

#[test]
fn exact_terminal_seek_has_no_pcm_and_preserves_repeated_restart() {
    fn check<const B: bool>(data: &[u8], rate: u32, fragment: usize) {
        let full = candidate::decode(data, 16 * 1024 * 1024).unwrap();
        let expected = resample(&full.samples, full.sample_rate, rate, full.channels);
        let frames = (full.samples.len() / full.channels as usize) as i64;
        let mut d = Timed::<B>::new(rate, Some(data.len() as u64));
        assert_pcm(&render(&mut d, data, 0, fragment), &expected);
        for _ in 0..2 {
            assert_eq!(
                d.seek(Timestamp::new(frames)).unwrap().required_ts.get(),
                frames
            );
            assert!(render(&mut d, data, data.len(), fragment).is_empty());
            let mut out = Vec::with_capacity(crate::bandlimited::MAX_SCRATCH_SAMPLES);
            assert_eq!(
                d.step(&[], true, &mut out, &AtomicBool::new(false))
                    .unwrap()
                    .progress,
                Progress::Finished
            );
            assert!(out.is_empty());
            d.seek(Timestamp::ZERO).unwrap();
            assert_pcm(&render(&mut d, data, data.len(), fragment), &expected);
        }
    }
    let mut count = 0;
    for name in [
        "0-seek-front.m4a",
        "0-seek-tail.m4a",
        "1-seek-front.m4a",
        "1-seek-tail.m4a",
        "1-seek-scaled.m4a",
        "1-seek-edit.m4a",
    ] {
        let data = fixture(name);
        for rate in [16000, 44100, 48000] {
            for fragment in [137, 100000] {
                check::<false>(&data, rate, fragment);
                check::<true>(&data, rate, fragment);
                count += 2;
            }
        }
    }
    assert_eq!(count, 72);
    println!("W09_TERMINAL72 exact empty endpoints with two full restarts each");
}
