//! Canvas2D records become `Canvas2DCmd`s.
//!
//! The counterpart of the GL decoding in this crate's root, and the same rules
//! apply: the words were structurally validated by
//! `frame_wire::stream::validate_stream` before anything here reads them, so
//! a record's word count and its bool positions are already known good, and
//! this reads fields rather than checking them.
//!
//! What it does *not* do is validate parameters. WebGL's model -- reject the
//! call, push an error, keep drawing -- exists because WebGL is a state machine
//! over a GPU that can be put in an illegal state. Canvas2D's model is
//! different: the specification says a non-finite coordinate makes the call a
//! no-op, silently, and everything else is clamped or ignored. So the checks
//! that belong here are the ones the *specification* names, and they live with
//! the renderer that has the state to judge them.

use shared::protocol::render_cmd::Canvas2DCmd;

use frame_wire::canvas2d::*;

/// Read a `f32` back from the word the producer wrote.
///
/// A reinterpretation, not a conversion: the producer wrote the bit pattern,
/// and converting would turn one NaN payload into another. The same reasoning
/// as the GL uniform path, which learned it from a test that pinned NaN bits.
#[inline]
fn f(word: u32) -> f32 {
    f32::from_bits(word)
}

/// Decode one Canvas2D record.
///
/// `None` for an opcode this build does not know, which the validator has
/// already refused -- so reaching it means the spec table and this function
/// disagree, and the caller treats that as a stream it cannot execute rather
/// than skipping a command whose effect the rest of the frame assumes.
pub fn decode_record(opcode: u32, record: &[u32]) -> Option<Canvas2DCmd> {
    Some(match opcode {
        OP2D_BEGIN_PATH => Canvas2DCmd::BeginPath,
        OP2D_CLOSE_PATH => Canvas2DCmd::ClosePath,
        OP2D_MOVE_TO => Canvas2DCmd::MoveTo {
            x: f(record[1]),
            y: f(record[2]),
        },
        OP2D_LINE_TO => Canvas2DCmd::LineTo {
            x: f(record[1]),
            y: f(record[2]),
        },
        OP2D_QUADRATIC_CURVE_TO => Canvas2DCmd::QuadraticCurveTo {
            cpx: f(record[1]),
            cpy: f(record[2]),
            x: f(record[3]),
            y: f(record[4]),
        },
        OP2D_BEZIER_CURVE_TO => Canvas2DCmd::BezierCurveTo {
            cp1x: f(record[1]),
            cp1y: f(record[2]),
            cp2x: f(record[3]),
            cp2y: f(record[4]),
            x: f(record[5]),
            y: f(record[6]),
        },
        OP2D_ARC => Canvas2DCmd::Arc {
            x: f(record[1]),
            y: f(record[2]),
            radius: f(record[3]),
            start_angle: f(record[4]),
            end_angle: f(record[5]),
            counterclockwise: record[6] != 0,
        },
        OP2D_ARC_TO => Canvas2DCmd::ArcTo {
            x1: f(record[1]),
            y1: f(record[2]),
            x2: f(record[3]),
            y2: f(record[4]),
            radius: f(record[5]),
        },
        OP2D_RECT => Canvas2DCmd::Rect {
            x: f(record[1]),
            y: f(record[2]),
            w: f(record[3]),
            h: f(record[4]),
        },
        OP2D_ELLIPSE => Canvas2DCmd::Ellipse {
            x: f(record[1]),
            y: f(record[2]),
            radius_x: f(record[3]),
            radius_y: f(record[4]),
            rotation: f(record[5]),
            start_angle: f(record[6]),
            end_angle: f(record[7]),
            counterclockwise: record[8] != 0,
        },

        OP2D_FILL => Canvas2DCmd::Fill,
        OP2D_STROKE => Canvas2DCmd::Stroke,
        OP2D_CLIP => Canvas2DCmd::Clip,

        OP2D_FILL_RECT => Canvas2DCmd::FillRect {
            x: f(record[1]),
            y: f(record[2]),
            w: f(record[3]),
            h: f(record[4]),
        },
        OP2D_STROKE_RECT => Canvas2DCmd::StrokeRect {
            x: f(record[1]),
            y: f(record[2]),
            w: f(record[3]),
            h: f(record[4]),
        },
        OP2D_CLEAR_RECT => Canvas2DCmd::ClearRect {
            x: f(record[1]),
            y: f(record[2]),
            w: f(record[3]),
            h: f(record[4]),
        },

        OP2D_SAVE => Canvas2DCmd::Save,
        OP2D_RESTORE => Canvas2DCmd::Restore,
        OP2D_SET_TRANSFORM => Canvas2DCmd::SetTransform {
            a: f(record[1]),
            b: f(record[2]),
            c: f(record[3]),
            d: f(record[4]),
            e: f(record[5]),
            f: f(record[6]),
        },
        OP2D_RESET_TRANSFORM => Canvas2DCmd::ResetTransform,
        OP2D_TRANSLATE => Canvas2DCmd::Translate {
            x: f(record[1]),
            y: f(record[2]),
        },
        OP2D_ROTATE => Canvas2DCmd::Rotate {
            angle: f(record[1]),
        },
        OP2D_SCALE => Canvas2DCmd::Scale {
            x: f(record[1]),
            y: f(record[2]),
        },

        OP2D_SET_LINE_WIDTH => Canvas2DCmd::SetLineWidth {
            width: f(record[1]),
        },
        OP2D_SET_GLOBAL_ALPHA => Canvas2DCmd::SetGlobalAlpha {
            alpha: f(record[1]),
        },
        OP2D_SET_MITER_LIMIT => Canvas2DCmd::SetMiterLimit {
            limit: f(record[1]),
        },
        OP2D_SET_LINE_DASH_OFFSET => Canvas2DCmd::SetLineDashOffset {
            offset: f(record[1]),
        },
        OP2D_SET_SHADOW_BLUR => Canvas2DCmd::SetShadowBlur { blur: f(record[1]) },
        OP2D_SET_SHADOW_OFFSET_X => Canvas2DCmd::SetShadowOffsetX {
            offset: f(record[1]),
        },
        OP2D_SET_SHADOW_OFFSET_Y => Canvas2DCmd::SetShadowOffsetY {
            offset: f(record[1]),
        },

        // Truncated rather than rejected: these are small enumerations on the
        // destination and the producer's shim already maps a string to one of
        // them. A value outside the range is the shim's bug, and the renderer
        // clamps it the same way it clamps one that arrived through an op.
        OP2D_SET_LINE_CAP => Canvas2DCmd::SetLineCap {
            cap: record[1] as u8,
        },
        OP2D_SET_LINE_JOIN => Canvas2DCmd::SetLineJoin {
            join: record[1] as u8,
        },
        // The first record a producer sends to a canvas it means to draw on.
        // Everything else in this block needs the context this makes.
        OP2D_CREATE_CONTEXT => Canvas2DCmd::CreateContext2D,

        OP2D_SET_COMPOSITE_OPERATION => Canvas2DCmd::SetCompositeOperation {
            op: record[1] as u8,
        },

        OP2D_SET_FILL_STYLE => Canvas2DCmd::SetFillStyle {
            color: color_of(record),
        },
        OP2D_SET_STROKE_STYLE => Canvas2DCmd::SetStrokeStyle {
            color: color_of(record),
        },
        OP2D_SET_SHADOW_COLOR => Canvas2DCmd::SetShadowColor {
            color: color_of(record),
        },

        _ => return None,
    })
}

/// Four floats, in the order the destination's `Color` declares them.
fn color_of(record: &[u32]) -> shared::protocol::color::Color {
    shared::protocol::color::Color {
        r: f(record[1]),
        g: f(record[2]),
        b: f(record[3]),
        a: f(record[4]),
    }
}
