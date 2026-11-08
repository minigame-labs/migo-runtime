// Surface-recreate probe: does the main canvas still describe the surface after
// the native surface was destroyed and recreated at a different size?
//
// The size the engine gives a canvas the content never sized is derived from the
// surface, so it has to be re-derived when the surface changes. A same-window
// resize (`migo_surface_update`) is one path and a destroy/recreate (background,
// display resize, fold, rotate-while-away) is the other; only content can see
// whether both arrive at the same answer, because `canvas.width` is the only
// place the decision surfaces.
//
// Deliberately never assigns `canvas.width`. Content that does owns its backing
// store and the engine must NOT resize it; that is the opposite property and a
// probe that set a size could not observe this one.
//
// The verdict is a colour so a screenshot is the evidence, and the numbers are
// drawn next to it so the failure says what it was rather than only that it
// happened. `frames` is the paired liveness reading: a green frame from an
// engine that stopped painting would be indistinguishable from a correct one
// without it.
const canvas = migo.createCanvas();
const ctx = canvas.getContext('2d');

const FOLLOWS = '#00a000';   // green -- canvas matches the window it draws into
const STALE = '#c00000';     // red   -- canvas kept a size the window no longer has
const NATIVE_REFERENCE = '#1b5e9c'; // blue -- compare with native size and density

let resizeEvents = 0;
let frames = 0;
let lastReport = 0;

// Optional on purpose: a Slim build has no window-info service, and the canvas
// must follow its surface there too.
if (typeof migo.onWindowResize === 'function') {
  migo.onWindowResize(function (event) {
    resizeEvents++;
    console.error('[srprobe] onWindowResize ' + event.windowWidth + 'x' + event.windowHeight +
      ' canvas=' + canvas.width + 'x' + canvas.height);
  });
}

function paint() {
  frames++;
  const info = typeof migo.getSystemInfoSync === 'function' ? migo.getSystemInfoSync() : null;
  // Both are CSS pixels, but only one of them is an integer: the backing store
  // is a whole number of pixels (the surface divided by the device pixel ratio,
  // rounded), while `windowWidth` carries the exact ratio. Comparing them
  // directly is a verdict that can never be green -- 1080/2.75 is 392.7272.
  // Slim has no independent window-info API. Report the canvas without a local
  // verdict; the host must compare it with its actual surface size and density.
  const wantW = info ? Math.round(info.windowWidth) : null;
  const wantH = info ? Math.round(info.windowHeight) : null;
  const follows = info && canvas.width === wantW && canvas.height === wantH;

  ctx.fillStyle = info ? (follows ? FOLLOWS : STALE) : NATIVE_REFERENCE;
  ctx.fillRect(0, 0, canvas.width, canvas.height);

  const size = Math.max(14, Math.round(canvas.width / 16));
  ctx.fillStyle = '#ffffff';
  ctx.font = size + 'px sans-serif';
  const line = size * 1.5;
  let y = line * 1.5;
  ctx.fillText('canvas ' + canvas.width + 'x' + canvas.height, size, y);
  y += line;
  if (info) {
    ctx.fillText('window ' + wantW + 'x' + wantH, size, y);
    y += line;
    ctx.fillText('screen ' + Math.round(info.screenWidth) + 'x' + Math.round(info.screenHeight), size, y);
    y += line;
    ctx.fillText('dpr ' + info.pixelRatio, size, y);
  } else {
    ctx.fillText('reference: native surface / density', size, y);
  }
  y += line;
  ctx.fillText('resizes ' + resizeEvents + ' frames ' + frames, size, y);

  // One report a second, so a log-only run has the same numbers without
  // flooding: the screenshot is the primary evidence.
  if (frames - lastReport >= 60) {
    lastReport = frames;
    console.error('[srprobe] canvas=' + canvas.width + 'x' + canvas.height +
      (info ? ' window=' + wantW + 'x' + wantH + ' dpr=' + info.pixelRatio : ' reference=native') +
      ' resizes=' + resizeEvents + ' frames=' + frames +
      (info ? ' verdict=' + (follows ? 'follows' : 'stale') : ''));
  }
  requestAnimationFrame(paint);
}
paint();
