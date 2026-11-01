//! Initial logical reservations hold one actual native backing after initialization.
use super::*;
const BYTES: usize = 24;
fn format() -> AudioBufferFormat {
    AudioBufferFormat {
        channels: 2,
        frames: 3,
        sample_rate: 48000,
    }
}
fn fixture(max: usize) -> (AudioResourceTestScope, AudioResourceRegistry) {
    let mut scope = AudioResourceTestScope::new(AudioResourceLimits {
        max_single_bytes: 96,
        max_runtime_bytes: 384,
        max_runtime_buffers: 16,
        max_process_bytes: 768,
        max_process_buffers: 32,
    });
    scope.physical = Arc::new(AudioAggregateLedger::new(max));
    let registry = scope.registry();
    (scope, registry)
}
fn initial(registry: &AudioResourceRegistry, key: AudioBufferKey) -> PreparedWritableAudioBacking {
    let result = registry.prepare_writable_backing(key);
    assert!(
        result.is_ok(),
        "initial reservation was not routed to native initialization"
    );
    result.unwrap()
}
pub(super) fn lifetime() {
    for release in ["key", "runtime", "registry"] {
        owned(release);
    }
    for mode in [
        "abandon",
        "abort-prepared",
        "abort-empty",
        "saturated",
        "retired",
        "invalid",
    ] {
        rollback(mode);
    }
    for order in [0, 1, 2] {
        freeze_and_rematerialize(order);
    }
    legacy();
    reserved_domain();
    refusal_before_allocation();
}
fn owned(release: &str) {
    let (scope, registry) = fixture(BYTES);
    let key = registry.reserve_initial_backing(7, format()).unwrap().key();
    assert_eq!(
        scope.process_usage(),
        (BYTES, 1),
        "initial reservation changed logical process allowance"
    );
    assert_eq!(scope.physical.used_bytes(), 0);
    let prepared = initial(&registry, key);
    let pointer = prepared.samples().as_ptr();
    assert_eq!(
        prepared.samples().len(),
        BYTES / 4,
        "initial backing shape changed"
    );
    assert!(
        prepared.samples().iter().all(|v| v.to_bits() == 0),
        "initial backing was not completely zero initialized"
    );
    assert_eq!(
        scope.process_usage(),
        (BYTES, 1),
        "initial preparation charged a second process buffer"
    );
    assert_eq!(
        scope.physical.used_bytes(),
        BYTES,
        "initial native allocation was not physically charged"
    );
    let backing = prepared.commit_owned();
    assert_eq!(
        backing.samples().as_ptr(),
        pointer,
        "initial backing handoff copied PCM"
    );
    assert_eq!(
        scope.physical.used_bytes(),
        BYTES,
        "initial commit returned physical credit"
    );
    let duplicate = registry.prepare_writable_backing(key);
    assert!(
        matches!(duplicate,Err(e)if e.code==ErrorCode::InvalidOperation),
        "initial backing was initialized more than once"
    );
    match release {
        "key" => {
            registry.release_buffer(key);
        }
        "runtime" => registry.finish_runtime_drop(7),
        _ => {}
    }
    drop(registry);
    assert_eq!(scope.process_usage(), (0, 0));
    assert_eq!(
        scope.physical.used_bytes(),
        BYTES,
        "logical release returned live initial backing credit"
    );
    assert!(
        scope.physical.try_reserve(1, "peer host").is_err(),
        "live initial backing reopened physical admission"
    );
    drop(backing);
    assert_eq!(
        scope.physical.used_bytes(),
        0,
        "initial backing final owner leaked credit"
    );
}
fn rollback(mode: &str) {
    let (scope, registry) = fixture(BYTES);
    if mode == "invalid" {
        let result = registry.reserve_initial_backing(
            7,
            AudioBufferFormat {
                channels: 0,
                ..format()
            },
        );
        assert!(matches!(result,Err(e)if e.code==ErrorCode::InvalidArgument));
        assert_eq!(scope.process_usage(), (0, 0));
        assert_eq!(scope.physical.used_bytes(), 0);
        return;
    }
    let key = registry.reserve_initial_backing(7, format()).unwrap().key();
    match mode {
        "abandon" => {
            let p = initial(&registry, key);
            drop(p);
            assert_eq!(
                scope.physical.used_bytes(),
                0,
                "abandoned initial preparation leaked credit"
            );
            let p = initial(&registry, key);
            drop(p);
        }
        "abort-prepared" => {
            let p = initial(&registry, key);
            registry.release_buffer(key);
            assert_eq!(
                scope.physical.used_bytes(),
                BYTES,
                "aborted preparation lost live allocation credit"
            );
            drop(p);
        }
        "abort-empty" => {
            registry.release_buffer(key);
        }
        "saturated" => {
            let peer = scope
                .physical
                .try_reserve(BYTES, "other audio domain")
                .unwrap();
            let result = registry.prepare_writable_backing(key);
            assert!(
                matches!(result,Err(e)if e.code==ErrorCode::InputSaturated),
                "initial allocation bypassed physical refusal"
            );
            assert_eq!(scope.process_usage(), (BYTES, 1));
            drop(peer);
            drop(initial(&registry, key));
        }
        "retired" => {
            registry.finish_runtime_drop(7);
            let result = registry.prepare_writable_backing(key);
            assert!(matches!(result,Err(e)if e.code==ErrorCode::InvalidOperation));
        }
        _ => unreachable!(),
    }
    registry.release_buffer(key);
    assert_eq!(scope.process_usage(), (0, 0));
    assert_eq!(
        scope.physical.used_bytes(),
        0,
        "initial rollback leaked physical credit"
    );
}
fn freeze_and_rematerialize(order: usize) {
    let (scope, registry) = fixture(BYTES * 3);
    let key = registry.reserve_initial_backing(7, format()).unwrap().key();
    let mut original = initial(&registry, key).commit_owned();
    let values = [-0.0_f32, 2.0, 4.5, 1.25, -3.0, 8.0];
    original
        .as_mut()
        .copy_from_slice(bytemuck::cast_slice(&values));
    let node = registry
        .prepare_snapshot(key, Some(original.samples()))
        .unwrap()
        .commit();
    assert_eq!(
        node.samples()
            .iter()
            .map(|v| v.to_bits())
            .collect::<Vec<_>>(),
        [-0.0_f32, 1.25, 2.0, -3.0, 4.5, 8.0].map(f32::to_bits),
        "initial freeze changed complete interleaving"
    );
    let materialized = registry
        .prepare_writable_backing(key)
        .unwrap()
        .commit_owned();
    assert_eq!(
        materialized
            .samples()
            .iter()
            .map(|v| v.to_bits())
            .collect::<Vec<_>>(),
        values.map(f32::to_bits),
        "initial rematerialization changed complete PCM"
    );
    assert_eq!(
        scope.physical.used_bytes(),
        BYTES * 3,
        "initial freeze materialize overlap was not fully charged"
    );
    registry.release_buffer(key);
    drop(registry);
    match order {
        0 => {
            drop(original);
            assert_eq!(scope.physical.used_bytes(), BYTES * 2);
            drop(node);
            assert_eq!(scope.physical.used_bytes(), BYTES);
            drop(materialized);
        }
        1 => {
            drop(node);
            assert_eq!(scope.physical.used_bytes(), BYTES * 2);
            drop(materialized);
            assert_eq!(scope.physical.used_bytes(), BYTES);
            drop(original);
        }
        _ => {
            drop(materialized);
            assert_eq!(scope.physical.used_bytes(), BYTES * 2);
            drop(original);
            assert_eq!(scope.physical.used_bytes(), BYTES);
            drop(node);
        }
    }
    assert_eq!(scope.physical.used_bytes(), 0);
}
fn legacy() {
    let (scope, registry) = fixture(BYTES);
    let key = registry.reserve_initial_backing(7, format()).unwrap().key();
    let prepared = initial(&registry, key);
    let pointer = prepared.samples().as_ptr();
    let raw = prepared.commit();
    assert_eq!(
        raw.as_ptr(),
        pointer,
        "raw materialization copied PCM allocation"
    );
    assert_eq!(scope.process_usage(), (BYTES, 1));
    assert_eq!(
        scope.physical.used_bytes(),
        BYTES,
        "raw commit lost allocation credit"
    );
    registry.release_buffer(key);
    assert_eq!(scope.process_usage(), (0, 0));
    assert_eq!(
        scope.physical.used_bytes(),
        BYTES,
        "raw initial credit ended at logical release"
    );
    drop(raw);
    assert_eq!(
        scope.physical.used_bytes(),
        0,
        "raw initial final owner leaked credit"
    );
}
fn reserved_domain() {
    let (mut scope, old) = fixture(BYTES);
    drop(old);
    scope.process = Arc::new(ProcessUsage::new(BYTES, 1));
    let registry = scope.registry();
    let key = registry.reserve_initial_backing(7, format()).unwrap().key();
    let result = registry.prepare_writable_backing(key);
    assert!(
        result.is_ok(),
        "initial initialization required second process allowance"
    );
    let backing = result.unwrap().commit_owned();
    assert_eq!(scope.process_usage(), (BYTES, 1));
    registry.release_buffer(key);
    drop(backing);
    assert_eq!(scope.process_usage(), (0, 0));
    assert_eq!(scope.physical.used_bytes(), 0);
}
fn refusal_before_allocation() {
    let bytes = 4 << 20;
    let mut scope = AudioResourceTestScope::new(AudioResourceLimits {
        max_single_bytes: bytes,
        max_runtime_bytes: bytes,
        max_runtime_buffers: 1,
        max_process_bytes: bytes,
        max_process_buffers: 1,
    });
    scope.physical = Arc::new(AudioAggregateLedger::new(bytes));
    let registry = scope.registry();
    let format = AudioBufferFormat {
        channels: 1,
        frames: (bytes / 4) as u32,
        sample_rate: 48000,
    };
    let key = registry.reserve_initial_backing(7, format).unwrap().key();
    let peer = scope
        .physical
        .try_reserve(bytes, "other audio domain")
        .unwrap();
    let before = migo_alloc_probe::thread_counts();
    let result = registry.prepare_writable_backing(key);
    assert!(matches!(result,Err(e)if e.code==ErrorCode::InputSaturated));
    assert!(
        migo_alloc_probe::thread_counts().bytes_allocated - before.bytes_allocated < 65536,
        "initial physical refusal allocated PCM before admission"
    );
    registry.release_buffer(key);
    drop(peer);
    assert_eq!(scope.physical.used_bytes(), 0);
}
