// DrawingBuffer-bypass probe: the one content shape that keeps bypass latched on.
//
// `can_bypass_drawing_buffer` needs all five of: exactly one canvas, a drawing
// buffer that is not preserved, no 2D context on the onscreen canvas, a
// DrawingBuffer the same size as the surface, and a context that asked for the
// buffers the window has. Every shipping bundle we have breaks the first one
// within a second of startup (an offscreen canvas for text or an atlas), so the
// bypass path only ever ran for a handful of warmup frames and nothing looked at
// what it presented. This fixture holds all five for the whole run, which makes
// the captured PNG a verdict on bypass rather than on the blit.
//
// Deliberately absent, because each would silently take the probe off the path
// it exists to exercise:
//   * `createOffscreenCanvas` / a second `createCanvas` -> canvas_count > 1.
//   * `getContext("2d")` on this canvas -> Skia targets the DrawingBuffer.
//   * `preserveDrawingBuffer: true` -> only the DrawingBuffer keeps its
//     contents across a swap.
//   * the default attributes -> no stencil buffer, which the window has, so
//     the window's buffers are not the ones the context asked for. Hence
//     `stencil: true`: the window's config has alpha, depth and stencil.
//   * `canvas.width = ...` -> DrawingBuffer stops matching the surface.
//   * `bindFramebuffer` of any kind -> would re-establish the binding under
//     test. Content that never binds a framebuffer is relying on whatever the
//     engine left bound, which is exactly the invariant being probed.
const canvas = migo.createCanvas();
const gl = canvas.getContext("webgl", { stencil: true });

// A colour no uninitialised buffer plausibly holds, and distinct per channel so
// a captured pixel says which channel survived: 0x33 / 0xcc / 0x66.
const R = 0.2;
const G = 0.8;
const B = 0.4;

let frames = 0;

function paint() {
  // The first frame paints a different colour on purpose. "The screen is the clear
  // colour" is satisfied by an engine that presented once and then stopped, which
  // is a real failure mode — a defect elsewhere in this file's subject matter made
  // damage tracking treat every clear as invisible and the engine presented
  // exactly one frame per run. A blue capture says frozen; green says live.
  if (frames === 0) {
    gl.clearColor(0.1, 0.3, 0.9, 1.0);
  } else {
    gl.clearColor(R, G, B, 1.0);
  }
  gl.clear(gl.COLOR_BUFFER_BIT);
  frames += 1;
  // Liveness, paired with the pixel assertion: a black capture from a run that
  // never painted is not evidence about bypass. The count says frames happened;
  // the PNG says where they went.
  if (frames === 1 || frames % 60 === 0) {
    console.error(`[bypass-probe] painted ${frames} frames, expect rgba(51,204,102,255)`);
  }
  requestAnimationFrame(paint);
}

requestAnimationFrame(paint);
