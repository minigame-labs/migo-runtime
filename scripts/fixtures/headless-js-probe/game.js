// Proves the shipping archive evaluates JavaScript, rather than only linking.
//
// Every other check on an assembled Migo archive is a link check or a symbol
// audit: `nm` says the engine is in there, xcodebuild says a consumer resolves
// against it, and the V8 lane's own tests say the *V8* archive runs JS. None of
// them evaluates a line of script through the bytes we ship.
//
// So this computes something. A constant would be satisfied by a parser: the
// sum below has to be executed to exist, and it is reported back with the value
// so a wrong answer is distinguishable from no answer.
let sum = 0;
for (let i = 1; i <= 1000; i += 1) {
  sum += i;
}

// A rejected assertion travels as an uncaught error, which the host receives
// through on_error with this text -- so the failure names the number rather
// than arriving as a timeout with nothing in it.
if (sum !== 500500) {
  throw new Error("migo-headless-probe: the loop summed to " + sum + ", not 500500");
}

// Whether V8 got JIT is not a question an exit status can answer, and the gate
// used to try: it ran the host with `com.apple.security.cs.allow-jit` withheld
// and read "the process completed" as "V8 silently degraded to jitless". Both
// halves of that inference were wrong. A hardened runtime with an ad-hoc
// signature does not deny JIT memory at all (tests/c_host/jit-entitlement-probe
// asks the kernel directly and gets it granted either way), and V8 has no
// runtime fallback to fall back TO -- jitless is a build-time flag, so a V8
// denied executable memory dies rather than degrading.
//
// So measure the thing instead of inferring it. Two signals, reported together:
//
//   wasm  -- jitless V8 does not slow WebAssembly down, it deletes it outright,
//            so `typeof WebAssembly` is a yes/no answer with no calibration.
//   mips  -- millions of loop iterations per second once the function is warm.
//            The two populations are far apart (708 M it/s against 17 on the
//            same content, measured through the capability probe), which is why
//            a floor anywhere in the middle separates them.
//
// The loop is warmed before it is timed: a first pass measures the interpreter
// in BOTH worlds and would report them as the same. It accumulates into a value
// that is reported, because a loop whose result is never read is a loop V8 is
// entitled to delete -- and a deleted loop measures infinitely fast.
function warmThroughput() {
  let acc = 0;
  for (let warm = 0; warm < 3; warm += 1) {
    for (let i = 0; i < 200000; i += 1) acc = (acc + i * 3) | 0;
  }
  let iterations = 1 << 20;
  for (;;) {
    const started = Date.now();
    for (let i = 0; i < iterations; i += 1) acc = (acc + i * 3) | 0;
    const elapsed = Date.now() - started;
    // Long enough that a millisecond clock is not the thing being measured.
    if (elapsed >= 120) return { mips: iterations / elapsed / 1000, acc: acc };
    if (iterations >= (1 << 28)) {
      return { mips: iterations / Math.max(elapsed, 1) / 1000, acc: acc };
    }
    iterations *= 4;
  }
}

const measured = warmThroughput();
// console.error, because console.log is filtered; this lands in the host's
// output through tracing::error! and the gate reads it from there.
console.error(
  "migo-headless-probe: v8 wasm=" + typeof WebAssembly +
    " mips=" + measured.mips.toFixed(1) +
    " acc=" + measured.acc,
);

// Canvas2D, which is a third question and the one nothing on macOS had ever
// asked. Every other check on this archive -- link, symbol, "the script ran",
// "the frames turned" -- is satisfied by an engine whose 2D backend cannot
// build a single surface, because WebGL never goes through Skia and the frame
// loop does not care what is drawn in it.
//
// It is asked here rather than in a unit test because this file runs the bytes
// that ship: a static archive, ANGLE loaded from beside the binary, a real
// CAMetalLayer with no window. A `GrDirectContext` either comes up against that
// or it does not.
//
// Reported as one line with the colour in it, so the three ways this can go
// wrong stay distinguishable from the outside: no context at all, a context
// that draws the wrong thing, and a readback that never returned.
function probeCanvas2D() {
  // One try around the whole thing, including `getContext`. A context that
  // cannot be built may surface as a null return or as a throw depending on
  // which layer declines, and an uncaught throw here would abort module
  // evaluation -- taking the frame loop and the V8 report down with it and
  // leaving the gate to report a timeout about something else entirely.
  try {
    const canvas = migo.createCanvas();
    const ctx = canvas.getContext("2d");
    if (!ctx) {
      return "canvas2d ctx=null";
    }
    // The size is reported, not assumed. The first version of this probe filled
    // 16x16 and read at (2,2), and the readback failed with
    // `Canvas2D getImageData read_pixels failed` -- which is what an
    // out-of-bounds read looks like from here, and the host's log had said
    // `CAMetalLayer ignoring invalid setDrawableSize width=0 height=0` twice on
    // the way in. Whether the content gets the surface the host declared is
    // therefore its own question, and it is asked by printing the answer rather
    // than by a readback failing for two possible reasons at once.
    const w = canvas.width;
    const h = canvas.height;
    // Clamped, so the draw and the read are in bounds whatever the size turns
    // out to be: a probe that cannot run on a small canvas cannot report how
    // small the canvas is.
    const side = Math.min(16, w, h);
    // Opaque and off every axis of the default state, so a readback that
    // reports it cannot be reporting a cleared buffer, a black surface, or a
    // premultiplied white.
    ctx.fillStyle = "rgb(0, 128, 255)";
    ctx.fillRect(0, 0, side, side);
    const px = ctx.getImageData(0, 0, 1, 1).data;
    return (
      "canvas2d size=" + w + "x" + h +
      " rgba=" + px[0] + "," + px[1] + "," + px[2] + "," + px[3]
    );
  } catch (err) {
    return "canvas2d threw=" + err;
  }
}

console.error("migo-headless-probe: " + probeCanvas2D());

// Then the render loop, which is the second question and a strictly harder one.
// Evaluating a module needs V8 and a thread; turning frames needs the surface to
// have been installed, the EGL context to be current and the presenter to be
// answering -- on macOS that is ANGLE on Metal against a CAMetalLayer with no
// window. Nothing in this repository had ever asked for that from a host.
//
// The two verdicts stay separable on purpose. If the loop never advances, the
// host times out with ready=1, which says the script was evaluated and the
// frames did not come; a script that never ran reports ready=0. One run, two
// distinguishable failures.
const FRAMES = 30;
let frames = 0;

function step() {
  frames += 1;
  if (frames >= FRAMES) {
    // The capability surface has to be installed for this to resolve at all,
    // which is the third thing worth proving: an engine that evaluated the
    // script and rendered but installed no `migo` namespace would reach here
    // and throw a TypeError, and the host would print it.
    migo.exitMiniProgram();
    return;
  }
  requestAnimationFrame(step);
}

requestAnimationFrame(step);
