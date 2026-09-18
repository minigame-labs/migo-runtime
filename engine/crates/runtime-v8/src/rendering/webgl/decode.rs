//! Pass 2 of the render command stream, as this runtime reaches it.
//!
//! The decoder itself is in `frame-decode`, which links no JavaScript engine:
//! turning validated words into render commands is engine-neutral work, and
//! the Apple Performance+ product needs exactly this with no V8 anywhere in
//! its dependency closure. It used to live here, which is what made that
//! product impossible.
//!
//! What is left is the adapter. `frame-decode` needs somewhere to push a WebGL
//! error, one piece of GL state, and somewhere to put the batches it cuts;
//! here all three are reached through the op state.
//!
//! # Why one object is both the context and the sink
//!
//! `OpState` hands out one mutable borrow at a time, and both halves of the
//! decode want it: the error queue lives there and so does the collector. A
//! separate context and sink could not be held at once, which is why
//! `decode_render_stream_into` takes a single value implementing both. The
//! alternative -- decode into a buffer of `FrameOp`s and replay it afterwards --
//! would put an intermediate vector of every batch in a frame on the path that
//! this whole exercise exists to make cheaper.

use deno_core::OpState;
use frame_decode::RenderSink;
use shared::command_vec_pool::PooledVec;
use shared::protocol::render_cmd::{Canvas2DCmd, GLCmd};

#[cfg(test)]
use crate::rendering::webgl::error_state::OpStateDecodeContext;
use crate::rendering::webgl::error_state::WebGLErrorState;
use crate::rendering::webgl::frame_collector::UnifiedFrameCollector;
use crate::rendering::webgl::stream::ValidatedStream;

/// The op state, as the shared decoder writes into it.
pub(crate) struct OpStateRenderTarget<'a> {
    state: &'a mut OpState,
    /// Whether any batch pushed the collector past its soft budget.
    ///
    /// Recorded rather than acted on: flushing a barrier dispatches a frame
    /// packet, and doing that in the middle of decoding a stream would put a
    /// bounded-blocking send between two commands of the same frame. The caller
    /// flushes once, after the stream is fully decoded.
    over_budget: bool,
}

impl<'a> OpStateRenderTarget<'a> {
    #[inline]
    fn new(state: &'a mut OpState) -> Self {
        Self {
            state,
            over_budget: false,
        }
    }
}

impl frame_decode::GlDecodeContext for OpStateRenderTarget<'_> {
    #[inline]
    fn push_error(&mut self, canvas_id: u32, code: u32) {
        crate::rendering::webgl::error_state::push_error(self.state, canvas_id, code);
    }

    #[inline]
    fn transform_feedback_captures(&self, canvas_id: u32) -> bool {
        self.state
            .borrow::<WebGLErrorState>()
            .transform_feedback_captures(canvas_id)
    }

    #[inline]
    fn set_transform_feedback(
        &mut self,
        canvas_id: u32,
        phase: frame_decode::TransformFeedbackPhase,
    ) {
        crate::rendering::webgl::error_state::set_transform_feedback(
            self.state,
            canvas_id,
            phase.into(),
        );
    }

    fn image_upload(
        &mut self,
        upload: frame_decode::ImageUpload,
    ) -> Option<shared::protocol::render_cmd::GLCmd> {
        crate::rendering::webgl::error_state::image_upload(self.state, upload)
    }
}

impl RenderSink for OpStateRenderTarget<'_> {
    fn canvas_batch(&mut self, canvas_id: u32, mut commands: PooledVec<Canvas2DCmd>) {
        // One at a time, not appended wholesale: `push_canvas2d` marks the
        // segment's dirty rectangle from each command and folds adjacent
        // `drawImage` runs. A bulk append would skip both, and the partial
        // update path would go quietly back to repainting whole canvases.
        let over_budget = match self.state.try_borrow_mut::<UnifiedFrameCollector>() {
            Some(collector) => {
                for command in commands.drain(..) {
                    collector.push_canvas2d(canvas_id, command);
                }
                collector.should_auto_flush()
            }
            // No collector installed (headless embedder): the commands have
            // nowhere to go. The loan returns itself on the way out.
            None => false,
        };
        self.over_budget |= over_budget;
    }

    fn gl_batch(&mut self, commands: PooledVec<GLCmd>, approx_bytes: usize) {
        let over_budget = match self.state.try_borrow_mut::<UnifiedFrameCollector>() {
            Some(collector) => collector.append_gl_batch(commands, approx_bytes),
            None => false,
        };
        self.over_budget |= over_budget;
    }

    fn materialize(&mut self, _canvas_id: u32) {
        // Deliberately dropped. The collector inserts its own barriers when it
        // builds the frame packet, at exactly the boundaries this reports --
        // every Canvas2D segment followed by a GL one, plus the trailing set
        // when a sync barrier needs them. Pushing these as well would ask the
        // renderer to flush the same canvas twice per boundary.
    }
}

/// Decode a structurally-validated GL command stream into owned `GLCmd` values.
///
/// Returns the saturating approximate byte count for all accepted commands.
///
/// Not on the submission path any more -- that decodes the mixed stream below.
/// This is the GL-only view, and what is left of it here is the oracle the
/// per-opcode cases in `webgl.rs` decode against: they check that a record
/// arriving as words builds the same `GLCmd` the corresponding raw op does, one
/// opcode at a time, which needs an entry point that returns the commands
/// rather than one that files them away in a collector. `#[cfg(test)]` rather
/// than an allow: it says which artifact this belongs to instead of hiding that
/// the shipping one does not call it.
#[cfg(test)]
pub(crate) fn decode_validated_stream(
    state: &mut OpState,
    stream: ValidatedStream<'_>,
    out: &mut Vec<GLCmd>,
) -> usize {
    frame_decode::decode_validated_stream(&mut OpStateDecodeContext(state), stream, out)
}

/// Decode a structurally-validated mixed 2D/GL stream straight into the
/// collector.
///
/// Returns the number of commands decoded and whether the collector crossed its
/// soft budget while they were added.
pub(crate) fn decode_render_stream(
    state: &mut OpState,
    stream: ValidatedStream<'_>,
) -> (usize, bool) {
    let mut target = OpStateRenderTarget::new(state);
    let decoded = frame_decode::decode_render_stream_into(&mut target, stream);
    (decoded, target.over_budget)
}
