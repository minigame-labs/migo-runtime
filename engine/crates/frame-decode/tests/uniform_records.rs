//! The unsigned vector and non-square matrix uniform records (WebGL 2): opcodes 267..=276.
//!
//! Each decodes into the command its name says, carrying the payload words as the type the uniform has -- the unsigned
//! ones as `u32` (a word above `i32::MAX` is not negative), the matrices as `f32` with their transpose flag -- and a
//! record whose bits make a number that would be wrong as the other type shows it.

use frame_decode::{GlDecodeContext, RenderSink, decode_render_stream_into};
use frame_wire::gl::{
    OP_UNIFORM_MATRIX2X3FV, OP_UNIFORM_MATRIX2X4FV, OP_UNIFORM_MATRIX3X2FV, OP_UNIFORM_MATRIX3X4FV,
    OP_UNIFORM_MATRIX4X2FV, OP_UNIFORM_MATRIX4X3FV, OP_UNIFORM1UIV, OP_UNIFORM2UIV, OP_UNIFORM3UIV,
    OP_UNIFORM4UIV,
};
use frame_wire::stream::{MAGIC, STREAM_VERSION, pack_header, validate_stream};
use shared::command_vec_pool::PooledVec;
use shared::protocol::render_cmd::{Canvas2DCmd, GLCmd};

#[derive(Default)]
struct Recorder {
    commands: Vec<String>,
}

impl GlDecodeContext for Recorder {
    fn image_source(&mut self, _: u32) -> Option<shared::protocol::render_cmd::TextureSource> {
        None
    }
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
    fn staged_payload(&mut self) -> Option<&mut frame_decode::StagedPayload> {
        None
    }
}

impl RenderSink for Recorder {
    fn canvas_batch(&mut self, _canvas_id: u32, _commands: PooledVec<Canvas2DCmd>) {}
    fn gl_batch(&mut self, commands: PooledVec<GLCmd>, _approx_bytes: usize) {
        self.commands
            .extend(commands.iter().map(|cmd| format!("{cmd:?}")));
    }
    fn materialize(&mut self, _canvas_id: u32) {}
}

fn decode_one(opcode: u32, words: &[u32]) -> Vec<String> {
    let mut record = vec![pack_header(opcode, words.len() as u32 + 1)];
    record.extend_from_slice(words);
    let mut stream = vec![MAGIC, STREAM_VERSION];
    stream.extend(record);
    let validated = validate_stream(&stream, stream.len() as u32).expect("structurally valid");
    let mut recorder = Recorder::default();
    decode_render_stream_into(&mut recorder, validated);
    recorder.commands
}

#[test]
fn an_unsigned_vector_record_keeps_its_words_unsigned() {
    // canvas 3, location 5, then the payload; 0x8000_0000 is 2147483648 as a uniform, not -2147483648.
    for (opcode, name, width) in [
        (OP_UNIFORM1UIV, "Uniform1uiv", 1usize),
        (OP_UNIFORM2UIV, "Uniform2uiv", 2),
        (OP_UNIFORM3UIV, "Uniform3uiv", 3),
        (OP_UNIFORM4UIV, "Uniform4uiv", 4),
    ] {
        let mut words = vec![3, 5];
        words.extend((0..2 * width).map(|i| 0x8000_0000u32 + i as u32));
        let commands = decode_one(opcode, &words);
        assert_eq!(commands.len(), 1, "{name}: {commands:?}");
        let text = &commands[0];
        assert!(text.starts_with(name), "{name}: {text}");
        for i in 0..2 * width {
            let word = 0x8000_0000u64 + i as u64;
            assert!(
                text.contains(&word.to_string()),
                "{name}: word {i} ({word}) is there, unsigned: {text}"
            );
        }
        assert!(!text.contains('-'), "{name}: nothing is negative: {text}");
        assert!(text.contains("location: Some(5)"), "{name}: {text}");
    }
}

#[test]
fn a_non_square_matrix_record_keeps_its_transpose_and_its_floats() {
    for (opcode, name, per_matrix) in [
        (OP_UNIFORM_MATRIX2X3FV, "UniformMatrix2x3fv", 6usize),
        (OP_UNIFORM_MATRIX2X4FV, "UniformMatrix2x4fv", 8),
        (OP_UNIFORM_MATRIX3X2FV, "UniformMatrix3x2fv", 6),
        (OP_UNIFORM_MATRIX3X4FV, "UniformMatrix3x4fv", 12),
        (OP_UNIFORM_MATRIX4X2FV, "UniformMatrix4x2fv", 8),
        (OP_UNIFORM_MATRIX4X3FV, "UniformMatrix4x3fv", 12),
    ] {
        for transpose in [0u32, 1] {
            let mut words = vec![3, 5, transpose];
            words.extend((0..per_matrix).map(|i| (i as f32 + 0.5).to_bits()));
            let commands = decode_one(opcode, &words);
            assert_eq!(commands.len(), 1, "{name}: {commands:?}");
            let text = &commands[0];
            assert!(text.starts_with(name), "{name}: {text}");
            assert!(
                text.contains(&format!("transpose: {}", transpose == 1)),
                "{name}: {text}"
            );
            assert!(
                text.contains("0.5") && text.contains(&format!("{}.5", per_matrix - 1)),
                "{name}: {text}"
            );
        }
    }
}

#[test]
fn a_matrix_larger_than_the_inline_payload_spills_and_is_still_one_command() {
    // 40 matrices of 2x3: 240 words, far past the 16 that are kept inline.
    let mut words = vec![3, 5, 0];
    words.extend((0..240).map(|i| (i as f32).to_bits()));
    let commands = decode_one(OP_UNIFORM_MATRIX2X3FV, &words);
    assert_eq!(commands.len(), 1);
    assert!(commands[0].contains("239.0"), "{}", commands[0]);
}
