// Touch probe: the whole screen is one colour, and it changes only when a touch
// arrives. Any pixel difference between two frames is therefore attributable to
// input having crossed the C ABI, the engine, and reached JS -- nothing else in
// this content changes over time.
//
// The colour encodes how many pointers are down, so a screenshot taken while a
// gesture is held is evidence of the pointer count that reached JS. That matters
// because a host has no other way to see it: engine logs need MIGO_CAPI_LOG to
// be set before the engine is created, and a pixel needs nothing at all.
const canvas = migo.createCanvas();
const ctx = canvas.getContext('2d');

const IDLE = '#c00000';       // red    -- untouched since launch
const RELEASED = '#0000c0';   // blue   -- every finger lifted
const BY_COUNT = [
  IDLE,
  '#00c000',                  // green   -- 1 pointer
  '#c000c0',                  // magenta -- 2 pointers
  '#c0c000',                  // yellow  -- 3 or more
];

let colour = IDLE;
let events = 0;

function paint() {
  ctx.fillStyle = colour;
  ctx.fillRect(0, 0, canvas.width, canvas.height);
  requestAnimationFrame(paint);
}
paint();

function describe(e) {
  const touches = (e && e.touches) || [];
  let text = 'count=' + touches.length;
  for (let i = 0; i < touches.length; i++) {
    text += ' [' + i + '] id=' + touches[i].identifier +
            ' x=' + touches[i].clientX + ' y=' + touches[i].clientY;
  }
  return text;
}

function onDown(e) {
  events++;
  const count = ((e && e.touches) || []).length;
  colour = BY_COUNT[Math.min(count, BY_COUNT.length - 1)];
  console.error('[touchprobe] start events=' + events + ' ' + describe(e));
}

migo.onTouchStart(onDown);
// A second finger landing on an existing gesture arrives as a move, not a start,
// so the colour would never reach the multi-pointer value without this.
migo.onTouchMove(onDown);
migo.onTouchEnd(function (e) {
  const remaining = ((e && e.touches) || []).length;
  colour = remaining > 0 ? BY_COUNT[Math.min(remaining, BY_COUNT.length - 1)] : RELEASED;
  console.error('[touchprobe] end ' + describe(e));
});
