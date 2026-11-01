//! Registry credit must replace response credit before publishing, and follow
//! the original samples through admission refusal and the last source Arc.
use super::audio_decoded_pcm::{DecodedPcm, DecodedPcmCredit, DecodedPcmCustody};
use super::audio_resources::*;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
struct Packet {
    samples: Vec<f32>,
    _credit: Option<DecodedPcmCredit>,
}
#[derive(Default)]
struct State {
    adopted: AtomicUsize,
    packets: Mutex<Vec<Packet>>,
}
struct Owner(Arc<State>);
impl DecodedPcmCustody for Owner {
    fn adopted(&mut self) {
        self.0.adopted.fetch_add(1, Ordering::Release);
    }
    fn dispose(self: Box<Self>, samples: Vec<f32>, credit: Option<DecodedPcmCredit>) {
        self.0.packets.lock().unwrap().push(Packet {
            samples,
            _credit: credit,
        });
    }
}
fn limits() -> AudioResourceLimits {
    AudioResourceLimits {
        max_single_bytes: 96,
        max_runtime_bytes: 48,
        max_runtime_buffers: 2,
        max_process_bytes: 96,
        max_process_buffers: 3,
    }
}
fn format() -> AudioBufferFormat {
    AudioBufferFormat {
        sample_rate: 48000,
        channels: 2,
        frames: 3,
    }
}
fn pcm(state: &Arc<State>) -> DecodedPcm {
    DecodedPcm::with_custody(
        48000,
        2,
        3,
        vec![-0.0, 1.0, 2.0, 3.0, 4.0, 5.0],
        Box::new(Owner(state.clone())),
    )
}
pub(crate) fn ownership() {
    for mode in ["release", "generation", "materialize", "registry-drop"] {
        final_owner(mode);
    }
    for mode in [
        "shape",
        "format",
        "runtime",
        "retired",
        "serial",
        "process-bytes",
        "process-count",
    ] {
        refusal(mode);
    }
}
fn final_owner(mode: &str) {
    let scope = AudioResourceTestScope::new(limits());
    let registry = scope.registry();
    let state = Arc::new(State::default());
    let decoded = pcm(&state);
    let pointer = decoded.samples.as_ptr();
    let lease = registry.adopt_decoded(1, format(), decoded).unwrap();
    assert_eq!(
        state.adopted.load(Ordering::Acquire),
        1,
        "decoded registry never exchanged transport credit"
    );
    assert_eq!(
        scope.process_usage(),
        (24, 1),
        "decoded registry double charged physical storage"
    );
    let prepared = registry.prepare_snapshot(lease.key(), None).unwrap();
    let node = prepared.snapshot();
    drop(prepared);
    assert_eq!(
        node.samples().as_ptr(),
        pointer,
        "decoded registry copied original PCM"
    );
    match mode {
        "release" => {
            registry.release_buffer(lease.key());
        }
        "generation" => registry.finish_runtime_drop(1),
        "materialize" => {
            let _ = registry.prepare_materialize(lease.key()).unwrap().commit();
            registry.release_buffer(lease.key());
        }
        "registry-drop" => {}
        _ => unreachable!(),
    }
    drop(registry);
    assert_eq!(
        scope.process_usage(),
        (24, 1),
        "decoded source Arc lost physical credit"
    );
    assert_eq!(
        node.samples()
            .iter()
            .map(|v| v.to_bits())
            .collect::<Vec<_>>(),
        [-0.0f32, 1.0, 2.0, 3.0, 4.0, 5.0].map(f32::to_bits)
    );
    drop(node);
    assert_eq!(
        scope.process_usage(),
        (24, 1),
        "decoded registry credit returned before sample disposal"
    );
    let mut packets = state.packets.lock().unwrap();
    assert_eq!(
        packets.len(),
        1,
        "decoded final owner lost disposal custody"
    );
    assert_eq!(packets[0].samples.as_ptr(), pointer);
    packets.clear();
    assert_eq!(
        scope.process_usage(),
        (0, 0),
        "decoded registry credit leaked after sample disposal"
    );
}
fn refusal(mode: &str) {
    let mut policy = limits();
    if mode == "process-count" {
        policy.max_process_buffers = 1;
    }
    if mode == "process-bytes" {
        policy.max_process_bytes = 24;
    }
    let scope = AudioResourceTestScope::new(policy);
    let registry = if mode == "serial" {
        scope.registry_with_next_serial(0)
    } else {
        scope.registry()
    };
    let state = Arc::new(State::default());
    let mut held = Vec::new();
    let other = scope.registry();
    if mode == "runtime" {
        for _ in 0..2 {
            held.push(registry.reserve_backing(1, format()).unwrap());
        }
    }
    if matches!(mode, "process-count" | "process-bytes") {
        held.push(other.reserve_backing(9, format()).unwrap());
    }
    if mode == "retired" {
        registry.finish_runtime_drop(1);
    }
    let before = scope.process_usage();
    let mut decoded = pcm(&state);
    let mut f = format();
    if mode == "shape" {
        decoded.samples.pop();
    }
    if mode == "format" {
        f.channels = 0;
    }
    let answer = registry.adopt_decoded(1, f, decoded);
    assert!(answer.is_err(), "decoded registry accepted refused input");
    let error = answer.unwrap_err();
    let expected = if matches!(mode, "shape" | "format") {
        crate::error::ErrorCode::InvalidArgument
    } else if matches!(mode, "retired" | "serial") {
        crate::error::ErrorCode::InvalidOperation
    } else {
        crate::error::ErrorCode::InputSaturated
    };
    assert_eq!(
        error.code, expected,
        "decoded registry changed typed refusal"
    );
    let transferred = usize::from(matches!(mode, "runtime" | "retired" | "serial"));
    assert_eq!(
        state.adopted.load(Ordering::Acquire),
        transferred,
        "decoded refusal returned transport credit without replacement admission"
    );
    assert_eq!(
        scope.process_usage(),
        (before.0 + 24 * transferred, before.1 + transferred),
        "refused decoded registry credit returned before sample disposal"
    );
    assert_eq!(state.packets.lock().unwrap().len(), 1);
    state.packets.lock().unwrap().clear();
    assert_eq!(scope.process_usage(), before);
    drop(held);
    drop(other);
    drop(registry);
}
