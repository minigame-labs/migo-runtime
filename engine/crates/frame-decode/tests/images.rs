//! Image records: drawing a loaded image in 2D, and uploading a TexImageSource
//! into a texture. An image's pixels never cross -- the records name an image
//! the host holds -- so what these check is that the id arrives exactly and that
//! the image is the host's image table's to resolve; `ImageData`'s pixels do
//! cross, and arrive whole or not at all.

use std::sync::Arc;

use frame_decode::{GlDecodeContext, decode_render_stream};
use frame_wire::canvas2d::{
    DRAW_IMAGE_BATCH_ENTRY_WORDS, OP2D_DRAW_IMAGE, OP2D_DRAW_IMAGE_BATCH, OP2D_SELECT_CANVAS,
};
use frame_wire::gl_resource::{OPR_TEX_IMAGE_SOURCE, tex_source};
use frame_wire::stream::{MAGIC, STREAM_VERSION, pack_header, validate_stream};
use shared::protocol::FrameOp;
use shared::protocol::render_cmd::{Canvas2DCmd, GLCmd, SourceUploadCall, TextureSource};

/// A shared image id where an `f32` would round: shared ids start at 2^30,
/// where consecutive `f32`s are 128 apart.
const SHARED_ID: u32 = 0x4000_0001;

#[derive(Default)]
struct ImageContext {
    /// The image ids the decoder asked the table about.
    asked: Vec<u32>,
    /// Whether the context resolves an image, the way the host's image table
    /// does for a live one.
    resolves: bool,
    errors: Vec<u32>,
}

impl GlDecodeContext for ImageContext {
    fn push_error(&mut self, _canvas_id: u32, code: u32) {
        self.errors.push(code);
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
    fn image_source(&mut self, image_id: u32) -> Option<TextureSource> {
        self.asked.push(image_id);
        self.resolves.then_some(TextureSource::Image {
            shared_id: Some(image_id),
            pixels: None,
            width: 4,
            height: 2,
        })
    }
    fn staged_payload(&mut self) -> Option<&mut frame_decode::StagedPayload> {
        None
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

/// An `OPR_TEX_IMAGE_SOURCE` record: the 18 words after the header, then the
/// pixels' length and the pixels, padded to a word.
fn source_record(fields: [u32; 18], pixels: &[u8]) -> Vec<u32> {
    let mut words = fields.to_vec();
    words.push(pixels.len() as u32);
    for chunk in pixels.chunks(4) {
        let mut word = [0u8; 4];
        word[..chunk.len()].copy_from_slice(chunk);
        words.push(u32::from_le_bytes(word));
    }
    record(OPR_TEX_IMAGE_SOURCE, &words)
}

/// `C call target level internalformat xoffset yoffset zoffset width height depth format type destination_format
/// kind source_id source_width source_height`, for a 4 x 2 RGBA upload into canvas 1's TEXTURE_2D.
fn fields(call: u32, kind: u32, source_id: u32) -> [u32; 18] {
    [
        1, call, 0x0DE1, 0, 0x1908, 5, 6, 7, 4, 2, 1, 0x1908, 0x1401, 0x8058, kind, source_id, 4, 2,
    ]
}

fn uploads(ops: &[FrameOp]) -> Vec<&GLCmd> {
    ops.iter()
        .filter_map(|op| match op {
            FrameOp::GlBatch(batch) => Some(batch.commands.iter()),
            _ => None,
        })
        .flatten()
        .collect()
}

#[test]
fn a_texture_upload_from_an_image_is_the_image_tables_to_resolve() {
    let mut context = ImageContext {
        resolves: true,
        ..Default::default()
    };
    let ops = decode(
        &[
            source_record(
                fields(
                    tex_source::CALL_IMAGE_2D,
                    tex_source::SOURCE_IMAGE,
                    SHARED_ID,
                ),
                &[],
            ),
            source_record(
                fields(
                    tex_source::CALL_SUB_IMAGE_3D,
                    tex_source::SOURCE_IMAGE,
                    SHARED_ID,
                ),
                &[],
            ),
        ],
        &mut context,
    );
    assert_eq!(
        context.asked,
        [SHARED_ID, SHARED_ID],
        "the id is a word, never rounded"
    );
    let image = TextureSource::Image {
        shared_id: Some(SHARED_ID),
        pixels: None,
        width: 4,
        height: 2,
    };
    match uploads(&ops).as_slice() {
        [
            GLCmd::TexImageSource {
                canvas_id: 1,
                target: 0x0DE1,
                call:
                    SourceUploadCall::Image2D {
                        internalformat: 0x1908,
                    },
                width: 4,
                height: 2,
                format: 0x1908,
                type_: 0x1401,
                source: first,
                ..
            },
            GLCmd::TexImageSource {
                call:
                    SourceUploadCall::SubImage3D {
                        xoffset: 5,
                        yoffset: 6,
                        zoffset: 7,
                        depth: 1,
                    },
                destination_format: 0x8058,
                source: second,
                ..
            },
        ] => {
            assert_eq!(first, &image);
            assert_eq!(second, &image);
        }
        other => panic!("expected the two uploads as their calls, got {other:?}"),
    }
}

#[test]
fn an_upload_the_table_cannot_resolve_is_skipped() {
    let ops = decode(
        &[source_record(
            fields(tex_source::CALL_IMAGE_2D, tex_source::SOURCE_IMAGE, 7),
            &[],
        )],
        &mut ImageContext::default(),
    );
    assert!(
        uploads(&ops).is_empty(),
        "an image the host does not hold uploads nothing"
    );
}

#[test]
fn a_canvas_and_a_snapshot_are_named_and_a_snapshot_of_none_is_nothing() {
    let mut context = ImageContext::default();
    let ops = decode(
        &[
            source_record(
                fields(tex_source::CALL_SUB_IMAGE_2D, tex_source::SOURCE_CANVAS, 3),
                &[],
            ),
            source_record(
                fields(tex_source::CALL_IMAGE_2D, tex_source::SOURCE_SNAPSHOT, 9),
                &[],
            ),
            source_record(
                fields(tex_source::CALL_IMAGE_2D, tex_source::SOURCE_SNAPSHOT, 0),
                &[],
            ),
        ],
        &mut context,
    );
    let sources: Vec<&TextureSource> = uploads(&ops)
        .into_iter()
        .map(|command| match command {
            GLCmd::TexImageSource { source, .. } => source,
            other => panic!("not a source upload: {other:?}"),
        })
        .collect();
    assert_eq!(
        sources,
        [
            &TextureSource::Canvas { canvas_2d_id: 3 },
            &TextureSource::Snapshot { snapshot_id: 9 }
        ]
    );
    assert!(context.errors.is_empty());
}

#[test]
fn image_data_s_pixels_arrive_whole_or_the_upload_is_refused() {
    let rows: Vec<u8> = (0..32).collect();
    let mut context = ImageContext::default();
    let ops = decode(
        &[
            source_record(
                fields(tex_source::CALL_IMAGE_2D, tex_source::SOURCE_PIXELS, 0),
                &rows,
            ),
            // A row short of 4 x 2.
            source_record(
                fields(tex_source::CALL_IMAGE_2D, tex_source::SOURCE_PIXELS, 0),
                &rows[..16],
            ),
        ],
        &mut context,
    );
    match uploads(&ops).as_slice() {
        [GLCmd::TexImageSource { source, .. }] => assert_eq!(
            source,
            &TextureSource::Pixels {
                bytes: Arc::new(rows.clone()),
                width: 4,
                height: 2,
            }
        ),
        other => panic!("expected the whole upload alone, got {other:?}"),
    }
    assert_eq!(
        context.errors,
        [0x0502],
        "the short one is INVALID_OPERATION"
    );
}

#[test]
fn a_record_that_names_no_call_or_no_source_is_invalid_value() {
    let mut context = ImageContext {
        resolves: true,
        ..Default::default()
    };
    let mut negative = fields(tex_source::CALL_IMAGE_2D, tex_source::SOURCE_IMAGE, 1);
    negative[8] = -1i32 as u32;
    let ops = decode(
        &[
            source_record(fields(4, tex_source::SOURCE_IMAGE, 1), &[]),
            source_record(fields(tex_source::CALL_IMAGE_2D, 5, 1), &[]),
            source_record(negative, &[]),
            // Bytes beside an image, which has none of its own to carry.
            source_record(
                fields(tex_source::CALL_IMAGE_2D, tex_source::SOURCE_IMAGE, 1),
                &[1, 2, 3, 4],
            ),
        ],
        &mut context,
    );
    assert!(uploads(&ops).is_empty());
    assert_eq!(context.errors, [0x0501; 4]);
}
