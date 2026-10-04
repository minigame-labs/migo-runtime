//! The Apple producer's decode estimate, and the packets it splits a frame into,
//! against this crate.
//!
//! A producer in WebContent cannot know this build's type sizes, and a packet
//! the host estimates over budget is refused -- which ends the content. So the
//! producer estimates with the contract's upper bounds
//! (`platforms/apple/WebContent/PerformancePlus/src/decode-budget.mjs`) and
//! splits frames against that. Two things are checked here, on data node wrote:
//!
//! 1. Its estimate is **exactly** `producer_estimated_bytes` for every stream --
//!    not merely at least this build's estimate, which a formula that drifted
//!    in the conservative direction would also satisfy -- and never below this
//!    build's own estimate.
//! 2. The packets it split frames into are admitted in order, each within the
//!    budget and the stream rules, presenting only at the end of a frame, and
//!    every 2D record decodes with its canvas selected.
//!
//! `#[ignore]` because the data comes from `node`, driven by
//! `scripts/test-frame-wire-js-encoder.sh`: the ignore is the visible form of
//! that dependency, and the gate is what guarantees this runs.

use std::{fs, path::PathBuf};

use frame_decode::{
    GlDecodeContext, MAX_DECODED_FRAME_BYTES, RenderSink, producer_estimated_bytes,
    validate_frame_budget,
};
use frame_wire::{FrameIngress, IngressDecision, stream::validate_frame_stream, validate};

const LAUNCH_NONCE: u128 = 0x0123_4567_89ab_cdef_fedc_ba98_7654_3210;

fn field<'a>(line: &'a str, key: &str) -> &'a str {
    let needle = format!("\"{key}\":");
    let start = line
        .find(&needle)
        .unwrap_or_else(|| panic!("manifest line has no {key}: {line}"))
        + needle.len();
    let rest = line[start..].trim_start();
    match rest.strip_prefix('"') {
        Some(quoted) => &quoted[..quoted.find('"').expect("closing quote")],
        None => rest[..rest.find([',', '}']).expect("value ends")].trim(),
    }
}

fn words_of(bytes: &[u8]) -> Vec<u32> {
    bytes
        .chunks_exact(4)
        .map(|w| u32::from_le_bytes([w[0], w[1], w[2], w[3]]))
        .collect()
}

fn directory() -> PathBuf {
    PathBuf::from(
        std::env::var("MIGO_ENGINE_FRAMES_TEST_DIR")
            .expect("MIGO_ENGINE_FRAMES_TEST_DIR names engine-frames.test.mjs's output"),
    )
}

#[test]
#[ignore = "needs streams from node; run through scripts/test-frame-wire-js-encoder.sh"]
fn the_producer_estimate_is_this_crate_s_formula_over_the_contract_s_bounds() {
    let root = directory().join("streams");
    let manifest = fs::read_to_string(root.join("manifest.jsonl")).expect("manifest");
    let mut checked = 0;
    let mut over = 0;
    for line in manifest.lines().filter(|line| !line.trim().is_empty()) {
        let name = field(line, "name");
        let words = words_of(&fs::read(root.join(name)).expect("stream"));
        let stream = validate_frame_stream(&words, words.len() as u32)
            .unwrap_or_else(|error| panic!("{name}: {error:?}"));
        let producer: usize = field(line, "estimate").parse().expect("estimate");
        assert_eq!(
            producer_estimated_bytes(&stream),
            producer,
            "{name}: the JavaScript estimate is not the contract's formula"
        );
        let actual = validate_frame_budget(&stream, usize::MAX)
            .expect("unbounded")
            .estimated_bytes();
        assert!(
            actual <= producer,
            "{name}: this build estimates {actual}, above the producer's {producer}"
        );
        if producer > MAX_DECODED_FRAME_BYTES {
            over += 1;
        }
        checked += 1;
    }
    assert!(checked >= 32, "checked {checked} streams");
    assert!(
        over > 0,
        "no stream was over the budget, so the split size was not exercised"
    );
    println!("{checked} producer estimates agree, {over} of them over budget");
}

/// Counts what decoding reports instead of drawing it.
#[derive(Default)]
struct Counting {
    errors: usize,
}

impl GlDecodeContext for Counting {
    fn image_source(&mut self, _: u32) -> Option<shared::protocol::render_cmd::TextureSource> {
        None
    }
    fn push_error(&mut self, _canvas_id: u32, _code: u32) {
        self.errors += 1;
    }
    fn transform_feedback_captures(&self, _canvas_id: u32) -> bool {
        false
    }
    fn set_transform_feedback(
        &mut self,
        _canvas_id: u32,
        _phase: frame_decode::TransformFeedbackPhase,
    ) {
    }
    fn staged_payload(&mut self) -> Option<&mut frame_decode::StagedPayload> {
        None
    }
}

impl RenderSink for Counting {
    fn canvas_batch(
        &mut self,
        _canvas_id: u32,
        _commands: shared::command_vec_pool::PooledVec<shared::protocol::render_cmd::Canvas2DCmd>,
    ) {
    }
    fn gl_batch(
        &mut self,
        _commands: shared::command_vec_pool::PooledVec<shared::protocol::render_cmd::GLCmd>,
        _approx_bytes: usize,
    ) {
    }
    fn materialize(&mut self, _canvas_id: u32) {}
}

#[test]
#[ignore = "needs packets from node; run through scripts/test-frame-wire-js-encoder.sh"]
fn a_frame_split_by_the_producer_is_admitted_whole_and_within_budget() {
    let root = directory().join("split");
    let manifest = fs::read_to_string(root.join("manifest.jsonl")).expect("manifest");
    let mut ingress = FrameIngress::new(LAUNCH_NONCE, 1);
    assert!(ingress.set_surface_generation(1));
    let (mut barriers, mut presents) = (0, 0);
    for line in manifest.lines().filter(|line| !line.trim().is_empty()) {
        let name = field(line, "name");
        let bytes = fs::read(root.join(name)).expect("packet");
        let frame = validate(&bytes).unwrap_or_else(|error| panic!("{name}: {error:?}"));
        assert_eq!(
            frame.presents().to_string(),
            field(line, "presents"),
            "{name}: presents"
        );
        if frame.presents() {
            presents += 1;
        } else {
            barriers += 1;
        }
        let words = words_of(frame.command_stream().expect("stream").bytes);
        let stream = validate_frame_stream(&words, words.len() as u32)
            .unwrap_or_else(|error| panic!("{name}: {error:?}"));
        let budget = validate_frame_budget(&stream, MAX_DECODED_FRAME_BYTES)
            .unwrap_or_else(|error| panic!("{name}: the host would refuse it: {error}"));
        let mut sink = Counting::default();
        frame_decode::decode_render_stream_into_with_plan(&mut sink, stream, budget);
        assert_eq!(
            sink.errors, 0,
            "{name}: decoding reported errors -- a 2D record without its canvas?"
        );

        let (outcome, owned) = ingress.submit(&bytes);
        assert_eq!(
            outcome.decision,
            IngressDecision::Accepted,
            "{name}: {outcome:?}"
        );
        drop(owned);
    }
    assert!(
        barriers >= 2 && presents == 2,
        "{barriers} barriers, {presents} presenting packets"
    );
    println!("admitted {barriers} barriers and {presents} presenting packets, each within budget");
}

/// A host that stages uploads, and keeps what each one decoded to.
#[derive(Default)]
struct Staging {
    staged: frame_decode::StagedPayload,
    /// `(opcode's name, the bytes the command owns)`, in decode order.
    uploads: Vec<(&'static str, Vec<u8>)>,
    errors: usize,
    /// The most the staging held at any packet boundary.
    peak_held: usize,
}

impl GlDecodeContext for Staging {
    fn push_error(&mut self, _canvas_id: u32, _code: u32) {
        self.errors += 1;
    }
    fn transform_feedback_captures(&self, _canvas_id: u32) -> bool {
        false
    }
    fn set_transform_feedback(
        &mut self,
        _canvas_id: u32,
        _phase: frame_decode::TransformFeedbackPhase,
    ) {
    }
    fn image_source(&mut self, _: u32) -> Option<shared::protocol::render_cmd::TextureSource> {
        None
    }
    fn staged_payload(&mut self) -> Option<&mut frame_decode::StagedPayload> {
        Some(&mut self.staged)
    }
}

impl RenderSink for Staging {
    fn canvas_batch(
        &mut self,
        _canvas_id: u32,
        _commands: shared::command_vec_pool::PooledVec<shared::protocol::render_cmd::Canvas2DCmd>,
    ) {
    }
    fn gl_batch(
        &mut self,
        commands: shared::command_vec_pool::PooledVec<shared::protocol::render_cmd::GLCmd>,
        _approx_bytes: usize,
    ) {
        use shared::protocol::render_cmd::GLCmd;
        for command in commands.iter() {
            match command {
                GLCmd::TexImage2D {
                    data: Some(shared::protocol::render_cmd::PixelUnpackSource::Bytes(data)),
                    ..
                } => {
                    self.uploads.push(("texImage2D", data.as_ref().clone()));
                }
                GLCmd::CompressedTexImage2D {
                    data: shared::protocol::render_cmd::CompressedImageData::Bytes(data),
                    ..
                } => {
                    self.uploads.push(("compressedTexImage2D", data.clone()));
                }
                GLCmd::BufferData {
                    data: Some(data), ..
                } => {
                    self.uploads.push(("bufferData", data.clone()));
                }
                _ => {}
            }
        }
    }
    fn materialize(&mut self, _canvas_id: u32) {}
}

/// Byte `i` of upload `seed`: `patternByte` in engine-frames.test.mjs.
fn pattern_byte(seed: u32, i: u32) -> u8 {
    (i.wrapping_mul(0x9e37_79b1).wrapping_add(seed) >> 24) as u8
}

#[test]
#[ignore = "needs packets from node; run through scripts/test-frame-wire-js-encoder.sh"]
fn uploads_larger_than_a_packet_arrive_staged_and_byte_for_byte() {
    let root = directory().join("staged");
    let manifest = fs::read_to_string(root.join("manifest.jsonl")).expect("manifest");
    let mut host = Staging::default();
    // Admission through ingress is the split test's; these come later in the
    // same producer's run, so they start mid-sequence. What holds here is that
    // they are one contiguous run, which is what a host admits.
    let mut previous: Option<u64> = None;
    for line in manifest.lines().filter(|line| !line.trim().is_empty()) {
        let name = field(line, "name");
        let bytes = fs::read(root.join(name)).expect("packet");
        let frame = validate(&bytes).unwrap_or_else(|error| panic!("{name}: {error:?}"));
        let sequence: u64 = field(line, "sequence").parse().expect("sequence");
        assert_eq!(frame.sequence(), sequence, "{name}: sequence");
        if let Some(previous) = previous {
            assert_eq!(sequence, previous + 1, "{name}: not contiguous");
        }
        previous = Some(sequence);
        let words = words_of(frame.command_stream().expect("stream").bytes);
        let stream = validate_frame_stream(&words, words.len() as u32)
            .unwrap_or_else(|error| panic!("{name}: {error:?}"));
        let budget = validate_frame_budget(&stream, MAX_DECODED_FRAME_BYTES)
            .unwrap_or_else(|error| panic!("{name}: the host would refuse it: {error}"));
        frame_decode::decode_render_stream_into_with_plan(&mut host, stream, budget);
        host.peak_held = host.peak_held.max(host.staged.held_bytes());
    }
    assert_eq!(host.errors, 0, "an upload failed to decode");
    assert_eq!(
        host.staged.held_bytes(),
        0,
        "nothing is left staged after its upload"
    );

    let expected = fs::read_to_string(root.join("uploads.jsonl")).expect("uploads");
    let expected: Vec<(u32, usize)> = expected
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            (
                field(line, "seed").parse().expect("seed"),
                field(line, "bytes").parse().expect("bytes"),
            )
        })
        .collect();
    assert_eq!(
        host.uploads.len(),
        expected.len(),
        "{:?}",
        host.uploads
            .iter()
            .map(|u| (u.0, u.1.len()))
            .collect::<Vec<_>>()
    );
    for ((name, got), (seed, length)) in host.uploads.iter().zip(&expected) {
        assert_eq!(got.len(), *length, "{name} {seed}: length");
        let first_wrong = (0..*length as u32).find(|&i| got[i as usize] != pattern_byte(*seed, i));
        assert_eq!(
            first_wrong, None,
            "{name} {seed}: the first byte that differs"
        );
    }
    // The largest payload is the only one staged at a packet boundary: no copy
    // of it is kept beside the one moved into its command.
    assert!(
        host.peak_held <= 8 * 1024 * 1024,
        "the staging held {} bytes at a packet boundary",
        host.peak_held
    );
    println!(
        "{} uploads decoded byte for byte, staging peaked at {} bytes between packets",
        host.uploads.len(),
        host.peak_held
    );
}
