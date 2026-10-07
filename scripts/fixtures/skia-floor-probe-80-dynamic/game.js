// The "animates many of them" regime that the static-label variants
// (skia-floor-probe-30/-80) cannot exercise.
// Same 80 offscreen contexts, but every canvas's text changes every frame --
// a different pseudo-random printable-ASCII run each time, so neither the
// rasterised text blob nor (over a run) the covered glyph set stays constant.
// A static label settles into a small, mostly-idle cache footprint after its
// first draw; this is the shape that should actually approach the per-context
// cap if anything does.
const N = 80;

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

function pseudoRandomLabel(seed, len) {
  let s = '';
  let x = seed;
  for (let k = 0; k < len; k++) {
    x = (x * 1103515245 + 12345) & 0x7fffffff;
    s += String.fromCharCode(33 + (x % 90)); // printable ASCII 33..122
  }
  return s;
}

function paint() {
  for (let i = 0; i < offscreens.length; i++) {
    const { c, octx } = offscreens[i];
    octx.clearRect(0, 0, c.width, c.height);
    octx.fillStyle = '#000000';
    octx.fillText(pseudoRandomLabel(frame * 97 + i, 10), 4, 24);
  }

  ctx.fillStyle = (frame % 60 < 30) ? '#20c060' : '#1f8f4c';
  ctx.fillRect(0, 0, canvas.width, canvas.height);

  frame += 1;
  if (frame % 60 === 0) {
    console.error('[skia-floor-probe-80-dynamic] frame ' + frame + ', ' + offscreens.length + ' offscreen canvases painted');
  }
  requestAnimationFrame(paint);
}
requestAnimationFrame(paint);
