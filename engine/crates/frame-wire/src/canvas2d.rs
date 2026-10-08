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
//! The ranges are load-bearing, not cosmetic. GL owns `1..=73` fixed and
//! `256..=276` variable; 2D owns `512..`. A reader classifies a record by its
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
// Everything above is numbers. Text is the block's first payload: a font's
// family names and a string to draw, which is why the two payload record
// shapes the resource block introduced are used here rather than restated.

/// A font, as the facade read `ctx.font`: `H size:F weight slants byte_length |
/// utf8`, the bytes the family names joined by NUL.
///
/// Not the shorthand. The facade is its only reader -- `ctx.font =` answers at
/// once whether the string was a font, and `font` reads back its serialisation
/// -- so what crosses is what it read: the size in CSS pixels, the weight
/// (1..=1000), whether the face slants (0 or 1), and the families. The host
/// checks those fields (`CanvasFont::from_parts`) because a record is not
/// trusted for being well-formed, and parses nothing.
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

// ─── Gradients and patterns (562..=565) ──────────────────────────────────────
//
// The two styles a colour cannot express. Both are set rarely -- once per style
// change, not per draw -- and both name something the host already holds or can
// build from what the record carries.

/// `fillStyle = gradient`: `H type x0 y0 r0 x1 y1 r1 byte_length | utf8`.
///
/// `type` is 0 linear, 1 radial, 2 conic, as the op takes it. The six floats are
/// the two circles the facade's `CanvasGradient` was built from, in the op's
/// order; a linear gradient carries zero radii and a conic one carries its start
/// angle where `x1` is, both of which the facade already decided.
///
/// The payload is the stops as the facade serialises them:
/// `JSON.stringify([{offset, r, g, b, a}, ...])`, the same string the in-process
/// op receives, read by the same
/// `shared::protocol::render_cmd::parse_gradient_stops` on both lanes. Text
/// rather than words deliberately -- it makes the two executions parse one
/// thing, on the host, instead of holding a producer-side port to a corpus.
pub const OP2D_SET_FILL_STYLE_GRADIENT: u32 = 562;
/// `strokeStyle = gradient`, the same shape.
pub const OP2D_SET_STROKE_STYLE_GRADIENT: u32 = 563;

/// `fillStyle = pattern`: `H image_id repeat_x:B repeat_y:B`.
///
/// The image is the host's already -- loaded and uploaded under its shared id,
/// as `drawImage` names one -- so no pixel crosses. The two bools are the
/// repetition the facade resolved: `repeat-x` is x without y, `no-repeat` is
/// neither.
pub const OP2D_SET_FILL_STYLE_PATTERN: u32 = 564;
/// `strokeStyle = pattern`, the same shape.
pub const OP2D_SET_STROKE_STYLE_PATTERN: u32 = 565;

// ─── Snapshots (566) ─────────────────────────────────────────────────────────

/// Capture a rectangle of the selected canvas into the host's snapshot pool:
/// `H x:I y:I width:U height:U snapshot_id:U`.
///
/// `getImageData` is this in both executions: the engine's 2D facade captures
/// rather than reading back, because the pixels usually go straight into a
/// texture and never need to cross to JavaScript at all. The id is the engine's
/// own counter's, allocated in the same JavaScript on both lanes, and a capture
/// with id 0 or a rectangle past the surface cap is one the op drops before it
/// queues anything -- so the producer drops it too, rather than writing a record
/// the host would refuse.
///
/// The cache-keyed form (`op_capture_canvas2d_snapshot_for_cache`) is not here:
/// it carries a text, a font and a colour, and the text-texture cache it feeds
/// is a piece of its own.
pub const OP2D_CAPTURE_SNAPSHOT: u32 = 566;

// ─── Image smoothing (567) ───────────────────────────────────────────────────

/// `imageSmoothingEnabled = b`: `H enabled:B`, where `enabled` is exactly 0 or 1.
///
/// It is drawing state like `globalAlpha`, not an argument of `drawImage`: the
/// specification saves it with `save()` and gives it back with `restore()`, and
/// a game sets it once for a whole pixel-art scene. Numbered after the snapshot
/// op rather than placed beside the other state setters because 543..548 are
/// taken and renumbering a block that shipped is what this block's ranges exist
/// to avoid.
///
/// Until this record existed the property was never sent anywhere: assigning
/// `imageSmoothingEnabled = false` left a plain property on the JavaScript
/// object and every scaled `drawImage` was sampled bilinearly, which blurs pixel
/// art on every platform.
pub const OP2D_SET_IMAGE_SMOOTHING: u32 = 567;

// ─── Fill rule (568, 569) ────────────────────────────────────────────────────

/// `fill("evenodd")`: the path as `OP2D_FILL` paints it, under the even-odd rule.
///
/// `OP2D_FILL` is the nonzero rule, the specification's default. The rule is a
/// second record rather than a word added to `OP2D_FILL` because that record is
/// nullary and shipped so; adding a word would change what every existing
/// writer and reader of it means. Until these existed the rule argument was
/// dropped on the floor and every fill was nonzero, which turns the holes of
/// an even-odd icon solid.
pub const OP2D_FILL_EVEN_ODD: u32 = 568;
/// `clip("evenodd")`: `OP2D_CLIP` under the even-odd rule. See [`OP2D_FILL_EVEN_ODD`].
pub const OP2D_CLIP_EVEN_ODD: u32 = 569;

// ─── Canvas as an image source (570) ─────────────────────────────────────────

/// `drawImage(canvas, sx, sy, sw, sh, dx, dy, dw, dh)`:
/// `H source_canvas:U sx sy sw sh dx dy dw dh:F`.
///
/// `OP2D_DRAW_IMAGE` names an image the host decoded; this names another canvas,
/// whose pixels the renderer reads when the record runs -- in stream order, so
/// the source has drawn everything the content drew to it before this call.
/// The facade has expanded the shorter forms, as for `OP2D_DRAW_IMAGE`. The
/// source may be the destination itself.
///
/// Until this record existed `drawImage` silently drew nothing for a canvas:
/// the facade only knew images the host had decoded.
pub const OP2D_DRAW_CANVAS: u32 = 570;

// ─── Pixels written to the canvas (571) ──────────────────────────────────────

/// `putImageData`: `H x:I y:I width:U height:U byte_length | rgba`, the pixels as `ImageData.data` has them -- RGBA8, not
/// premultiplied, rows top to bottom with no padding -- and `byte_length` exactly `width * height * 4`.
///
/// The record replaces the pixels of the rectangle it names: not drawn through the transform, the clip, `globalAlpha`,
/// the composite operation or the shadow, which is the one place the specification says a canvas call ignores its drawing
/// state. A producer splits a large `ImageData` into bands of rows, each its own record with its own `y`, so a record
/// stays well inside a packet (`MAX_TOTAL_BYTES`) and a band is the same write as the whole would have been.
///
/// Until this record existed `putImageData` was an empty function: p5.js's `updatePixels`, EaselJS's filters and every
/// game that edits pixels on the CPU changed nothing.
pub const OP2D_PUT_IMAGE_DATA: u32 = 571;

/// The most pixel bytes a producer should put in one `OP2D_PUT_IMAGE_DATA`: a band of rows is cut to fit it.
pub const PUT_IMAGE_DATA_BAND_BYTES: u32 = 1024 * 1024;

// ─── A canvas kept as an image (572) ─────────────────────────────────────────

/// `createPattern(canvas, ...)`'s copy of the canvas: `H image_id:U`, under the selection of the canvas it copies.
///
/// A pattern is made from the canvas as it is when `createPattern` is called, and what the canvas draws afterwards
/// does not reach it. The record takes a copy of the selected canvas's pixels -- in stream order, so the canvas has
/// drawn everything the content drew to it before the call -- and registers it in the renderer's image store under
/// `image_id`, which the producer allocated (`op_create_image`) and later destroys (`op_destroy_image`) when the pattern
/// is collected. The pattern then names the id like one of an image the host decoded
/// (`OP2D_SET_FILL_STYLE_PATTERN`). A canvas that cannot be copied registers nothing, and the pattern paints nothing.
///
/// Until this record existed `createPattern` returned null for a canvas, so every pattern fill from an offscreen tile
/// (the way a 2D game builds a tiled background) was lost.
pub const OP2D_CAPTURE_IMAGE: u32 = 572;

// ─── Rounded rectangles and paths as values (573..576) ────────────────────────

/// `roundRect(x, y, w, h, radii)`: `H x y w h:F`, then the radii of the four corners as `rx ry:F` each -- top left, top
/// right, bottom right, bottom left. The facade has turned `radii` (a number, a `DOMPointInit`, or a list of one to four
/// of them) into the four corners the specification assigns them to and refused what it refuses; the renderer scales
/// radii that would overlap, flips the corners of a negative width or height, and starts a new subpath at `(x, y)`.
pub const OP2D_ROUND_RECT: u32 = 573;

/// `fill(path, fillRule)` with a `Path2D`: `H rule:U count path...` -- `rule` 0 for nonzero and 1 for even-odd, then the
/// path's segments ([`path2d`]).
///
/// A `Path2D` lives in the facade, as the segments content added to it, and travels with each use: its coordinates are
/// its own, it is drawn through the transform current at this record, and the current default path is not touched. A
/// path content keeps and draws every frame costs its segments every frame, which a `Path2D` -- a shape, an icon -- has
/// few of; the alternative, paths the renderer holds by id, would need a lifetime the renderer learns of from a
/// collector it cannot see.
pub const OP2D_FILL_PATH: u32 = 574;
/// `stroke(path)` with a `Path2D`: `H count path...`. See [`OP2D_FILL_PATH`].
pub const OP2D_STROKE_PATH: u32 = 575;
/// `clip(path, fillRule)` with a `Path2D`: `H rule:U count path...`. See [`OP2D_FILL_PATH`].
pub const OP2D_CLIP_PATH: u32 = 576;

/// `reset()`: `H`. The context's default state again -- the bitmap transparent black, the state stack empty, every
/// attribute and the transform at its default, the current path empty -- without the surface being made again, which
/// is what assigning the canvas's size does.
pub const OP2D_RESET: u32 = 577;

/// One past the last 2D opcode in this block.
pub const OP2D_END: u32 = 578;

/// The most words of segments a path carried by value may hold: some 50,000 cubic curves, far above a shape anybody
/// keeps in a `Path2D` and far below a record that would cost a frame anything.
pub const MAX_PATH_WORDS: u32 = 1 << 18;

/// The segments of a path carried by value (`OP2D_FILL_PATH`, `OP2D_STROKE_PATH`, `OP2D_CLIP_PATH`, and the hit test's
/// path, `sync::Canvas2DHitTestParams`): each an opcode word followed by its arguments, floats as `f32` bits and flags as
/// 0 or 1. They are the `CanvasPath` calls content made on the `Path2D`, in order and with the same arguments, so the
/// renderer builds the path with the code that builds the current default path, and the two cannot differ.
pub mod path2d {
    /// `x y`
    pub const MOVE_TO: u32 = 1;
    /// `x y`
    pub const LINE_TO: u32 = 2;
    /// `cpx cpy x y`
    pub const QUADRATIC_CURVE_TO: u32 = 3;
    /// `cp1x cp1y cp2x cp2y x y`
    pub const BEZIER_CURVE_TO: u32 = 4;
    /// `x y radius startAngle endAngle counterclockwise:0|1`
    pub const ARC: u32 = 5;
    /// `x1 y1 x2 y2 radius`
    pub const ARC_TO: u32 = 6;
    /// `x y radiusX radiusY rotation startAngle endAngle counterclockwise:0|1`
    pub const ELLIPSE: u32 = 7;
    /// `x y w h`
    pub const RECT: u32 = 8;
    /// `x y w h`, then `rx ry` for each corner as in `OP2D_ROUND_RECT`
    pub const ROUND_RECT: u32 = 9;
    pub const CLOSE_PATH: u32 = 10;
    /// An SVG path's elliptical arc, `A`: `rx ry xAxisRotation(degrees) largeArc:0|1 sweep:0|1 x y`. Not a `CanvasPath`
    /// call -- the one segment of a path string the canvas calls cannot express.
    pub const SVG_ARC_TO: u32 = 11;
    /// `addPath(path, transform)`: `a b c d e f count`, then the other path's segments, copied when `addPath` was called.
    pub const ADD_PATH: u32 = 12;

    /// How deep `ADD_PATH` may nest: a path added to a path added to a path. Deeper is a producer that built its paths
    /// out of themselves in a loop, and a reader that followed it would run out of stack.
    pub const MAX_NESTING: u32 = 32;

    /// The arguments an opcode takes, and which of them are flags (0 or 1) rather than floats. `ADD_PATH` is not here:
    /// it carries a count and a nested path.
    pub fn arguments(op: u32) -> Option<(usize, &'static [usize])> {
        Some(match op {
            MOVE_TO | LINE_TO => (2, &[]),
            QUADRATIC_CURVE_TO => (4, &[]),
            BEZIER_CURVE_TO => (6, &[]),
            ARC => (6, &[5]),
            ARC_TO => (5, &[]),
            ELLIPSE => (8, &[7]),
            RECT => (4, &[]),
            ROUND_RECT => (12, &[]),
            CLOSE_PATH => (0, &[]),
            SVG_ARC_TO => (7, &[3, 4]),
            _ => return None,
        })
    }

    /// Whether `words` are segments a reader can build a path from: known opcodes with all their arguments, flags that
    /// are 0 or 1, finite floats -- the facade drops a call with a non-finite argument, as the specification has it, so
    /// one here is a producer bug -- and `ADD_PATH`s whose counts and nesting hold.
    pub fn is_valid(words: &[u32]) -> bool {
        valid_at(words, 0)
    }

    fn valid_at(words: &[u32], depth: u32) -> bool {
        let finite = |word: u32| f32::from_bits(word).is_finite();
        let mut at = 0;
        while at < words.len() {
            let op = words[at];
            at += 1;
            if op == ADD_PATH {
                if depth >= MAX_NESTING || words.len() < at + 7 {
                    return false;
                }
                if !words[at..at + 6].iter().all(|w| finite(*w)) {
                    return false;
                }
                let count = words[at + 6] as usize;
                at += 7;
                if words.len() - at < count || !valid_at(&words[at..at + count], depth + 1) {
                    return false;
                }
                at += count;
                continue;
            }
            let Some((arity, flags)) = arguments(op) else {
                return false;
            };
            if words.len() - at < arity {
                return false;
            }
            for (i, word) in words[at..at + arity].iter().enumerate() {
                let ok = if flags.contains(&i) {
                    *word <= 1
                } else {
                    finite(*word)
                };
                if !ok {
                    return false;
                }
            }
            at += arity;
        }
        true
    }
}

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

        OP2D_FILL | OP2D_STROKE | OP2D_CLIP | OP2D_FILL_EVEN_ODD | OP2D_CLIP_EVEN_ODD => (1, &[]),
        // x, y, w, h, then rx, ry for each of the four corners
        OP2D_ROUND_RECT => (13, &[]),
        // The segments' shape is a fact about their meaning, so the decoder checks it (`path2d::is_valid`), as it does
        // a fill rule that is not 0 or 1.
        OP2D_FILL_PATH | OP2D_CLIP_PATH => {
            return Some(RecordSpec::Words {
                prefix_words: 2,
                max_count: MAX_PATH_WORDS,
            });
        }
        OP2D_STROKE_PATH => {
            return Some(RecordSpec::Words {
                prefix_words: 1,
                max_count: MAX_PATH_WORDS,
            });
        }

        OP2D_FILL_RECT | OP2D_STROKE_RECT | OP2D_CLEAR_RECT => (5, &[]),

        OP2D_SAVE | OP2D_RESTORE | OP2D_RESET_TRANSFORM | OP2D_RESET => (1, &[]),
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

        // enabled: a boolean, not a number a producer may round -- 2 is a bug
        OP2D_SET_IMAGE_SMOOTHING => (2, &[1]),

        // image_id, repeat_x, repeat_y
        OP2D_SET_FILL_STYLE_PATTERN | OP2D_SET_STROKE_STYLE_PATTERN => (4, &[2, 3]),

        OP2D_DRAW_IMAGE | OP2D_DRAW_CANVAS => (10, &[]),
        // x, y, width, height, snapshot_id
        OP2D_CAPTURE_SNAPSHOT => (6, &[]),
        // image_id
        OP2D_CAPTURE_IMAGE => (2, &[]),
        OP2D_DRAW_IMAGE_BATCH => {
            return Some(RecordSpec::Words {
                prefix_words: 1,
                max_count: MAX_DRAW_IMAGE_BATCH_ENTRIES * DRAW_IMAGE_BATCH_ENTRY_WORDS,
            });
        }

        // The payload records: their length is a word of their own rather than
        // their word count. Both shapes are the ones the resource block
        // introduced; see `RecordSpec::Bytes` and `Words`.
        // size, weight, slants | x, y, max_width; then the text
        OP2D_SET_FONT | OP2D_FILL_TEXT | OP2D_STROKE_TEXT => {
            return Some(RecordSpec::Bytes {
                prefix_words: 4,
                presence_word: None,
                text: true,
                stageable: false,
            });
        }
        // x, y, width, height, then the pixels. That `byte_length` is `width * height * 4` is a fact about this record's
        // meaning, not its shape, so the decoder checks it where it reads the words.
        OP2D_PUT_IMAGE_DATA => {
            return Some(RecordSpec::Bytes {
                prefix_words: 5,
                presence_word: None,
                text: false,
                stageable: false,
            });
        }
        OP2D_SET_LINE_DASH => {
            return Some(RecordSpec::Words {
                prefix_words: 1,
                max_count: MAX_LINE_DASH_SEGMENTS,
            });
        }
        OP2D_SET_FILL_STYLE_GRADIENT | OP2D_SET_STROKE_STYLE_GRADIENT => {
            return Some(RecordSpec::Bytes {
                prefix_words: 8,
                presence_word: None,
                text: true,
                stageable: false,
            });
        }

        _ => return None,
    };
    Some(RecordSpec::Fixed {
        word_count,
        bool_words,
    })
}
