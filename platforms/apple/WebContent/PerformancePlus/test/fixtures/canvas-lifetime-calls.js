// Creating, resizing and destroying canvases, through the engine's own facade.
//
// Run twice, like the fixtures beside it: in the embedded runtime, where
// `createCanvas`, `canvas.width` and the finalizer reach the Rust ops, and on
// the producer, where the same calls become records the host decodes. The
// effects of both, in order, must be equal -- see
// engine/crates/runtime-v8/src/rendering/webgl/canvas_lifetime_parity.rs.
//
// What this covers that no other fixture does: a canvas that does not exist
// until a record in the frame creates it. Every other 2D or WebGL fixture draws
// on a canvas the host already had, so "create it, then draw on it" -- the one
// ordering a stream lane has to carry itself -- was never exercised.

// The main canvas. Neither lane creates it: it is the surface, and the first
// `createCanvas()` is how content takes hold of it.
const main = createCanvas();
main.width = 320;
main.height = 240;

// Offscreen canvases. `createCanvas()` after the first is `createOffscreenCanvas(1, 1)`,
// which is how the engine's own SDK makes one: a 1x1 that content then sizes.
const label = createCanvas();
label.width = 64;
label.height = 32;

const atlas = createCanvas();
atlas.width = 128;

// Refused by both lanes at the same bound: a dimension past the surface cap
// (`MAX_CANVAS_DIMENSION`). The assignment throws, so the size never changes
// and neither lane records anything for it.
let refusal = "none";
try {
    atlas.height = 9000;
} catch (error) {
    refusal = error.message;
}
if (refusal === "none") throw new Error("a 9000px canvas dimension was not refused");
if (atlas.height !== 1) throw new Error(`a refused resize changed the height to ${atlas.height}`);

// Destroying is not here, and the reason is the facade: there is no
// `canvas.destroy()` in either runtime -- the engine calls `op_destroy_canvas`
// from a `FinalizationRegistry`, which no test can schedule, and reaching the op
// around the facade would mean the two runtimes calling it two different ways,
// which is the one thing a parity fixture must not do. The record and its
// refusals are checked where they can be checked exactly:
// test/canvas-lifetime.test.mjs for what the producer writes, and
// engine/crates/frame-decode/src/canvas2d.rs for what the host reads back.

// One more canvas, so the comparison covers a creation that follows other
// canvases' work rather than only the opening burst.
const after = createCanvas();
after.width = 16;
after.height = 16;
