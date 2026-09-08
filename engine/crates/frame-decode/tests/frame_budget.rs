use frame_decode::validate_frame_budget;
use frame_wire::canvas2d::{OP2D_SAVE, OP2D_SELECT_CANVAS};
use frame_wire::gl::{OP_CLEAR, OP_UNIFORM1FV};
use frame_wire::stream::{MAGIC, STREAM_VERSION, pack_header, validate_frame_stream};

const BUDGET: usize = 4 * 1024 * 1024;

#[test]
fn tiny_wire_records_cannot_expand_past_the_decoded_frame_budget() {
    let mut words = vec![MAGIC, STREAM_VERSION, pack_header(OP2D_SELECT_CANVAS, 2), 7];
    words.resize(100_004, pack_header(OP2D_SAVE, 1));
    let stream = validate_frame_stream(&words, words.len() as u32).unwrap();
    assert!(words.len() * 4 < BUDGET);
    assert!(validate_frame_budget(&stream, BUDGET).is_err());
}

#[test]
fn alternating_batches_include_their_minimum_allocations_and_frame_ops() {
    let mut words = vec![MAGIC, STREAM_VERSION, pack_header(OP2D_SELECT_CANVAS, 2), 7];
    for _ in 0..2000 {
        words.extend([
            pack_header(OP2D_SAVE, 1),
            pack_header(OP_CLEAR, 3),
            1,
            0x4000,
        ]);
    }
    let stream = validate_frame_stream(&words, words.len() as u32).unwrap();
    assert!(validate_frame_budget(&stream, BUDGET).is_err());
}

#[test]
fn spilled_uniform_payloads_are_admitted_before_copying() {
    let mut words = vec![MAGIC, STREAM_VERSION];
    for _ in 0..2000 {
        words.extend([pack_header(OP_UNIFORM1FV, 515), 1, 0]);
        words.extend(std::iter::repeat_n(1.0_f32.to_bits(), 512));
    }
    let stream = validate_frame_stream(&words, words.len() as u32).unwrap();
    assert!(words.len() * 4 < BUDGET);
    assert!(validate_frame_budget(&stream, BUDGET).is_err());
}

#[test]
fn an_ordinary_full_frame_is_admitted_with_a_bounded_op_capacity() {
    let mut words = vec![MAGIC, STREAM_VERSION, pack_header(OP2D_SELECT_CANVAS, 2), 7];
    words.resize(12_004, pack_header(OP2D_SAVE, 1));
    let stream = validate_frame_stream(&words, words.len() as u32).unwrap();
    let budget = validate_frame_budget(&stream, BUDGET).unwrap();
    assert_eq!(budget.max_frame_ops(), 4); // Begin, batch, materialize, Present.
    assert!(budget.frame_op_capacity() >= budget.max_frame_ops());
    assert!(budget.estimated_bytes() < BUDGET);
    assert!(validate_frame_budget(&stream, budget.estimated_bytes()).is_ok());
    assert!(validate_frame_budget(&stream, budget.estimated_bytes() - 1).is_err());
}

#[test]
fn admitted_storage_covers_real_capacities_after_a_larger_pooled_frame() {
    use frame_decode::{GlDecodeContext, RenderSink, decode_render_stream_into};
    use shared::command_vec_pool::PooledVec;
    use shared::protocol::render_cmd::{Canvas2DCmd, CanvasBatchPayload, GLCmd, GlBatchPayload};
    use shared::{FrameOp, FramePacket, FramePacketBuilder};

    struct Sink {
        builder: FramePacketBuilder,
        command_bytes: usize,
        errors: usize,
    }
    impl GlDecodeContext for Sink {
        fn push_error(&mut self, _: u32, _: u32) {
            self.errors += 1;
        }
        fn transform_feedback_captures(&self, _: u32) -> bool {
            false
        }
    }
    impl RenderSink for Sink {
        fn canvas_batch(&mut self, canvas_id: u32, commands: PooledVec<Canvas2DCmd>) {
            self.command_bytes += commands.capacity() * size_of::<Canvas2DCmd>();
            self.builder
                .push_op(FrameOp::CanvasBatch(CanvasBatchPayload {
                    canvas_id,
                    commands,
                    present: false,
                    dirty_rect: None,
                }));
        }
        fn gl_batch(&mut self, commands: PooledVec<GLCmd>, _: usize) {
            self.command_bytes += commands.capacity() * size_of::<GLCmd>();
            self.command_bytes += commands
                .iter()
                .map(|cmd| cmd.approx_deep_size_bytes() - size_of::<GLCmd>())
                .sum::<usize>();
            self.builder
                .push_op(FrameOp::GlBatch(GlBatchPayload { commands }));
        }
        fn materialize(&mut self, canvas_id: u32) {
            self.builder.push_op(FrameOp::Materialize { canvas_id });
        }
    }

    drop(PooledVec::<Canvas2DCmd>::from(Vec::with_capacity(8192)));
    drop(PooledVec::<GLCmd>::from(Vec::with_capacity(8192)));
    let mut words = vec![MAGIC, STREAM_VERSION, pack_header(OP2D_SAVE, 1)];
    for payload in [17, 33, 257, 512] {
        words.extend([pack_header(OP_UNIFORM1FV, payload + 3), 1, 0]);
        words.extend(std::iter::repeat_n(0, payload as usize));
    }
    for id in [7, 9, 7] {
        words.extend([
            pack_header(OP2D_SELECT_CANVAS, 2),
            id,
            pack_header(OP2D_SAVE, 1),
        ]);
    }
    words.extend([pack_header(OP_CLEAR, 3), 1, 0x4000]);
    let stream = validate_frame_stream(&words, words.len() as u32).unwrap();
    let budget = validate_frame_budget(&stream, BUDGET).unwrap();
    let mut sink = Sink {
        builder: FramePacketBuilder::with_op_capacity(1, 0.0, budget.frame_op_capacity())
            .push(FrameOp::BeginFrame),
        command_bytes: 0,
        errors: 0,
    };
    decode_render_stream_into(&mut sink, stream);
    assert_eq!(sink.errors, 1, "the orphan 2D command is still skipped");
    let packet = sink.builder.push(FrameOp::Present).finish();
    assert!(packet.ops().len() <= budget.max_frame_ops());
    let materialized: Vec<_> = packet
        .ops()
        .iter()
        .filter_map(|op| match op {
            FrameOp::Materialize { canvas_id } => Some(*canvas_id),
            _ => None,
        })
        .collect();
    assert_eq!(
        materialized,
        [7, 9],
        "barriers retain first-use order and deduplicate canvases"
    );
    packet.into_ops().consume(|ops| {
        assert!(ops.capacity() <= budget.frame_op_capacity());
        let owned_bytes =
            size_of::<FramePacket>() + ops.capacity() * size_of::<FrameOp>() + sink.command_bytes;
        assert!(
            owned_bytes <= budget.estimated_bytes(),
            "real owned capacities {owned_bytes} exceeded {budget:?}"
        );
    });
}
