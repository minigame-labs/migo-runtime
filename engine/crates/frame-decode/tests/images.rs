//! Image records: drawing a loaded image in 2D, and uploading one into a
//! texture. The pixels never cross -- the records name an image the host holds
//! -- so what these check is that the id arrives exactly and that the upload is
//! the host's image table's to resolve.

use frame_decode::{GlDecodeContext, ImageUpload, decode_render_stream};
use frame_wire::canvas2d::{
    DRAW_IMAGE_BATCH_ENTRY_WORDS, OP2D_DRAW_IMAGE, OP2D_DRAW_IMAGE_BATCH, OP2D_SELECT_CANVAS,
};
use frame_wire::gl_resource::{OPR_TEX_IMAGE_2D_FROM_IMAGE, OPR_TEX_SUB_IMAGE_2D_FROM_IMAGE};
use frame_wire::stream::{MAGIC, STREAM_VERSION, pack_header, validate_stream};
use shared::protocol::FrameOp;
use shared::protocol::render_cmd::{Canvas2DCmd, GLCmd};

/// A shared image id where an `f32` would round: shared ids start at 2^30,
/// where consecutive `f32`s are 128 apart.
const SHARED_ID: u32 = 0x4000_0001;

#[derive(Default)]
struct ImageContext {
    uploads: Vec<ImageUpload>,
    /// Whether the context resolves an upload, the way the host's image table
    /// does for a live image.
    resolves: bool,
}

impl GlDecodeContext for ImageContext {
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
    fn image_upload(&mut self, upload: ImageUpload) -> Option<GLCmd> {
        self.uploads.push(upload);
        self.resolves
            .then(|| GLCmd::DebugLoseContext { canvas_id: 99 })
    }
}

fn record(opcode: u32, words: &[u32]) -> Vec<u32> {
    let mut out = vec![pack_header(opcode, words.len() as u32 + 1)];
    out.extend_from_slice(words);
    out
}

fn decode(records: &[Vec<u32>], context: &mut ImageContext) -> Vec<FrameOp> {
    let mut words = vec![MAGIC, STREAM_VERSION];
    for record in records {
        words.extend_from_slice(record);
    }
    let stream = validate_stream(&words, words.len() as u32).expect("structurally valid");
    let mut out = Vec::new();
    decode_render_stream(context, stream, &mut out);
    out
}

fn canvas_commands(ops: &[FrameOp]) -> Vec<&Canvas2DCmd> {
    ops.iter()
        .filter_map(|op| match op {
            FrameOp::CanvasBatch(batch) => Some(batch.commands.iter()),
            _ => None,
        })
        .flatten()
        .collect()
}

fn rect(values: [f32; 8]) -> Vec<u32> {
    values.iter().map(|value| value.to_bits()).collect()
}

#[test]
fn a_drawn_image_arrives_with_its_exact_id_and_rectangles() {
    let mut args = vec![SHARED_ID];
    args.extend(rect([1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0]));
    let ops = decode(
        &[
            record(OP2D_SELECT_CANVAS, &[3]),
            record(OP2D_DRAW_IMAGE, &args),
        ],
        &mut ImageContext::default(),
    );
    match canvas_commands(&ops).as_slice() {
        [
            Canvas2DCmd::DrawImage {
                image_id, sx, dh, ..
            },
        ] => {
            assert_eq!(
                *image_id, SHARED_ID,
                "the id is a word, never rounded through an f32"
            );
            assert_eq!((*sx, *dh), (1.0, 8.0));
        }
        other => panic!("expected one DrawImage, got {other:?}"),
    }
}

#[test]
fn a_batch_carries_whole_entries_with_exact_ids() {
    let mut entries = Vec::new();
    for id in [SHARED_ID, SHARED_ID + 1] {
        entries.push(id);
        entries.extend(rect([0.0, 0.0, 8.0, 8.0, 16.0, 16.0, 8.0, 8.0]));
    }
    let mut args = vec![entries.len() as u32];
    args.extend(&entries);
    let ops = decode(
        &[
            record(OP2D_SELECT_CANVAS, &[3]),
            record(OP2D_DRAW_IMAGE_BATCH, &args),
        ],
        &mut ImageContext::default(),
    );
    match canvas_commands(&ops).as_slice() {
        [Canvas2DCmd::DrawImageBatch { draws }] => {
            let ids: Vec<u32> = draws.iter().map(|draw| draw.image_id).collect();
            assert_eq!(
                ids,
                vec![SHARED_ID, SHARED_ID + 1],
                "two consecutive ids stay two ids"
            );
        }
        other => panic!("expected one DrawImageBatch, got {other:?}"),
    }
}

#[test]
fn a_batch_that_is_not_whole_entries_is_dropped_not_misread() {
    let words = DRAW_IMAGE_BATCH_ENTRY_WORDS + 1;
    let mut args = vec![words];
    args.extend(std::iter::repeat_n(0, words as usize));
    let ops = decode(
        &[
            record(OP2D_SELECT_CANVAS, &[3]),
            record(OP2D_DRAW_IMAGE_BATCH, &args),
        ],
        &mut ImageContext::default(),
    );
    assert!(canvas_commands(&ops).is_empty());
}

#[test]
fn a_texture_upload_from_an_image_is_the_image_tables_to_resolve() {
    let mut context = ImageContext {
        resolves: true,
        ..Default::default()
    };
    let ops = decode(
        &[
            record(
                OPR_TEX_IMAGE_2D_FROM_IMAGE,
                &[1, 0x0DE1, 0, 0x1908, 0x1908, 0x1401, SHARED_ID],
            ),
            record(
                OPR_TEX_SUB_IMAGE_2D_FROM_IMAGE,
                &[1, 0x0DE1, 0, 4, 8, 0x1908, 0x1401, SHARED_ID],
            ),
        ],
        &mut context,
    );
    assert_eq!(
        context.uploads,
        vec![
            ImageUpload::Full {
                canvas_id: 1,
                target: 0x0DE1,
                level: 0,
                internalformat: 0x1908,
                format: 0x1908,
                type_: 0x1401,
                image_id: SHARED_ID,
            },
            ImageUpload::Sub {
                canvas_id: 1,
                target: 0x0DE1,
                level: 0,
                xoffset: 4,
                yoffset: 8,
                format: 0x1908,
                type_: 0x1401,
                image_id: SHARED_ID,
            },
        ]
    );
    let gl: usize = ops
        .iter()
        .map(|op| match op {
            FrameOp::GlBatch(batch) => batch.commands.len(),
            _ => 0,
        })
        .sum();
    assert_eq!(gl, 2, "what the table resolved is what the frame carries");
}

#[test]
fn an_upload_the_table_cannot_resolve_is_skipped() {
    let ops = decode(
        &[record(
            OPR_TEX_IMAGE_2D_FROM_IMAGE,
            &[1, 0x0DE1, 0, 0x1908, 0x1908, 0x1401, 7],
        )],
        &mut ImageContext::default(),
    );
    assert!(
        ops.iter()
            .all(|op| !matches!(op, FrameOp::GlBatch(batch) if !batch.commands.is_empty())),
        "an image the host does not hold uploads nothing"
    );
}
