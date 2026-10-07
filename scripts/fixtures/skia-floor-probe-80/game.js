// Same as skia-floor-probe-30 but at 80 offscreen contexts -- the count
// render_thread's own reorder fixture uses, on the record that "nothing bounds
// how many canvases a game draws to in one frame". 80 contexts * 4 MiB floor =
// 320 MiB, 3.3x TierA's 96 MiB aggregate ceiling, if the overshoot materialises
// at all (it is a ceiling, not a reservation -- see the script this fixture is
// read by).
//
// Also used by scripts/measure-skia-floor-cpu.sh to answer the other half:
// does the floor earn its keep, by comparing render-thread CPU% against a build
// with MIN_PER_CTX_BYTES forced to 0?
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
    console.error('[skia-floor-probe-80] frame ' + frame + ', ' + offscreens.length + ' offscreen canvases painted');
  }
  requestAnimationFrame(paint);
}
requestAnimationFrame(paint);
