//! The vertex attribute records: constant values (`vertexAttrib*`, `vertexAttribI4*`) and `vertexAttribIPointer`.
//!
//! A record decodes into exactly the command the call means, and `vertexAttribIPointer` is refused, with the
//! error the specification names and nothing forwarded to the renderer, for the arguments that make it invalid:
//! a size outside 1..=4 or a stride outside 0..=255 (INVALID_VALUE), and a type that is not one of the six integer
//! types -- FLOAT and HALF_FLOAT among them, because there is nothing to convert an integer attribute to (INVALID_ENUM).

use frame_decode::{GlDecodeContext, RenderSink, decode_render_stream_into};
use frame_wire::gl::{
    OP_VERTEX_ATTRIB_4F, OP_VERTEX_ATTRIB_I_POINTER, OP_VERTEX_ATTRIB_I4I, OP_VERTEX_ATTRIB_I4UI,
};
use frame_wire::stream::{MAGIC, STREAM_VERSION, pack_header, validate_stream};
use shared::command_vec_pool::PooledVec;
use shared::protocol::render_cmd::{Canvas2DCmd, GLCmd};

const INVALID_ENUM: u32 = 0x0500;
const INVALID_VALUE: u32 = 0x0501;

#[derive(Default)]
struct Recorder {
    commands: Vec<String>,
    errors: Vec<(u32, u32)>,
}

impl GlDecodeContext for Recorder {
    fn image_upload(&mut self, _upload: frame_decode::ImageUpload) -> Option<GLCmd> {
        None
    }
    fn push_error(&mut self, canvas_id: u32, code: u32) {
        self.errors.push((canvas_id, code));
    }
    fn transform_feedback_captures(&self, _canvas_id: u32) -> bool {
        false
    }
    fn set_transform_feedback(&mut self, _canvas_id: u32, _phase: frame_decode::TransformFeedbackPhase) {}
    fn staged_payload(&mut self) -> Option<&mut frame_decode::StagedPayload> {
        None
    }
}

impl RenderSink for Recorder {
    fn canvas_batch(&mut self, _canvas_id: u32, _commands: PooledVec<Canvas2DCmd>) {}
    fn gl_batch(&mut self, commands: PooledVec<GLCmd>, _approx_bytes: usize) {
        self.commands.extend(commands.iter().map(|cmd| format!("{cmd:?}")));
    }
    fn materialize(&mut self, _canvas_id: u32) {}
}

fn record(opcode: u32, words: &[u32]) -> Vec<u32> {
    let mut out = vec![pack_header(opcode, words.len() as u32 + 1)];
    out.extend_from_slice(words);
    out
}

fn decode(records: &[Vec<u32>]) -> Recorder {
    let mut words = vec![MAGIC, STREAM_VERSION];
    for r in records {
        words.extend_from_slice(r);
    }
    let stream = validate_stream(&words, words.len() as u32).expect("structurally valid");
    let mut recorder = Recorder::default();
    decode_render_stream_into(&mut recorder, stream);
    recorder
}

#[test]
fn the_constant_value_records_decode_into_their_commands() {
    let recorder = decode(&[
        record(
            OP_VERTEX_ATTRIB_4F,
            &[3, 2, 0.5f32.to_bits(), 1.5f32.to_bits(), (-2.0f32).to_bits(), 1.0f32.to_bits()],
        ),
        record(OP_VERTEX_ATTRIB_I4I, &[3, 4, (-1i32) as u32, 2, (-3i32) as u32, 4]),
        record(OP_VERTEX_ATTRIB_I4UI, &[3, 5, u32::MAX, 2, 3, 4]),
    ]);
    assert!(recorder.errors.is_empty(), "{:?}", recorder.errors);
    assert_eq!(recorder.commands.len(), 3, "{:?}", recorder.commands);
    let all = recorder.commands.join(" | ");
    assert!(all.contains("VertexAttrib4f") && all.contains("x: 0.5") && all.contains("y: 1.5") && all.contains("z: -2.0"), "{all}");
    assert!(all.contains("VertexAttribI4i") && all.contains("x: -1") && all.contains("z: -3"), "{all}");
    assert!(all.contains("VertexAttribI4ui") && all.contains(&format!("x: {}", u32::MAX)), "{all}");
}

fn ipointer(size: i32, type_: u32, stride: i32, offset: i32) -> Recorder {
    decode(&[record(
        OP_VERTEX_ATTRIB_I_POINTER,
        &[3, 1, size as u32, type_, stride as u32, offset as u32],
    )])
}

#[test]
fn an_integer_pointer_is_one_of_the_six_integer_types_and_nothing_else() {
    for type_ in [0x1400u32, 0x1401, 0x1402, 0x1403, 0x1404, 0x1405] {
        let recorder = ipointer(2, type_, 8, 4);
        assert!(recorder.errors.is_empty(), "type {type_:#06x}: {:?}", recorder.errors);
        assert_eq!(recorder.commands.len(), 1, "type {type_:#06x}");
        assert!(recorder.commands[0].contains("VertexAttribIPointer"), "{}", recorder.commands[0]);
    }
    // FLOAT, HALF_FLOAT and a made-up type: INVALID_ENUM, and nothing reaches the renderer.
    for type_ in [0x1406u32, 0x140B, 0x1234] {
        let recorder = ipointer(2, type_, 8, 4);
        assert_eq!(recorder.errors, vec![(3, INVALID_ENUM)], "type {type_:#06x}");
        assert!(recorder.commands.is_empty(), "type {type_:#06x}: {:?}", recorder.commands);
    }
}

#[test]
fn an_integer_pointer_with_a_size_stride_or_offset_out_of_range_is_invalid_value() {
    for (size, stride, offset) in [(0, 0, 0), (5, 0, 0), (-1, 0, 0), (2, -1, 0), (2, 256, 0), (2, 0, -1)] {
        let recorder = ipointer(size, 0x1404, stride, offset);
        assert_eq!(recorder.errors, vec![(3, INVALID_VALUE)], "size {size} stride {stride} offset {offset}");
        assert!(recorder.commands.is_empty(), "size {size} stride {stride} offset {offset}");
    }
    // the edges that are fine
    for (size, stride, offset) in [(1, 0, 0), (4, 255, 0), (3, 12, 1 << 20)] {
        let recorder = ipointer(size, 0x1405, stride, offset);
        assert!(recorder.errors.is_empty() && recorder.commands.len() == 1, "size {size} stride {stride} offset {offset}");
    }
}
