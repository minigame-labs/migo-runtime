//! Canvas 2D Context - Command Batching Implementation
//!
//! Canvas 2D Context ops.
//!
//! Draw commands are collected by `UnifiedFrameCollector` (see `frame_collector.rs`)
//! and sent as a single interleaved `FramePacket` per frame.
//!
//! Sync operations (`op_measure_text`, `op_get_image_data`) use
//! `RenderCommand::Canvas2D` for synchronous request/response.
//! `op_create_context_2d` is fire-and-forget: render thread FIFO
//! ordering is sufficient to serialise it before subsequent draws.

use deno_core::{OpState, op2};
use std::collections::HashMap;
use std::sync::LazyLock;
use tracing::error;

use shared::{
    op_state::CanvasOpState,
    protocol::{
        color::Color,
        render_cmd::{
            Canvas2DCmd, GradientType, MAX_DRAW_IMAGE_BATCH_ENTRIES, RenderCommand, TextAlign,
            TextBaseline, TextMetrics, checked_canvas_rgba_byte_len,
        },
        send_render_with_resp_sync,
    },
};

// ============================================================================
// Color parsing
// ============================================================================

/// `pub(crate)` so the agreement case can enumerate it. That case needs a
/// JavaScript runtime to drive the encoder, which lives beside the other
/// runtime-driven cases rather than here.
pub(crate) static NAMED_COLORS: LazyLock<HashMap<&'static str, Color>> = LazyLock::new(|| {
    [
        ("aliceblue", Color::rgb(240, 248, 255)),
        ("antiquewhite", Color::rgb(250, 235, 215)),
        ("aqua", Color::rgb(0, 255, 255)),
        ("aquamarine", Color::rgb(127, 255, 212)),
        ("azure", Color::rgb(240, 255, 255)),
        ("beige", Color::rgb(245, 245, 220)),
        ("bisque", Color::rgb(255, 228, 196)),
        ("black", Color::rgb(0, 0, 0)),
        ("blanchedalmond", Color::rgb(255, 235, 205)),
        ("blue", Color::rgb(0, 0, 255)),
        ("blueviolet", Color::rgb(138, 43, 226)),
        ("brown", Color::rgb(165, 42, 42)),
        ("burlywood", Color::rgb(222, 184, 135)),
        ("cadetblue", Color::rgb(95, 158, 160)),
        ("chartreuse", Color::rgb(127, 255, 0)),
        ("chocolate", Color::rgb(210, 105, 30)),
        ("coral", Color::rgb(255, 127, 80)),
        ("cornflowerblue", Color::rgb(100, 149, 237)),
        ("cornsilk", Color::rgb(255, 248, 220)),
        ("crimson", Color::rgb(220, 20, 60)),
        ("cyan", Color::rgb(0, 255, 255)),
        ("darkblue", Color::rgb(0, 0, 139)),
        ("darkcyan", Color::rgb(0, 139, 139)),
        ("darkgoldenrod", Color::rgb(184, 134, 11)),
        ("darkgray", Color::rgb(169, 169, 169)),
        ("darkgreen", Color::rgb(0, 100, 0)),
        ("darkgrey", Color::rgb(169, 169, 169)),
        ("darkkhaki", Color::rgb(189, 183, 107)),
        ("darkmagenta", Color::rgb(139, 0, 139)),
        ("darkolivegreen", Color::rgb(85, 107, 47)),
        ("darkorange", Color::rgb(255, 140, 0)),
        ("darkorchid", Color::rgb(153, 50, 204)),
        ("darkred", Color::rgb(139, 0, 0)),
        ("darksalmon", Color::rgb(233, 150, 122)),
        ("darkseagreen", Color::rgb(143, 188, 143)),
        ("darkslateblue", Color::rgb(72, 61, 139)),
        ("darkslategray", Color::rgb(47, 79, 79)),
        ("darkslategrey", Color::rgb(47, 79, 79)),
        ("darkturquoise", Color::rgb(0, 206, 209)),
        ("darkviolet", Color::rgb(148, 0, 211)),
        ("deeppink", Color::rgb(255, 20, 147)),
        ("deepskyblue", Color::rgb(0, 191, 255)),
        ("dimgray", Color::rgb(105, 105, 105)),
        ("dimgrey", Color::rgb(105, 105, 105)),
        ("dodgerblue", Color::rgb(30, 144, 255)),
        ("firebrick", Color::rgb(178, 34, 34)),
        ("floralwhite", Color::rgb(255, 250, 240)),
        ("forestgreen", Color::rgb(34, 139, 34)),
        ("fuchsia", Color::rgb(255, 0, 255)),
        ("gainsboro", Color::rgb(220, 220, 220)),
        ("ghostwhite", Color::rgb(248, 248, 255)),
        ("gold", Color::rgb(255, 215, 0)),
        ("goldenrod", Color::rgb(218, 165, 32)),
        ("gray", Color::rgb(128, 128, 128)),
        ("green", Color::rgb(0, 128, 0)),
        ("greenyellow", Color::rgb(173, 255, 47)),
        ("grey", Color::rgb(128, 128, 128)),
        ("honeydew", Color::rgb(240, 255, 240)),
        ("hotpink", Color::rgb(255, 105, 180)),
        ("indianred", Color::rgb(205, 92, 92)),
        ("indigo", Color::rgb(75, 0, 130)),
        ("ivory", Color::rgb(255, 255, 240)),
        ("khaki", Color::rgb(240, 230, 140)),
        ("lavender", Color::rgb(230, 230, 250)),
        ("lavenderblush", Color::rgb(255, 240, 245)),
        ("lawngreen", Color::rgb(124, 252, 0)),
        ("lemonchiffon", Color::rgb(255, 250, 205)),
        ("lightblue", Color::rgb(173, 216, 230)),
        ("lightcoral", Color::rgb(240, 128, 128)),
        ("lightcyan", Color::rgb(224, 255, 255)),
        ("lightgoldenrodyellow", Color::rgb(250, 250, 210)),
        ("lightgray", Color::rgb(211, 211, 211)),
        ("lightgreen", Color::rgb(144, 238, 144)),
        ("lightgrey", Color::rgb(211, 211, 211)),
        ("lightpink", Color::rgb(255, 182, 193)),
        ("lightsalmon", Color::rgb(255, 160, 122)),
        ("lightseagreen", Color::rgb(32, 178, 170)),
        ("lightskyblue", Color::rgb(135, 206, 250)),
        ("lightslategray", Color::rgb(119, 136, 153)),
        ("lightslategrey", Color::rgb(119, 136, 153)),
        ("lightsteelblue", Color::rgb(176, 196, 222)),
        ("lightyellow", Color::rgb(255, 255, 224)),
        ("lime", Color::rgb(0, 255, 0)),
        ("limegreen", Color::rgb(50, 205, 50)),
        ("linen", Color::rgb(250, 240, 230)),
        ("magenta", Color::rgb(255, 0, 255)),
        ("maroon", Color::rgb(128, 0, 0)),
        ("mediumaquamarine", Color::rgb(102, 205, 170)),
        ("mediumblue", Color::rgb(0, 0, 205)),
        ("mediumorchid", Color::rgb(186, 85, 211)),
        ("mediumpurple", Color::rgb(147, 112, 219)),
        ("mediumseagreen", Color::rgb(60, 179, 113)),
        ("mediumslateblue", Color::rgb(123, 104, 238)),
        ("mediumspringgreen", Color::rgb(0, 250, 154)),
        ("mediumturquoise", Color::rgb(72, 209, 204)),
        ("mediumvioletred", Color::rgb(199, 21, 133)),
        ("midnightblue", Color::rgb(25, 25, 112)),
        ("mintcream", Color::rgb(245, 255, 250)),
        ("mistyrose", Color::rgb(255, 228, 225)),
        ("moccasin", Color::rgb(255, 228, 181)),
        ("navajowhite", Color::rgb(255, 222, 173)),
        ("navy", Color::rgb(0, 0, 128)),
        ("oldlace", Color::rgb(253, 245, 230)),
        ("olive", Color::rgb(128, 128, 0)),
        ("olivedrab", Color::rgb(107, 142, 35)),
        ("orange", Color::rgb(255, 165, 0)),
        ("orangered", Color::rgb(255, 69, 0)),
        ("orchid", Color::rgb(218, 112, 214)),
        ("palegoldenrod", Color::rgb(238, 232, 170)),
        ("palegreen", Color::rgb(152, 251, 152)),
        ("paleturquoise", Color::rgb(175, 238, 238)),
        ("palevioletred", Color::rgb(219, 112, 147)),
        ("papayawhip", Color::rgb(255, 239, 213)),
        ("peachpuff", Color::rgb(255, 218, 185)),
        ("peru", Color::rgb(205, 133, 63)),
        ("pink", Color::rgb(255, 192, 203)),
        ("plum", Color::rgb(221, 160, 221)),
        ("powderblue", Color::rgb(176, 224, 230)),
        ("purple", Color::rgb(128, 0, 128)),
        ("rebeccapurple", Color::rgb(102, 51, 153)),
        ("red", Color::rgb(255, 0, 0)),
        ("rosybrown", Color::rgb(188, 143, 143)),
        ("royalblue", Color::rgb(65, 105, 225)),
        ("saddlebrown", Color::rgb(139, 69, 19)),
        ("salmon", Color::rgb(250, 128, 114)),
        ("sandybrown", Color::rgb(244, 164, 96)),
        ("seagreen", Color::rgb(46, 139, 87)),
        ("seashell", Color::rgb(255, 245, 238)),
        ("sienna", Color::rgb(160, 82, 45)),
        ("silver", Color::rgb(192, 192, 192)),
        ("skyblue", Color::rgb(135, 206, 235)),
        ("slateblue", Color::rgb(106, 90, 205)),
        ("slategray", Color::rgb(112, 128, 144)),
        ("slategrey", Color::rgb(112, 128, 144)),
        ("snow", Color::rgb(255, 250, 250)),
        ("springgreen", Color::rgb(0, 255, 127)),
        ("steelblue", Color::rgb(70, 130, 180)),
        ("tan", Color::rgb(210, 180, 140)),
        ("teal", Color::rgb(0, 128, 128)),
        ("thistle", Color::rgb(216, 191, 216)),
        ("tomato", Color::rgb(255, 99, 71)),
        ("turquoise", Color::rgb(64, 224, 208)),
        ("violet", Color::rgb(238, 130, 238)),
        ("wheat", Color::rgb(245, 222, 179)),
        ("white", Color::rgb(255, 255, 255)),
        ("whitesmoke", Color::rgb(245, 245, 245)),
        ("yellow", Color::rgb(255, 255, 0)),
        ("yellowgreen", Color::rgb(154, 205, 50)),
        // The one entry that is not a colour name in the CSS colour-keyword
        // list but a keyword the Canvas 2D specification resolves the same
        // way: `transparent` is `rgba(0, 0, 0, 0)`.
        //
        // It was missing, so `ctx.fillStyle = "transparent"` fell through to
        // the unknown-name branch and painted opaque black -- the loudest
        // possible wrong answer for a keyword whose whole meaning is "do not
        // paint". Found by
        // `the_javascript_colour_parser_never_disagrees_with_the_rust_one` on
        // its first run: the JavaScript table had it and this one did not.
        ("transparent", Color::rgbai(0, 0, 0, 0)),
    ]
    .into_iter()
    .collect()
});

/// Case-insensitive prefix check without allocation.
#[inline]
fn starts_with_ci(s: &str, prefix: &str) -> bool {
    s.len() >= prefix.len()
        && s.as_bytes()[..prefix.len()]
            .iter()
            .zip(prefix.as_bytes())
            .all(|(a, b)| a.to_ascii_lowercase() == *b)
}

/// Parse comma-separated values from an already-trimmed inner string.
/// Uses a stack-based array to avoid Vec allocation.
#[inline]
fn split_comma_parts(s: &str) -> ([&str; 4], usize) {
    let mut parts = [""; 4];
    let mut count = 0;
    for part in s.split(',') {
        if count >= 4 {
            return (parts, count + 1); // signal overflow
        }
        parts[count] = part.trim();
        count += 1;
    }
    (parts, count)
}

/// The authority on what a CSS colour string means.
///
/// `pub(crate)` for the same reason [`NAMED_COLORS`] is: the JavaScript
/// encoder answers the common forms itself to keep them off the op path, and
/// `the_javascript_colour_parser_never_disagrees_with_the_rust_one` requires
/// the two to produce the same `Color` for every string in the corpus.
pub(crate) fn parse_color_string(s: &str) -> Color {
    let s = s.trim();

    // #hex — no lowercase needed
    if s.starts_with('#') {
        return Color::hex(s);
    }

    // rgba(...) — case-insensitive prefix, zero-alloc
    if starts_with_ci(s, "rgba(") && s.ends_with(')') {
        let inner = &s[5..s.len() - 1];
        let (parts, count) = split_comma_parts(inner);
        if count == 4 {
            let r = parts[0].parse::<u8>().unwrap_or(0);
            let g = parts[1].parse::<u8>().unwrap_or(0);
            let b = parts[2].parse::<u8>().unwrap_or(0);
            let a = (parts[3].parse::<f32>().unwrap_or(1.0).clamp(0.0, 1.0) * 255.0) as u8;
            return Color::rgbai(r, g, b, a);
        }
        return Color::black();
    }

    // rgb(...) — case-insensitive prefix, zero-alloc
    if starts_with_ci(s, "rgb(") && s.ends_with(')') {
        let inner = &s[4..s.len() - 1];
        let (parts, count) = split_comma_parts(inner);
        if count == 3 {
            let r = parts[0].parse::<u8>().unwrap_or(0);
            let g = parts[1].parse::<u8>().unwrap_or(0);
            let b = parts[2].parse::<u8>().unwrap_or(0);
            return Color::rgb(r, g, b);
        }
        return Color::black();
    }

    // Named colors — lowercase on stack buffer (max 24 bytes, no heap alloc)
    let bytes = s.as_bytes();
    if bytes.len() <= 24 {
        let mut buf = [0u8; 24];
        for (i, &b) in bytes.iter().enumerate() {
            buf[i] = b.to_ascii_lowercase();
        }
        // SAFETY: input is valid UTF-8, lowercasing ASCII preserves UTF-8 validity
        let lower = unsafe { std::str::from_utf8_unchecked(&buf[..bytes.len()]) };
        NAMED_COLORS.get(lower).copied().unwrap_or(Color::black())
    } else {
        Color::black()
    }
}

// ============================================================================
// Sync operations (request/response via RenderCommand::Canvas2D)
// ============================================================================

#[op2(fast)]
pub fn op_create_context_2d(state: &mut OpState, #[smi] canvas_id: u32) -> i32 {
    // The JS facade submits its typed GL stream before this op. Materialize the
    // resulting collector segments before dispatching the direct Canvas2D
    // command, otherwise CreateContext2D can overtake earlier GL/2D work on the
    // render channel.
    if let Err(e) = crate::rendering::webgl::frame_collector::flush_unified_barrier(state) {
        error!("createContext2D barrier flush failed: {e}");
        return -1;
    }

    let ctx = state.borrow::<CanvasOpState>();
    // Fire-and-forget: render thread processes Canvas2D commands FIFO
    // on this channel. The barrier above has already put prior collector
    // work on that FIFO, so subsequent draws are guaranteed to run after
    // `init_skia_for_canvas`. The previous sync RTT was pure
    // stall — its only reply was the caller's own canvas_id — and was
    // observed as ~7–17 ms × dozens per shop scene open on Mali
    // ([SyncOp] canvas2d create context blocked V8 …).  Same shape as
    // the op_create_image RTT removal upstream.
    if let Err(e) = ctx.tx.dispatch(RenderCommand::Canvas2D {
        canvas_id,
        cmd: Canvas2DCmd::CreateContext2D,
    }) {
        error!("createContext2D dispatch failed: {e}");
        return -1;
    }
    canvas_id as i32
}

/// Best-effort barrier before a state-sync op (e.g. `measureText`): the
/// dispatch is bounded-blocking (no silent drop), but a delivery failure
/// only means a possibly-stale *font* measurement — the caller's fallback
/// estimate handles that, so we log and proceed rather than fail the op.
fn flush_pending_commands_for_state_sync(state: &mut OpState, _canvas_id: u32) {
    if let Err(e) = crate::rendering::webgl::frame_collector::flush_unified_barrier(state) {
        error!("canvas2d state-sync barrier flush failed: {e}");
    }
}

/// Required barrier before a pixel readback (`getImageData`): if the flush
/// can't be delivered we must NOT read — the result would reflect
/// un-materialized 2D content. Returns `Err(())` so the caller bails to its
/// error path instead of returning stale pixels.
fn flush_pending_commands_for_readback_sync(
    state: &mut OpState,
    _canvas_id: u32,
) -> Result<(), ()> {
    crate::rendering::webgl::frame_collector::flush_unified_barrier(state).map_err(|e| {
        error!("canvas2d readback barrier flush failed, refusing stale read: {e}");
    })
}

const OP_MEASURE_TEXT: &str = "canvas2d measure_text";

/// Conservative TextMetrics estimate used when the render thread
/// cannot produce a real measurement (timeout, disconnect, drop).
///
/// The estimate is deliberately **over** the real width rather
/// than under: UI code auto-sizing labels will reserve a bit more
/// space than strictly needed instead of clipping glyphs.  Width
/// is approximated as `text.chars().count() * 0.55 * font_size`;
/// 0.55 is a hair wider than the average advance ratio of Latin
/// text at proportional fonts and covers most CJK fullwidth
/// glyphs (1.0 * size) without degenerating to zero for punctuation.
///
/// Baselines follow Canvas 2D's typographic defaults: ascent
/// ≈ 0.8 * size, descent ≈ 0.2 * size.  Games that inspect them
/// get plausible numbers instead of the old all-zero response
/// that made layouts collapse.
/// `#[cfg(test)]`: the reference form, retained only so
/// `the_glyph_count_fallback_matches_the_string_one` has something to check the
/// count-based version against. Production callers all take a count — keeping
/// this compiled into shipped builds would advertise a second entry point whose
/// only distinction is that it allocates.
#[cfg(test)]
fn fallback_text_metrics(text: &str, font_size: f32) -> TextMetrics {
    fallback_text_metrics_for_glyph_count(text.chars().count(), font_size)
}

/// [`fallback_text_metrics`] for a glyph count already in hand.
///
/// **Exists so the measure ops need not keep the text alive for a path they
/// usually do not take.** The estimate reads exactly one thing from the string —
/// `chars().count()` — but the command that carries the text to the render
/// thread must own it, so the ops used to `text.clone()` unconditionally,
/// buying a heap allocation proportional to the label's length on every call in
/// order to have a `usize` available in the timeout branch. Counting first and
/// moving the original costs nothing.
fn fallback_text_metrics_for_glyph_count(glyphs: usize, font_size: f32) -> TextMetrics {
    // `font_size` gets passed in from the canvas state; we don't
    // know the live value here because measureText doesn't carry
    // font state over the wire.  We default to the Canvas 2D
    // baseline of 10 px if the caller hasn't plumbed the real
    // value through.  Call sites that *do* know the size should
    // plumb it through `fallback_text_metrics_for_state` below.
    let size = if font_size.is_finite() && font_size > 0.0 {
        font_size
    } else {
        10.0
    };
    let glyph_estimate = glyphs.max(1) as f32;
    let width = glyph_estimate * size * 0.55;
    let ascent = size * 0.8;
    let descent = size * 0.2;
    TextMetrics {
        width,
        actual_bounding_box_left: 0.0,
        actual_bounding_box_right: width,
        actual_bounding_box_ascent: ascent,
        actual_bounding_box_descent: descent,
        font_bounding_box_ascent: ascent,
        font_bounding_box_descent: descent,
        em_height_ascent: ascent,
        em_height_descent: descent,
        hanging_baseline: ascent * 0.8,
        alphabetic_baseline: 0.0,
        ideographic_baseline: -descent,
    }
}

#[op2]
#[serde]
pub fn op_measure_text(
    state: &mut OpState,
    #[smi] canvas_id: u32,
    #[string] text: String,
) -> TextMetrics {
    // Flush pending commands so the render thread has the latest font state.
    flush_pending_commands_for_state_sync(state, canvas_id);
    let ctx = state.borrow::<CanvasOpState>();
    // P0-1 (drop-safety) + P1-4 (measure has 4 ms deadline): the fallback needs
    // to reason about the text if the render thread times out, but the command
    // must own the `String` to carry it across the thread boundary.
    //
    // A count, not a clone. `fallback_text_metrics` reads exactly
    // `chars().count()` from the string, so cloning the whole thing bought a
    // heap allocation the length of the label on every call — including the
    // ones that succeed — to have a `usize` in a branch usually not taken.
    let glyphs = text.chars().count();
    match send_render_with_resp_sync(ctx, OP_MEASURE_TEXT, |resp| RenderCommand::Canvas2D {
        canvas_id,
        cmd: Canvas2DCmd::MeasureText { text, resp },
    }) {
        Ok(m) => m,
        Err(e) => {
            // P2-5: do not collapse to zero metrics on failure.
            // Zero width made auto-layout code stack every label
            // at (0, 0), producing the visible "text missing"
            // symptom even before the P0 responder bug was
            // fixed.  A conservative estimate keeps layouts
            // roughly correct until the next frame succeeds.
            error!("{OP_MEASURE_TEXT} failed: {e}; returning estimated metrics");
            fallback_text_metrics_for_glyph_count(glyphs, 10.0)
        }
    }
}

/// R-7: flat-buffer variant of `op_measure_text` that skips
/// serde_v8's per-property V8 object construction.
///
/// The 12 f32 fields of [`TextMetrics`] are written little-endian
/// into a `Vec<u8>` which `op2`'s `#[buffer]` attribute hands back
/// as an `ArrayBuffer` — JS code then re-interprets it as a
/// `Float32Array` and reads the fields by index.  On the hot
/// measure path (hundreds of calls per frame for UI-heavy scenes)
/// this saves the ~12 `v8::Object::set` invocations and the
/// matching string interning that `#[serde]` emits, cutting the
/// native-side overhead from ~30 μs to ~5 μs per call in the
/// cache-hit case.
///
/// Layout (byte offsets, little-endian):
///
/// ```text
/// 00: width                       20: font_bounding_box_descent
/// 04: actual_bounding_box_left    24: actual_bounding_box_ascent
/// 08: actual_bounding_box_right   28: actual_bounding_box_descent
/// 0C: em_height_ascent            32: font_bounding_box_ascent
/// 10: em_height_descent           36: hanging_baseline
/// 14: alphabetic_baseline         40: ideographic_baseline
/// ```
///
/// Kept alongside the `#[serde]` variant above so downstream
/// callers can switch gradually; the JS side prefers the flat op
/// and falls back to the serde op for older hosts.
#[op2]
#[buffer]
pub fn op_measure_text_flat(
    state: &mut OpState,
    #[smi] canvas_id: u32,
    #[string] text: String,
    #[string] css_font: String,
) -> Vec<u8> {
    // F-2 + G-2: fast lane.  When the host has published a
    // `SharedTextMeasurer` on `CanvasOpState`, the measurement
    // runs inline on the JS thread via a mutex-guarded
    // TextContext — no command channel trip, no `flush`
    // barrier, no serde round-trip.  Cache-hit cost drops from
    // ~30 μs to ~5 μs; cache-miss cost (shaping via Skia) is
    // unchanged because that's the real work either way.
    //
    // G-2: the JS side now hands us the raw CSS `font` string
    // and the single source of truth for parsing lives in
    // `shared::css_font`.  This eliminates the previous
    // JS/Rust parser duplication where `_parseCssFont` in JS
    // could disagree with `SetFont` parsing on the render
    // side, producing silent measure-vs-paint divergence.
    //
    // Falls back to the render-thread RPC when the measurer is
    // absent (headless tests, embedders that haven't wired it)
    // so behaviour stays identical.
    {
        let ctx = state.borrow::<CanvasOpState>();
        if let Some(m) = ctx.text_measurer.as_ref() {
            let metrics = m.measure_css(&text, &css_font);
            return encode_text_metrics(&metrics);
        }
    }

    // Fallback: cross-thread sync-op path, identical to the
    // legacy `op_measure_text` behaviour.  `canvas_id` is used
    // to pick up the per-canvas font state which the render
    // thread already knows; we ignore `css_font` in this
    // branch because the server side re-reads them from
    // `ctx.renderer.state.text`.
    flush_pending_commands_for_state_sync(state, canvas_id);
    let ctx = state.borrow::<CanvasOpState>();
    // Count rather than clone — see `op_measure_text`. `css_font` needs
    // neither: the closure below moves only `text`, so it is still in hand at
    // the fallback and the clone it used to take was pure waste.
    let glyphs = text.chars().count();
    let metrics =
        match send_render_with_resp_sync(ctx, OP_MEASURE_TEXT, |resp| RenderCommand::Canvas2D {
            canvas_id,
            cmd: Canvas2DCmd::MeasureText { text, resp },
        }) {
            Ok(m) => m,
            Err(e) => {
                error!("{OP_MEASURE_TEXT} (flat) failed: {e}; returning estimated metrics");
                let parsed = shared::css_font::parse_css_font(&css_font);
                fallback_text_metrics_for_glyph_count(glyphs, parsed.size)
            }
        };
    encode_text_metrics(&metrics)
}

/// Encode a [`TextMetrics`] into 48 bytes of little-endian f32s.
/// Field order matches the layout table on [`op_measure_text_flat`]
/// so the JS side can use a zero-copy `Float32Array` view.
#[inline]
fn encode_text_metrics(m: &TextMetrics) -> Vec<u8> {
    let fields: [f32; 12] = [
        m.width,
        m.actual_bounding_box_left,
        m.actual_bounding_box_right,
        m.em_height_ascent,
        m.em_height_descent,
        m.alphabetic_baseline,
        m.font_bounding_box_descent,
        m.actual_bounding_box_ascent,
        m.actual_bounding_box_descent,
        m.font_bounding_box_ascent,
        m.hanging_baseline,
        m.ideographic_baseline,
    ];
    let mut out = Vec::with_capacity(fields.len() * 4);
    for f in fields {
        out.extend_from_slice(&f.to_le_bytes());
    }
    out
}

const OP_GET_IMAGE_DATA: &str = "canvas2d get_image_data";
#[op2]
#[buffer]
pub fn op_get_image_data(
    state: &mut OpState,
    #[smi] canvas_id: u32,
    x: i32,
    y: i32,
    width: u32,
    height: u32,
) -> Vec<u8> {
    if checked_canvas_rgba_byte_len(width, height).is_none() {
        error!("{OP_GET_IMAGE_DATA}: dimensions exceed the synchronous RGBA cap");
        return vec![];
    }
    // Flush only dirty Canvas2D work before sync readback. If the barrier
    // can't be delivered, refuse to read stale/un-materialized pixels.
    if flush_pending_commands_for_readback_sync(state, canvas_id).is_err() {
        error!("{OP_GET_IMAGE_DATA}: barrier flush failed, returning empty");
        return vec![];
    }
    let ctx = state.borrow::<CanvasOpState>();
    match send_render_with_resp_sync(ctx, OP_GET_IMAGE_DATA, |resp| RenderCommand::Canvas2D {
        canvas_id,
        cmd: Canvas2DCmd::GetImageData {
            x,
            y,
            width,
            height,
            resp,
        },
    }) {
        Ok(d) => d,
        Err(e) => {
            error!("{OP_GET_IMAGE_DATA} failed: {e}");
            vec![]
        }
    }
}

/// Fire-and-forget snapshot capture: queues a Canvas2DCmd into the
/// frame collector so the capture rides the next FramePacket dispatch
/// alongside the surrounding canvas2D draws.  No sync round-trip; the
/// JS-side counter pre-allocates the id.
#[op2(fast)]
pub fn op_capture_canvas2d_snapshot(
    state: &mut OpState,
    #[smi] canvas_id: u32,
    x: i32,
    y: i32,
    #[smi] width: u32,
    #[smi] height: u32,
    #[smi] snapshot_id: u32,
) {
    if snapshot_id == 0 || checked_canvas_rgba_byte_len(width, height).is_none() {
        return;
    }
    queue_canvas2d(
        state,
        canvas_id,
        Canvas2DCmd::CaptureSnapshot {
            x,
            y,
            width,
            height,
            snapshot_id,
            cache_key: None,
        },
    );
}

const OP_FORCE_READBACK_SNAPSHOT: &str = "canvas2d force_readback_snapshot";
/// Backs the lazy `ImageData.data` getter. Returns a tightly packed RGBA8 byte
/// buffer (top-down rows, length `w * h * 4`),
/// matching the legacy `op_get_image_data` layout.  Empty `Vec` on
/// failure (snapshot already drained, FBO incomplete, …).
#[op2]
#[buffer]
pub fn op_force_readback_snapshot(state: &mut OpState, #[smi] snapshot_id: u32) -> Vec<u8> {
    if snapshot_id == 0 {
        return Vec::new();
    }
    // Flush the unified frame collector before the sync round-trip.
    // The matching `op_capture_canvas2d_snapshot` was queued (fire-
    // and-forget) into the same collector earlier on this JS turn; if
    // we read directly via the bypass channel, the render thread sees
    // the ReadSnapshotPixels first, the snapshot pool is empty, and
    // we return zeros -- which the lazy ImageData getter then writes
    // into the placeholder, producing blank textures (visible as
    // missing dynamic labels in cocos's gl.texImage2D(canvas) path).
    //
    // Required barrier: if it can't be delivered, refuse to read — the
    // snapshot pool would be empty and we'd return zeros (blank texture).
    if crate::rendering::webgl::frame_collector::flush_unified_barrier(state).is_err() {
        error!("{OP_FORCE_READBACK_SNAPSHOT}: barrier flush failed, returning empty");
        return Vec::new();
    }
    let ctx = state.borrow::<CanvasOpState>();
    // canvas_id is irrelevant for the readback path; the manager
    // hops onto any current canvas to issue the FBO bind.  Use 1
    // (onscreen) as a stable id for the routing.
    match send_render_with_resp_sync(ctx, OP_FORCE_READBACK_SNAPSHOT, |resp| {
        RenderCommand::Canvas2D {
            canvas_id: 1,
            cmd: Canvas2DCmd::ReadSnapshotPixels { snapshot_id, resp },
        }
    }) {
        Ok(d) => d,
        Err(e) => {
            error!("{OP_FORCE_READBACK_SNAPSHOT} failed: {e}");
            Vec::new()
        }
    }
}

// ============================================================================
// Text texture cache integration
// ============================================================================
//
// Three ops bridge the JS-side pattern recognizer in `02_2d_context.js` to
// this session's `SessionTextCache`, reached through
// `CanvasOpState::text_cache` (bound to the host id at extension
// bring-up, so no op ever touches another session's cache or its lock):
//   * `op_text_cache_peek_pin` — lookup-and-pin at `fillText` time.
//     Returns 1 on hit (caller MUST balance with `op_text_cache_unpin` once
//     the matching `texImage2D` consumes the entry, OR if the pattern
//     fails to materialise and the cache hit is abandoned).
//   * `op_text_cache_unpin` — pin balance, used for abandon paths.
//   * `op_capture_canvas2d_snapshot_for_cache` — miss-path record.  Same
//     shape as `op_capture_canvas2d_snapshot` but routes through
//     `Canvas2DCmd::CaptureSnapshot { cache_key: Some(_), .. }`, so the
//     render-thread snapshot drain transfers the texture into the cache
//     instead of deleting it.
//   * `op_tex_image_2d_from_text_cache` — hit-path GL upload.  Emits
//     `GLCmd::TexImage2DFromTextCache`, skipping the offscreen Canvas2D
//     pipeline entirely.
//
// The 11-field cache key is passed as primitives across the FFI rather
// than serialized (e.g. JSON) because each fillText / texImage2D pair
// pays the cost ≥twice per label, and on a cocos shop scene that's ~250
// op crossings.  Primitive args compile to a `Box<TextCacheKey>` once
// per call site without intermediate allocation.

fn build_text_cache_key(
    text: String,
    font_request: String,
    font_size: f32,
    font_weight: u16,
    italic: bool,
    fill_color: u32,
    text_align_u8: u8,
    text_baseline_u8: u8,
    canvas_w: u32,
    canvas_h: u32,
    // This session's current font generation, read from its own
    // `SessionTextCache`.  Another session reloading a font leaves this
    // value alone, so it cannot invalidate this session's cached text.
    font_generation: u64,
) -> shared::text_texture_cache::TextCacheKey {
    let text_align = match text_align_u8 {
        0 => TextAlign::Start,
        1 => TextAlign::End,
        2 => TextAlign::Left,
        3 => TextAlign::Right,
        4 => TextAlign::Center,
        _ => TextAlign::Start,
    };
    let text_baseline = match text_baseline_u8 {
        0 => TextBaseline::Top,
        1 => TextBaseline::Hanging,
        2 => TextBaseline::Middle,
        3 => TextBaseline::Alphabetic,
        4 => TextBaseline::Ideographic,
        5 => TextBaseline::Bottom,
        _ => TextBaseline::Alphabetic,
    };
    shared::text_texture_cache::TextCacheKey {
        text,
        font_request,
        font_size_bits: font_size.to_bits(),
        font_weight,
        italic,
        fill_color,
        text_align,
        text_baseline,
        canvas_w,
        canvas_h,
        font_generation,
    }
}

/// Look up this session's text texture cache; on hit, increment the pin
/// count and return `1` so the caller can safely emit a
/// `TexImage2DFromTextCache` later in the same frame.  On miss, returns
/// `0` without side effects.
/// The pin acquired on hit MUST be balanced by either
/// `op_tex_image_2d_from_text_cache` (which the render thread unpins
/// after executing the copy) or `op_text_cache_unpin` if the JS-side
/// abandons the hit before the consuming texImage2D arrives.
#[op2(fast)]
#[allow(clippy::too_many_arguments)]
pub fn op_text_cache_peek_pin(
    state: &mut OpState,
    #[string] text: String,
    #[string] font_request: String,
    font_size: f32,
    #[smi] font_weight: u32,
    italic: bool,
    #[smi] fill_color: u32,
    #[smi] text_align: u8,
    #[smi] text_baseline: u8,
    #[smi] canvas_w: u32,
    #[smi] canvas_h: u32,
) -> u8 {
    let text_cache = state.borrow::<CanvasOpState>().text_cache.clone();
    let key = build_text_cache_key(
        text,
        font_request,
        font_size,
        font_weight as u16,
        italic,
        fill_color,
        text_align,
        text_baseline,
        canvas_w,
        canvas_h,
        text_cache.font_generation(),
    );
    let mut cache = text_cache.lock();
    // One lookup, not a peek followed by a pin: pinning only succeeds for a
    // resident entry, so its result *is* the hit answer this op returns.
    u8::from(cache.pin(&key))
}

/// Drop a pin previously acquired by `op_text_cache_peek_pin`.  Used on
/// abandon paths (the JS-side cocos pattern broke before the consuming
/// `texImage2D` arrived; the cache entry would otherwise stay pinned
/// indefinitely).
#[op2(fast)]
#[allow(clippy::too_many_arguments)]
pub fn op_text_cache_unpin(
    state: &mut OpState,
    #[string] text: String,
    #[string] font_request: String,
    font_size: f32,
    #[smi] font_weight: u32,
    italic: bool,
    #[smi] fill_color: u32,
    #[smi] text_align: u8,
    #[smi] text_baseline: u8,
    #[smi] canvas_w: u32,
    #[smi] canvas_h: u32,
) {
    let text_cache = state.borrow::<CanvasOpState>().text_cache.clone();
    let key = build_text_cache_key(
        text,
        font_request,
        font_size,
        font_weight as u16,
        italic,
        fill_color,
        text_align,
        text_baseline,
        canvas_w,
        canvas_h,
        text_cache.font_generation(),
    );
    text_cache.lock().unpin(&key);
}

/// Miss-path snapshot capture: identical to `op_capture_canvas2d_snapshot`
/// except the pushed `Canvas2DCmd::CaptureSnapshot` carries `cache_key =
/// Some(_)`.  At frame end the render thread transfers the resulting GL
/// texture into the text texture cache instead of deleting it, so the
/// next identical fillText hits.
#[op2(fast)]
#[allow(clippy::too_many_arguments)]
pub fn op_capture_canvas2d_snapshot_for_cache(
    state: &mut OpState,
    #[smi] canvas_id: u32,
    x: i32,
    y: i32,
    #[smi] width: u32,
    #[smi] height: u32,
    #[smi] snapshot_id: u32,
    #[string] text: String,
    #[string] font_request: String,
    font_size: f32,
    #[smi] font_weight: u32,
    italic: bool,
    #[smi] fill_color: u32,
    #[smi] text_align: u8,
    #[smi] text_baseline: u8,
    #[smi] canvas_w: u32,
    #[smi] canvas_h: u32,
) {
    if snapshot_id == 0 || checked_canvas_rgba_byte_len(width, height).is_none() {
        return;
    }
    let font_generation = state.borrow::<CanvasOpState>().text_cache.font_generation();
    let key = build_text_cache_key(
        text,
        font_request,
        font_size,
        font_weight as u16,
        italic,
        fill_color,
        text_align,
        text_baseline,
        canvas_w,
        canvas_h,
        font_generation,
    );
    queue_canvas2d(
        state,
        canvas_id,
        Canvas2DCmd::CaptureSnapshot {
            x,
            y,
            width,
            height,
            snapshot_id,
            cache_key: Some(Box::new(key)),
        },
    );
}

/// Hit-path GL upload.  Emits `GLCmd::TexImage2DFromTextCache` so the
/// destination texture currently bound to `target` on `canvas_id` is
/// populated from the cached source texture via FBO + glCopyTexImage2D
/// on the render thread.  The cached entry is unpinned by the render
/// thread inside `tex_image_2d_from_text_cache`.
#[op2(fast)]
#[allow(clippy::too_many_arguments)]
pub fn op_tex_image_2d_from_text_cache(
    state: &mut OpState,
    #[smi] canvas_id: u32,
    #[smi] target: u32,
    #[smi] level: i32,
    #[smi] internalformat: i32,
    #[string] text: String,
    #[string] font_request: String,
    font_size: f32,
    #[smi] font_weight: u32,
    italic: bool,
    #[smi] fill_color: u32,
    #[smi] text_align: u8,
    #[smi] text_baseline: u8,
    #[smi] canvas_w: u32,
    #[smi] canvas_h: u32,
) {
    let font_generation = state.borrow::<CanvasOpState>().text_cache.font_generation();
    let key = build_text_cache_key(
        text,
        font_request,
        font_size,
        font_weight as u16,
        italic,
        fill_color,
        text_align,
        text_baseline,
        canvas_w,
        canvas_h,
        font_generation,
    );
    super::webgl::queue_gl_fire_and_forget(
        state,
        shared::protocol::render_cmd::GLCmd::TexImage2DFromTextCache {
            canvas_id,
            target,
            level,
            internalformat,
            key: Box::new(key),
        },
    );
}

// ============================================================================
// Frame lifecycle operations
// ============================================================================

/// Build and send one interleaved FramePacket from all accumulated
/// Canvas2D + GL segments, with Materialize barriers at 2D->GL transitions.
fn do_frame_end_unified(state: &mut OpState) {
    let packet = {
        if let Some(collector) = state
            .try_borrow_mut::<crate::rendering::webgl::frame_collector::UnifiedFrameCollector>(
        ) {
            collector.build_frame_packet(true)
        } else {
            None
        }
    };

    if let Some(packet) = packet {
        let ctx = state.borrow::<CanvasOpState>();
        // FramePacket carries the frame's Canvas2D + GL draw work.
        // Non-idempotent, so route via `dispatch()` — on sustained
        // backpressure we prefer blocking the JS tick for a
        // sub-frame window over silently dropping an entire
        // frame's pixels.  The `BLOCKING_SEND_DEADLINE` (8 ms) cap
        // still bounds worst-case stall.
        if let Err(e) = ctx.tx.dispatch(RenderCommand::FramePacket(packet)) {
            error!("frame_end_unified: dispatch failed: {e}");
        }
    }
}

/// Unified frame-end: primary frame-end path called from the RAF loop.
#[op2(fast)]
pub fn op_frame_end_unified(state: &mut OpState) {
    do_frame_end_unified(state);
}

#[op2(fast)]
pub fn op_invalidate(state: &mut OpState, #[smi] _canvas_id: u32) {
    let ctx = state.borrow::<CanvasOpState>();
    if let Err(e) = ctx.tx.send(RenderCommand::Invalidate) {
        error!("op_invalidate: send failed: {e}");
    }
}

// ============================================================================
// Batched Canvas 2D operations
// ============================================================================

/// Queue one Canvas2D command, cutting a barrier only if the batch crossed the
/// soft byte budget.
///
/// **One `OpState` borrow, and backpressure that costs nothing when it does not
/// fire.** This is the Canvas2D counterpart of
/// `webgl::queue_gl_fire_and_forget`, which has always had this shape; the
/// Canvas2D ops had drifted into two others, each missing half of it:
///
/// * The `batched_op!` ops pushed through one borrow and then called
///   `maybe_auto_flush`, which re-borrows `OpState` purely to read
///   `should_auto_flush()` — a field comparison available on the borrow just
///   released. `OpState`'s store is a `BTreeMap<TypeId, Box<dyn Any>>`, so a
///   borrow is a tree descent, not a field read: measured at ~15 ns, which the
///   second one doubled to ~31 ns per op. See
///   `bench_second_op_state_borrow_per_canvas2d_op`.
/// * The hand-written path and rect ops paid one borrow but never checked the
///   budget at all, so a JS turn issuing `fillRect` in a loop accumulated
///   pending commands past the 4 MiB ceiling
///   [`AUTO_FLUSH_SOFT_BUDGET_BYTES`](crate::rendering::webgl::frame_collector::AUTO_FLUSH_SOFT_BUDGET_BYTES)
///   exists to enforce — the exact "pin the JS heap unbounded" case its doc
///   names.
///
/// Reading the budget on the borrow already held is what reconciles them: the
/// check becomes free, so no op has a reason to skip it, and every Canvas2D op
/// can share one funnel.
#[inline]
fn queue_canvas2d(state: &mut OpState, canvas_id: u32, cmd: Canvas2DCmd) {
    with_collector(state, |collector| collector.push(canvas_id, cmd));
}

/// Run one mutation against the frame collector, then cut a barrier only if it
/// crossed the soft byte budget.
///
/// The primitive [`queue_canvas2d`] is built on, separate because four ops reach
/// the collector through methods of their own rather than `push` —
/// `set_font`, `set_line_dash`, and the two gradient setters — and they need the
/// same single-borrow discipline. Taking a closure is what lets one funnel serve
/// both: the budget read happens on the borrow the mutation already holds,
/// whatever the mutation was.
#[inline]
fn with_collector(
    state: &mut OpState,
    mutate: impl FnOnce(&mut crate::rendering::webgl::frame_collector::UnifiedFrameCollector),
) {
    let over_budget = match state
        .try_borrow_mut::<crate::rendering::webgl::frame_collector::UnifiedFrameCollector>()
    {
        Some(collector) => {
            collector.record_boundary_crossing();
            mutate(collector);
            collector.should_auto_flush()
        }
        // No collector installed (headless embedder): the command has nowhere to
        // go, and `maybe_auto_flush` would find nothing to flush either.
        None => false,
    };
    if over_budget {
        crate::rendering::webgl::webgl::maybe_auto_flush(state);
    }
}

macro_rules! batched_op {
    ($fn_name:ident, $cmd:expr) => {
        #[op2(fast)]
        pub fn $fn_name(state: &mut OpState, #[smi] canvas_id: u32) {
            queue_canvas2d(state, canvas_id, $cmd);
        }
    };
}

// Path operations
batched_op!(op_begin_path, Canvas2DCmd::BeginPath);
batched_op!(op_close_path, Canvas2DCmd::ClosePath);
batched_op!(op_fill, Canvas2DCmd::Fill);
batched_op!(op_stroke, Canvas2DCmd::Stroke);
batched_op!(op_clip, Canvas2DCmd::Clip);

// State operations
batched_op!(op_save, Canvas2DCmd::Save);

#[op2(fast)]
pub fn op_restore(state: &mut OpState, #[smi] canvas_id: u32) {
    with_collector(state, |collector| {
        collector.restore(canvas_id);
    });
}

// Transform operations
batched_op!(op_reset_transform, Canvas2DCmd::ResetTransform);

#[op2(fast)]
pub fn op_move_to(state: &mut OpState, #[smi] canvas_id: u32, x: f32, y: f32) {
    queue_canvas2d(state, canvas_id, Canvas2DCmd::MoveTo { x, y });
}

#[op2(fast)]
pub fn op_line_to(state: &mut OpState, #[smi] canvas_id: u32, x: f32, y: f32) {
    queue_canvas2d(state, canvas_id, Canvas2DCmd::LineTo { x, y });
}

#[op2(fast)]
pub fn op_quadratic_curve_to(
    state: &mut OpState,
    #[smi] canvas_id: u32,
    cpx: f32,
    cpy: f32,
    x: f32,
    y: f32,
) {
    queue_canvas2d(
        state,
        canvas_id,
        Canvas2DCmd::QuadraticCurveTo { cpx, cpy, x, y },
    );
}

#[op2(fast)]
pub fn op_bezier_curve_to(
    state: &mut OpState,
    #[smi] canvas_id: u32,
    cp1x: f32,
    cp1y: f32,
    cp2x: f32,
    cp2y: f32,
    x: f32,
    y: f32,
) {
    queue_canvas2d(
        state,
        canvas_id,
        Canvas2DCmd::BezierCurveTo {
            cp1x,
            cp1y,
            cp2x,
            cp2y,
            x,
            y,
        },
    );
}

#[op2(fast)]
pub fn op_arc(
    state: &mut OpState,
    #[smi] canvas_id: u32,
    x: f32,
    y: f32,
    radius: f32,
    start_angle: f32,
    end_angle: f32,
    counterclockwise: bool,
) {
    queue_canvas2d(
        state,
        canvas_id,
        Canvas2DCmd::Arc {
            x,
            y,
            radius,
            start_angle,
            end_angle,
            counterclockwise,
        },
    );
}

#[op2(fast)]
pub fn op_arc_to(
    state: &mut OpState,
    #[smi] canvas_id: u32,
    x1: f32,
    y1: f32,
    x2: f32,
    y2: f32,
    radius: f32,
) {
    queue_canvas2d(
        state,
        canvas_id,
        Canvas2DCmd::ArcTo {
            x1,
            y1,
            x2,
            y2,
            radius,
        },
    );
}

#[op2(fast)]
pub fn op_rect(state: &mut OpState, #[smi] canvas_id: u32, x: f32, y: f32, w: f32, h: f32) {
    queue_canvas2d(state, canvas_id, Canvas2DCmd::Rect { x, y, w, h });
}

#[op2(fast)]
#[allow(clippy::too_many_arguments)]
pub fn op_ellipse(
    state: &mut OpState,
    #[smi] canvas_id: u32,
    x: f32,
    y: f32,
    radius_x: f32,
    radius_y: f32,
    rotation: f32,
    start_angle: f32,
    end_angle: f32,
    counterclockwise: bool,
) {
    queue_canvas2d(
        state,
        canvas_id,
        Canvas2DCmd::Ellipse {
            x,
            y,
            radius_x,
            radius_y,
            rotation,
            start_angle,
            end_angle,
            counterclockwise,
        },
    );
}

// Rectangle operations
#[op2(fast)]
pub fn op_fill_rect(state: &mut OpState, #[smi] canvas_id: u32, x: f32, y: f32, w: f32, h: f32) {
    queue_canvas2d(state, canvas_id, Canvas2DCmd::FillRect { x, y, w, h });
}

#[op2(fast)]
pub fn op_stroke_rect(state: &mut OpState, #[smi] canvas_id: u32, x: f32, y: f32, w: f32, h: f32) {
    queue_canvas2d(state, canvas_id, Canvas2DCmd::StrokeRect { x, y, w, h });
}

#[op2(fast)]
pub fn op_clear_rect(state: &mut OpState, #[smi] canvas_id: u32, x: f32, y: f32, w: f32, h: f32) {
    queue_canvas2d(state, canvas_id, Canvas2DCmd::ClearRect { x, y, w, h });
}

// Text operations
#[op2(fast)]
pub fn op_fill_text(
    state: &mut OpState,
    #[smi] canvas_id: u32,
    #[string] text: String,
    x: f32,
    y: f32,
    max_width: f32,
) {
    queue_canvas2d(
        state,
        canvas_id,
        Canvas2DCmd::FillText {
            text,
            x,
            y,
            max_width,
        },
    );
}

#[op2(fast)]
pub fn op_stroke_text(
    state: &mut OpState,
    #[smi] canvas_id: u32,
    #[string] text: String,
    x: f32,
    y: f32,
    max_width: f32,
) {
    queue_canvas2d(
        state,
        canvas_id,
        Canvas2DCmd::StrokeText {
            text,
            x,
            y,
            max_width,
        },
    );
}

// Style operations (with deduplication)
#[op2(fast)]
pub fn op_set_fill_style(state: &mut OpState, #[smi] canvas_id: u32, #[string] color_str: String) {
    with_collector(state, |collector| {
        let color = parse_color_string(&color_str);
        collector.set_fill_color(canvas_id, color);
    });
}

#[op2(fast)]
pub fn op_set_stroke_style(
    state: &mut OpState,
    #[smi] canvas_id: u32,
    #[string] color_str: String,
) {
    with_collector(state, |collector| {
        let color = parse_color_string(&color_str);
        collector.set_stroke_color(canvas_id, color);
    });
}

#[op2(fast)]
pub fn op_set_line_width(state: &mut OpState, #[smi] canvas_id: u32, width: f32) {
    with_collector(state, |collector| {
        collector.set_line_width(canvas_id, width);
    });
}

#[op2(fast)]
pub fn op_set_line_cap(state: &mut OpState, #[smi] canvas_id: u32, #[smi] cap: u8) {
    with_collector(state, |collector| {
        collector.set_line_cap(canvas_id, cap);
    });
}

#[op2(fast)]
pub fn op_set_line_join(state: &mut OpState, #[smi] canvas_id: u32, #[smi] join: u8) {
    with_collector(state, |collector| {
        collector.set_line_join(canvas_id, join);
    });
}

#[op2(fast)]
pub fn op_set_miter_limit(state: &mut OpState, #[smi] canvas_id: u32, limit: f32) {
    with_collector(state, |collector| {
        collector.set_miter_limit(canvas_id, limit);
    });
}

#[op2(fast)]
pub fn op_set_global_alpha(state: &mut OpState, #[smi] canvas_id: u32, alpha: f32) {
    with_collector(state, |collector| {
        collector.set_global_alpha(canvas_id, alpha);
    });
}

#[op2(fast)]
pub fn op_set_composite_operation(state: &mut OpState, #[smi] canvas_id: u32, #[smi] op: u8) {
    with_collector(state, |collector| {
        collector.set_composite_operation(canvas_id, op);
    });
}

#[op2(fast)]
pub fn op_set_line_dash(state: &mut OpState, #[smi] canvas_id: u32, #[buffer] segments: &[u8]) {
    with_collector(state, |collector| {
        // segments is a Float32Array transferred as raw bytes
        let floats: Vec<f32> = segments
            .chunks_exact(4)
            .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
            .collect();
        collector.set_line_dash(canvas_id, floats);
    });
}

#[op2(fast)]
pub fn op_set_line_dash_offset(state: &mut OpState, #[smi] canvas_id: u32, offset: f32) {
    with_collector(state, |collector| {
        collector.set_line_dash_offset(canvas_id, offset);
    });
}

#[op2(fast)]
pub fn op_set_shadow_blur(state: &mut OpState, #[smi] canvas_id: u32, blur: f32) {
    with_collector(state, |collector| {
        collector.set_shadow_blur(canvas_id, blur);
    });
}

#[op2(fast)]
pub fn op_set_shadow_color(
    state: &mut OpState,
    #[smi] canvas_id: u32,
    #[string] color_str: String,
) {
    with_collector(state, |collector| {
        let color = parse_color_string(&color_str);
        collector.set_shadow_color(canvas_id, color);
    });
}

#[op2(fast)]
pub fn op_set_shadow_offset_x(state: &mut OpState, #[smi] canvas_id: u32, offset: f32) {
    with_collector(state, |collector| {
        collector.set_shadow_offset_x(canvas_id, offset);
    });
}

#[op2(fast)]
pub fn op_set_shadow_offset_y(state: &mut OpState, #[smi] canvas_id: u32, offset: f32) {
    with_collector(state, |collector| {
        collector.set_shadow_offset_y(canvas_id, offset);
    });
}

#[op2(fast)]
pub fn op_set_fill_style_gradient(
    state: &mut OpState,
    #[smi] canvas_id: u32,
    #[smi] gradient_type: u8,
    x0: f32,
    y0: f32,
    r0: f32,
    x1: f32,
    y1: f32,
    r1: f32,
    #[string] stops_json: String,
) {
    with_collector(state, |collector| {
        // Parse stops from JSON: [{"offset":0,"r":255,"g":0,"b":0,"a":255}, ...]
        let stops = parse_gradient_stops(&stops_json);
        let gradient_type = match gradient_type {
            1 => GradientType::Radial,
            2 => GradientType::Conic,
            _ => GradientType::Linear,
        };
        collector.set_fill_style_gradient(canvas_id, gradient_type, x0, y0, r0, x1, y1, r1, stops);
    });
}

#[op2(fast)]
pub fn op_set_stroke_style_gradient(
    state: &mut OpState,
    #[smi] canvas_id: u32,
    #[smi] gradient_type: u8,
    x0: f32,
    y0: f32,
    r0: f32,
    x1: f32,
    y1: f32,
    r1: f32,
    #[string] stops_json: String,
) {
    with_collector(state, |collector| {
        let stops = parse_gradient_stops(&stops_json);
        let gradient_type = match gradient_type {
            1 => GradientType::Radial,
            2 => GradientType::Conic,
            _ => GradientType::Linear,
        };
        collector.set_stroke_style_gradient(
            canvas_id,
            gradient_type,
            x0,
            y0,
            r0,
            x1,
            y1,
            r1,
            stops,
        );
    });
}

fn parse_gradient_stops(json: &str) -> Vec<shared::protocol::render_cmd::GradientStop> {
    // Minimal JSON array parser for gradient stops to avoid serde dependency.
    // Format: [{"offset":0.0,"r":255,"g":0,"b":0,"a":255}, ...]
    let mut stops = Vec::new();
    // Simple approach: split by "},{" boundaries
    let trimmed = json.trim().trim_start_matches('[').trim_end_matches(']');
    if trimmed.is_empty() {
        return stops;
    }
    for entry in trimmed.split("},{") {
        let s = entry.trim().trim_start_matches('{').trim_end_matches('}');
        let mut offset = 0.0f32;
        let mut r = 0u8;
        let mut g = 0u8;
        let mut b = 0u8;
        let mut a = 255u8;
        for pair in s.split(',') {
            let pair = pair.trim().trim_matches('"');
            if let Some((key, val)) = pair.split_once(':') {
                let key = key.trim().trim_matches('"');
                let val = val.trim().trim_matches('"');
                match key {
                    "offset" => offset = val.parse().unwrap_or(0.0),
                    "r" => r = val.parse().unwrap_or(0),
                    "g" => g = val.parse().unwrap_or(0),
                    "b" => b = val.parse().unwrap_or(0),
                    "a" => a = val.parse().unwrap_or(255),
                    _ => {}
                }
            }
        }
        stops.push(shared::protocol::render_cmd::GradientStop {
            offset,
            color: shared::protocol::color::Color::rgbai(r, g, b, a),
        });
    }
    stops
}

#[op2(fast)]
pub fn op_set_fill_style_pattern(
    state: &mut OpState,
    #[smi] canvas_id: u32,
    #[smi] image_id: u32,
    repeat_x: bool,
    repeat_y: bool,
) {
    with_collector(state, |collector| {
        collector.set_fill_style_pattern(canvas_id, image_id, repeat_x, repeat_y);
    });
}

#[op2(fast)]
pub fn op_set_stroke_style_pattern(
    state: &mut OpState,
    #[smi] canvas_id: u32,
    #[smi] image_id: u32,
    repeat_x: bool,
    repeat_y: bool,
) {
    with_collector(state, |collector| {
        collector.set_stroke_style_pattern(canvas_id, image_id, repeat_x, repeat_y);
    });
}

/// Set the 2D context's font, or report that the value was not a font.
///
/// WHATWG: assigning an unparseable value to `ctx.font` is a no-op, and the
/// previous font stays in effect. That rule was enforced on the render thread
/// only -- it rejected the shorthand and kept the old state -- while the JS
/// thread went on measuring with the best-effort parse of the same string. The
/// result was the one divergence the two font parsers exist to prevent: for
/// `64px ""`, `measureText` answered for 64 px and `fillText` painted at the
/// font before it. The check moves here, ahead of both, so an invalid value
/// never reaches either side and `ctx.font` never reports one.
///
/// @return whether the value was applied; the caller keeps its previous font
///         when this is false.
#[op2(fast)]
pub fn op_set_font(state: &mut OpState, #[smi] canvas_id: u32, #[string] font: String) -> bool {
    if shared::css_font_shorthand::parse_font_shorthand(&font).is_none() {
        return false;
    }
    with_collector(state, |collector| {
        collector.set_font(canvas_id, font);
    });
    true
}

#[op2(fast)]
pub fn op_set_text_align(state: &mut OpState, #[smi] canvas_id: u32, #[smi] align: u8) {
    with_collector(state, |collector| {
        collector.set_text_align(canvas_id, frame_decode::canvas2d::text_align_of(align));
    });
}

#[op2(fast)]
pub fn op_set_text_baseline(state: &mut OpState, #[smi] canvas_id: u32, #[smi] baseline: u8) {
    with_collector(state, |collector| {
        collector.set_text_baseline(canvas_id, frame_decode::canvas2d::text_baseline_of(baseline));
    });
}

/// Canvas 2D `ctx.direction = "ltr" | "rtl" | "inherit"`.  Maps the
/// JS string through a compact u8 so the op signature stays in the
/// fast-call lane; unknown values fall back to `Inherit`, matching
/// browser behaviour of ignoring unsupported directions.
#[op2(fast)]
pub fn op_set_text_direction(state: &mut OpState, #[smi] canvas_id: u32, #[smi] direction: u8) {
    with_collector(state, |collector| {
        collector.push(
            canvas_id,
            Canvas2DCmd::SetTextDirection {
                direction: frame_decode::canvas2d::text_direction_of(direction),
            },
        );
    });
}

// Transform operations
#[op2(fast)]
pub fn op_translate(state: &mut OpState, #[smi] canvas_id: u32, x: f32, y: f32) {
    queue_canvas2d(state, canvas_id, Canvas2DCmd::Translate { x, y });
}

#[op2(fast)]
pub fn op_rotate(state: &mut OpState, #[smi] canvas_id: u32, angle: f32) {
    queue_canvas2d(state, canvas_id, Canvas2DCmd::Rotate { angle });
}

#[op2(fast)]
pub fn op_scale(state: &mut OpState, #[smi] canvas_id: u32, x: f32, y: f32) {
    queue_canvas2d(state, canvas_id, Canvas2DCmd::Scale { x, y });
}

#[op2(fast)]
#[allow(clippy::too_many_arguments)]
pub fn op_set_transform(
    state: &mut OpState,
    #[smi] canvas_id: u32,
    a: f32,
    b: f32,
    c: f32,
    d: f32,
    e: f32,
    f: f32,
) {
    queue_canvas2d(
        state,
        canvas_id,
        Canvas2DCmd::SetTransform { a, b, c, d, e, f },
    );
}

// Image operations
#[op2(fast)]
#[allow(clippy::too_many_arguments)]
pub fn op_draw_image(
    state: &mut OpState,
    #[smi] canvas_id: u32,
    #[smi] image_id: u32,
    sx: f32,
    sy: f32,
    sw: f32,
    sh: f32,
    dx: f32,
    dy: f32,
    dw: f32,
    dh: f32,
) {
    queue_canvas2d(
        state,
        canvas_id,
        Canvas2DCmd::DrawImage {
            image_id,
            sx,
            sy,
            sw,
            sh,
            dx,
            dy,
            dw,
            dh,
        },
    );
}

#[op2(fast)]
pub fn op_draw_image_batch(state: &mut OpState, #[smi] canvas_id: u32, #[buffer] data: &[u8]) {
    use shared::protocol::render_cmd::DrawImageEntry;

    const ENTRY_SIZE: usize = 9 * 4;

    if data.len() % ENTRY_SIZE != 0 {
        error!("op_draw_image_batch: invalid buffer size");
        return;
    }

    let entry_count = data.len() / ENTRY_SIZE;
    if entry_count == 0 {
        return;
    }
    if entry_count > MAX_DRAW_IMAGE_BATCH_ENTRIES {
        error!("op_draw_image_batch: entry count exceeds {MAX_DRAW_IMAGE_BATCH_ENTRIES}");
        return;
    }

    let mut draws = Vec::new();
    if draws.try_reserve_exact(entry_count).is_err() {
        error!("op_draw_image_batch: allocation failed for {entry_count} entries");
        return;
    }

    for i in 0..entry_count {
        let offset = i * ENTRY_SIZE;
        let floats: &[f32] = bytemuck::cast_slice(&data[offset..offset + ENTRY_SIZE]);

        draws.push(DrawImageEntry {
            image_id: floats[0] as u32,
            sx: floats[1],
            sy: floats[2],
            sw: floats[3],
            sh: floats[4],
            dx: floats[5],
            dy: floats[6],
            dw: floats[7],
            dh: floats[8],
        });
    }

    queue_canvas2d(state, canvas_id, Canvas2DCmd::DrawImageBatch { draws });
}

// Tests for the unified frame collector are in frame_collector.rs.

#[cfg(test)]
mod tests {
    use super::super::font::{OP_GET_TEXT_LINE_HEIGHT, OP_LOAD_FONT};
    use super::{OP_FORCE_READBACK_SNAPSHOT, OP_GET_IMAGE_DATA, OP_MEASURE_TEXT};
    use shared::protocol::{SyncOpClass, class_for_op};

    /// **A sync op's deadline is chosen by a substring match on its name, so
    /// renaming the name constant is a 2500x behaviour change that nothing else
    /// would catch.**
    ///
    /// `shared::protocol` gives measure-class ops 4 ms and everything else
    /// 10 s. The only thing keeping `measureText` — which auto-sizing UI calls
    /// hundreds of times per frame — on the short deadline is that its name
    /// still contains `measure_text`. Rewriting the constant as
    /// `"canvas2d measureText"` reads like a tidy-up and silently moves it onto
    /// the 10 s deadline, where a wedged render thread freezes the whole tick
    /// instead of falling back to a JS-side estimate within a sub-frame.
    ///
    /// The test has to live here, not next to `class_for_op`: `shared` is
    /// upstream of this crate and cannot see these constants. A first version
    /// did live there, with the strings copied in — and it passed unchanged
    /// through exactly the rename above, because the copies moved with nothing.
    /// Importing the real constants is the whole point.
    /// The glyph-count fallback must produce exactly what the string-based one
    /// did, or the change traded an allocation for different layout numbers.
    ///
    /// `fallback_text_metrics` now delegates, so this compares the delegating
    /// form against the count taken independently — including the cases where
    /// `chars().count()` is not `len()`: CJK, combining marks, emoji.
    #[test]
    fn the_glyph_count_fallback_matches_the_string_one() {
        for text in [
            "",
            "a",
            "Hello, world",
            "价格：￥1,280",
            "e\u{301}cole", // combining acute
            "🎮🕹️",
            "The quick brown fox jumps over the lazy dog",
        ] {
            for size in [0.0f32, 10.0, 16.0, 64.0, f32::NAN] {
                let from_string = super::fallback_text_metrics(text, size);
                let from_count =
                    super::fallback_text_metrics_for_glyph_count(text.chars().count(), size);
                assert_eq!(
                    from_string.width, from_count.width,
                    "width diverged for {text:?} at size {size}"
                );
                assert_eq!(
                    from_string.actual_bounding_box_ascent, from_count.actual_bounding_box_ascent,
                    "ascent diverged for {text:?} at size {size}"
                );
            }
        }
    }

    /// **Estimating a fallback must not allocate**, which it did: the measure
    /// ops cloned the whole label on every call — success included — so a
    /// `usize` would be available in the timeout branch.
    ///
    /// The clone is gone, and the gate is on the arithmetic rather than the op,
    /// because driving `op_measure_text` needs a `deno_core` runtime and a
    /// render thread. What that leaves uncovered is stated rather than implied:
    /// this proves the estimate itself is allocation-free, and that the ops no
    /// longer clone is visible in their source — `glyphs` is a `usize`, and the
    /// `String` moves into the command.
    #[test]
    fn estimating_fallback_metrics_never_reaches_the_heap() {
        // Warm the formatting/paths the burst will touch.
        let text = "价格：￥1,280 — a label a shop UI re-measures every frame";
        let glyphs = text.chars().count();
        let _ = super::fallback_text_metrics_for_glyph_count(glyphs, 16.0);

        migo_alloc_probe::assert_no_steady_state_allocation(
            migo_alloc_probe::Burst {
                path: "canvas2d: measureText fallback estimate",
                warmup: 4,
                measured: 64,
            },
            |iteration| {
                // Counting is part of the op's work, so it is inside the burst:
                // `chars().count()` must not allocate either.
                let glyphs = text.chars().count();
                let m =
                    super::fallback_text_metrics_for_glyph_count(glyphs, 16.0 + iteration as f32);
                m.width.to_bits()
            },
        );
    }

    /// **Every op that reaches the frame collector must do it through the
    /// funnel, or it silently loses the byte-budget guard.**
    ///
    /// Two shapes coexisted before, each missing half of what the funnel does.
    /// Eighteen hand-written ops borrowed `OpState` once and never checked the
    /// budget, so a JS turn issuing `fillRect` in a loop accumulated pending
    /// commands with nothing to stop it — 74,899 of them reach the 4 MiB
    /// ceiling, which `frame_collector::a_run_of_plain_rect_draws_reaches_the_
    /// auto_flush_budget` measures. Eight macro-generated ops did check, by
    /// borrowing a second time.
    ///
    /// `with_collector` reads the budget on the borrow the mutation already
    /// holds, so the check costs nothing and no op has a reason to skip it. What
    /// keeps a future op from reintroducing either shape is this test: reaching
    /// the collector directly is what it looks for.
    ///
    /// Source-level, and deliberately so. Driving a real op needs a `deno_core`
    /// runtime with a collector installed; what is in doubt is not whether the
    /// collector works — that is tested next door — but whether every op still
    /// routes through it.
    #[test]
    fn every_canvas2d_op_reaches_the_collector_through_the_funnel() {
        const SRC: &str = include_str!("context2d.rs");
        // Production code only. The scan is for string literals that this test
        // itself contains, so including the test module counts them twice —
        // which the first run of this test duly reported.
        //
        // The marker is the module declaration, not a bare `#[cfg(test)]`: the
        // file also has a `#[cfg(test)]` *function*, and matching that instead
        // truncated the scan to the first few hundred lines and produced counts
        // that looked like a regression. Found by adding that function.
        const TEST_MODULE: &str = "#[cfg(test)]\nmod tests {";
        let boundary = SRC
            .find(TEST_MODULE)
            .expect("the test module this test lives in");
        let production = &SRC[..boundary];

        // The funnel and the frame-end packet builder are the only places
        // allowed to borrow the collector directly. Everything else goes through
        // `with_collector` / `queue_canvas2d`.
        const ALLOWED_DIRECT_BORROWS: usize = 2;
        let direct = production
            .matches(
                "try_borrow_mut::<crate::rendering::webgl::frame_collector::UnifiedFrameCollector>",
            )
            .count();
        assert_eq!(
            direct, ALLOWED_DIRECT_BORROWS,
            "there are {direct} direct collector borrows, expected \
             {ALLOWED_DIRECT_BORROWS} (`with_collector` itself and \
             `do_frame_end_unified`). A new one means an op is pushing outside \
             the funnel, and it has no byte-budget check"
        );

        // And `maybe_auto_flush` is reached from exactly one place: inside the
        // funnel, gated on the budget read it already made. A second call site
        // is an op paying the extra borrow again.
        const ALLOWED_FLUSH_CALLS: usize = 1;
        let flushes = production
            .matches("webgl::maybe_auto_flush(state);")
            .count();
        assert_eq!(
            flushes, ALLOWED_FLUSH_CALLS,
            "`maybe_auto_flush` has {flushes} call sites, expected \
             {ALLOWED_FLUSH_CALLS} (inside `with_collector`). An op calling it \
             directly pays a second `OpState` borrow — a BTreeMap descent — on \
             every call, which is what the funnel removed"
        );
    }

    /// **What a second `OpState` borrow costs, before deciding whether to
    /// remove one.**
    ///
    /// `batched_op!` reaches `OpState` twice per Canvas2D op: once through
    /// `try_borrow_mut` to push the command, then again inside
    /// `webgl::maybe_auto_flush`, which re-borrows only to read
    /// `should_auto_flush()` — a field comparison it could have made on the
    /// borrow just released. The scalar GL path already avoids this:
    /// `queue_gl_fire_and_forget` checks the budget on the borrow it holds and
    /// pays the second one only when it trips.
    ///
    /// Whether that matters depends entirely on what a borrow costs, and
    /// `OpState`'s store is `deno_core`'s `GothamState`, which is a
    /// **`BTreeMap<TypeId, Box<dyn Any>>`** — not a hash map. So a borrow is an
    /// O(log n) tree descent comparing 128-bit `TypeId`s through pointer-chased
    /// nodes, over the ~12-14 types this runtime installs. That is a different
    /// order from a field read, but "different order" is a guess until measured.
    ///
    /// Direction only, so the assertion stays machine independent.
    #[test]
    #[ignore = "timing benchmark; run with --ignored"]
    fn bench_second_op_state_borrow_per_canvas2d_op() {
        use std::any::{Any, TypeId};
        use std::collections::BTreeMap;
        use std::time::Instant;

        // Stand-in for `GothamState`: same container, same key type, sized to
        // the number of types this extension installs.
        struct T0(u64);
        struct T1(u64);
        struct T2(u64);
        struct T3(u64);
        struct T4(u64);
        struct T5(u64);
        struct T6(u64);
        struct T7(u64);
        struct T8(u64);
        struct T9(u64);
        struct T10(u64);
        struct T11(u64);
        struct Collector(u64);

        let mut store: BTreeMap<TypeId, Box<dyn Any>> = BTreeMap::new();
        macro_rules! install {
            ($($t:ty),*) => { $( store.insert(TypeId::of::<$t>(), Box::new(<$t>::new())); )* };
        }
        macro_rules! impl_new {
            ($($t:ident),*) => { $( impl $t { fn new() -> Self { Self(0) } } )* };
        }
        impl_new!(T0, T1, T2, T3, T4, T5, T6, T7, T8, T9, T10, T11, Collector);
        install!(T0, T1, T2, T3, T4, T5, T6, T7, T8, T9, T10, T11, Collector);

        #[inline(never)]
        fn borrow_collector(store: &BTreeMap<TypeId, Box<dyn Any>>) -> Option<&Collector> {
            store
                .get(&TypeId::of::<Collector>())
                .and_then(|b| b.downcast_ref())
        }

        // 300 Canvas2D ops in a frame is a text-heavy UI; 200 frames of them.
        const OPS_PER_FRAME: usize = 300;
        const FRAMES: usize = 200;
        let mut sink = 0u64;

        // Warm.
        for _ in 0..OPS_PER_FRAME {
            sink += borrow_collector(&store).map_or(0, |c| c.0);
        }

        let t0 = Instant::now();
        for _ in 0..FRAMES {
            for _ in 0..OPS_PER_FRAME {
                // Current shape: push through one borrow, then re-borrow to read
                // the budget.
                sink += borrow_collector(&store).map_or(0, |c| c.0);
                sink += borrow_collector(&store).map_or(0, |c| c.0);
            }
        }
        let two_borrows = t0.elapsed();

        let t1 = Instant::now();
        for _ in 0..FRAMES {
            for _ in 0..OPS_PER_FRAME {
                // Proposed shape: one borrow, budget read on the value in hand.
                let over = borrow_collector(&store).map_or(false, |c| {
                    sink += c.0;
                    c.0 > u64::MAX / 2
                });
                if over {
                    sink += borrow_collector(&store).map_or(0, |c| c.0);
                }
            }
        }
        let one_borrow = t1.elapsed();

        let ops = (FRAMES * OPS_PER_FRAME) as f64;
        println!(
            "  two borrows (now)      : {:>9} ns  {:>6.2} ns/op  {:>6.2} us/frame",
            two_borrows.as_nanos(),
            two_borrows.as_nanos() as f64 / ops,
            two_borrows.as_nanos() as f64 / FRAMES as f64 / 1000.0
        );
        println!(
            "  one borrow  (proposed) : {:>9} ns  {:>6.2} ns/op  {:>6.2} us/frame",
            one_borrow.as_nanos(),
            one_borrow.as_nanos() as f64 / ops,
            one_borrow.as_nanos() as f64 / FRAMES as f64 / 1000.0
        );
        println!("  (sink {sink})");

        assert!(
            one_borrow < two_borrows,
            "dropping the second OpState borrow ({one_borrow:?}) is not faster \
             than keeping it ({two_borrows:?}), so the `batched_op!` change \
             below is not worth its risk"
        );
    }

    #[test]
    fn each_sync_op_name_still_selects_the_deadline_its_op_needs() {
        // Per-frame ops. A stall must surface as a JS-side fallback inside a
        // frame, so these must stay measure-class.
        for (constant, name) in [
            ("OP_MEASURE_TEXT", OP_MEASURE_TEXT),
            ("OP_GET_TEXT_LINE_HEIGHT", OP_GET_TEXT_LINE_HEIGHT),
        ] {
            assert_eq!(
                class_for_op(name),
                SyncOpClass::Measure,
                "{constant} = {name:?} now classifies as {:?}. It is called \
                 hundreds of times per frame; on any other class a render-thread \
                 stall freezes the tick for 10 s instead of ~4 ms",
                class_for_op(name)
            );
        }

        // Readback ops. Legitimately slow, and rare — the long deadline is
        // correct for them, so a rename *into* measure-class would be the bug.
        for (constant, name) in [
            ("OP_GET_IMAGE_DATA", OP_GET_IMAGE_DATA),
            ("OP_FORCE_READBACK_SNAPSHOT", OP_FORCE_READBACK_SNAPSHOT),
            ("OP_LOAD_FONT", OP_LOAD_FONT),
        ] {
            assert_ne!(
                class_for_op(name),
                SyncOpClass::Measure,
                "{constant} = {name:?} is now measure-class, so a full-screen \
                 readback has 4 ms to complete and will spuriously return empty \
                 pixels"
            );
        }
    }
}
