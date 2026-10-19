// The recorder end to end, on the engine: a game that reaches for names the engine has,
// names it lacks and a name of its own, asks WebGL for extensions, and draws -- the
// player fails a run that presents nothing, so a recorder that broke the game fails too.
var began = Date.now();
while (Date.now() - began < 150) {
  // The entry's own work, so "ready" has something to come after.
}
migo.getSystemInfoSync();
migo.getSystemInfoSync();
var vk = migo.createVKSession;
var gpu = "getGPU" in migo;
migo.shim = function shim() {};
migo.shim();
console.log("FIXTURE identity " + (migo.createCanvas === migo.createCanvas));

var canvas = migo.createCanvas();
var gl = canvas.getContext("webgl");
gl.getExtension("OES_vertex_array_object");
gl.getExtension("MIGO_no_such_extension");
gl.enable(0x1234);
console.log("FIXTURE getError " + gl.getError());

function paint() {
  gl.clearColor(0.2, 0.8, 0.4, 1);
  gl.clear(gl.COLOR_BUFFER_BIT);
  migo.requestAnimationFrame(paint);
}
migo.requestAnimationFrame(paint);
console.log("FIXTURE entry evaluated");
