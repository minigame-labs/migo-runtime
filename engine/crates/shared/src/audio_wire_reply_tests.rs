//! Final little-endian response owns its capacity beyond logical release.
use super::*;
const PCM: usize = 24;
const REPLY: usize = PCM + 12;
fn format() -> AudioBufferFormat {
    AudioBufferFormat {
        channels: 2,
        frames: 3,
        sample_rate: 48000,
    }
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
pub(super) fn lifetime() {
    for initial in [true, false] {
        for release in ["key", "runtime", "registry"] {
            owned(initial, release);
        }
        refused(initial);
    }
    before_allocation();
}
fn owned(initial: bool, release: &str) {
    let (s, r) = fixture(PCM * 2 + REPLY);
    let bits = [
        0x80000000, 0x3fc00000, 0x40000000, 0xc0500000, 0x7f800000, 0x7fc00003,
    ];
    let key = if initial {
        r.reserve_initial_backing(7, format()).unwrap().key()
    } else {
        r.adopt_frozen(7, format(), bits.map(f32::from_bits).to_vec())
            .unwrap()
            .key()
    };
    let p = r.prepare_writable_backing(key).unwrap();
    let values = if initial {
        vec![0; 6]
    } else {
        vec![bits[0], bits[2], bits[4], bits[1], bits[3], bits[5]]
    };
    let prefix = [0x25; 12];
    let reply = p.encode_le_reply(&prefix).unwrap();
    assert_eq!(reply.len(), REPLY, "wire response shape changed");
    assert_eq!(
        reply.capacity(),
        REPLY,
        "wire response allocation exceeded physical ticket"
    );
    assert_eq!(&reply[..12], &prefix, "wire response prefix changed");
    let actual: Vec<_> = reply[12..]
        .chunks_exact(4)
        .map(|b| u32::from_le_bytes(b.try_into().unwrap()))
        .collect();
    assert_eq!(
        actual, values,
        "wire response changed complete little endian PCM"
    );
    assert_eq!(
        s.physical.used_bytes(),
        REPLY,
        "wire response omitted final capacity or retained temporary PCM"
    );
    assert!(
        r.prepare_writable_backing(key).is_err(),
        "wire response did not commit writable transition"
    );
    match release {
        "key" => {
            r.release_buffer(key);
        }
        "runtime" => r.finish_runtime_drop(7),
        _ => {}
    }
    let pointer = reply.as_ptr();
    drop(r);
    assert_eq!(s.process_usage(), (0, 0));
    assert_eq!(
        s.physical.used_bytes(),
        REPLY,
        "logical release returned live wire response credit"
    );
    let moved = reply;
    assert_eq!(
        moved.as_ptr(),
        pointer,
        "wire response ownership handoff copied bytes"
    );
    drop(moved);
    assert_eq!(
        s.physical.used_bytes(),
        0,
        "wire response final release leaked credit"
    );
}
fn refused(initial: bool) {
    let (s, r) = fixture(PCM * 2 + REPLY);
    let key = if initial {
        r.reserve_initial_backing(7, format()).unwrap().key()
    } else {
        r.adopt_frozen(7, format(), vec![1.0; 6]).unwrap().key()
    };
    let p = r.prepare_writable_backing(key).unwrap();
    let held = if initial { 0 } else { PCM };
    let peer = s.physical.try_reserve(PCM + 1 - held, "peer").unwrap();
    let result = p.encode_le_reply(&[0; 12]);
    assert!(
        matches!(result,Err(e)if e.code==ErrorCode::InputSaturated),
        "wire reply bypassed overlapping output admission"
    );
    assert_eq!(
        s.physical.used_bytes(),
        PCM + 1,
        "wire reply refusal leaked temporary PCM or credit"
    );
    drop(peer);
    let retry = r.prepare_writable_backing(key);
    assert!(
        retry.is_ok(),
        "wire reply refusal committed original registry state"
    );
    let reply = retry.unwrap().encode_le_reply(&[0; 12]).unwrap();
    r.release_buffer(key);
    drop(reply);
    assert_eq!(s.physical.used_bytes(), 0);
    assert_eq!(s.process_usage(), (0, 0));
}
fn before_allocation() {
    let bytes = 4 << 20;
    let mut s = AudioResourceTestScope::new(AudioResourceLimits {
        max_single_bytes: bytes,
        max_runtime_bytes: bytes,
        max_runtime_buffers: 1,
        max_process_bytes: bytes,
        max_process_buffers: 1,
    });
    s.physical = Arc::new(AudioAggregateLedger::new(bytes * 2 + 11));
    let r = s.registry();
    let k = r
        .reserve_initial_backing(
            7,
            AudioBufferFormat {
                channels: 1,
                frames: (bytes / 4) as u32,
                sample_rate: 48000,
            },
        )
        .unwrap()
        .key();
    let p = r.prepare_writable_backing(k).unwrap();
    let before = migo_alloc_probe::thread_counts();
    let result = p.encode_le_reply(&[0; 12]);
    assert!(matches!(result,Err(e)if e.code==ErrorCode::InputSaturated));
    assert!(
        migo_alloc_probe::thread_counts().bytes_allocated - before.bytes_allocated < 65536,
        "wire reply refusal allocated output before physical admission"
    );
    assert_eq!(s.physical.used_bytes(), 0);
    r.release_buffer(k);
}
