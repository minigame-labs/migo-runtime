// The throughput half of the CAMetalLayer drawable-pool question.
//
// `maximumDrawableCount` is a host property precisely because Migo takes a
// CAMetalLayer rather than a view -- hand ANGLE a plain CALayer and it allocates
// its own metal layer, after which the host can no longer set it. The trade it
// controls is latency against throughput: a smaller pool makes `nextDrawable`
// block sooner, which caps frames in flight and therefore how stale a presented
// frame can be.
//
// Only one half of that is answerable without a screen. Headless there is no
// display to be late to, so what a run measures is how fast the pipeline can
// turn frames at a given pool size, and NOT the latency the setting exists to
// buy. The other half needs a device and stays open.
//
// Same shape as headless-js-probe, with a frame budget large enough to measure
// rather than to prove.
//
// IT DRAWS, and it did not until 2026-09-11, which invalidated the first set of
// numbers it produced. A pool depth buys one thing: whether a drawable is
// available while the GPU is still finishing the previous frame. A run that
// creates no canvas acquires and presents nothing, so there is nothing for the
// depth to gate, and 3-against-2 was measuring the rAF/vsync round trip between
// the host and the engine. (The surface was also 1x1 at the time, for an
// unrelated reason -- see tests/c_host/macos-headless/main.m -- so the same run
// was wrong twice.)
//
// A clear per frame is still modest GPU work, and that is stated rather than
// hidden: what this can answer is "how fast can the pipeline turn real present
// cycles at this pool depth", not "how much latency does the depth buy". The
// latency half needs a display and stays open.
const canvas = migo.createCanvas();
const gl = canvas.getContext("webgl", { antialias: false, depth: false, stencil: false });
if (!gl) {
  throw new Error("migo-perf-probe: no WebGL context; ANGLE did not come up");
}

const FRAMES = 600;
let frames = 0;

function step() {
  frames += 1;
  // A different colour every frame, so no layer of this stack can decide the
  // frame is identical to the last one and skip the present -- which would turn
  // a throughput measurement into a measurement of how well something elides.
  gl.clearColor((frames % 255) / 255, 0.25, 0.5, 1.0);
  gl.clear(gl.COLOR_BUFFER_BIT);
  if (frames >= FRAMES) {
    migo.exitMiniProgram();
    return;
  }
  requestAnimationFrame(step);
}

requestAnimationFrame(step);
