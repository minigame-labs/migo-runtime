// Does a GPU context loss take the Canvas2D drawing state with it?
//
// fillStyle is set exactly ONCE. The JS setter de-duplicates against its own
// shadow, so it is never re-sent -- which is the whole point: if the render
// side rebuilds its context at spec defaults, every later fill paints opaque
// black and nothing reports an error.
const L = (...a) => console.error("[ctxloss]", ...a);
const canvas = migo.createCanvas();
const ctx = canvas.getContext("2d");
const W = canvas.width, H = canvas.height;

ctx.fillStyle = "#ff00ff";   // magenta, set once and never again
ctx.globalAlpha = 1.0;

// A separate WebGL canvas purely to reach MIGO_debug_gpu_reset. A GPU reset
// tears down the whole share group, so it reaches the 2D context too.
// (WEBGL_lose_context would not: it loses only its own context.)
const glCanvas = migo.createCanvas();
const gl = glCanvas.getContext("webgl");
const loseExt = gl && gl.getExtension("MIGO_debug_gpu_reset");
L("lose-context extension:", loseExt ? "available" : "MISSING");

let frame = 0, lost = false;
function loop() {
    frame++;
    ctx.fillRect(0, 0, W, H);          // fillStyle never re-sent
    if (frame === 90 && loseExt && !lost) {
        lost = true;
        L("triggering context loss at frame", frame);
        loseExt.reset();
    }
    if (frame % 60 === 0) L("frame", frame, "lost:", lost);
    requestAnimationFrame(loop);
}
loop();
L("ready", W + "x" + H);
