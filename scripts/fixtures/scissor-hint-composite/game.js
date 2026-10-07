// Test half: identical to
// scissor-hint-baseline, except once per frame -- midway through the
// batch -- globalCompositeOperation is set to a non-source-over mode and
// immediately set back to 'source-over' before the next draw.
//
// The resulting *state* at draw time is ordinary source-over, so
// render_thread's state_allows_partial would allow a partial update on its
// own. What this isolates is frame_collector::mark_dirty_for_cmd's own
// poisoning: `Canvas2DCmd::SetCompositeOperation { .. }` matches on the call
// itself, not on the value it lands on, so the mere event -- even one that
// nets out to a no-op -- widens the segment's scissor hint to the full
// canvas for the rest of the frame. Before this change (the catch-all arm
// that did nothing) this exact sequence would have kept the tight,
// cluster-sized scissor; after it, a full-canvas repaint.
//
// Compare render-thread CPU% against scissor-hint-baseline
// (scripts/measure-scissor-hint.sh).
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
    if (i === RECTS >> 1) {
      // Nets out to a no-op state-wise; poisons the scissor hint anyway.
      ctx.globalCompositeOperation = 'xor';
      ctx.globalCompositeOperation = 'source-over';
    }
    const x = CLUSTER_X + (next() % (CLUSTER_SIZE - RECT_SIZE));
    const y = CLUSTER_Y + (next() % (CLUSTER_SIZE - RECT_SIZE));
    ctx.fillStyle = 'rgb(' + (next() % 256) + ',' + (next() % 256) + ',' + (next() % 256) + ')';
    ctx.fillRect(x, y, RECT_SIZE, RECT_SIZE);
  }

  frame += 1;
  if (frame === 2 || frame % 60 === 0) {
    console.error('[scissor-hint-composite] painted ' + frame + ' frames, ' + RECTS + ' rects/frame, one set-and-reset composite-mode touch');
  }
  requestAnimationFrame(paint);
}
requestAnimationFrame(paint);
