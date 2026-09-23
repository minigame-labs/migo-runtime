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

use shared::protocol::render_cmd::{
    Canvas2DCmd, GradientType, TextAlign, TextBaseline, TextDirection,
};

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

        OP2D_REGISTER_CANVAS => Canvas2DCmd::RegisterCanvas {
            width: record[1],
            height: record[2],
        },
        OP2D_DESTROY_CANVAS => Canvas2DCmd::DestroyCanvas,
        // The one record in this block whose words the envelope cannot check.
        // A word count says how many numbers arrived, and `bool_words` says
        // which are 0 or 1; neither can say that a flags word names at least one
        // dimension. So this is the reading, and a flags word that names none --
        // or a bit this build does not know -- is a producer that encoded
        // something this reader would have to guess at, which is what `None`
        // means here: a stream the host does not execute rather than a resize
        // applied to whichever dimension seemed likely.
        OP2D_RESIZE_CANVAS => {
            let flags = record[1];
            if flags == 0 || flags & !(RESIZE_CANVAS_WIDTH | RESIZE_CANVAS_HEIGHT) != 0 {
                return None;
            }
            Canvas2DCmd::ResizeCanvas {
                w: (flags & RESIZE_CANVAS_WIDTH != 0).then_some(record[2]),
                h: (flags & RESIZE_CANVAS_HEIGHT != 0).then_some(record[3]),
            }
        }

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

        // ── Text ────────────────────────────────────────────────────────────
        OP2D_SET_FONT => Canvas2DCmd::SetFont {
            font: text_of(record, 1)?,
        },
        OP2D_FILL_TEXT => Canvas2DCmd::FillText {
            text: text_of(record, 4)?,
            x: f(record[1]),
            y: f(record[2]),
            max_width: f(record[3]),
        },
        OP2D_STROKE_TEXT => Canvas2DCmd::StrokeText {
            text: text_of(record, 4)?,
            x: f(record[1]),
            y: f(record[2]),
            max_width: f(record[3]),
        },
        // The stops are read by the same parser the in-process op uses, from
        // the same string the facade serialised: one reading of the engine's own
        // JSON rather than two that have to be held equal.
        OP2D_SET_FILL_STYLE_GRADIENT => Canvas2DCmd::SetFillStyleGradient {
            gradient_type: gradient_type_of(record[1]),
            x0: f(record[2]),
            y0: f(record[3]),
            r0: f(record[4]),
            x1: f(record[5]),
            y1: f(record[6]),
            r1: f(record[7]),
            stops: shared::protocol::render_cmd::parse_gradient_stops(&text_of(record, 8)?),
        },
        OP2D_SET_STROKE_STYLE_GRADIENT => Canvas2DCmd::SetStrokeStyleGradient {
            gradient_type: gradient_type_of(record[1]),
            x0: f(record[2]),
            y0: f(record[3]),
            r0: f(record[4]),
            x1: f(record[5]),
            y1: f(record[6]),
            r1: f(record[7]),
            stops: shared::protocol::render_cmd::parse_gradient_stops(&text_of(record, 8)?),
        },
        OP2D_SET_FILL_STYLE_PATTERN => Canvas2DCmd::SetFillStylePattern {
            image_id: record[1],
            repeat_x: record[2] != 0,
            repeat_y: record[3] != 0,
        },
        OP2D_SET_STROKE_STYLE_PATTERN => Canvas2DCmd::SetStrokeStylePattern {
            image_id: record[1],
            repeat_x: record[2] != 0,
            repeat_y: record[3] != 0,
        },

        // The capture the engine's `getImageData` makes: the pixels stay on the
        // host, in its snapshot pool, and what crosses back is whatever the
        // content actually asks for -- the bytes, or a texture upload that never
        // brings them to JavaScript at all.
        OP2D_CAPTURE_SNAPSHOT => Canvas2DCmd::CaptureSnapshot {
            x: record[1] as i32,
            y: record[2] as i32,
            width: record[3],
            height: record[4],
            snapshot_id: record[5],
            cache_key: None,
        },

        OP2D_SET_TEXT_ALIGN => Canvas2DCmd::SetTextAlign {
            align: text_align_of(record[1] as u8),
        },
        OP2D_SET_TEXT_BASELINE => Canvas2DCmd::SetTextBaseline {
            baseline: text_baseline_of(record[1] as u8),
        },
        OP2D_SET_TEXT_DIRECTION => Canvas2DCmd::SetTextDirection {
            direction: text_direction_of(record[1] as u8),
        },
        OP2D_SET_LINE_DASH => Canvas2DCmd::SetLineDash {
            segments: floats_of(record, 1)?,
        },

        // ── Images ──────────────────────────────────────────────────────────
        OP2D_DRAW_IMAGE => Canvas2DCmd::DrawImage {
            image_id: record[1],
            sx: f(record[2]),
            sy: f(record[3]),
            sw: f(record[4]),
            sh: f(record[5]),
            dx: f(record[6]),
            dy: f(record[7]),
            dw: f(record[8]),
            dh: f(record[9]),
        },
        OP2D_DRAW_IMAGE_BATCH => Canvas2DCmd::DrawImageBatch {
            draws: draw_image_entries(record)?,
        },

        _ => return None,
    })
}

/// The `u8` a text-state call carries, as the enum it names.
///
/// One body per call, the resource block's rule: `op_set_text_align` and the
/// record that stands in for it must agree on which number is `Center`, and the
/// only way to guarantee that is for both to call this. An unknown value is the
/// initial state rather than a refusal, which is what a browser does with a
/// `textAlign` it does not know.
pub fn text_align_of(value: u8) -> TextAlign {
    match value {
        0 => TextAlign::Start,
        1 => TextAlign::End,
        2 => TextAlign::Left,
        3 => TextAlign::Right,
        4 => TextAlign::Center,
        _ => TextAlign::Start,
    }
}

/// See [`text_align_of`].
pub fn text_baseline_of(value: u8) -> TextBaseline {
    match value {
        0 => TextBaseline::Top,
        1 => TextBaseline::Hanging,
        2 => TextBaseline::Middle,
        3 => TextBaseline::Alphabetic,
        4 => TextBaseline::Ideographic,
        5 => TextBaseline::Bottom,
        _ => TextBaseline::Alphabetic,
    }
}

/// See [`text_align_of`].
pub fn text_direction_of(value: u8) -> TextDirection {
    match value {
        1 => TextDirection::Ltr,
        2 => TextDirection::Rtl,
        _ => TextDirection::Inherit,
    }
}

/// The gradient kind the op takes as a number: anything but radial or conic is
/// linear, which is the op's own `match` and the browser's default.
fn gradient_type_of(word: u32) -> GradientType {
    match word as u8 {
        1 => GradientType::Radial,
        2 => GradientType::Conic,
        _ => GradientType::Linear,
    }
}

/// A record's text payload: the length word at `prefix_words`, then the bytes.
///
/// Pass 1 checked that they are UTF-8 and that the padding is zero, so this is
/// a copy rather than a parse. `None` only when the allocation fails, which
/// drops the one command rather than the frame.
fn text_of(record: &[u32], prefix_words: usize) -> Option<String> {
    let len = record[prefix_words] as usize;
    let words = &record[prefix_words + 1..];
    let mut bytes = Vec::new();
    bytes.try_reserve_exact(len).ok()?;
    let whole = len / 4;
    for word in &words[..whole] {
        bytes.extend_from_slice(&word.to_le_bytes());
    }
    if len % 4 != 0 {
        bytes.extend_from_slice(&words[whole].to_le_bytes()[..len % 4]);
    }
    String::from_utf8(bytes).ok()
}

/// A record's `f32` list: the count word at `prefix_words`, then the bits.
///
/// A reinterpretation, like every other float in this block.
fn floats_of(record: &[u32], prefix_words: usize) -> Option<Vec<f32>> {
    let count = record[prefix_words] as usize;
    let mut values = Vec::new();
    values.try_reserve_exact(count).ok()?;
    values.extend(
        record[prefix_words + 1..prefix_words + 1 + count]
            .iter()
            .map(|word| f(*word)),
    );
    Some(values)
}

/// A `drawImageBatch` record's entries: nine words each, the id then the eight
/// rectangle floats. `None` for a word count that is not whole entries, which
/// drops the one command -- what the op does with a buffer that is not.
fn draw_image_entries(record: &[u32]) -> Option<Vec<shared::protocol::render_cmd::DrawImageEntry>> {
    let words = &record[2..2 + record[1] as usize];
    let per = frame_wire::canvas2d::DRAW_IMAGE_BATCH_ENTRY_WORDS as usize;
    if words.is_empty() || !words.len().is_multiple_of(per) {
        return None;
    }
    let mut draws = Vec::new();
    draws.try_reserve_exact(words.len() / per).ok()?;
    draws.extend(words.chunks_exact(per).map(|entry| {
        shared::protocol::render_cmd::DrawImageEntry {
            image_id: entry[0],
            sx: f(entry[1]),
            sy: f(entry[2]),
            sw: f(entry[3]),
            sh: f(entry[4]),
            dx: f(entry[5]),
            dy: f(entry[6]),
            dw: f(entry[7]),
            dh: f(entry[8]),
        }
    }));
    Some(draws)
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

/// The twelve `f32` a `measureText` answer is, in the order the flat op writes
/// them.
///
/// One layout, not two: `op_measure_text_flat` answers with these bytes in
/// process, the Canvas2D metrics query answers with them across the boundary,
/// and the engine's facade reads a `TextMetrics` back from exactly this order.
/// A field swapped here is a label laid out against another field's number,
/// which lays the text out wrong rather than failing.
pub fn encode_text_metrics(metrics: &shared::protocol::render_cmd::TextMetrics) -> Vec<u8> {
    let fields: [f32; 12] = [
        metrics.width,
        metrics.actual_bounding_box_left,
        metrics.actual_bounding_box_right,
        metrics.em_height_ascent,
        metrics.em_height_descent,
        metrics.alphabetic_baseline,
        metrics.font_bounding_box_descent,
        metrics.actual_bounding_box_ascent,
        metrics.actual_bounding_box_descent,
        metrics.font_bounding_box_ascent,
        metrics.hanging_baseline,
        metrics.ideographic_baseline,
    ];
    let mut out = Vec::with_capacity(fields.len() * 4);
    for field in fields {
        out.extend_from_slice(&field.to_le_bytes());
    }
    out
}
