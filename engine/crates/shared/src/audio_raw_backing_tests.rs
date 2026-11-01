//! Raw materialization must retain actual storage credit beyond registry lifetime.
use super::*;
const BYTES: usize = 24;
fn format() -> AudioBufferFormat {
    AudioBufferFormat {
        channels: 2,
        frames: 3,
        sample_rate: 48000,
    }
}
fn values() -> Vec<f32> {
    [
        -0.0,
        1.5,
        2.0,
        -3.25,
        f32::INFINITY,
        f32::from_bits(0x7fc00003),
    ]
    .to_vec()
}
fn fixture(max: usize) -> (AudioResourceTestScope, AudioResourceRegistry) {
    let mut s = AudioResourceTestScope::new(AudioResourceLimits {
        max_single_bytes: 96,
        max_runtime_bytes: 384,
        max_runtime_buffers: 16,
        max_process_bytes: 768,
        max_process_buffers: 32,
    });
    s.physical = Arc::new(AudioAggregateLedger::new(max));
    let r = s.registry();
    (s, r)
}
fn prepare(r: &AudioResourceRegistry, kind: &str) -> (AudioBufferKey, AudioPlanarBacking) {
    let key = if kind == "initial" {
        r.reserve_initial_backing(7, format()).unwrap().key()
    } else {
        r.adopt_frozen(7, format(), values()).unwrap().key()
    };
    let prepared = if kind == "direct" {
        PreparedWritableAudioBacking::Materialized(r.prepare_materialize(key).unwrap())
    } else {
        r.prepare_writable_backing(key).unwrap()
    };
    let ptr = prepared.samples().as_ptr();
    let before = migo_alloc_probe::thread_counts();
    let backing = match prepared {
        PreparedWritableAudioBacking::Materialized(p) if kind == "direct" => p.commit(),
        p => p.commit(),
    };
    assert_eq!(
        backing.as_ptr(),
        ptr,
        "raw materialization copied PCM allocation"
    );
    assert_eq!(
        migo_alloc_probe::thread_counts().bytes_allocated,
        before.bytes_allocated,
        "raw commit allocated replacement storage"
    );
    assert_eq!(
        backing.capacity(),
        BYTES / 4,
        "raw commit changed fixed allocation capacity"
    );
    (key, backing)
}
pub(super) fn lifetime() {
    for kind in ["initial", "direct", "enum-frozen"] {
        for release in ["key", "runtime", "registry"] {
            held(kind, release);
        }
        drop_first(kind);
        thread_handoff(kind);
        logical_allowance(kind);
    }
    freeze_roundtrip();
}
fn held(kind: &str, release: &str) {
    let (s, r) = fixture(BYTES * 2);
    let (key, mut backing) = prepare(&r, kind);
    let want = if kind == "initial" {
        vec![0; 6]
    } else {
        let b = values().iter().map(|v| v.to_bits()).collect::<Vec<_>>();
        vec![b[0], b[2], b[4], b[1], b[3], b[5]]
    };
    assert_eq!(
        backing
            .samples()
            .iter()
            .map(|v| v.to_bits())
            .collect::<Vec<_>>(),
        want,
        "raw backing changed complete planar PCM"
    );
    backing[0] = -8.0;
    assert_eq!(
        backing.samples()[0].to_bits(),
        (-8.0f32).to_bits(),
        "raw mutable slice changed the wrong sample"
    );
    assert_eq!(s.process_usage(), (BYTES, 1));
    assert_eq!(
        s.physical.used_bytes(),
        BYTES,
        "raw commit lost allocation credit"
    );
    match release {
        "key" => {
            r.release_buffer(key);
        }
        "runtime" => r.finish_runtime_drop(7),
        _ => {}
    }
    drop(r);
    assert_eq!(s.process_usage(), (0, 0));
    assert_eq!(
        s.physical.used_bytes(),
        BYTES,
        "raw backing credit ended at logical release"
    );
    let peer = s.physical.try_reserve(BYTES, "peer").unwrap();
    assert!(
        s.physical.try_reserve(1, "another host").is_err(),
        "raw backing reopened peer physical admission"
    );
    drop(peer);
    drop(backing);
    assert_eq!(
        s.physical.used_bytes(),
        0,
        "raw final owner leaked physical credit"
    );
}
fn drop_first(kind: &str) {
    let (s, r) = fixture(BYTES * 2);
    let (key, backing) = prepare(&r, kind);
    drop(backing);
    assert_eq!(
        s.physical.used_bytes(),
        0,
        "raw storage drop retained physical credit in registry"
    );
    assert_eq!(
        s.process_usage(),
        (BYTES, 1),
        "raw storage drop changed logical process allowance"
    );
    r.release_buffer(key);
    assert_eq!(s.process_usage(), (0, 0));
}
fn thread_handoff(kind: &str) {
    let (s, r) = fixture(BYTES * 2);
    let (key, backing) = prepare(&r, kind);
    let pointer = backing.as_ptr() as usize;
    let (ready, started) = std::sync::mpsc::sync_channel(0);
    let (release, go) = std::sync::mpsc::sync_channel(0);
    let handle = std::thread::spawn(move || {
        ready.send(()).unwrap();
        go.recv_timeout(std::time::Duration::from_secs(15)).unwrap();
        assert_eq!(
            backing.as_ptr() as usize,
            pointer,
            "raw cross-thread handoff copied PCM"
        );
        assert_eq!(backing.len(), 6);
        drop(backing);
    });
    started
        .recv_timeout(std::time::Duration::from_secs(15))
        .unwrap();
    r.release_buffer(key);
    drop(r);
    assert_eq!(
        s.physical.used_bytes(),
        BYTES,
        "raw cross-thread owner lost credit at registry drop"
    );
    release.send(()).unwrap();
    handle.join().unwrap();
    assert_eq!(
        s.physical.used_bytes(),
        0,
        "raw cross-thread final release leaked credit"
    );
}
fn logical_allowance(kind: &str) {
    let (mut s, old) = fixture(BYTES * 2);
    drop(old);
    let count = if kind == "initial" { 1 } else { 2 };
    s.process = Arc::new(ProcessUsage::new(BYTES * count, count));
    let r = s.registry();
    let key = if kind == "initial" {
        r.reserve_initial_backing(7, format()).unwrap().key()
    } else {
        r.adopt_frozen(7, format(), values()).unwrap().key()
    };
    let p = if kind == "direct" {
        PreparedWritableAudioBacking::Materialized(r.prepare_materialize(key).unwrap())
    } else {
        r.prepare_writable_backing(key).unwrap()
    };
    let committed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match p {
        PreparedWritableAudioBacking::Materialized(p) if kind == "direct" => p.commit(),
        p => p.commit(),
    }));
    assert!(
        committed.is_ok(),
        "raw commit required additional logical allowance"
    );
    let backing = committed.unwrap();
    r.release_buffer(key);
    drop(backing);
    assert_eq!(s.process_usage(), (0, 0));
    assert_eq!(s.physical.used_bytes(), 0);
}
fn freeze_roundtrip() {
    let (s, r) = fixture(BYTES * 3);
    let (key, mut original) = prepare(&r, "initial");
    let data = [-0.0, 2.0, 4.5, 1.25, -3.0, 8.0];
    original.copy_from_slice(&data);
    let node = r.prepare_snapshot(key, Some(&original)).unwrap().commit();
    let second = r.prepare_writable_backing(key).unwrap().commit();
    assert_eq!(
        second.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
        data.map(f32::to_bits),
        "raw freeze roundtrip changed PCM"
    );
    assert_eq!(
        s.physical.used_bytes(),
        BYTES * 3,
        "raw freeze overlap omitted a live allocation"
    );
    r.release_buffer(key);
    drop(r);
    drop(node);
    assert_eq!(s.physical.used_bytes(), BYTES * 2);
    drop(original);
    assert_eq!(s.physical.used_bytes(), BYTES);
    drop(second);
    assert_eq!(s.physical.used_bytes(), 0);
}
