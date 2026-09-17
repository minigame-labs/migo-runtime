//! Frames the engine's own WebGL facade produced on the Performance+ producer,
//! admitted by the host's ingress and read by its stream validator.
//!
//! `test/engine-bundle.test.mjs` runs the engine's JavaScript API layer staged
//! for WebContent and writes the packets it sends. That test checks their bytes
//! against what the facade encodes; this checks that the host takes them --
//! identity, sequence, surface generation and a command stream it accepts -- in
//! the order they were sent. Between them, a clear issued by content through
//! `migo.createCanvas().getContext("webgl")` is shown to reach the renderer's
//! decoder unchanged, without an iOS device.
//!
//! `#[ignore]` because the packets come from `node`;
//! `scripts/test-performance-plus-engine-contract.sh` is what runs it.

use std::{fs, path::PathBuf};

use frame_wire::{FrameIngress, IngressDecision, stream::validate_frame_stream, validate};

/// The identity `engine-bundle.test.mjs` binds its engine host with.
const LAUNCH_NONCE: u128 = 0x0123_4567_89ab_cdef_fedc_ba98_7654_3210;
const RUNTIME_GENERATION: u64 = 1;
const SURFACE_GENERATION: u64 = 7;

#[test]
#[ignore = "needs frames from node; run through scripts/test-performance-plus-engine-contract.sh"]
fn frames_from_the_engine_facade_are_admitted_in_order_and_their_streams_validate() {
    let directory = PathBuf::from(
        std::env::var("MIGO_ENGINE_FRAME_DIR").expect("MIGO_ENGINE_FRAME_DIR names the frames"),
    );
    let mut frames: Vec<PathBuf> = fs::read_dir(&directory)
        .expect("the frame directory exists")
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|path| path.extension().is_some_and(|ext| ext == "bin"))
        .collect();
    frames.sort();
    assert!(
        frames.len() >= 2,
        "the node test writes at least two frames"
    );

    let mut ingress = FrameIngress::new(LAUNCH_NONCE, RUNTIME_GENERATION);
    assert!(ingress.set_surface_generation(SURFACE_GENERATION));
    let mut admitted = 0;
    for path in &frames {
        let bytes = fs::read(path).expect("read a frame");
        let frame =
            validate(&bytes).unwrap_or_else(|error| panic!("{}: {error:?}", path.display()));
        let stream = frame
            .command_stream()
            .expect("every frame carries a command stream");
        let words: Vec<u32> = stream
            .bytes
            .chunks_exact(4)
            .map(|w| u32::from_le_bytes([w[0], w[1], w[2], w[3]]))
            .collect();
        validate_frame_stream(&words, words.len() as u32)
            .unwrap_or_else(|error| panic!("{}: the stream is refused: {error:?}", path.display()));

        let (outcome, owned) = ingress.submit(&bytes);
        assert_eq!(
            outcome.decision,
            IngressDecision::Accepted,
            "{} was not admitted: {outcome:?}",
            path.display()
        );
        drop(owned);
        admitted += 1;
    }
    println!("admitted {admitted} frames from the engine's WebGL facade");
}
