//! The Canvas2D half of the render command stream.
//!
//! # Why 2D needs a command stream at all
//!
//! The WebGL path already has one: content JavaScript encodes a frame's draw
//! calls into `u32` words and one op carries the lot. Canvas2D never got that.
//! Its commands are batched on the *Rust* side -- `UnifiedFrameCollector` turns
//! them into a `FrameOp::CanvasBatch` -- but the JavaScript-to-Rust crossing is
//! still one op per call, which is `android-ceiling-review.md`'s G2: a frame
//! that draws a few hundred rectangles pays a few hundred boundary crossings.
//!
//! It is also what iOS needs. On the Performance+ lane the producer is in
//! another process, so *every* command has to be bytes; a 2D path that can only
//! cross as individual ops cannot cross at all. One encoding closes an Android
//! cost and unblocks an Apple product, which is the only reason the Apple work
//! is affordable.
//!
//! # One stream, one opcode space
//!
//! 2D and GL commands interleave within a frame -- a game draws its background
//! with 2D, its sprites with GL, its HUD with 2D -- and the renderer needs the
//! order they were issued in. Two streams would need a merge with timestamps or
//! a barrier protocol; one stream with two opcode ranges needs neither, and the
//! order is the order.
//!
//! The ranges are load-bearing, not cosmetic. GL owns `1..=58` fixed and
//! `256..=266` variable; 2D owns `512..`. A reader classifies a record by its
//! opcode alone, and the gap between the blocks is what makes an opcode added
//! to the wrong one a rejection rather than a record read with the wrong shape.
//!
//! # What is here and what is not
//!
//! Every command whose arguments are numbers: paths, rectangles, transforms,
//! the state scalars, and the three colours. Those are the per-frame traffic
//! and the whole of the G2 cost.
//!
//! Not here: anything carrying a string or a variable-length array -- fonts,
//! text, gradients, patterns, line-dash arrays -- and anything synchronous.
//! They are rare per frame, they need the variable-length record shape the GL
//! block already has a spec table for, and adding them without that shape is
//! how a fixed-length decoder ends up with a length field it does not check.

use crate::stream::RecordSpec;

/// The first 2D opcode. Everything at or above this is a 2D record.
pub const OP2D_BASE: u32 = 512;

/// Select the canvas the following 2D records apply to.
///
/// Once per batch rather than once per command: `Canvas2DCmd` carries no canvas
/// id -- the id lives on the batch that holds the commands -- so repeating it
/// in every record would be a word per command that the destination discards.
pub const OP2D_SELECT_CANVAS: u32 = 512;

pub const OP2D_BEGIN_PATH: u32 = 513;
pub const OP2D_CLOSE_PATH: u32 = 514;
pub const OP2D_MOVE_TO: u32 = 515;
pub const OP2D_LINE_TO: u32 = 516;
pub const OP2D_QUADRATIC_CURVE_TO: u32 = 517;
pub const OP2D_BEZIER_CURVE_TO: u32 = 518;
pub const OP2D_ARC: u32 = 519;
pub const OP2D_ARC_TO: u32 = 520;
pub const OP2D_RECT: u32 = 521;
pub const OP2D_ELLIPSE: u32 = 522;

pub const OP2D_FILL: u32 = 523;
pub const OP2D_STROKE: u32 = 524;
pub const OP2D_CLIP: u32 = 525;

pub const OP2D_FILL_RECT: u32 = 526;
pub const OP2D_STROKE_RECT: u32 = 527;
pub const OP2D_CLEAR_RECT: u32 = 528;

pub const OP2D_SAVE: u32 = 529;
pub const OP2D_RESTORE: u32 = 530;

pub const OP2D_SET_TRANSFORM: u32 = 531;
pub const OP2D_RESET_TRANSFORM: u32 = 532;
pub const OP2D_TRANSLATE: u32 = 533;
pub const OP2D_ROTATE: u32 = 534;
pub const OP2D_SCALE: u32 = 535;

pub const OP2D_SET_LINE_WIDTH: u32 = 536;
pub const OP2D_SET_GLOBAL_ALPHA: u32 = 537;
pub const OP2D_SET_MITER_LIMIT: u32 = 538;
pub const OP2D_SET_LINE_DASH_OFFSET: u32 = 539;
pub const OP2D_SET_SHADOW_BLUR: u32 = 540;
pub const OP2D_SET_SHADOW_OFFSET_X: u32 = 541;
pub const OP2D_SET_SHADOW_OFFSET_Y: u32 = 542;

pub const OP2D_SET_LINE_CAP: u32 = 543;
pub const OP2D_SET_LINE_JOIN: u32 = 544;
pub const OP2D_SET_COMPOSITE_OPERATION: u32 = 545;

pub const OP2D_SET_FILL_STYLE: u32 = 546;
pub const OP2D_SET_STROKE_STYLE: u32 = 547;
pub const OP2D_SET_SHADOW_COLOR: u32 = 548;

/// Bring a 2D context into existence on the selected canvas.
///
/// Without it this block is a complete drawing vocabulary with no way to be
/// used: `get_2d_context_mut` answers `NotFound` for a canvas that has none,
/// and `execute_canvas_batch` treats that as "no context yet -- no draws". So a
/// producer sending 2D records to a fresh canvas had them accepted, decoded,
/// batched and silently dropped -- an accepted frame that drew nothing, with no
/// error anywhere. The in-process runtime never hit it because `getContext`
/// there is an op that creates the context directly; this block is the only
/// path the external lane has, and it was missing its first step.
///
/// One word. The canvas is whichever `OP2D_SELECT_CANVAS` named, the same way
/// every other record in this block takes its canvas.
pub const OP2D_CREATE_CONTEXT: u32 = 549;

// ─── Text (550..=556) ────────────────────────────────────────────────────────
//
// Everything above is numbers. Text is the block's first payload: a font
// shorthand and a string to draw, which is why the two payload record shapes
// the resource block introduced are used here rather than restated.

/// The font shorthand, as CSS writes it: `italic bold 16px "Noto Sans", sans`.
///
/// `H byte_length | utf8`. The producer answers `setFont` locally -- the op it
/// stands in for returns whether the shorthand parses -- so a record only ever
/// carries a shorthand the producer already parsed. The host parses it again,
/// because it is the one that has to turn it into a typeface, and because a
/// record is not trusted for being well-formed.
pub const OP2D_SET_FONT: u32 = 550;

/// `fillText(text, x, y, maxWidth)`: `H x:F y:F max_width:F byte_length | utf8`.
///
/// `max_width` is `+inf` when content passed none, which is what the facade
/// already does and what the renderer reads as "no limit".
pub const OP2D_FILL_TEXT: u32 = 551;
/// `strokeText`, the same shape.
pub const OP2D_STROKE_TEXT: u32 = 552;

/// `textAlign`, as the op's `u8`: start, end, left, right, center.
pub const OP2D_SET_TEXT_ALIGN: u32 = 553;
/// `textBaseline`: top, hanging, middle, alphabetic, ideographic, bottom.
pub const OP2D_SET_TEXT_BASELINE: u32 = 554;
/// `direction`: ltr, rtl, inherit.
pub const OP2D_SET_TEXT_DIRECTION: u32 = 555;

/// `setLineDash([...])`: `H count | f32 bits`.
///
/// A word list rather than a byte payload, because the segments are `f32` and
/// the bits are what crosses -- the same reinterpretation the uniform records
/// make, for the same reason.
pub const OP2D_SET_LINE_DASH: u32 = 556;

// ─── Images (557..=558) ──────────────────────────────────────────────────────
//
// A loaded image is a texture the host already holds under its shared id --
// `Image.src` decoded and uploaded it there -- so drawing one names the id and
// the rectangles, and no pixel crosses.

/// `drawImage(image, sx, sy, sw, sh, dx, dy, dw, dh)`:
/// `H image_id:U sx sy sw sh dx dy dw dh:F`. The facade has already expanded
/// the two- and four-argument forms, so every record carries all eight.
pub const OP2D_DRAW_IMAGE: u32 = 557;

/// `drawImageBatch(draws)`: `H count | entries`, nine words per entry --
/// `image_id:U` then the eight rectangle `f32`s -- so `count` is a multiple of
/// nine. The id is an exact word: shared image ids live above 2^30, where an
/// `f32` cannot tell two consecutive ids apart.
pub const OP2D_DRAW_IMAGE_BATCH: u32 = 558;

/// Words per `drawImageBatch` entry.
pub const DRAW_IMAGE_BATCH_ENTRY_WORDS: u32 = 9;

/// The most entries a batch record may carry: the engine's own bound on
/// `drawImageBatch` (`shared::protocol::render_cmd::MAX_DRAW_IMAGE_BATCH_ENTRIES`).
pub const MAX_DRAW_IMAGE_BATCH_ENTRIES: u32 = 65_536;

// ─── Canvas lifetime (559..=561) ─────────────────────────────────────────────
//
// A canvas is not a drawing command, but every one of these names the canvas
// the block already selected, applies in the order the rest of the run applies,
// and has to be *in* that run: "create it, draw on it, hand the pixels to a
// texture" is one ordered sequence, and a lifetime change arriving beside the
// run rather than inside it is the race `Canvas2DCmd::ResizeCanvas` was added
// to close in process -- a resize that overtook its own fillText left cocos's
// pooled label canvas at the wrong size and the label blank.
//
// The in-process runtime reaches the same renderer calls through
// `CanvasCmd::RegisterOffscreen` and a synchronous `CanvasCmd::DestroyCanvas`,
// because there the ops and the renderer share a FIFO. The external producer
// has neither an op nor that FIFO, so these are its only path.

/// Bring the selected canvas into existence: `H width height`.
///
/// The id is the selected canvas's, allocated by the producer out of
/// `shared::protocol::render_cmd::PRODUCER_CANVAS_ID_BASE` exactly as the
/// in-process runtime allocates it, and the renderer refuses a registration
/// below that base -- which is what stops a producer in another process from
/// naming a canvas the renderer is about to allocate, or the onscreen one.
pub const OP2D_REGISTER_CANVAS: u32 = 559;

/// Resize the selected canvas: `H flags width height`.
///
/// `flags` is bit 0 for width and bit 1 for height, because content assigns
/// `canvas.width` and `canvas.height` separately and the op this stands for
/// takes each as an option. A pair arrives as one record rather than two, so
/// the renderer validates the final size the way the op does instead of
/// allocating an intermediate surface no frame ever drew to. Neither bit set is
/// a producer that encoded nothing; the decoder refuses it rather than applying
/// a resize to nothing.
pub const OP2D_RESIZE_CANVAS: u32 = 560;

/// Destroy the selected canvas: `H`.
///
/// The onscreen canvas is refused by the renderer, on this path and the
/// in-process one, so a producer cannot destroy the window's canvas by naming
/// it.
pub const OP2D_DESTROY_CANVAS: u32 = 561;

/// The bits `OP2D_RESIZE_CANVAS`'s flags word may set.
pub const RESIZE_CANVAS_WIDTH: u32 = 1;
/// The height bit of the same word.
pub const RESIZE_CANVAS_HEIGHT: u32 = 2;

/// One past the last 2D opcode in this block.
pub const OP2D_END: u32 = 562;

/// The longest dash pattern a record may carry.
///
/// A dash array is a handful of numbers -- `[5, 5]`, `[10, 3, 2, 3]` -- and the
/// specification lets content pass any array at all. The cap is far above any
/// pattern that draws differently from a shorter one and far below a record
/// that would cost a frame anything, so a producer that reaches it has a bug
/// rather than a dashed line.
pub const MAX_LINE_DASH_SEGMENTS: u32 = 256;

/// The shape of one 2D record, or `None` for an opcode this reader does not
/// know.
///
/// Word counts include the header word, which is the convention the GL block
/// already uses and the one a fixture written from the opcode name alone gets
/// wrong. `bool_words` are positions whose value must be exactly 0 or 1: a
/// `counterclockwise` of 2 is not a truthy value here, it is a producer bug,
/// and accepting it would mean the decoder and the producer disagree about what
/// the record said.
pub fn record_spec(opcode: u32) -> Option<RecordSpec> {
    let (word_count, bool_words): (u32, &'static [u8]) = match opcode {
        OP2D_SELECT_CANVAS => (2, &[]),

        OP2D_CREATE_CONTEXT | OP2D_BEGIN_PATH | OP2D_CLOSE_PATH | OP2D_DESTROY_CANVAS => (1, &[]),
        OP2D_REGISTER_CANVAS => (3, &[]),
        OP2D_RESIZE_CANVAS => (4, &[]),
        OP2D_MOVE_TO | OP2D_LINE_TO => (3, &[]),
        OP2D_QUADRATIC_CURVE_TO => (5, &[]),
        OP2D_BEZIER_CURVE_TO => (7, &[]),
        // x, y, radius, startAngle, endAngle, counterclockwise
        OP2D_ARC => (7, &[6]),
        OP2D_ARC_TO => (6, &[]),
        OP2D_RECT => (5, &[]),
        // x, y, radiusX, radiusY, rotation, startAngle, endAngle, ccw
        OP2D_ELLIPSE => (9, &[8]),

        OP2D_FILL | OP2D_STROKE | OP2D_CLIP => (1, &[]),

        OP2D_FILL_RECT | OP2D_STROKE_RECT | OP2D_CLEAR_RECT => (5, &[]),

        OP2D_SAVE | OP2D_RESTORE | OP2D_RESET_TRANSFORM => (1, &[]),
        OP2D_SET_TRANSFORM => (7, &[]),
        OP2D_TRANSLATE | OP2D_SCALE => (3, &[]),
        OP2D_ROTATE => (2, &[]),

        OP2D_SET_LINE_WIDTH
        | OP2D_SET_GLOBAL_ALPHA
        | OP2D_SET_MITER_LIMIT
        | OP2D_SET_LINE_DASH_OFFSET
        | OP2D_SET_SHADOW_BLUR
        | OP2D_SET_SHADOW_OFFSET_X
        | OP2D_SET_SHADOW_OFFSET_Y => (2, &[]),

        OP2D_SET_LINE_CAP | OP2D_SET_LINE_JOIN | OP2D_SET_COMPOSITE_OPERATION => (2, &[]),

        // Four floats, not a packed word: `Color` is four `f32` on the
        // destination, and packing to 8-bit channels here would quantise a
        // value the renderer keeps at full precision.
        OP2D_SET_FILL_STYLE | OP2D_SET_STROKE_STYLE | OP2D_SET_SHADOW_COLOR => (5, &[]),

        OP2D_SET_TEXT_ALIGN | OP2D_SET_TEXT_BASELINE | OP2D_SET_TEXT_DIRECTION => (2, &[]),

        OP2D_DRAW_IMAGE => (10, &[]),
        OP2D_DRAW_IMAGE_BATCH => {
            return Some(RecordSpec::Words {
                prefix_words: 1,
                max_count: MAX_DRAW_IMAGE_BATCH_ENTRIES * DRAW_IMAGE_BATCH_ENTRY_WORDS,
            });
        }

        // The payload records: their length is a word of their own rather than
        // their word count. Both shapes are the ones the resource block
        // introduced; see `RecordSpec::Bytes` and `Words`.
        OP2D_SET_FONT => {
            return Some(RecordSpec::Bytes {
                prefix_words: 1,
                presence_word: None,
                text: true,
            });
        }
        OP2D_FILL_TEXT | OP2D_STROKE_TEXT => {
            return Some(RecordSpec::Bytes {
                prefix_words: 4,
                presence_word: None,
                text: true,
            });
        }
        OP2D_SET_LINE_DASH => {
            return Some(RecordSpec::Words {
                prefix_words: 1,
                max_count: MAX_LINE_DASH_SEGMENTS,
            });
        }

        _ => return None,
    };
    Some(RecordSpec::Fixed {
        word_count,
        bool_words,
    })
}
