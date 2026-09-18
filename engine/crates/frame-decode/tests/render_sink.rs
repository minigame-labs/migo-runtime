//! What the sink is told, and how much of it.
//!
//! `RenderSink::gl_batch` carries a byte count, and it is not decoration: it is
//! what the in-process collector adds to the budget that decides when a frame's
//! pending commands have to be flushed early. A count that is cumulative rather
//! than per batch, or estimated rather than measured, makes that budget wrong in
//! the direction that pins memory -- and nothing downstream can tell, because
//! every command still arrives and every frame still draws.

use frame_decode::{GlDecodeContext, RenderSink, decode_render_stream_into};
use frame_wire::canvas2d::*;
use frame_wire::gl::{OP_CLEAR, OP_UNIFORM1FV};
use frame_wire::stream::{MAGIC, STREAM_VERSION, pack_header, validate_stream};
use shared::command_vec_pool::PooledVec;
use shared::protocol::render_cmd::{Canvas2DCmd, GLCmd};

/// Every batch the decoder handed over, with what it said about it.
///
/// The commands themselves are not kept -- neither command enum is `Clone`, and
/// stealing the vector would keep the pool's allocation out of circulation for
/// the rest of the run. What matters here is measurable while the batch is
/// still in hand: how many commands it carried, and what they actually weigh.
#[derive(Default)]
struct Recorder {
    canvas: Vec<(u32, usize)>,
    /// `(command count, what the decoder reported, what the commands weigh)`.
    gl: Vec<(usize, usize, usize)>,
    materialized: Vec<u32>,
}

impl GlDecodeContext for Recorder {
    fn image_upload(
        &mut self,

        _upload: frame_decode::ImageUpload,
    ) -> Option<shared::protocol::render_cmd::GLCmd> {
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
}

impl RenderSink for Recorder {
    fn canvas_batch(&mut self, canvas_id: u32, commands: PooledVec<Canvas2DCmd>) {
        self.canvas.push((canvas_id, commands.len()));
    }

    fn gl_batch(&mut self, commands: PooledVec<GLCmd>, approx_bytes: usize) {
        let measured = commands.iter().map(GLCmd::approx_deep_size_bytes).sum();
        self.gl.push((commands.len(), approx_bytes, measured));
    }

    fn materialize(&mut self, canvas_id: u32) {
        self.materialized.push(canvas_id);
    }
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

/// A uniform upload of `count` floats: H C location payload...
fn uniform1fv(canvas_id: u32, location: u32, count: usize) -> Vec<u32> {
    let mut words = vec![canvas_id, location];
    words.extend(std::iter::repeat_n(1.0f32.to_bits(), count));
    record(OP_UNIFORM1FV, &words)
}

#[test]
fn each_gl_batch_is_measured_on_its_own() {
    // Two GL runs of deliberately different weight, separated by 2D work so the
    // decoder has to cut them apart. The second is the light one: a cumulative
    // count would report it as larger than the first, which is the specific
    // mistake this pins.
    let recorder = decode(&[
        uniform1fv(1, 0, 64),
        record(OP2D_SELECT_CANVAS, &[7]),
        record(OP2D_FILL_RECT, &[0, 0, 0, 0]),
        record(OP_CLEAR, &[1, 0x4000]),
    ]);

    assert_eq!(recorder.gl.len(), 2, "two GL runs, two batches");

    for (count, reported, measured) in &recorder.gl {
        assert_eq!(
            reported, measured,
            "a batch of {count} command(s) was reported as {reported} bytes, not {measured}"
        );
    }

    let heavy = recorder.gl[0].1;
    let light = recorder.gl[1].1;
    assert!(
        heavy > light,
        "the 64-float upload ({heavy} bytes) should outweigh a clear ({light} bytes); \
         equal or inverted means the count is cumulative"
    );
}

#[test]
fn the_sink_sees_the_order_the_frame_was_issued_in() {
    let recorder = decode(&[
        record(OP2D_SELECT_CANVAS, &[7]),
        record(OP2D_FILL_RECT, &[0, 0, 0, 0]),
        record(OP_CLEAR, &[1, 0x4000]),
        record(OP2D_SELECT_CANVAS, &[7]),
        record(OP2D_STROKE_RECT, &[0, 0, 0, 0]),
    ]);

    assert_eq!(recorder.canvas.len(), 2);
    assert_eq!(recorder.gl.len(), 1);
    // The GL batch draws over canvas 7, so 7 is materialized before it and
    // again after the trailing 2D work that nothing else would flush.
    assert_eq!(recorder.materialized, vec![7, 7]);
}
