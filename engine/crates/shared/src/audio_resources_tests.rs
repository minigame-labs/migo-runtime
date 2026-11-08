use crate::audio_resources::{AudioBufferFormat, AudioResourceLimits, AudioResourceTestScope};
use crate::error::ErrorCode;

fn format(bytes: u32) -> AudioBufferFormat {
    assert_eq!(bytes % 4, 0);
    AudioBufferFormat {
        channels: 1,
        frames: bytes / 4,
        sample_rate: 48_000,
    }
}

fn tiny_limits() -> AudioResourceLimits {
    AudioResourceLimits {
        max_single_bytes: 16,
        max_runtime_bytes: 16,
        max_runtime_buffers: 2,
        max_process_bytes: 24,
        max_process_buffers: 3,
    }
}

fn production_limits() -> AudioResourceLimits {
    AudioResourceLimits {
        max_single_bytes: 64 * 1024 * 1024,
        max_runtime_bytes: 128 * 1024 * 1024,
        max_runtime_buffers: 512,
        max_process_bytes: 256 * 1024 * 1024,
        max_process_buffers: 2_048,
    }
}

#[test]
fn format_validation_accepts_exact_public_boundaries() {
    let scope = AudioResourceTestScope::new(production_limits());
    let registry = scope.registry();
    let exact_64_mib = AudioBufferFormat {
        channels: 2,
        frames: 8_388_608,
        sample_rate: 3_000,
    };

    let lease = registry
        .reserve_backing(1, exact_64_mib)
        .expect("the exact single-buffer byte limit is accepted");

    assert_eq!(lease.byte_len(), 64 * 1024 * 1024);
    assert_eq!(lease.format(), exact_64_mib);
    assert_eq!(lease.key().runtime_generation, 1);
    assert_eq!(lease.key().serial, 1);
    let max_rate = registry
        .reserve_backing(
            1,
            AudioBufferFormat {
                channels: 1,
                frames: 1,
                sample_rate: 768_000,
            },
        )
        .expect("the exact upper sample-rate boundary is accepted");
    assert!(registry.release_buffer(lease.key()));
    assert!(registry.release_buffer(max_rate.key()));
}

#[test]
fn format_validation_rejects_invalid_shape_rate_and_oversize() {
    let scope = AudioResourceTestScope::new(production_limits());
    let registry = scope.registry();
    let invalid = [
        AudioBufferFormat {
            channels: 0,
            frames: 1,
            sample_rate: 48_000,
        },
        AudioBufferFormat {
            channels: 33,
            frames: 1,
            sample_rate: 48_000,
        },
        AudioBufferFormat {
            channels: 1,
            frames: 0,
            sample_rate: 48_000,
        },
        AudioBufferFormat {
            channels: 1,
            frames: 1,
            sample_rate: 2_999,
        },
        AudioBufferFormat {
            channels: 1,
            frames: 1,
            sample_rate: 768_001,
        },
        AudioBufferFormat {
            channels: 32,
            frames: u32::MAX,
            sample_rate: 48_000,
        },
    ];

    for format in invalid {
        let error = registry
            .reserve_backing(1, format)
            .expect_err("invalid AudioBuffer format must fail before allocation");
        assert_eq!(error.code, ErrorCode::InvalidArgument, "{format:?}");
    }
}

#[test]
fn runtime_byte_and_item_limits_are_enforced_before_allocation() {
    let scope = AudioResourceTestScope::new(tiny_limits());
    let registry = scope.registry();

    let first = registry.reserve_backing(7, format(8)).unwrap();
    let second = registry.reserve_backing(7, format(8)).unwrap();
    let error = registry
        .reserve_backing(7, format(4))
        .expect_err("runtime byte/count capacity is exhausted");

    assert_eq!(error.code, ErrorCode::InputSaturated);
    assert_eq!(registry.runtime_usage(7), (16, 2));
    assert_eq!(scope.process_usage(), (16, 2));
    registry.release_buffer(first.key());
    registry.release_buffer(second.key());
}

#[test]
fn a_runtime_rejection_rolls_back_the_process_reservation() {
    let mut limits = tiny_limits();
    limits.max_runtime_bytes = 4;
    limits.max_runtime_buffers = 1;
    limits.max_process_bytes = 64;
    limits.max_process_buffers = 8;
    let scope = AudioResourceTestScope::new(limits);
    let registry = scope.registry();
    let kept = registry.reserve_backing(3, format(4)).unwrap();

    registry
        .reserve_backing(3, format(4))
        .expect_err("the per-runtime limit rejects the second reservation");

    assert_eq!(scope.process_usage(), (4, 1));
    assert_eq!(registry.runtime_usage(3), (4, 1));
    registry.release_buffer(kept.key());
    assert_eq!(scope.process_usage(), (0, 0));
}

#[test]
fn process_budget_is_shared_across_independent_registries() {
    let mut limits = tiny_limits();
    limits.max_process_bytes = 8;
    limits.max_process_buffers = 2;
    let scope = AudioResourceTestScope::new(limits);
    let first_registry = scope.registry();
    let second_registry = scope.registry();

    let first = first_registry.reserve_backing(1, format(4)).unwrap();
    let second = second_registry.reserve_backing(1, format(4)).unwrap();
    let error = second_registry
        .reserve_backing(2, format(4))
        .expect_err("process budget spans every host registry");

    assert_eq!(error.code, ErrorCode::InputSaturated);
    assert_eq!(scope.process_usage(), (8, 2));
    assert!(first_registry.release_buffer(first.key()));
    let replacement = second_registry
        .reserve_backing(2, format(4))
        .expect("dropping an entry returns its process permit");
    assert!(second_registry.release_buffer(second.key()));
    assert!(second_registry.release_buffer(replacement.key()));
}

#[test]
fn release_is_idempotent_and_returns_both_budget_permits() {
    let scope = AudioResourceTestScope::new(tiny_limits());
    let registry = scope.registry();
    let lease = registry.reserve_backing(9, format(12)).unwrap();

    assert!(registry.release_buffer(lease.key()));
    assert!(!registry.release_buffer(lease.key()));
    assert_eq!(registry.runtime_usage(9), (0, 0));
    assert_eq!(scope.process_usage(), (0, 0));
}

#[test]
fn serial_exhaustion_is_permanent_and_never_reuses_a_released_key() {
    let scope = AudioResourceTestScope::new(tiny_limits());
    let registry = scope.registry_with_next_serial(i32::MAX as u32);

    let last = registry.reserve_backing(1, format(4)).unwrap();
    assert_eq!(last.key().serial, i32::MAX as u32);
    let exhausted = registry
        .reserve_backing(2, format(4))
        .expect_err("the public Smi-safe id space is exhausted");
    assert_eq!(exhausted.code, ErrorCode::InvalidOperation);

    registry.release_buffer(last.key());
    let still_exhausted = registry
        .reserve_backing(3, format(4))
        .expect_err("release must never rewind or reuse a serial");
    assert_eq!(still_exhausted.code, ErrorCode::InvalidOperation);
}

#[test]
fn retire_fences_new_reservations_but_allows_idempotent_release() {
    let scope = AudioResourceTestScope::new(tiny_limits());
    let registry = scope.registry();
    let lease = registry.reserve_backing(11, format(4)).unwrap();

    registry.begin_retire(11);

    let error = registry
        .reserve_backing(11, format(4))
        .expect_err("a retiring isolate cannot acquire more backing");
    assert_eq!(error.code, ErrorCode::InvalidOperation);
    assert!(registry.release_buffer(lease.key()));
    assert!(!registry.release_buffer(lease.key()));
}

#[test]
fn finish_drop_reclaims_entries_and_tombstones_generation_against_aba() {
    let scope = AudioResourceTestScope::new(tiny_limits());
    let registry = scope.registry();
    let old = registry.reserve_backing(21, format(8)).unwrap();
    let next = registry.reserve_backing(22, format(4)).unwrap();
    registry.begin_retire(21);

    registry.finish_runtime_drop(21);

    assert_eq!(registry.runtime_usage(21), (0, 0));
    assert_eq!(registry.runtime_usage(22), (4, 1));
    assert_eq!(scope.process_usage(), (4, 1));
    assert!(!registry.release_buffer(old.key()));
    let error = registry
        .reserve_backing(21, format(4))
        .expect_err("a dropped generation stays tombstoned forever");
    assert_eq!(error.code, ErrorCode::InvalidOperation);
    let fresh = registry
        .reserve_backing(23, format(4))
        .expect("a different generation is not fenced");
    assert_ne!(fresh.key(), old.key());
    registry.release_buffer(next.key());
    registry.release_buffer(fresh.key());
}

#[test]
fn cloned_registry_handles_share_entries_and_lifecycle_state() {
    let scope = AudioResourceTestScope::new(tiny_limits());
    let registry = scope.registry();
    let clone = registry.clone();
    let lease = registry.reserve_backing(31, format(4)).unwrap();

    assert!(clone.release_buffer(lease.key()));
    clone.begin_retire(31);
    assert_eq!(
        registry.reserve_backing(31, format(4)).unwrap_err().code,
        ErrorCode::InvalidOperation
    );
}

#[test]
fn finish_advances_a_monotonic_retired_high_watermark() {
    let scope = AudioResourceTestScope::new(tiny_limits());
    let registry = scope.registry();

    registry.finish_runtime_drop(10_000);

    for retired in [0, 1, 9_999, 10_000] {
        assert_eq!(
            registry
                .reserve_backing(retired, format(4))
                .expect_err("every older generation is covered by one watermark")
                .code,
            ErrorCode::InvalidOperation
        );
    }
    let live = registry
        .reserve_backing(10_001, format(4))
        .expect("the successor remains admissible");
    assert!(registry.release_buffer(live.key()));
}

#[test]
fn live_entry_format_is_queryable_until_release_or_runtime_drop() {
    let scope = AudioResourceTestScope::new(tiny_limits());
    let registry = scope.registry();
    let expected = AudioBufferFormat {
        channels: 2,
        frames: 2,
        sample_rate: 44_100,
    };
    let lease = registry.reserve_backing(41, expected).unwrap();

    assert_eq!(registry.format(lease.key()), Some(expected));
    assert!(registry.release_buffer(lease.key()));
    assert_eq!(registry.format(lease.key()), None);

    let dropped = registry.reserve_backing(42, expected).unwrap();
    registry.finish_runtime_drop(42);
    assert_eq!(registry.format(dropped.key()), None);
}

fn transition_limits(process_bytes: usize, process_buffers: usize) -> AudioResourceLimits {
    AudioResourceLimits {
        max_single_bytes: 64,
        max_runtime_bytes: 128,
        max_runtime_buffers: 8,
        max_process_bytes: process_bytes,
        max_process_buffers: process_buffers,
    }
}

fn stereo_three_frames() -> AudioBufferFormat {
    AudioBufferFormat {
        channels: 2,
        frames: 3,
        sample_rate: 48_000,
    }
}

#[test]
fn freeze_interleaves_planar_pcm_and_reuses_one_physical_snapshot() {
    let scope = AudioResourceTestScope::new(transition_limits(96, 8));
    let registry = scope.registry();
    let format = stereo_three_frames();
    let lease = registry.reserve_backing(51, format).unwrap();
    let planar = [1.0, 2.0, 3.0, 10.0, 20.0, 30.0];

    let prepared = registry
        .prepare_snapshot(lease.key(), Some(&planar))
        .unwrap();
    assert_eq!(scope.process_usage(), (48, 2));
    assert_eq!(registry.runtime_usage(51), (24, 1));
    let node_snapshot = prepared.snapshot();
    assert_eq!(node_snapshot.format(), format);
    assert_eq!(node_snapshot.samples(), &[1.0, 10.0, 2.0, 20.0, 3.0, 30.0]);

    let committed = prepared.commit();
    assert!(std::sync::Arc::ptr_eq(&node_snapshot, &committed));
    drop(committed);
    assert_eq!(scope.process_usage(), (24, 1));
    assert_eq!(registry.runtime_usage(51), (24, 1));

    let repeated = registry.prepare_snapshot(lease.key(), None).unwrap();
    let repeated_snapshot = repeated.snapshot();
    assert!(std::sync::Arc::ptr_eq(&node_snapshot, &repeated_snapshot));
    let repeated_committed = repeated.commit();
    assert!(std::sync::Arc::ptr_eq(&node_snapshot, &repeated_committed));
    assert_eq!(scope.process_usage(), (24, 1));
    assert_eq!(
        registry
            .prepare_snapshot(lease.key(), Some(&planar))
            .expect_err("Frozen plus a second JS backing is a state mismatch")
            .code,
        ErrorCode::InvalidOperation
    );

    assert!(registry.release_buffer(lease.key()));
    assert_eq!(registry.runtime_usage(51), (0, 0));
    assert_eq!(scope.process_usage(), (24, 1));
    drop(repeated_committed);
    drop(repeated_snapshot);
    drop(node_snapshot);
    assert_eq!(scope.process_usage(), (0, 0));
}

#[test]
fn failed_or_abandoned_freeze_keeps_the_entry_writable() {
    let scope = AudioResourceTestScope::new(transition_limits(32, 4));
    let registry = scope.registry();
    let lease = registry.reserve_backing(52, format(8)).unwrap();

    assert_eq!(
        registry
            .prepare_snapshot(lease.key(), None)
            .expect_err("Writable requires its exact planar backing")
            .code,
        ErrorCode::InvalidOperation
    );
    assert_eq!(
        registry
            .prepare_materialize(lease.key())
            .expect_err("Writable is already materialized")
            .code,
        ErrorCode::InvalidOperation
    );
    assert_eq!(
        registry
            .prepare_snapshot(lease.key(), Some(&[1.0]))
            .expect_err("one sample is shorter than the two-sample backing")
            .code,
        ErrorCode::InvalidArgument
    );
    assert_eq!(scope.process_usage(), (8, 1));

    let prepared = registry
        .prepare_snapshot(lease.key(), Some(&[1.0, 2.0]))
        .unwrap();
    assert_eq!(scope.process_usage(), (16, 2));
    drop(prepared);
    assert_eq!(scope.process_usage(), (8, 1));
    assert_eq!(
        registry
            .prepare_snapshot(lease.key(), None)
            .expect_err("dropping an uncommitted freeze preserves Writable")
            .code,
        ErrorCode::InvalidOperation
    );

    assert!(registry.release_buffer(lease.key()));
    assert_eq!(scope.process_usage(), (0, 0));
}

#[test]
fn freeze_reserves_peak_process_capacity_before_allocating() {
    let scope = AudioResourceTestScope::new(transition_limits(8, 1));
    let registry = scope.registry();
    let lease = registry.reserve_backing(53, format(8)).unwrap();

    let error = registry
        .prepare_snapshot(lease.key(), Some(&[1.0, 2.0]))
        .expect_err("writable plus frozen peak exceeds the process budget");

    assert_eq!(error.code, ErrorCode::InputSaturated);
    assert_eq!(scope.process_usage(), (8, 1));
    assert_eq!(registry.runtime_usage(53), (8, 1));
    assert!(registry.release_buffer(lease.key()));
}

#[test]
fn materialize_round_trip_keeps_old_nodes_on_the_old_snapshot() {
    let scope = AudioResourceTestScope::new(transition_limits(96, 8));
    let registry = scope.registry();
    let format = stereo_three_frames();
    let lease = registry.reserve_backing(54, format).unwrap();
    let original_planar = [1.0, 2.0, 3.0, 10.0, 20.0, 30.0];
    let frozen = registry
        .prepare_snapshot(lease.key(), Some(&original_planar))
        .unwrap();
    let old_node = frozen.snapshot();
    frozen.commit();
    assert_eq!(scope.process_usage(), (24, 1));

    let materialized = registry.prepare_materialize(lease.key()).unwrap();
    assert_eq!(materialized.samples(), &original_planar);
    assert_eq!(scope.process_usage(), (48, 2));
    assert_eq!(registry.runtime_usage(54), (24, 1));
    let mut writable = materialized.commit();
    assert_eq!(writable.len(), 6);
    assert_eq!(writable.capacity(), writable.len());
    assert_eq!(scope.process_usage(), (48, 2));
    writable[0] = 99.0;

    let replacement = registry
        .prepare_snapshot(lease.key(), Some(&writable))
        .unwrap();
    assert_eq!(scope.process_usage(), (72, 3));
    let new_node = replacement.snapshot();
    assert!(!std::sync::Arc::ptr_eq(&old_node, &new_node));
    assert_eq!(new_node.samples()[0], 99.0);
    // The runtime detaches/transfers the JS allocation before releasing its
    // Writable permit at commit.
    drop(writable);
    replacement.commit();
    assert_eq!(scope.process_usage(), (48, 2));
    assert_eq!(registry.runtime_usage(54), (24, 1));

    assert!(registry.release_buffer(lease.key()));
    assert_eq!(scope.process_usage(), (48, 2));
    drop(new_node);
    assert_eq!(scope.process_usage(), (24, 1));
    drop(old_node);
    assert_eq!(scope.process_usage(), (0, 0));
}

#[test]
fn failed_or_abandoned_materialize_keeps_the_entry_frozen() {
    let scope = AudioResourceTestScope::new(transition_limits(16, 2));
    let registry = scope.registry();
    let frozen_lease = registry.reserve_backing(55, format(8)).unwrap();
    let frozen = registry
        .prepare_snapshot(frozen_lease.key(), Some(&[4.0, 5.0]))
        .unwrap();
    let snapshot = frozen.snapshot();
    frozen.commit();
    let blocker = registry.reserve_backing(56, format(8)).unwrap();
    assert_eq!(scope.process_usage(), (16, 2));

    let error = registry
        .prepare_materialize(frozen_lease.key())
        .expect_err("materialization peak is process-accounted");
    assert_eq!(error.code, ErrorCode::InputSaturated);
    let still_frozen = registry.prepare_snapshot(frozen_lease.key(), None).unwrap();
    assert!(std::sync::Arc::ptr_eq(&snapshot, &still_frozen.snapshot()));
    drop(still_frozen);

    assert!(registry.release_buffer(blocker.key()));
    let abandoned = registry.prepare_materialize(frozen_lease.key()).unwrap();
    assert_eq!(scope.process_usage(), (16, 2));
    drop(abandoned);
    assert_eq!(scope.process_usage(), (8, 1));
    let still_frozen = registry.prepare_snapshot(frozen_lease.key(), None).unwrap();
    assert!(std::sync::Arc::ptr_eq(&snapshot, &still_frozen.snapshot()));
    drop(still_frozen);

    assert!(registry.release_buffer(frozen_lease.key()));
    assert_eq!(scope.process_usage(), (8, 1));
    drop(snapshot);
    assert_eq!(scope.process_usage(), (0, 0));
}

#[test]
fn runtime_drop_reclaims_frozen_entry_but_not_a_live_node_arc() {
    let scope = AudioResourceTestScope::new(transition_limits(16, 2));
    let registry = scope.registry();
    let lease = registry.reserve_backing(57, format(8)).unwrap();
    let prepared = registry
        .prepare_snapshot(lease.key(), Some(&[7.0, 8.0]))
        .unwrap();
    let node = prepared.snapshot();
    prepared.commit();

    registry.begin_retire(57);
    assert_eq!(
        registry
            .prepare_snapshot(lease.key(), None)
            .expect_err("a retiring runtime cannot publish a snapshot")
            .code,
        ErrorCode::InvalidOperation
    );
    assert_eq!(
        registry
            .prepare_materialize(lease.key())
            .expect_err("a retiring runtime cannot materialize backing")
            .code,
        ErrorCode::InvalidOperation
    );
    registry.finish_runtime_drop(57);

    assert_eq!(registry.runtime_usage(57), (0, 0));
    assert_eq!(registry.format(lease.key()), None);
    assert_eq!(scope.process_usage(), (8, 1));
    assert_eq!(node.samples(), &[7.0, 8.0]);
    drop(node);
    assert_eq!(scope.process_usage(), (0, 0));
}

#[test]
fn an_adopted_decode_is_frozen_from_the_start_and_plays_without_a_copy() {
    let scope = AudioResourceTestScope::new(transition_limits(96, 8));
    let registry = scope.registry();
    let format = stereo_three_frames();
    let interleaved = vec![1.0, 10.0, 2.0, 20.0, 3.0, 30.0];
    let samples_at = interleaved.as_ptr();

    let lease = registry.adopt_frozen(55, format, interleaved).unwrap();
    assert_eq!(lease.format(), format);
    assert_eq!(lease.byte_len(), 24);
    // One allocation, charged once, to the process and to its runtime.
    assert_eq!(scope.process_usage(), (24, 1));
    assert_eq!(registry.runtime_usage(55), (24, 1));

    // Starting it reuses the adopted samples themselves: no JavaScript backing
    // is asked for, and no second allocation is made.
    let playing = registry.prepare_snapshot(lease.key(), None).unwrap();
    let node = playing.snapshot();
    assert_eq!(node.samples().as_ptr(), samples_at);
    playing.commit();
    assert_eq!(scope.process_usage(), (24, 1));

    // Reading its channels is what makes a planar copy.
    let materialized = registry.prepare_materialize(lease.key()).unwrap();
    assert_eq!(materialized.samples(), &[1.0, 2.0, 3.0, 10.0, 20.0, 30.0]);
    drop(materialized);

    assert!(registry.release_buffer(lease.key()));
    assert_eq!(registry.runtime_usage(55), (0, 0));
    drop(node);
    assert_eq!(scope.process_usage(), (0, 0));
}

#[test]
fn an_adopted_decode_is_admitted_as_a_reservation_is_and_refusal_returns_everything() {
    let scope = AudioResourceTestScope::new(transition_limits(24, 8));
    let registry = scope.registry();
    let format = stereo_three_frames();

    assert_eq!(
        registry
            .adopt_frozen(56, format, vec![0.0; 5])
            .expect_err("five samples are not three stereo frames")
            .code,
        ErrorCode::InvalidArgument
    );
    let held = registry.adopt_frozen(56, format, vec![0.0; 6]).unwrap();
    assert_eq!(
        registry
            .adopt_frozen(56, format, vec![0.0; 6])
            .expect_err("the process budget holds one of these")
            .code,
        ErrorCode::InputSaturated
    );
    assert_eq!(scope.process_usage(), (24, 1));
    assert!(registry.release_buffer(held.key()));
    assert_eq!(scope.process_usage(), (0, 0));

    // With the budget free again, a retiring runtime is refused for being
    // retired, and returns the budget it was about to take.
    registry.begin_retire(57);
    assert_eq!(
        registry
            .adopt_frozen(57, format, vec![0.0; 6])
            .expect_err("a retiring runtime takes nothing new")
            .code,
        ErrorCode::InvalidOperation
    );
    assert_eq!(scope.process_usage(), (0, 0));
}

#[test]
fn spare_decoder_capacity_is_not_retained_outside_the_budget() {
    let scope = AudioResourceTestScope::new(transition_limits(96, 8));
    let registry = scope.registry();
    let mut interleaved = Vec::with_capacity(64);
    interleaved.extend_from_slice(&[0.5; 6]);

    let lease = registry
        .adopt_frozen(58, stereo_three_frames(), interleaved)
        .unwrap();
    let playing = registry.prepare_snapshot(lease.key(), None).unwrap();
    let node = playing.commit();
    assert_eq!(node.samples().len(), 6);
    // The budget charged six samples; six is what is kept.
    assert_eq!(scope.process_usage(), (24, 1));
    assert!(registry.release_buffer(lease.key()));
    drop(node);
}

#[test]
fn a_planar_backing_sent_as_bytes_freezes_as_the_same_snapshot() {
    let scope = AudioResourceTestScope::new(transition_limits(96, 8));
    let registry = scope.registry();
    let format = stereo_three_frames();
    let planar = [1.0f32, 2.0, 3.0, 10.0, 20.0, 30.0];
    let bytes: Vec<u8> = planar
        .iter()
        .flat_map(|sample| sample.to_le_bytes())
        .collect();

    let from_bytes = registry.reserve_backing(59, format).unwrap();
    assert_eq!(
        registry
            .prepare_snapshot_from_le_bytes(from_bytes.key(), &bytes[..bytes.len() - 1])
            .expect_err("bytes that end mid-sample")
            .code,
        ErrorCode::InvalidArgument
    );
    assert_eq!(
        registry
            .prepare_snapshot_from_le_bytes(from_bytes.key(), &bytes[..bytes.len() - 4])
            .expect_err("one sample short")
            .code,
        ErrorCode::InvalidArgument
    );
    // An odd offset: a message gives no alignment, and none is needed.
    let mut unaligned = vec![0u8];
    unaligned.extend_from_slice(&bytes);
    let prepared = registry
        .prepare_snapshot_from_le_bytes(from_bytes.key(), &unaligned[1..])
        .unwrap();
    let node = prepared.commit();
    assert_eq!(node.samples(), &[1.0, 10.0, 2.0, 20.0, 3.0, 30.0]);

    let from_samples = registry.reserve_backing(59, format).unwrap();
    let same = registry
        .prepare_snapshot(from_samples.key(), Some(&planar))
        .unwrap()
        .commit();
    assert_eq!(node.samples(), same.samples());

    assert!(registry.release_buffer(from_bytes.key()));
    assert!(registry.release_buffer(from_samples.key()));
}
