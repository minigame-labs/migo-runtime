use super::*;

fn input(samples: usize) -> Vec<i16> {
    (0..samples).map(|i| ((i * 193) % 65_536) as i16).collect()
}

fn append_all(collector: &mut PcmCollector, values: &[i16], partition: usize) {
    for chunk in values.chunks(partition) {
        collector.append_i16(chunk).unwrap();
    }
}

fn output_len(collector: &PcmCollector) -> usize {
    match &collector.phase {
        Phase::Flattening { output, .. } | Phase::Complete(output) => output.len(),
        _ => 0,
    }
}

#[test]
fn every_partition_preserves_sample_order_and_keeps_existing_pages_in_place() {
    for length in [0, 1, 8191, 8192, 8193, 16_383, 16_384, 16_385, 49_169] {
        let values = input(length);
        for partition in [1, 17, 2304, 8192] {
            let mut collector = PcmCollector::new();
            for chunk in values.chunks(partition) {
                let pointers: Vec<_> = match &collector.phase {
                    Phase::Collecting { pages, .. } => pages.iter().map(|p| p.as_ptr()).collect(),
                    _ => unreachable!(),
                };
                collector.append_i16(chunk).unwrap();
                let Phase::Collecting { pages, .. } = &collector.phase else {
                    unreachable!();
                };
                assert!(pages.iter().all(|page| page.capacity() <= 16_384));
                assert!(
                    pointers
                        .iter()
                        .zip(pages)
                        .all(|(pointer, page)| *pointer == page.as_ptr())
                );
            }
            collector.seal().unwrap();
            loop {
                let before = output_len(&collector);
                let done = collector.step().unwrap();
                assert!(
                    output_len(&collector) - before <= 8192,
                    "actual copied output must obey the budget"
                );
                if done {
                    break;
                }
            }
            assert!(collector.step().unwrap());
            let expected: Vec<_> = values.iter().map(|v| f32::from(*v) / 32768.0).collect();
            assert_eq!(collector.finish().unwrap(), expected);
        }
    }
}

#[test]
fn append_and_flatten_bound_allocations_and_never_reallocate_the_final_prefix() {
    let values = input(400_019);
    let mut collector = PcmCollector::new();
    for chunk in values.chunks(8192) {
        let before = migo_alloc_probe::thread_counts();
        collector.append_i16(chunk).unwrap();
        let after = migo_alloc_probe::thread_counts();
        assert!(
            after.bytes_allocated - before.bytes_allocated <= 131_072,
            "append must not grow a whole PCM prefix"
        );
    }
    collector.seal().unwrap();
    let Phase::Flattening { output, .. } = &collector.phase else {
        unreachable!();
    };
    assert!(
        output.is_empty(),
        "sealing must not initialize or copy the complete output"
    );
    let pointer = output.as_ptr();
    let capacity = output.capacity();
    loop {
        let before = migo_alloc_probe::thread_counts();
        let old_len = output_len(&collector);
        let done = collector.step().unwrap();
        let after = migo_alloc_probe::thread_counts();
        assert_eq!(after.allocations, before.allocations);
        assert_eq!(after.reallocations, before.reallocations);
        assert!(
            after.deallocations - before.deallocations <= 3,
            "at most two PCM pages and the descriptor storage retire"
        );
        assert!(output_len(&collector) - old_len <= 8192);
        match &collector.phase {
            Phase::Flattening { output, .. } | Phase::Complete(output) => {
                assert_eq!(output.as_ptr(), pointer);
                assert_eq!(output.capacity(), capacity);
            }
            _ => unreachable!(),
        }
        if done {
            break;
        }
    }
    assert_eq!(collector.finish().unwrap().len(), values.len());
}

#[test]
fn pages_release_as_their_last_samples_are_copied() {
    let mut collector = PcmCollector::new();
    append_all(&mut collector, &input(32_781), 8192);
    collector.seal().unwrap();
    assert!(!collector.step().unwrap());
    let Phase::Flattening { pages, .. } = &collector.phase else {
        unreachable!();
    };
    assert_eq!(pages.len(), 3);
    let before = migo_alloc_probe::thread_counts();
    assert!(!collector.step().unwrap());
    let after = migo_alloc_probe::thread_counts();
    assert!(after.bytes_freed - before.bytes_freed >= 65_536);
    let Phase::Flattening { pages, .. } = &collector.phase else {
        unreachable!();
    };
    assert_eq!(pages.len(), 2);
    while !collector.step().unwrap() {}
    assert_eq!(collector.finish().unwrap().len(), 32_781);
}

#[test]
fn exact_pcm_limit_is_accepted_and_limit_plus_one_releases_the_whole_prefix() {
    let limit = crate::limits::MAX_AUDIO_PCM_SAMPLES as usize;
    let chunk = vec![0_i16; 8192];
    for reject in [false, true] {
        let mut collector = PcmCollector::new();
        for _ in 0..limit / chunk.len() {
            collector.append_i16(&chunk).unwrap();
        }
        let Phase::Collecting { pages, samples, .. } = &collector.phase else {
            unreachable!();
        };
        assert_eq!(*samples, limit);
        assert!(pages.iter().map(|page| page.capacity()).sum::<usize>() <= limit);
        assert!(pages.capacity() * std::mem::size_of::<Vec<f32>>() <= 32_768);
        if reject {
            let before = migo_alloc_probe::thread_counts();
            assert!(
                matches!(collector.append_i16(&[1]), Err(e) if e.code == ErrorCode::InvalidArgument)
            );
            assert!(
                migo_alloc_probe::thread_counts().bytes_freed - before.bytes_freed
                    >= (limit * 4) as u64
            );
            assert!(matches!(collector.phase, Phase::Failed));
            assert!(collector.step().is_err());
            assert!(collector.finish().is_err());
        } else {
            collector.seal().unwrap();
            while !collector.step().unwrap() {}
            let output = collector.finish().unwrap();
            assert_eq!(output.len(), limit);
            assert!(output.capacity() <= limit);
            assert!(output.iter().all(|value| *value == 0.0));
        }
    }
}

#[test]
fn incomplete_or_invalid_protocol_never_publishes_partial_pcm() {
    let mut collector = PcmCollector::new();
    assert!(collector.step().is_err());
    collector.append_i16(&[i16::MIN, i16::MAX]).unwrap();
    assert!(collector.finish().is_err());
    let mut collector = PcmCollector::new();
    collector.append_i16(&[1; 8192]).unwrap();
    assert!(collector.append_i16(&[0; 8193]).is_err());
    assert!(matches!(collector.phase, Phase::Failed));
    assert!(collector.finish().is_err());
    let mut collector = PcmCollector::new();
    collector.append_i16(&[1; 8192]).unwrap();
    collector.seal().unwrap();
    assert!(collector.append_i16(&[2]).is_err());
    assert!(collector.seal().is_err());
    assert!(collector.finish().is_err());
    let mut empty = PcmCollector::new();
    empty.append_i16(&[]).unwrap();
    empty.seal().unwrap();
    assert!(empty.step().unwrap());
    assert!(empty.finish().unwrap().is_empty());
}

#[test]
fn dropping_each_phase_releases_all_retained_storage() {
    for steps in [None, Some(0), Some(1), Some(10)] {
        let mut collector = PcmCollector::new();
        append_all(&mut collector, &input(32_781), 8192);
        if let Some(steps) = steps {
            collector.seal().unwrap();
            for _ in 0..steps {
                collector.step().unwrap();
            }
        }
        let bytes = match &collector.phase {
            Phase::Collecting { pages, .. } => {
                pages.iter().map(|p| p.capacity() * 4).sum::<usize>()
            }
            Phase::Flattening { pages, output, .. } => {
                pages.iter().map(|p| p.capacity() * 4).sum::<usize>() + output.capacity() * 4
            }
            Phase::Complete(output) => output.capacity() * 4,
            Phase::Failed => unreachable!(),
        };
        let before = migo_alloc_probe::thread_counts();
        drop(collector);
        assert!(migo_alloc_probe::thread_counts().bytes_freed - before.bytes_freed >= bytes as u64);
    }
}

#[test]
fn normalized_float_collection_preserves_every_sample_bit_and_copy_budget() {
    let patterns = [
        0,
        0x8000_0000,
        1,
        0x007f_ffff,
        0x3f80_0000,
        0xbf80_0000,
        0x7f80_0000,
        0xff80_0000,
        0x7fc0_1234,
        0xffc0_4321,
    ];
    let values: Vec<f32> = (0..49_169)
        .map(|i| f32::from_bits(patterns[i % patterns.len()]))
        .collect();
    for partition in [1, 17, 8192] {
        let mut collector = PcmCollector::new();
        for chunk in values.chunks(partition) {
            collector
                .append_with(chunk, std::convert::identity)
                .unwrap();
        }
        collector.seal().unwrap();
        loop {
            let len = output_len(&collector);
            let done = collector.step().unwrap();
            assert!(output_len(&collector) - len <= 8192);
            if done {
                break;
            }
        }
        let actual = collector.finish().unwrap();
        assert_eq!(
            actual.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
            values.iter().map(|v| v.to_bits()).collect::<Vec<_>>()
        );
    }
}

#[test]
fn normalized_float_collection_obeys_the_same_admission_and_allocation_limits() {
    let values = vec![0.25_f32; 8192];
    let mut collector = PcmCollector::new();
    for _ in 0..9 {
        let before = migo_alloc_probe::thread_counts();
        collector
            .append_with(&values, std::convert::identity)
            .unwrap();
        assert!(
            migo_alloc_probe::thread_counts().bytes_allocated - before.bytes_allocated <= 131_072
        );
    }
    let before = migo_alloc_probe::thread_counts();
    assert!(
        collector
            .append_with(&[0.0; 8193], std::convert::identity)
            .is_err()
    );
    assert!(migo_alloc_probe::thread_counts().bytes_freed > before.bytes_freed);
    assert!(collector.finish().is_err());
}

#[test]
fn declared_partitions_write_once_without_reallocation_and_preserve_every_bit() {
    let bits = [
        0_u32,
        0x8000_0000,
        1,
        0x807f_ffff,
        0x3e80_0000,
        0x7f80_0000,
        0xff80_0000,
        0x7fc1_2345,
    ];
    for length in [0, 1, 8191, 8192, 8193, 16_385, 49_169] {
        let values: Vec<_> = (0..length)
            .map(|i| f32::from_bits(bits[i % bits.len()]))
            .collect();
        for partition in [1, 17, 8192] {
            let mut collector = PcmCollector::with_expected_samples(Some(length));
            assert!(collector.needs_reservation());
            let before = migo_alloc_probe::thread_counts();
            assert!(
                collector
                    .append_with(&[], std::convert::identity::<f32>)
                    .is_err()
            );
            let after = migo_alloc_probe::thread_counts();
            assert!(after.bytes_allocated - before.bytes_allocated < 1024);
            assert_eq!(after.reallocations, before.reallocations);
            assert!(!collector.step().unwrap());
            assert!(!collector.needs_reservation());
            let pointer = match &collector.phase {
                Phase::Collecting { pages, samples, .. } => {
                    assert_eq!(*samples, 0, "reservation must not initialize samples");
                    assert_eq!(pages.len(), usize::from(length != 0));
                    pages.front().map(|page| {
                        assert_eq!(page.len(), 0);
                        assert_eq!(page.capacity(), length);
                        page.as_ptr()
                    })
                }
                _ => unreachable!(),
            };
            for chunk in values.chunks(partition) {
                let before = migo_alloc_probe::thread_counts();
                collector
                    .append_with(chunk, std::convert::identity)
                    .unwrap();
                assert_eq!(migo_alloc_probe::thread_counts(), before);
                let Phase::Collecting { pages, samples, .. } = &collector.phase else {
                    unreachable!()
                };
                assert_eq!(pages.len(), 1);
                assert_eq!(pages[0].len(), *samples);
                assert_eq!(Some(pages[0].as_ptr()), pointer);
            }
            let before = migo_alloc_probe::thread_counts();
            collector.seal().unwrap();
            assert!(collector.step().unwrap());
            let output = collector.finish().unwrap();
            let after = migo_alloc_probe::thread_counts();
            assert_eq!(after.allocations, before.allocations);
            assert_eq!(after.reallocations, before.reallocations);
            if let Some(pointer) = pointer {
                assert_eq!(output.as_ptr(), pointer);
            }
            assert_eq!(output.len(), values.len());
            assert!(
                output
                    .iter()
                    .zip(&values)
                    .all(|(actual, expected)| actual.to_bits() == expected.to_bits())
            );
        }
    }
}

#[test]
fn declared_reservation_rejects_oversized_lengths_before_any_heap_request() {
    let before = migo_alloc_probe::thread_counts();
    for length in [
        crate::limits::MAX_AUDIO_PCM_SAMPLES as usize + 1,
        usize::MAX,
    ] {
        let mut collector = PcmCollector::with_expected_samples(Some(length));
        assert!(collector.needs_reservation());
        assert!(matches!(collector.step(), Err(error) if error.code == ErrorCode::InvalidArgument));
        assert!(collector.step().is_err());
        assert!(collector.finish().is_err());
    }
    let after = migo_alloc_probe::thread_counts();
    // EngineError detail strings are bounded, but no declared PCM allocation ran.
    assert!(after.bytes_allocated - before.bytes_allocated < 4096);
    assert_eq!(after.reallocations, before.reallocations);
}

#[test]
fn declared_short_excess_and_unprepared_protocol_never_publishes_partial_pcm() {
    let mut unprepared = PcmCollector::with_expected_samples(Some(8192));
    assert!(unprepared.seal().is_err());
    assert!(unprepared.finish().is_err());
    for excess in [false, true] {
        let mut collector = PcmCollector::with_expected_samples(Some(8192));
        assert!(!collector.step().unwrap());
        collector.append_i16(&[1; 4096]).unwrap();
        let before = migo_alloc_probe::thread_counts();
        if excess {
            assert!(collector.append_i16(&[1; 4097]).is_err());
        } else {
            assert!(collector.seal().is_err());
        }
        assert!(migo_alloc_probe::thread_counts().bytes_freed - before.bytes_freed >= 8192 * 4);
        assert!(collector.step().is_err());
        assert!(collector.finish().is_err());
    }
    let mut collector = PcmCollector::with_expected_samples(Some(8193));
    assert!(!collector.step().unwrap());
    assert!(
        collector.append_i16(&[0; 8193]).is_err(),
        "a declaration cannot enlarge the step budget"
    );
    assert!(collector.finish().is_err());
}

#[test]
fn declared_drop_at_each_phase_retires_storage_and_completion_moves_the_result() {
    for stage in 0..4 {
        let before = migo_alloc_probe::thread_counts();
        let mut collector = PcmCollector::with_expected_samples(Some(8192));
        if stage > 0 {
            assert!(!collector.step().unwrap());
        }
        if stage > 1 {
            collector.append_i16(&[123; 8192]).unwrap();
        }
        if stage > 2 {
            collector.seal().unwrap();
            assert!(collector.step().unwrap());
        }
        let allocated = migo_alloc_probe::thread_counts();
        drop(collector);
        let after = migo_alloc_probe::thread_counts();
        assert_eq!(after.live_bytes(), before.live_bytes());
        if stage > 0 {
            assert_eq!(
                allocated.bytes_allocated - before.bytes_allocated,
                8192 * 4 + std::mem::size_of::<Vec<f32>>() as u64
            );
            assert_eq!(
                after.bytes_freed - before.bytes_freed,
                allocated.bytes_allocated - before.bytes_allocated
            );
        }
    }
}

#[test]
fn declared_exact_global_limit_completes_without_an_intermediate_pcm_copy() {
    let limit = crate::limits::MAX_AUDIO_PCM_SAMPLES as usize;
    let input = [0.125_f32; 8192];
    let before = migo_alloc_probe::thread_counts();
    let mut collector = PcmCollector::with_expected_samples(Some(limit));
    assert!(!collector.step().unwrap());
    let Phase::Collecting { pages, .. } = &collector.phase else {
        unreachable!()
    };
    let pointer = pages[0].as_ptr();
    let reserved = migo_alloc_probe::thread_counts();
    assert_eq!(
        reserved.bytes_allocated - before.bytes_allocated,
        (limit * 4 + std::mem::size_of::<Vec<f32>>()) as u64
    );
    assert_eq!(reserved.allocations - before.allocations, 2);
    for _ in 0..limit / input.len() {
        collector
            .append_with(&input, std::convert::identity)
            .unwrap();
    }
    assert_eq!(migo_alloc_probe::thread_counts(), reserved);
    collector.seal().unwrap();
    assert!(collector.step().unwrap());
    let output = collector.finish().unwrap();
    assert_eq!(output.len(), limit);
    assert_eq!(output.as_ptr(), pointer);
    assert!(output.iter().all(|sample| *sample == 0.125));
    let completed = migo_alloc_probe::thread_counts();
    assert_eq!(completed.allocations, reserved.allocations);
    assert_eq!(completed.reallocations, reserved.reallocations);
}
