// Control: zero offscreen
// canvases, same heartbeat as skia-floor-probe-N. Isolates the process's
// fixed PSS/Graphics overhead so the N=30/80 runs can be read as a delta
// caused by context count, not as an absolute figure.
const canvas = migo.createCanvas();
const ctx = canvas.getContext('2d');

let frame = 0;

function paint() {
  ctx.fillStyle = (frame % 60 < 30) ? '#20c060' : '#1f8f4c';
  ctx.fillRect(0, 0, canvas.width, canvas.height);

  frame += 1;
  if (frame % 60 === 0) {
    console.error('[skia-floor-probe-0] frame ' + frame + ', 0 offscreen canvases painted');
  }
  requestAnimationFrame(paint);
}
requestAnimationFrame(paint);
