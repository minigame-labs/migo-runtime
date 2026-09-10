// Pixel evidence that ANGLE-Metal draws what it is told, on Apple hardware.
//
// G0's P2 asks for `glReadPixels` verification rather than someone looking at a
// screen, and this is the half of it that is reachable today. The half that is
// not: on iOS the shipping product carries no engine -- content JavaScript runs
// in WebKit's WebContent process, not ours -- so there is no in-process
// `readPixels` to call, and the external-frame ABI has no readback entry point
// (`submit_external_frame`, `request_external_frame`, `take_external_gl_error`
// and nothing else). So the phone half waits on A3 or on an affordance nobody
// has decided to add.
//
// What runs here is the SAME presenter iOS will use: `platform/src/apple/
// presenter.rs` on ANGLE's Metal backend against a CAMetalLayer. Verifying it on
// macOS does not verify the phone, and it does verify the renderer.
//
// WHY EXACT COMPARISON AND NOT A TOLERANCE. A tolerance is a number somebody
// picks, and every wrong-colour-space, wrong-blend and wrong-premultiply bug
// this project has had would fit inside a generous one. RGBA8 with antialiasing
// off, sampled well inside a solid triangle, is exactly reproducible: the value
// that comes back is the value that was written, or the renderer is wrong.

const WIDTH = 64;
const HEIGHT = 64;

function fail(message) {
  throw new Error("migo-webgl-probe: " + message);
}

const canvas = migo.createCanvas();
canvas.width = WIDTH;
canvas.height = HEIGHT;

// `antialias: false` is load-bearing: a multisampled buffer blends edges, and a
// blended pixel is not a pixel this can compare exactly.
// `preserveDrawingBuffer: true` so the readback sees what was drawn rather than
// a buffer the presenter may already have recycled.
const gl = canvas.getContext("webgl", {
  antialias: false,
  preserveDrawingBuffer: true,
  alpha: false,
  depth: false,
  stencil: false
});
if (!gl) {
  fail("no WebGL context; ANGLE did not come up");
}

// --- 1. the clear colour ------------------------------------------------------

gl.viewport(0, 0, WIDTH, HEIGHT);
gl.clearColor(0.0, 0.0, 1.0, 1.0);
gl.clear(gl.COLOR_BUFFER_BIT);

const pixel = new Uint8Array(4);

function readPixel(x, y) {
  gl.readPixels(x, y, 1, 1, gl.RGBA, gl.UNSIGNED_BYTE, pixel);
  return [pixel[0], pixel[1], pixel[2], pixel[3]];
}

function expect(where, got, want) {
  for (let i = 0; i < 4; i += 1) {
    if (got[i] !== want[i]) {
      fail(where + " read [" + got.join(",") + "] and should have been ["
        + want.join(",") + "]");
    }
  }
}

expect("the cleared buffer at (32,32)", readPixel(32, 32), [0, 0, 255, 255]);
expect("the cleared buffer at (0,0)", readPixel(0, 0), [0, 0, 255, 255]);

// --- 2. a solid triangle over the lower-left half -----------------------------

const vertexSource = [
  "attribute vec2 position;",
  "void main() { gl_Position = vec4(position, 0.0, 1.0); }"
].join("\n");
const fragmentSource = [
  "precision mediump float;",
  "void main() { gl_FragColor = vec4(1.0, 0.0, 0.0, 1.0); }"
].join("\n");

function compile(type, source) {
  const shader = gl.createShader(type);
  gl.shaderSource(shader, source);
  gl.compileShader(shader);
  if (!gl.getShaderParameter(shader, gl.COMPILE_STATUS)) {
    fail("a shader did not compile: " + gl.getShaderInfoLog(shader));
  }
  return shader;
}

const program = gl.createProgram();
gl.attachShader(program, compile(gl.VERTEX_SHADER, vertexSource));
gl.attachShader(program, compile(gl.FRAGMENT_SHADER, fragmentSource));
gl.linkProgram(program);
if (!gl.getProgramParameter(program, gl.LINK_STATUS)) {
  fail("the program did not link: " + gl.getProgramInfoLog(program));
}
gl.useProgram(program);

// Covers the lower-left triangle of clip space, so (8,8) is well inside it and
// (56,56) is well outside. Both are far from the hypotenuse, which is where a
// half-covered pixel would be ambiguous even without antialiasing.
const vertices = new Float32Array([-1, -1, 1, -1, -1, 1]);
const buffer = gl.createBuffer();
gl.bindBuffer(gl.ARRAY_BUFFER, buffer);
gl.bufferData(gl.ARRAY_BUFFER, vertices, gl.STATIC_DRAW);
const positionLocation = gl.getAttribLocation(program, "position");
gl.enableVertexAttribArray(positionLocation);
gl.vertexAttribPointer(positionLocation, 2, gl.FLOAT, false, 0, 0);
gl.drawArrays(gl.TRIANGLES, 0, 3);

expect("inside the triangle at (8,8)", readPixel(8, 8), [255, 0, 0, 255]);
expect("outside the triangle at (56,56)", readPixel(56, 56), [0, 0, 255, 255]);

// --- 3. the GL error queue ----------------------------------------------------
//
// A renderer that produced the right pixels while raising errors is a renderer
// that got there by a path nobody designed.
const error = gl.getError();
if (error !== gl.NO_ERROR) {
  fail("the GL error queue was not empty: 0x" + error.toString(16));
}

migo.exitMiniProgram();
