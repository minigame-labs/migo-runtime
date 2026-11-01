//! Native registry allocations share the same physical ledger as audio work.
use super::*;
const BYTES: usize = 24;
fn format() -> AudioBufferFormat {
    AudioBufferFormat {
        channels: 2,
        frames: 3,
        sample_rate: 48000,
    }
}
fn samples() -> Vec<f32> {
    vec![
        -0.0,
        1.5,
        2.0,
        -3.25,
        f32::INFINITY,
        f32::from_bits(0x7fc00003),
    ]
}
fn same(a: &[f32], b: &[f32]) {
    assert_eq!(
        a.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
        b.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
        "registry physical custody changed complete PCM"
    );
}
fn fixture(max: usize) -> (AudioResourceTestScope, AudioResourceRegistry) {
    let mut scope = AudioResourceTestScope::new(AudioResourceLimits {
        max_single_bytes: 96,
        max_runtime_bytes: 192,
        max_runtime_buffers: 16,
        max_process_bytes: 384,
        max_process_buffers: 32,
    });
    scope.physical = Arc::new(AudioAggregateLedger::new(max));
    let registry = scope.registry();
    (scope, registry)
}
pub(super) fn lifetime() {
    assert!(
        Arc::ptr_eq(
            &AudioResourceRegistry::new().inner.physical,
            &AudioAggregateLedger::process_global()
        ),
        "registry production ledger forked from native audio"
    );
    for release in ["key", "runtime", "registry"] {
        for last in ["node", "backing"] {
            owned(release, last);
        }
    }
    for mode in ["freeze", "materialize", "adopt", "malformed"] {
        refused(mode);
    }
    process_priority();
    rollback();
    native_freeze();
    legacy();
}
fn owned(release: &str, last: &str) {
    let (scope, registry) = fixture(BYTES * 2);
    let data = samples();
    let source_ptr = data.as_ptr();
    let key = registry.adopt_frozen(1, format(), data).unwrap().key();
    assert_eq!(
        scope.physical.used_bytes(),
        BYTES,
        "native adoption omitted physical charge"
    );
    let node = registry.prepare_snapshot(key, None).unwrap().snapshot();
    assert_eq!(node.samples().as_ptr(), source_ptr);
    same(node.samples(), &samples());
    assert_eq!(
        scope.physical.used_bytes(),
        BYTES,
        "frozen reuse charged a second physical copy"
    );
    let prepared = registry.prepare_materialize(key).unwrap();
    let ptr = prepared.samples().as_ptr();
    let expected = vec![
        -0.0,
        2.0,
        f32::INFINITY,
        1.5,
        -3.25,
        f32::from_bits(0x7fc00003),
    ];
    same(prepared.samples(), &expected);
    assert_eq!(
        scope.physical.used_bytes(),
        BYTES * 2,
        "materialize omitted physical overlap charge"
    );
    let mut backing = prepared.commit_owned();
    assert_eq!(
        backing.samples().as_ptr(),
        ptr,
        "materialize physical handoff copied PCM"
    );
    same(backing.samples(), &expected);
    assert_eq!(
        scope.physical.used_bytes(),
        BYTES * 2,
        "materialize commit returned physical credit"
    );
    let bytes: &mut [u8] = backing.as_mut();
    assert_eq!(bytes.len(), BYTES);
    assert_eq!(&bytes[..4], &(-0.0_f32).to_ne_bytes());
    match release {
        "key" => {
            registry.release_buffer(key);
        }
        "runtime" => registry.finish_runtime_drop(1),
        _ => {}
    }
    drop(registry);
    assert_eq!(
        scope.process_usage(),
        (BYTES, 1),
        "logical release changed process-domain snapshot ownership"
    );
    assert_eq!(
        scope.physical.used_bytes(),
        BYTES * 2,
        "logical release returned live backing physical credit"
    );
    assert!(
        scope.physical.try_reserve(1, "peer host").is_err(),
        "live native copies reopened physical admission"
    );
    if last == "node" {
        drop(backing);
        assert_eq!(
            scope.physical.used_bytes(),
            BYTES,
            "materialized backing drop lost snapshot credit"
        );
        same(node.samples(), &samples());
        drop(node);
    } else {
        drop(node);
        assert_eq!(
            scope.physical.used_bytes(),
            BYTES,
            "snapshot drop lost materialized backing credit"
        );
        same(backing.samples(), &expected);
        drop(backing);
    }
    assert_eq!(
        scope.physical.used_bytes(),
        0,
        "native registry physical credit leaked"
    );
    assert_eq!(scope.process_usage(), (0, 0));
}
fn refused(mode: &str) {
    let (scope, registry) = fixture(BYTES);
    let key = if mode == "freeze" || mode == "malformed" {
        registry.reserve_backing(1, format()).unwrap().key()
    } else {
        registry.adopt_frozen(1, format(), samples()).unwrap().key()
    };
    let mut blocker = None;
    if matches!(mode, "freeze" | "malformed") {
        blocker = Some(
            scope
                .physical
                .try_reserve(BYTES, "other audio domain")
                .unwrap(),
        );
    }
    let before = scope.process_usage();
    let result = match mode {
        "freeze" => registry.prepare_snapshot(key, Some(&samples())).map(|_| ()),
        "malformed" => registry.prepare_snapshot(key, Some(&[0.0])).map(|_| ()),
        "materialize" => registry.prepare_materialize(key).map(|_| ()),
        "adopt" => registry.adopt_frozen(2, format(), samples()).map(|_| ()),
        _ => unreachable!(),
    };
    assert!(
        matches!(result,Err(ref e) if e.code==if mode=="malformed"{ErrorCode::InvalidArgument}else{ErrorCode::InputSaturated}),
        "native registry changed typed physical refusal"
    );
    assert_eq!(
        scope.process_usage(),
        before,
        "physical refusal leaked process credit"
    );
    assert_eq!(
        scope.physical.used_bytes(),
        BYTES,
        "physical refusal changed old live credit"
    );
    assert_eq!(
        registry.format(key),
        Some(format()),
        "physical refusal replaced old backing"
    );
    registry.release_buffer(key);
    drop(blocker);
    assert_eq!(scope.physical.used_bytes(), 0);
}
fn rollback() {
    let (scope, registry) = fixture(BYTES * 2);
    let key = registry.adopt_frozen(1, format(), samples()).unwrap().key();
    let prepared = registry.prepare_materialize(key).unwrap();
    assert_eq!(
        scope.physical.used_bytes(),
        BYTES * 2,
        "materialize preparation was uncharged"
    );
    drop(prepared);
    assert_eq!(
        scope.physical.used_bytes(),
        BYTES,
        "abandoned materialize leaked physical credit"
    );
    assert_eq!(
        registry
            .prepare_snapshot(key, None)
            .unwrap()
            .snapshot()
            .samples()
            .len(),
        6
    );
    registry.release_buffer(key);
    assert_eq!(scope.physical.used_bytes(), 0);
}
fn native_freeze() {
    let (scope, registry) = fixture(BYTES);
    let key = registry.reserve_backing(1, format()).unwrap().key();
    assert_eq!(
        scope.physical.used_bytes(),
        0,
        "legacy JS backing was silently claimed as actual native ownership"
    );
    let planar = [
        -0.0,
        2.0,
        f32::INFINITY,
        1.5,
        -3.25,
        f32::from_bits(0x7fc00003),
    ];
    let prepared = registry.prepare_snapshot(key, Some(&planar)).unwrap();
    let ptr = prepared.snapshot.samples().as_ptr();
    assert_eq!(
        scope.physical.used_bytes(),
        BYTES,
        "writable freeze omitted native physical charge"
    );
    let node = prepared.commit();
    assert_eq!(node.samples().as_ptr(), ptr);
    same(node.samples(), &samples());
    registry.release_buffer(key);
    assert_eq!(
        scope.physical.used_bytes(),
        BYTES,
        "freeze lost final node physical custody"
    );
    drop(node);
    assert_eq!(scope.physical.used_bytes(), 0);
}
fn legacy() {
    let (scope, registry) = fixture(BYTES * 2);
    let key = registry.adopt_frozen(1, format(), samples()).unwrap().key();
    let prepared = registry.prepare_materialize(key).unwrap();
    let ptr = prepared.samples().as_ptr();
    let raw = prepared.commit();
    assert_eq!(
        raw.as_ptr(),
        ptr,
        "raw materialization copied PCM allocation"
    );
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
        "raw backing credit ended at logical release"
    );
    drop(raw);
    assert_eq!(
        scope.physical.used_bytes(),
        0,
        "raw backing final owner leaked credit"
    );
}

fn process_priority() {
    let (scope, registry) = fixture(BYTES);
    let key = registry.adopt_frozen(1, format(), samples()).unwrap().key();
    let blocker = scope.process.try_reserve(384 - BYTES).unwrap();
    let error = registry.prepare_materialize(key).unwrap_err();
    assert!(
        error.code == ErrorCode::InputSaturated
            && error.to_string().contains("process AudioBuffer"),
        "registry physical guard changed process refusal priority"
    );
    assert_eq!(scope.physical.used_bytes(), BYTES);
    drop(blocker);
    registry.release_buffer(key);
    assert_eq!(scope.physical.used_bytes(), 0);
}
