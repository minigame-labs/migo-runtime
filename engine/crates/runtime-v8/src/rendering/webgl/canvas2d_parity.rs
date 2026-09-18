//! The producer's Canvas2D text records decode to exactly the commands this
//! crate's ops build for the same calls.
//!
//! The WebGL half of this argument is in `resource_parity.rs`, and the reason is
//! the same one: on iOS Performance+ the engine's 2D facade runs in WebContent
//! and its text calls become records (`frame_wire::canvas2d`) the host decodes
//! (`frame_decode::canvas2d`); in process the same facade calls the ops in
//! `context2d.rs`. Text is where the two are most likely to drift, because it is
//! the one part of the 2D block that carries strings -- a font shorthand parsed
//! on both sides, a `maxWidth` that is usually `Infinity`, and alignment
//! keywords that are numbers on the wire.
//!
//! `#[ignore]` because the producer's packets come from node, driven by
//! `scripts/test-performance-plus-engine-contract.sh`.

use std::path::PathBuf;

use shared::command_vec_pool::PooledVec;
use shared::protocol::render_cmd::{Canvas2DCmd, GLCmd, RenderCommand};
use shared::protocol::{CanvasBatchPayload, FrameOp};

use super::webgl::tests::{end_test_frame, new_webgl_runtime};

/// The decoder's host for the producer's packets. Canvas2D pushes no WebGL
/// errors and keeps no transform-feedback state, so the context half answers
/// what a canvas-only decode needs and nothing else; the sink is what collects.
#[derive(Default)]
struct Host {
    commands: Vec<String>,
}

impl frame_decode::GlDecodeContext for Host {
    fn push_error(&mut self, _canvas_id: u32, _code: u32) {}
    fn transform_feedback_captures(&self, _canvas_id: u32) -> bool {
        false
    }
    fn set_transform_feedback(
        &mut self,
        _canvas_id: u32,
        _phase: frame_decode::TransformFeedbackPhase,
    ) {
    }
}

impl frame_decode::RenderSink for Host {
    fn canvas_batch(&mut self, canvas_id: u32, commands: PooledVec<Canvas2DCmd>) {
        for command in commands.iter() {
            self.commands.push(format!("{canvas_id} {command:?}"));
        }
    }
    fn gl_batch(&mut self, commands: PooledVec<GLCmd>, _approx_bytes: usize) {
        for command in commands.iter() {
            panic!("the 2D fixture produced a GL command: {command:?}");
        }
    }
    fn materialize(&mut self, _canvas_id: u32) {}
}

fn repository() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

/// The commands of one canvas batch, tagged with the canvas so a record that
/// reached the wrong one is a difference rather than a reordering.
fn batch_commands(batch: &CanvasBatchPayload) -> Vec<String> {
    batch
        .commands
        .iter()
        .map(|command| format!("{} {command:?}", batch.canvas_id))
        .collect()
}

#[test]
#[ignore = "needs the producer's packets from node; run through scripts/test-performance-plus-engine-contract.sh"]
fn the_producer_s_text_records_decode_to_the_commands_the_ops_build() {
    let directory = PathBuf::from(
        std::env::var("MIGO_CANVAS2D_PARITY_DIR")
            .expect("MIGO_CANVAS2D_PARITY_DIR names engine-resource-parity.mjs's output"),
    );
    let script = std::fs::read_to_string(
        repository()
            .join("platforms/apple/WebContent/PerformancePlus/test/fixtures/canvas2d-text-calls.js"),
    )
    .expect("the fixture script");

    // In process.
    let (mut runtime, render_rx) = new_webgl_runtime();
    runtime
        .exec_script_owned("canvas2d-text-calls.js".to_string(), &script)
        .expect("the fixture runs in the embedded runtime");
    end_test_frame(&mut runtime);
    let mut embedded = Vec::new();
    while let Ok(command) = render_rx.try_recv() {
        if let RenderCommand::FramePacket(packet) = command {
            for op in packet.into_ops() {
                if let FrameOp::CanvasBatch(batch) = op {
                    embedded.extend(batch_commands(&batch));
                }
            }
        }
    }

    // On the producer.
    let mut packets: Vec<PathBuf> = std::fs::read_dir(&directory)
        .expect("the parity directory")
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().is_some_and(|ext| ext == "bin"))
        .collect();
    packets.sort();
    assert!(!packets.is_empty(), "the producer wrote no packets");
    let mut host = Host::default();
    for path in &packets {
        let bytes = std::fs::read(path).expect("a packet");
        let frame = frame_wire::validate(&bytes)
            .unwrap_or_else(|error| panic!("{}: {error:?}", path.display()));
        let words: Vec<u32> = frame
            .command_stream()
            .expect("a command stream")
            .bytes
            .chunks_exact(4)
            .map(|w| u32::from_le_bytes([w[0], w[1], w[2], w[3]]))
            .collect();
        let stream = frame_wire::stream::validate_frame_stream(&words, words.len() as u32)
            .unwrap_or_else(|error| panic!("{}: the stream is refused: {error:?}", path.display()));
        frame_decode::decode_render_stream_into(&mut host, stream);
    }
    // One record has no command on the other side, by design:
    // `OP2D_CREATE_CONTEXT` is how the external lane brings a 2D context into
    // existence, where in process `getContext("2d")` is an op that creates it
    // directly. Its absence is what the opcode's own documentation describes,
    // and the producer must still send exactly one.
    let contexts = host
        .commands
        .iter()
        .filter(|command| command.contains("CreateContext2D"))
        .count();
    assert_eq!(
        contexts, 1,
        "the producer sends one CreateContext2D for the fixture's one context"
    );
    let decoded: Vec<String> = host
        .commands
        .into_iter()
        .filter(|command| !command.contains("CreateContext2D"))
        .collect();

    if let Some(index) =
        (0..embedded.len().max(decoded.len())).find(|&index| embedded.get(index) != decoded.get(index))
    {
        panic!(
            "command {index} differs (in process {} commands, producer {}):\n  in process: {}\n  producer:   {}",
            embedded.len(),
            decoded.len(),
            embedded.get(index).map_or("<none>", String::as_str),
            decoded.get(index).map_or("<none>", String::as_str),
        );
    }
    assert!(
        embedded.len() >= 15,
        "the fixture built only {} commands; it covers more than that",
        embedded.len()
    );
    println!(
        "{} Canvas2D commands agree between the in-process ops and the producer's records",
        embedded.len()
    );
}
