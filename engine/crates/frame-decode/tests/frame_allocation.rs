//! One test owns the process-wide pools so allocation measurements are isolated.
use frame_decode::{GlDecodeContext, decode_render_stream, validate_frame_budget};
use frame_wire::canvas2d::{OP2D_RESTORE, OP2D_SAVE, OP2D_SELECT_CANVAS};
use frame_wire::stream::{
    MAGIC, STREAM_VERSION, pack_header, validate_frame_stream, validate_stream,
};
use migo_alloc_probe::{Burst, CountingAllocator, assert_no_steady_state_allocation};

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator::system();

struct Context;
impl GlDecodeContext for Context {
    fn push_error(&mut self, _: u32, _: u32) {
        panic!("valid fixture");
    }
    fn transform_feedback_captures(&self, _: u32) -> bool {
        false
    }
}

#[test]
fn repeated_canvas_frames_reuse_large_commands_and_materialization_scratch() {
    let mut large = vec![MAGIC, STREAM_VERSION, pack_header(OP2D_SELECT_CANVAS, 2), 7];
    for _ in 0..4094 {
        large.extend([pack_header(OP2D_SAVE, 1), pack_header(OP2D_RESTORE, 1)]);
    }
    let mut many_canvases = vec![MAGIC, STREAM_VERSION];
    for id in 0..12 {
        many_canvases.extend([
            pack_header(OP2D_SELECT_CANVAS, 2),
            id,
            pack_header(OP2D_SAVE, 1),
        ]);
    }
    for (path, words) in [
        ("8188 commands", large),
        ("12 pending canvases", many_canvases),
    ] {
        let mut ops = Vec::with_capacity(32);
        assert_no_steady_state_allocation(
            Burst {
                path,
                warmup: 32,
                measured: 64,
            },
            |_| {
                let stream = validate_stream(&words, words.len() as u32).unwrap();
                decode_render_stream(&mut Context, stream, &mut ops);
                std::hint::black_box(&ops);
                ops.clear();
            },
        );
    }

    let mut hostile = vec![MAGIC, STREAM_VERSION, pack_header(OP2D_SELECT_CANVAS, 2), 7];
    hostile.resize(100_004, pack_header(OP2D_SAVE, 1));
    assert_no_steady_state_allocation(
        Burst {
            path: "reject expanded frame before decode",
            warmup: 4,
            measured: 64,
        },
        |_| {
            let stream = validate_frame_stream(&hostile, hostile.len() as u32).unwrap();
            assert!(validate_frame_budget(&stream, 4 * 1024 * 1024).is_err());
        },
    );
}
