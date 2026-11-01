use super::*;
use crate::resampler::fixture::audio;
use rubato::{Indexing, Resampler, audioadapter_buffers::direct::InterleavedSlice};

#[test]
fn job_steps_bound_both_sides_and_complete_only_after_the_tail() {
    let cancel = AtomicBool::new(false);
    for (source, target, channels, frames) in [
        (1, 65_537, 2, 2),
        (65_537, 1, 2, 20_000),
        (44_100, 48_000, 32, 513),
        (48001, 3000, 32, 16),
    ] {
        let input = audio(source, channels, frames);
        let want = reference(&input, target);
        let mut job = ResampleJob::new(input, target).unwrap();
        let budget = STEP_SAMPLES.max(channels as usize);
        let mut steps = 0;
        loop {
            let Phase::Running {
                position, output, ..
            } = &job.phase
            else {
                panic!("a resampling job starts incomplete");
            };
            let (old_position, old_output) = (*position, output.samples.len());
            bandlimited::take_trace();
            let done = job.step(&cancel).unwrap();
            let trace = bandlimited::take_trace();
            assert!(
                trace.calls <= 1 && trace.input_samples <= 8192 && trace.output_samples <= 8192
            );
            let output_len = match &job.phase {
                Phase::Running {
                    position, output, ..
                } => {
                    assert!(
                        *position - old_position <= budget,
                        "input work must be bounded"
                    );
                    assert!(!done);
                    output.samples.len()
                }
                Phase::Complete(output) => {
                    assert!(done);
                    output.samples.len()
                }
                Phase::Cancelled | Phase::Failed(_) => panic!("not a successful job"),
            };
            assert!(
                output_len - old_output <= budget,
                "output work must be bounded"
            );
            steps += 1;
            assert!(steps <= want.len() + frames + 1);
            if done {
                break;
            }
        }
        assert!(steps > 1);
        assert!(job.step(&cancel).unwrap());
        let output = job.finish().unwrap();
        assert_eq!(output.samples, want);
        assert_eq!(output.channels, channels);
        assert_eq!(output.sample_rate, target);
    }
}

#[test]
fn cancellation_releases_input_output_and_never_publishes_a_partial_buffer() {
    for stop_after in [0, 1, 9] {
        let mut job = ResampleJob::new(audio(1, 2, 1), 65_537).unwrap();
        let no = AtomicBool::new(false);
        for _ in 0..stop_after {
            assert!(!job.step(&no).unwrap());
        }
        let Phase::Running { input, output, .. } = &job.phase else {
            panic!("still owns incomplete PCM");
        };
        let bytes = (input.samples.capacity() + output.samples.capacity()) * 4;
        let before = migo_alloc_probe::thread_counts();
        assert_eq!(
            job.step(&AtomicBool::new(true)).unwrap_err().code,
            ErrorCode::Cancelled
        );
        let freed = migo_alloc_probe::thread_counts().bytes_freed - before.bytes_freed;
        assert!(
            freed >= bytes as u64,
            "cancellation must release both PCM buffers"
        );
        assert!(matches!(job.phase, Phase::Cancelled));
        assert_eq!(job.step(&no).unwrap_err().code, ErrorCode::Cancelled);
        assert_eq!(job.finish().unwrap_err().code, ErrorCode::Cancelled);
    }
    let mut complete = ResampleJob::new(audio(48_000, 2, 10), 48_000).unwrap();
    assert_eq!(
        complete.step(&AtomicBool::new(true)).unwrap_err().code,
        ErrorCode::Cancelled
    );
    assert_eq!(complete.finish().unwrap_err().code, ErrorCode::Cancelled);
    let mut partial = ResampleJob::new(audio(44_100, 2, 100), 48_000).unwrap();
    assert!(!partial.step(&AtomicBool::new(false)).unwrap());
    assert_eq!(partial.finish().unwrap_err().code, ErrorCode::Internal);
}

#[test]
fn bypass_preserves_the_allocation_and_invalid_pcm_is_rejected_before_output_alloc() {
    let input = audio(48_000, 2, 100);
    let pointer = input.samples.as_ptr();
    let output = ResampleJob::new(input, 48_000).unwrap().finish().unwrap();
    assert_eq!(output.samples.as_ptr(), pointer);
    for (source, target, channels, samples) in [
        (0, 48_000, 2, 2),
        (48_000, 0, 2, 2),
        (1, 2, 0, 2),
        (1, 2, 2, 3),
        (48000, 44100, 33, 33),
    ] {
        let input = DecodedAudio {
            samples: vec![0.0; samples],
            sample_rate: source,
            channels,
        };
        let error = ResampleJob::new(input, target)
            .err()
            .expect("invalid input");
        assert_eq!(error.code, ErrorCode::InvalidArgument);
    }
    let input = audio(1, 2, 1);
    let error = ResampleJob::new(input, crate::limits::MAX_AUDIO_PCM_SAMPLES as u32 / 2 + 1)
        .err()
        .expect("over-budget output");
    assert_eq!(error.code, ErrorCode::InvalidArgument);
}

#[test]
fn full_buffer_resampling_keeps_output_capacity_inside_the_pcm_ceiling() {
    // A tiny low-rate WAV can require nearly 64 MiB at the device's rate.
    // Reserving an estimated chunk plus one frame used to double the output
    // allocation even though its logical sample count passed the PCM limit.
    let input = audio(1, 2, 190);
    let output = crate::resampler::resample_if_needed(input, 44_100).unwrap();
    assert_eq!(output.samples.len(), 190 * 44_100 * 2);
    assert!(output.samples.capacity() as u64 <= crate::limits::MAX_AUDIO_PCM_SAMPLES);
}

#[test]
fn productive_resample_steps_reuse_all_preallocated_storage() {
    let mut job = ResampleJob::new(audio(44_100, 2, 600_000), 48_000).unwrap();
    let cancel = AtomicBool::new(false);
    for _ in 0..32 {
        if matches!(&job.phase,Phase::Running {output,..} if !output.samples.is_empty()) {
            break;
        }
        assert!(!job.step(&cancel).unwrap());
    }
    assert!(matches!(&job.phase,Phase::Running {output,..} if !output.samples.is_empty()));
    migo_alloc_probe::assert_no_steady_state_allocation(
        migo_alloc_probe::Burst {
            path: "whole-buffer bounded resample step",
            warmup: 8,
            measured: 64,
        },
        |_| {
            assert!(!job.step(&cancel).unwrap());
            let Phase::Running { output, .. } = &job.phase else {
                panic!("productive step");
            };
            assert!(!output.samples.is_empty());
        },
    );
}

#[test]
fn job_keeps_its_phase_and_pcm_across_real_audio_cpu_handoffs() {
    let input = audio(3, 2, 101);
    let want = reference(&input, 997);
    let host = migo_io::scheduler::IoScheduler::new(0);
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .build()
        .unwrap();
    runtime.block_on(async {
        let mut owner = crate::off_worker::OffWorker::new(
            Some(ResampleJob::new(input, 997).unwrap()),
            host.audio_cpu(),
        );
        let mut steps = 0;
        loop {
            let (next, result) = owner
                .with(|job| {
                    let job = job.as_mut().unwrap();
                    job.step(&AtomicBool::new(false))
                })
                .await
                .unwrap();
            owner = next;
            steps += 1;
            if result.unwrap() {
                break;
            }
            // An independent task gets a CPU turn between suspended DSP steps.
            assert_eq!(host.audio_cpu().run(|| 123).await.unwrap(), 123);
        }
        assert!(steps > 2);
        let (_, output) = owner
            .with(|job| job.take().unwrap().finish().unwrap())
            .await
            .unwrap();
        assert_eq!(output.samples, want);
        assert_eq!(output.frame_count(), (101_u64 * 997).div_ceil(3) as usize);
    });
}

// Independent complete-buffer oracle: the raw mature kernel sees the complete
// planar-time source through indexing offsets, without the product's queues,
// partial input storage, phase machine or publication path.
pub(crate) fn reference(input: &DecodedAudio, target: u32) -> Vec<f32> {
    if input.sample_rate == target || input.samples.is_empty() {
        return input.samples.clone();
    }
    let channels = input.channels as usize;
    let original_frames = input.frame_count() as u128;
    let original_rate = input.sample_rate;
    let mut data = input.samples.clone();
    let mut source = input.sample_rate;
    let mut plan = Vec::new();
    let mut rate = source;
    while u64::from(rate) > u64::from(target) * 4 {
        rate = rate.div_ceil(4);
        plan.push(rate);
    }
    plan.push(target);
    for output in plan {
        let ratio = f64::from(output) / f64::from(source);
        let max_input = (((8192 / channels) - 10) as f64 / ratio).floor() as usize;
        let (fixed, chunk) = if max_input > 0 {
            (rubato::FixedAsync::Input, max_input.min(128))
        } else {
            (rubato::FixedAsync::Output, 8192 / channels)
        };
        let mut core = rubato::Async::<f32>::new_sinc(
            ratio,
            1.,
            &rubato::SincInterpolationParameters::default(),
            chunk,
            channels,
            fixed,
        )
        .unwrap();
        let n = data.len() / channels;
        let count =
            (original_frames * u128::from(output)).div_ceil(u128::from(original_rate)) as usize;
        let delay = core.output_delay();
        let max = core
            .process_all_needed_output_len(n)
            .max(delay + count + core.output_frames_max());
        let mut raw = vec![0.; max * channels];
        let input_view = InterleavedSlice::new(&data, channels, n).unwrap();
        let mut destination = InterleavedSlice::new_mut(&mut raw, channels, max).unwrap();
        let (mut consumed, mut produced) = (0, 0);
        for _ in 0..1_000_000 {
            if consumed == n && produced >= delay + count {
                break;
            }
            let need = core.input_frames_next();
            let take = (n - consumed).min(need);
            let indexing = Indexing::new()
                .input_offset(consumed)
                .output_offset(produced)
                .partial_len(take);
            let (used, out) = core
                .process_into_buffer(&input_view, &mut destination, Some(&indexing))
                .unwrap();
            assert_eq!(used, need);
            consumed += take;
            produced += out;
        }
        assert_eq!(consumed, n);
        assert!(produced >= delay + count);
        data = raw[delay * channels..(delay + count) * channels].to_vec();
        source = output;
    }
    data
}

fn run(mut job: ResampleJob) -> DecodedAudio {
    for _ in 0..MAX_STEPS {
        bandlimited::take_trace();
        let done = job.step(&AtomicBool::new(false)).unwrap();
        let t = bandlimited::take_trace();
        assert!(t.calls <= 1 && t.input_samples <= 8192 && t.output_samples <= 8192);
        if done {
            return job.finish().unwrap();
        }
    }
    panic!("SRC made no bounded progress")
}
#[test]
fn cascaded_rates_keep_the_original_ceil_clock_and_all_reference_pcm() {
    for (source, target) in [
        (48001, 3000),
        (768000, 3000),
        (48000, 8000),
        (44100, 48000),
        (3000, 768000),
    ] {
        for channels in [1, 2, 7, 32] {
            for frames in [1, 16, 47, 513] {
                let input = audio(source, channels, frames);
                let want = reference(&input, target);
                let actual = run(ResampleJob::new(input, target).unwrap());
                assert_eq!(
                    actual.frame_count(),
                    (frames as u64 * u64::from(target)).div_ceil(u64::from(source)) as usize
                );
                assert_eq!(
                    actual.samples, want,
                    "{source}->{target}/{channels}/{frames}"
                );
            }
        }
    }
}
fn tone(rate: u32, frequency: f64) -> DecodedAudio {
    DecodedAudio {
        samples: (0..rate)
            .map(|i| {
                (0.5 * (std::f64::consts::TAU * frequency * f64::from(i) / f64::from(rate)).sin())
                    as f32
            })
            .collect(),
        sample_rate: rate,
        channels: 1,
    }
}
fn rms(samples: &[f32]) -> f64 {
    (samples.iter().map(|v| f64::from(*v).powi(2)).sum::<f64>() / samples.len() as f64).sqrt()
}
#[test]
fn bandlimiting_keeps_passband_and_rejects_aliases_at_extreme_downsample_ratios() {
    for (source, target) in [(48000, 16000), (48000, 3000), (768000, 3000), (96000, 8000)] {
        for pass in [true, false] {
            let frequency = f64::from(target) * if pass { 0.1 } else { 0.75 };
            let output =
                crate::resampler::resample_if_needed(tone(source, frequency), target).unwrap();
            let level = rms(&output.samples[512..output.samples.len() - 512]);
            if pass {
                assert!(
                    (level / (0.5 / 2f64.sqrt()) - 1.).abs() < 0.005,
                    "passband {source}->{target}: {level}"
                );
            } else {
                let db = 20. * (level / (0.5 / 2f64.sqrt())).log10();
                assert!(db < -85., "alias {source}->{target}: {db}dB");
            }
        }
    }
}
#[test]
fn setup_stages_and_total_work_errors_are_terminal_and_release_owned_pcm() {
    let cancel = AtomicBool::new(false);
    let mut setup = ResampleJob::new(audio(768000, 2, 9737), 3000).unwrap();
    for count in 1..=4 {
        bandlimited::take_trace();
        assert!(!setup.step(&cancel).unwrap());
        assert_eq!(bandlimited::take_trace().calls, 0);
        let Phase::Running {
            pipeline,
            position,
            output,
            ..
        } = &setup.phase
        else {
            panic!("setup")
        };
        assert_eq!(
            pipeline.prepared_layers(),
            count,
            "only one filter layer may initialize per step"
        );
        assert_eq!(*position, 0);
        assert!(output.samples.is_empty());
    }
    for stop in 0..5 {
        let mut job = ResampleJob::new(audio(768000, 2, 9737), 3000).unwrap();
        for _ in 0..stop {
            assert!(!job.step(&cancel).unwrap());
        }
        let before = migo_alloc_probe::thread_counts();
        assert_eq!(
            job.step(&AtomicBool::new(true)).unwrap_err().code,
            ErrorCode::Cancelled
        );
        assert!(migo_alloc_probe::thread_counts().bytes_freed > before.bytes_freed);
        assert!(job.finish().is_err());
    }
    let mut job = ResampleJob::new(audio(48000, 2, 9737), 16000).unwrap();
    if let Phase::Running { steps, .. } = &mut job.phase {
        *steps = MAX_STEPS;
    }
    assert_eq!(
        job.step(&cancel).unwrap_err().code,
        ErrorCode::InvalidArgument
    );
    assert!(matches!(
        job.phase,
        Phase::Failed(ErrorCode::InvalidArgument)
    ));
    assert_eq!(
        job.step(&cancel).unwrap_err().code,
        ErrorCode::InvalidArgument
    );
    assert!(job.finish().is_err());
}

#[test]
fn converted_pcm_keeps_its_reserved_pointer_without_allocating_through_flush() {
    let mut job = ResampleJob::new(audio(48000, 2, 9737), 16000).unwrap();
    loop {
        let Phase::Running { pipeline, .. } = &job.phase else {
            panic!("setup")
        };
        if !pipeline.is_preparing() {
            break;
        }
        assert!(!job.step(&AtomicBool::new(false)).unwrap());
    }
    let Phase::Running { output, .. } = &job.phase else {
        panic!("running")
    };
    let pointer = output.samples.as_ptr();
    for _ in 0..1000 {
        let before = migo_alloc_probe::thread_counts();
        let done = job.step(&AtomicBool::new(false)).unwrap();
        let after = migo_alloc_probe::thread_counts();
        assert_eq!(
            after.allocations, before.allocations,
            "processing or completion allocated"
        );
        assert_eq!(after.reallocations, before.reallocations);
        if done {
            let out = job.finish().unwrap();
            assert_eq!(out.samples.as_ptr(), pointer);
            assert_eq!(
                out.frame_count(),
                (9737_u64 * 16000).div_ceil(48000) as usize
            );
            return;
        }
    }
    panic!("complete PCM did not finish")
}

#[test]
fn output_fixed_quanta_obey_actual_library_and_copy_budgets() {
    let mut job = ResampleJob::new(audio(1, 2, 1), 16385).unwrap();
    for _ in 0..MAX_STEPS {
        let (old_input, old_output) = match &job.phase {
            Phase::Running {
                position, output, ..
            } => (*position, output.samples.len()),
            _ => panic!("running"),
        };
        bandlimited::take_trace();
        let done = job.step(&AtomicBool::new(false)).unwrap();
        let trace = bandlimited::take_trace();
        assert!(
            trace.calls <= 1 && trace.input_samples <= 8192 && trace.output_samples <= 8192,
            "actual kernel work exceeded one step"
        );
        if done {
            assert_eq!(job.finish().unwrap().samples.len(), 32770);
            return;
        }
        if let Phase::Running {
            position, output, ..
        } = &job.phase
        {
            assert!(*position - old_input <= 8192);
            assert!(output.samples.len() - old_output <= 8192);
        }
    }
    panic!("output-fixed stream failed to finish")
}

#[test]
fn constructing_an_owner_resets_reused_library_history_and_phase() {
    let input = audio(44100, 2, 9737);
    let expected = reference(&input, 48000);
    let mut used = kernel(44100, 48000, 2).unwrap();
    let warm = vec![0.75; used.input_frames_next() * 2];
    let mut discarded = vec![0.; used.output_frames_max() * 2];
    let discarded_frames = discarded.len() / 2;
    for _ in 0..4 {
        used.process_into_buffer(
            &InterleavedSlice::new(&warm, 2, warm.len() / 2).unwrap(),
            &mut InterleavedSlice::new_mut(&mut discarded, 2, discarded_frames).unwrap(),
            None,
        )
        .unwrap();
    }
    let mut stream = bandlimited::Stream::new(used, 44100, 48000, 2).unwrap();
    let mut scratch = Vec::with_capacity(stream.output_capacity());
    let mut actual = Vec::new();
    let mut position = 0;
    for _ in 0..1000 {
        scratch.clear();
        let s = stream
            .step(
                &input.samples[position..],
                position == input.samples.len(),
                &mut scratch,
            )
            .unwrap();
        position += s.consumed;
        actual.extend_from_slice(&scratch);
        if matches!(s.progress, bandlimited::Progress::Finished) {
            assert_eq!(actual, expected);
            return;
        }
    }
    panic!("reused state did not finish")
}
