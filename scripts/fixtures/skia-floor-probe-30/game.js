// Does the per-context Skia resource-cache floor's overshoot actually cost
// PSS?
//
// `backend/gl/surface.rs` caps each Canvas2DContext's Ganesh cache at
// max(aggregate / live_contexts, MIN_PER_CTX_BYTES), and the 4 MiB floor
// outranks the aggregate past a known context count. This is the "static
// shop-UI" regime: N offscreen canvases, each redrawn every frame with text
// that never changes -- the ~30-label UI `canvas_id_set`'s inline capacity was
// sized for. 30 is just past TierA's 24-context crossover, so the floor is
// already in control of the per-context cap here, not the aggregate share.
//
// No pixel assertion: this fixture is read by PSS sampling
// (scripts/measure-skia-floor-pss.sh), not by a screencap. The main canvas
// still paints a heartbeat so a screenshot can confirm liveness by eye.
const N = 30;

const canvas = migo.createCanvas();
const ctx = canvas.getContext('2d');

const offscreens = [];
for (let i = 0; i < N; i++) {
  const c = migo.createCanvas();
  c.width = 128;
  c.height = 64;
  offscreens.push({ c, octx: c.getContext('2d') });
}

let frame = 0;

function paint() {
  for (let i = 0; i < offscreens.length; i++) {
    const { c, octx } = offscreens[i];
    octx.clearRect(0, 0, c.width, c.height);
    octx.fillStyle = '#000000';
    octx.fillText('Label ' + i, 4, 24);
  }

  ctx.fillStyle = (frame % 60 < 30) ? '#20c060' : '#1f8f4c';
  ctx.fillRect(0, 0, canvas.width, canvas.height);

  frame += 1;
  if (frame % 60 === 0) {
    console.error('[skia-floor-probe-30] frame ' + frame + ', ' + offscreens.length + ' offscreen canvases painted');
  }
  requestAnimationFrame(paint);
}
requestAnimationFrame(paint);
