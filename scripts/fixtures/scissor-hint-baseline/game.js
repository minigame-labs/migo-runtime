// Baseline half: many small
// fillRects clustered in a small region every frame, all default
// source-over. The scissor hint should stay tight to the cluster (the union
// of 300 small rects in a ~250x250 region), never widening to the full
// canvas -- this is the partial-update path scissor-hint-composite's extra
// SetCompositeOperation call is expected to poison.
//
// Compare render-thread CPU% against scissor-hint-composite
// (scripts/measure-scissor-hint.sh): frame time is not the instrument here
// either, for the same vsync-ceiling reason -- 300 tiny rects is
// nowhere near expensive enough to miss 60fps on its own, composite-poisoned
// or not, so a fps reading would show nothing regardless of the true cost.
const canvas = migo.createCanvas();
const ctx = canvas.getContext('2d');

const CLUSTER_X = 40, CLUSTER_Y = 40, CLUSTER_SIZE = 250;
const RECTS = 300, RECT_SIZE = 10;

function prng(seed) {
  let x = seed;
  return () => {
    x = (x * 1103515245 + 12345) & 0x7fffffff;
    return x;
  };
}

let frame = 0;
function paint() {
  const next = prng(frame * 97 + 1);
  for (let i = 0; i < RECTS; i++) {
    const x = CLUSTER_X + (next() % (CLUSTER_SIZE - RECT_SIZE));
    const y = CLUSTER_Y + (next() % (CLUSTER_SIZE - RECT_SIZE));
    ctx.fillStyle = 'rgb(' + (next() % 256) + ',' + (next() % 256) + ',' + (next() % 256) + ')';
    ctx.fillRect(x, y, RECT_SIZE, RECT_SIZE);
  }

  frame += 1;
  if (frame === 2 || frame % 60 === 0) {
    console.error('[scissor-hint-baseline] painted ' + frame + ' frames, ' + RECTS + ' rects/frame, no composite-mode touch');
  }
  requestAnimationFrame(paint);
}
requestAnimationFrame(paint);
