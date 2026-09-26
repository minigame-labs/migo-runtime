// Touch probe: the whole screen is one colour, and it changes only when a touch
// arrives. Any pixel difference between two frames is therefore attributable to
// input having crossed the C ABI, the engine, and reached JS -- nothing else in
// this content changes over time.
//
// The colour encodes how many pointers are down, so a screenshot taken while a
// gesture is held is evidence of the pointer count that reached JS. That matters
// because a host has no other way to see it: engine logs need MIGO_CAPI_LOG to
// be set before the engine is created, and a pixel needs nothing at all.
//
// The untouched red is not filled: it is a decoded image, and nothing is painted
// until two images have decoded. Android builds carry no Rust image decoder, so
// a host that never registered one decoded nothing -- the C host shipped that
// way and every WebGL game on it rendered black while its frame loop ran on.
// The engine decodes by one of two paths, chosen by whether any WebGL context
// exists yet, so the first image loads before one does and the second after.
// A failed decode paints FAILED instead, which the first-paint check rejects.
const canvas = migo.createCanvas();
const ctx = canvas.getContext('2d');

const RED_PNG = 'data:image/png;base64,' +
  'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAIAAACQd1PeAAAADElEQVR4nGM4wMAAAAJEAMGTjGgiAAAAAElFTkSuQmCC';
const FAILED = '#404040';     // grey   -- an image did not decode

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
let decoded = null;           // the red image, once both decodes succeeded
let failed = false;

function load(what, next) {
  const image = migo.createImage();
  image.onload = function () {
    console.error('[touchprobe] decoded ' + what + ' ' + image.width + 'x' + image.height);
    next(image);
  };
  image.onerror = function () {
    failed = true;
    console.error('[touchprobe] could not decode ' + what);
  };
  image.src = RED_PNG;
}

load('before any WebGL context', function () {
  migo.createCanvas().getContext('webgl');
  load('after a WebGL context', function (image) { decoded = image; });
});

function paint() {
  if (failed) {
    ctx.fillStyle = FAILED;
    ctx.fillRect(0, 0, canvas.width, canvas.height);
  } else if (decoded && colour === IDLE) {
    ctx.drawImage(decoded, 0, 0, canvas.width, canvas.height);
  } else if (decoded) {
    ctx.fillStyle = colour;
    ctx.fillRect(0, 0, canvas.width, canvas.height);
  }
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
