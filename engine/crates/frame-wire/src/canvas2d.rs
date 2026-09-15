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

/// One past the last 2D opcode in this block.
pub const OP2D_END: u32 = 550;

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

        OP2D_CREATE_CONTEXT | OP2D_BEGIN_PATH | OP2D_CLOSE_PATH => (1, &[]),
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

        _ => return None,
    };
    Some(RecordSpec::Fixed {
        word_count,
        bool_words,
    })
}
