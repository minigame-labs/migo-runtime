//! Absolute-clock windows compared with independent whole Rubato renders.
use super::*;
use crate::{resample_job::tests::reference, resampler::fixture::audio};

fn scaled(frame: u64, source: u32, target: u32) -> u64 {
    u64::try_from((u128::from(frame) * u128::from(target)).div_ceil(u128::from(source))).unwrap()
}
fn period(source: u32, target: u32) -> u64 {
    let mut common = source;
    for rate in rates(source, target) {
        let (mut a, mut b) = (common, rate);
        while b != 0 {
            (a, b) = (b, a % b);
        }
        common = a;
    }
    u64::from(source / common)
}
fn render(mut stream: Stream, input: &[f32], partition: usize) -> Vec<f32> {
    let mut result = Vec::new();
    let mut offset = 0;
    let mut out = Vec::with_capacity(STEP_SAMPLES);
    for _ in 0..2_000_000 {
        let stop = (offset + partition * stream.channels).min(input.len());
        let preparing = stream.is_preparing();
        out.clear();
        bandlimited::take_trace();
        let before = migo_alloc_probe::thread_counts();
        let step = stream
            .step(
                &input[offset..stop],
                offset == input.len(),
                &mut out,
                &AtomicBool::new(false),
            )
            .expect("absolute SRC rejected a complete window");
        let after = migo_alloc_probe::thread_counts();
        let trace = bandlimited::take_trace();
        assert!(
            trace.calls <= 1 && trace.input_samples <= 8192 && trace.output_samples <= 8192,
            "absolute SRC exceeded its original kernel step budget"
        );
        assert!(
            step.consumed <= stop - offset
                && step.consumed <= STEP_SAMPLES
                && out.len() <= STEP_SAMPLES,
            "absolute SRC exceeded its original sample budget"
        );
        if !preparing {
            assert_eq!(
                (after.allocations, after.reallocations),
                (before.allocations, before.reallocations),
                "absolute SRC processing grew storage"
            );
        }
        offset += step.consumed;
        result.extend_from_slice(&out);
        if step.progress == bandlimited::Progress::Finished {
            assert_eq!(
                offset,
                input.len(),
                "absolute SRC discarded its source tail"
            );
            return result;
        }
    }
    panic!("absolute SRC failed its bounded window");
}
fn compare(got: &[f32], want: &[f32], key: &str) {
    assert_eq!(
        got.len(),
        want.len(),
        "absolute SRC suffix length changed: {key}"
    );
    let error = got
        .iter()
        .zip(want)
        .map(|(a, b)| (*a - *b).abs())
        .fold(0.0f32, f32::max);
    assert!(
        error <= 0.00001 && got.iter().chain(want).all(|s| s.is_finite()),
        "absolute SRC suffix PCM changed: {key}, error={error}"
    );
}
#[test]
fn known_end_windows_match_the_original_full_track_grid_and_complete_tail() {
    for (source, target, channels, frames) in [
        (48000, 16000, 1, 9737),
        (44100, 48000, 2, 8193),
        (100000, 10000, 2, 32769),
        (768000, 3000, 2, 80001),
        (48000, 48000, 2, 4097),
        (48001, 3000, 32, 17),
    ] {
        let data = audio(source, channels, frames);
        let expected = reference(&data, target);
        for requested in [frames / 2, frames - 1, frames] {
            let history = if source == 768000 { 50000 } else { 2048 };
            let origin = (requested.saturating_sub(history) as u64) / period(source, target)
                * period(source, target);
            let start = scaled(requested as u64, source, target);
            for partition in [137, 1024] {
                let stream =
                    Stream::new_at(source, target, channels, Some(frames as u64), origin, start)
                        .unwrap();
                assert_eq!(
                    stream.input_frames, origin,
                    "SRC input clock lost absolute origin"
                );
                assert_eq!(
                    stream.output_frames, start,
                    "SRC output clock lost publication start"
                );
                let got = render(
                    stream,
                    &data.samples[origin as usize * channels as usize..],
                    partition,
                );
                let key = format!("{source}/{target}/{channels}/{frames}/{requested}/{partition}");
                compare(&got, &expected[start as usize * channels as usize..], &key);
                if std::env::var_os("MIGO_DUMP_SINC_ORIGIN_PCM").is_some() {
                    println!(
                        "MIGO_SINC_ORIGIN_PCM {key} {:?}",
                        got.iter().map(|s| s.to_bits()).collect::<Vec<_>>()
                    );
                }
            }
        }
    }
}
#[test]
fn unknown_end_and_exact_endpoint_keep_a_separate_real_eof() {
    for (source, target) in [(44100, 48000), (100000, 10000), (48000, 48000)] {
        let data = audio(source, 2, 4097);
        let full = reference(&data, target);
        let origin = period(source, target) * 2;
        for frame in [2048, 4096, 4097] {
            let start = scaled(frame, source, target);
            let got = render(
                Stream::new_at(source, target, 2, None, origin, start).unwrap(),
                &data.samples[origin as usize * 2..],
                3,
            );
            compare(&got, &full[start as usize * 2..], "unknown endpoint");
        }
    }
    let mut stream = Stream::new_at(48000, 48000, 1, Some(3), 2, 3).unwrap();
    let mut out = Vec::with_capacity(STEP_SAMPLES);
    let step = stream
        .step(&[0.5], false, &mut out, &AtomicBool::new(false))
        .unwrap();
    assert!(
        out.is_empty() && step.progress != bandlimited::Progress::Finished,
        "absolute SRC invented EOF at the publication boundary"
    );
    assert_eq!(
        stream
            .step(&[], true, &mut out, &AtomicBool::new(false))
            .unwrap()
            .progress,
        bandlimited::Progress::Finished
    );
}
#[test]
fn every_cascade_grid_is_checked_before_any_filter_is_built() {
    bandlimited::take_trace();
    // Origin10 is integral at the final10000Hz grid, but half-integral at25000Hz.
    assert!(
        Stream::new_at(100000, 10000, 1, Some(10000), 10, 100).is_err(),
        "SRC accepted an origin off an intermediate grid"
    );
    for (source, target, end, origin, start) in [
        (44100, 48000, Some(10000), 1, 100),
        (48000, 16000, Some(10000), 30, 9),
        (48000, 16000, Some(10), 30, 10),
        (48000, 16000, Some(30), 0, 11),
        (1, 2, None, u64::MAX, u64::MAX),
    ] {
        assert!(
            Stream::new_at(source, target, 1, end, origin, start).is_err(),
            "SRC accepted an invalid absolute window"
        );
    }
    assert_eq!(bandlimited::take_trace().calls, 0);
}
#[test]
fn positions_above_f64_integer_precision_run_real_kernels_on_the_same_grid() {
    for (source, target) in [(44100, 48000), (100000, 10000), (768000, 3000)] {
        let p = period(source, target);
        let base = (9_007_199_254_740_993u64 / p) * p;
        let data = audio(source, 2, 2049);
        let full = reference(&data, target);
        let origin = scaled(base, source, target);
        let skip = scaled(1025, source, target);
        let stream = Stream::new_at(source, target, 2, Some(base + 2049), base, origin + skip)
            .expect("absolute SRC lost integer-only origin arithmetic");
        let got = render(stream, &data.samples, 127);
        compare(
            &got,
            &full[skip as usize * 2..],
            "large integer translation",
        );
    }
}
#[test]
fn bypass_discards_only_preroll_and_preserves_capacity_and_overflow_refusals() {
    let got = render(
        Stream::new_at(48000, 48000, 2, Some(10), 4, 7).unwrap(),
        &[1., 2., 3., 4., 5., 6., 7., 8., 9., 10., 11., 12.],
        1,
    );
    assert_eq!(
        got,
        [7., 8., 9., 10., 11., 12.],
        "bypass published its absolute preroll"
    );
    for (end, origin, start, input, capacity) in [
        (Some(10), 4, 7, vec![1.; 12], 1),
        (Some(10), 4, 7, vec![1.; 1], 8192),
        (None, u64::MAX, u64::MAX, vec![1.; 2], 8192),
    ] {
        let mut stream = Stream::new_at(48000, 48000, 2, end, origin, start).unwrap();
        let mut out = Vec::with_capacity(capacity);
        assert!(
            stream
                .step(&input, false, &mut out, &AtomicBool::new(false))
                .is_err(),
            "absolute bypass ignored capacity/frame/clock bounds"
        );
        assert!(out.is_empty());
        assert!(
            stream
                .step(&[], true, &mut out, &AtomicBool::new(false))
                .is_err(),
            "failed absolute stream was resurrected"
        );
    }
}
#[test]
fn shortened_input_cannot_finish_an_absolute_window_or_succeed_after_cancellation() {
    for source in [48000, 100000] {
        let mut stream = Stream::new_at(source, 16000, 1, Some(10000), 0, 100).unwrap();
        let mut out = Vec::with_capacity(STEP_SAMPLES);
        assert!(
            stream
                .step(&[], true, &mut out, &AtomicBool::new(false))
                .is_err(),
            "absolute SRC accepted a shortened source"
        );
        assert!(out.is_empty());
    }
    let mut stream = Stream::new_at(48000, 16000, 1, Some(10000), 3, 100).unwrap();
    let mut out = Vec::with_capacity(STEP_SAMPLES);
    assert_eq!(
        stream
            .step(&[], false, &mut out, &AtomicBool::new(true))
            .unwrap_err()
            .code,
        ErrorCode::Cancelled
    );
    assert_eq!(
        stream
            .step(&[], false, &mut out, &AtomicBool::new(false))
            .unwrap_err()
            .code,
        ErrorCode::Cancelled,
        "cancelled absolute SRC was resurrected"
    );
}
#[tokio::test]
async fn original_cpu_cancellation_really_frees_absolute_window_capacity() {
    use std::sync::Arc;
    let host = migo_io::scheduler::IoScheduler::new(1051);
    let peer = migo_io::scheduler::IoScheduler::new(1052);
    let owner = crate::off_worker::OffWorker::new(None::<Stream>, host.audio_cpu());
    let (owner, watches) = owner
        .with(|slot| {
            let mut stream = Stream::new_at(768000, 3000, 2, Some(80001), 768, 100).unwrap();
            let mut out = Vec::with_capacity(STEP_SAMPLES);
            while stream.is_preparing() {
                stream
                    .step(&[], false, &mut out, &AtomicBool::new(false))
                    .unwrap();
            }
            let watches = stream
                .layers
                .iter()
                .map(|l| {
                    (
                        migo_alloc_probe::DeallocationWatch::new(l.pcm.as_ptr().cast()).unwrap(),
                        l.pcm.capacity() * 4,
                    )
                })
                .collect::<Vec<_>>();
            assert!(!watches.is_empty());
            *slot = Some(stream);
            watches
        })
        .await
        .unwrap();
    let (start, started) = std::sync::mpsc::channel();
    let (release, blocked) = std::sync::mpsc::channel();
    let cancelled = Arc::new(AtomicBool::new(false));
    let flag = cancelled.clone();
    let bytes = watches.iter().map(|(_, n)| *n as u64).sum::<u64>();
    let task = tokio::spawn(owner.with(move |slot| {
        start.send(()).unwrap();
        blocked
            .recv_timeout(std::time::Duration::from_secs(15))
            .unwrap();
        let mut out = Vec::with_capacity(STEP_SAMPLES);
        let before = migo_alloc_probe::thread_counts();
        assert_eq!(
            slot.as_mut()
                .unwrap()
                .step(&[], false, &mut out, &flag)
                .unwrap_err()
                .code,
            ErrorCode::Cancelled
        );
        assert!(
            migo_alloc_probe::thread_counts().bytes_freed - before.bytes_freed >= bytes,
            "original CPU did not free the absolute SRC capacity"
        );
        assert!(
            std::thread::current()
                .name()
                .unwrap_or("")
                .starts_with("Migo-IO-")
        );
    }));
    tokio::task::spawn_blocking(move || {
        started
            .recv_timeout(std::time::Duration::from_secs(15))
            .unwrap()
    })
    .await
    .unwrap();
    cancelled.store(true, Ordering::Release);
    let held = watches.iter().all(|(w, _)| w.freed_bytes().is_none());
    assert_eq!(peer.audio_cpu().run(|| 17).await.unwrap(), 17);
    release.send(()).unwrap();
    let (owner, ()) = task.await.unwrap().unwrap();
    assert!(
        held,
        "absolute SRC storage freed before original CPU completion"
    );
    assert!(
        watches.iter().all(|(w, n)| w.freed_bytes() == Some(*n)),
        "original CPU failed exact SRC allocation destruction"
    );
    drop(owner);
}
