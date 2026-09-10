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
