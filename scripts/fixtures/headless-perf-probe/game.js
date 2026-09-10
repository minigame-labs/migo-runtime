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
const FRAMES = 600;
let frames = 0;

function step() {
  frames += 1;
  if (frames >= FRAMES) {
    migo.exitMiniProgram();
    return;
  }
  requestAnimationFrame(step);
}

requestAnimationFrame(step);
