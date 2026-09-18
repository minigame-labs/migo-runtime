//! The Performance+ producer's resource records decode to exactly the commands
//! this crate's ops build for the same calls.
//!
//! On iOS Performance+ the engine's WebGL facade runs in WebKit's WebContent
//! process, and every resource call it makes -- `createBuffer`, `shaderSource`,
//! `texImage3D` -- becomes a record (`frame_wire::gl_resource`) that the host
//! decodes (`frame_decode::resource`). In process the same facade calls the ops
//! in `webgl.rs`. The two must be indistinguishable downstream, or the lane that
//! is tested less draws differently.
//!
//! So the same script -- `platforms/apple/WebContent/PerformancePlus/test/
//! fixtures/webgl-resource-calls.js` -- runs twice: here, in a real V8 runtime
//! whose ops fill the frame collector, and on the producer, whose packets
//! `engine-resource-parity.mjs` wrote. The GL commands of both, in order, and the
//! WebGL errors each recorded, must be equal.
//!
//! `#[ignore]` because the producer's packets come from node, driven by
//! `scripts/test-performance-plus-engine-contract.sh`; the ignore is the visible
//! form of that dependency, and the gate is what guarantees this runs.

use std::path::PathBuf;

use shared::protocol::{FrameOp, render_cmd::RenderCommand};

use super::error_state::WebGLErrorState;
use super::webgl::tests::{end_test_frame, new_webgl_runtime};

/// The decoder's host for the producer's packets: errors into a list, and the
/// transform-feedback state the in-process runtime keeps beside its queues.
#[derive(Default)]
struct Host {
    errors: Vec<(u32, u32)>,
    capturing: Vec<u32>,
}

impl frame_decode::GlDecodeContext for Host {
    fn image_upload(
        &mut self,
        _upload: frame_decode::ImageUpload,
    ) -> Option<shared::protocol::render_cmd::GLCmd> {
        None
    }
    fn push_error(&mut self, canvas_id: u32, code: u32) {
        self.errors.push((canvas_id, code));
    }

    fn transform_feedback_captures(&self, canvas_id: u32) -> bool {
        self.capturing.contains(&canvas_id)
    }

    fn set_transform_feedback(
        &mut self,
        canvas_id: u32,
        phase: frame_decode::TransformFeedbackPhase,
    ) {
        self.capturing.retain(|id| *id != canvas_id);
        if phase == frame_decode::TransformFeedbackPhase::Active {
            self.capturing.push(canvas_id);
        }
    }
}

fn repository() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

#[test]
#[ignore = "needs the producer's packets from node; run through scripts/test-performance-plus-engine-contract.sh"]
fn the_producer_s_resource_records_decode_to_the_commands_the_ops_build() {
    let directory = PathBuf::from(
        std::env::var("MIGO_RESOURCE_PARITY_DIR")
            .expect("MIGO_RESOURCE_PARITY_DIR names engine-resource-parity.mjs's output"),
    );
    let script =
        std::fs::read_to_string(repository().join(
            "platforms/apple/WebContent/PerformancePlus/test/fixtures/webgl-resource-calls.js",
        ))
        .expect("the fixture script");

    // In process.
    let (mut runtime, render_rx) = new_webgl_runtime();
    runtime
        .exec_script_owned("webgl-resource-calls.js".to_string(), &script)
        .expect("the fixture runs in the embedded runtime");
    end_test_frame(&mut runtime);
    let mut embedded = Vec::new();
    while let Ok(command) = render_rx.try_recv() {
        if let RenderCommand::FramePacket(packet) = command {
            for op in packet.into_ops() {
                if let FrameOp::GlBatch(batch) = op {
                    embedded.extend(batch.commands.iter().map(|command| format!("{command:?}")));
                }
            }
        }
    }
    let mut embedded_errors = Vec::new();
    {
        let state = runtime.op_state_for_test();
        let mut state = state.borrow_mut();
        let queue = state.borrow_mut::<WebGLErrorState>();
        for canvas_id in [0, 1] {
            loop {
                let code = queue.drain_one(canvas_id);
                if code == 0 {
                    break;
                }
                embedded_errors.push((canvas_id, code));
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
    let mut decoded = Vec::new();
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
        let mut commands = Vec::new();
        frame_decode::decode_validated_stream(&mut host, stream, &mut commands);
        decoded.extend(commands.iter().map(|command| format!("{command:?}")));
    }
    let mut producer_errors = host.errors;
    let recorded =
        std::fs::read_to_string(directory.join("producer-errors.txt")).unwrap_or_default();
    for line in recorded.lines().filter(|line| !line.trim().is_empty()) {
        let mut fields = line
            .split_whitespace()
            .map(|field| field.parse::<u32>().expect("a number"));
        producer_errors.push((fields.next().expect("canvas"), fields.next().expect("code")));
    }

    if let Some(index) = (0..embedded.len().max(decoded.len()))
        .find(|&index| embedded.get(index) != decoded.get(index))
    {
        panic!(
            "command {index} differs (in process {} commands, producer {}):\n  in process: {}\n  producer:   {}",
            embedded.len(),
            decoded.len(),
            embedded.get(index).map_or("<none>", String::as_str),
            decoded.get(index).map_or("<none>", String::as_str),
        );
    }
    // Where an error is found can differ -- the facade, an op, the decoder --
    // and so can when it is queued; which errors, per canvas, cannot.
    embedded_errors.sort_unstable();
    producer_errors.sort_unstable();
    assert_eq!(
        producer_errors, embedded_errors,
        "the recorded WebGL errors differ"
    );
    assert!(
        embedded.len() >= 60,
        "the fixture built only {} commands; it covers more than that",
        embedded.len()
    );
    println!(
        "{} commands and {} errors agree between the in-process ops and the producer's records",
        embedded.len(),
        embedded_errors.len()
    );
}
