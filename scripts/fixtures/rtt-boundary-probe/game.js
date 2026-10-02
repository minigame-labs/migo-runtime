// Render-to-texture across the *frame boundary*: the sibling of `rtt-probe`, and
// the only probe that reaches the post-swap restore.
//
// The property: a present leaves the content's framebuffer bindings as the
// content left them. WebGL keeps a context's bindings across compositing -- a
// frame that ends holding a framebuffer object begins the next one holding it --
// and the GL-state contract this gate enforces forbids the engine to change them
// behind the content's back. A present lands between two of the content's
// frames, or between any two of its batches, so content that caches what it has
// bound (three.js does) draws its next pass wherever the present left the driver.
//
// An earlier version of this probe asserted the opposite -- that every frame
// begins with the default framebuffer bound, which the engine used to arrange by
// re-pointing the driver after each swap -- and turned red, correctly, the day
// the engine stopped doing that (#390). The content here is what a browser runs
// the same way: no frame relies on the engine to rebind anything.
//
// Every frame, holding framebuffer X (a 64x64 texture) that the last frame left RED:
//
//   1. clear, with no bind              -> into X, if the present left X bound
//   2. bind the default framebuffer and draw X's texture over the whole screen
//   3. bind X and clear it RED          -> the frame ends holding X, RED again
//
// A clear that missed X -- because the driver was re-pointed at the default
// framebuffer, with or without the dedup shadow told -- lands on the screen and
// is then covered by X's texture, still red: red means the binding was lost. A
// shadow that disagrees with the driver the other way (the driver holds X, the
// shadow says default) dedups step 2's bind away, the draw samples the
// framebuffer it draws into, and the screen keeps an earlier frame. The first
// frame clears BLUE, so blue means the engine stopped presenting; green, which
// only reaches the screen through X, means the property holds.
const canvas = migo.createCanvas();
const gl = canvas.getContext("webgl");

// A second canvas keeps DrawingBuffer bypass off, so a blit runs every frame and
// the post-swap restore is on the path. It is never drawn to: unlike `rtt-probe`
// this fixture does not need a mid-frame canvas switch.
//
// On `globalThis` for the reason `blit-probe` explains: a module binding nothing
// reads again is not reachable, and once V8 drops it the FinalizationRegistry
// destroys the canvas, bypass latches, and this probe stops reaching the
// post-swap restore it exists to cover.
globalThis.__rttBoundaryKeepBypassOff = migo.createCanvas();

const SIZE = 64;
const tex = gl.createTexture();
gl.bindTexture(gl.TEXTURE_2D, tex);
gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA, SIZE, SIZE, 0, gl.RGBA, gl.UNSIGNED_BYTE, null);
gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.NEAREST);
gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.NEAREST);
gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
const rtt = gl.createFramebuffer();
gl.bindFramebuffer(gl.FRAMEBUFFER, rtt);
gl.framebufferTexture2D(gl.FRAMEBUFFER, gl.COLOR_ATTACHMENT0, gl.TEXTURE_2D, tex, 0);
const status = gl.checkFramebufferStatus(gl.FRAMEBUFFER);
gl.clearColor(0.85, 0.1, 0.15, 1.0);
gl.clear(gl.COLOR_BUFFER_BIT);

// What shows X on the screen: a full-screen pair of triangles sampling its texture.
const compile = (type, source) => {
  const shader = gl.createShader(type);
  gl.shaderSource(shader, source);
  gl.compileShader(shader);
  return shader;
};
const program = gl.createProgram();
gl.attachShader(program, compile(gl.VERTEX_SHADER,
  "attribute vec2 p; varying vec2 uv; void main(){ uv = p * 0.5 + 0.5; gl_Position = vec4(p, 0.0, 1.0); }"));
gl.attachShader(program, compile(gl.FRAGMENT_SHADER,
  "precision mediump float; varying vec2 uv; uniform sampler2D t; void main(){ gl_FragColor = texture2D(t, uv); }"));
gl.linkProgram(program);
const linked = gl.getProgramParameter(program, gl.LINK_STATUS);
gl.useProgram(program);
gl.bindBuffer(gl.ARRAY_BUFFER, gl.createBuffer());
gl.bufferData(gl.ARRAY_BUFFER, new Float32Array([-1, -1, 1, -1, -1, 1, -1, 1, 1, -1, 1, 1]), gl.STATIC_DRAW);
const p = gl.getAttribLocation(program, "p");
gl.enableVertexAttribArray(p);
gl.vertexAttribPointer(p, 2, gl.FLOAT, false, 0, 0);
gl.uniform1i(gl.getUniformLocation(program, "t"), 0);
gl.activeTexture(gl.TEXTURE0);
gl.bindTexture(gl.TEXTURE_2D, tex);
// Setup ends holding X, as every frame does: rtt is still bound from above.

let frames = 0;

function paint() {
  // 1. No bind: X is what the last frame left bound, and no present may have
  //    changed that since.
  if (frames === 0) {
    gl.clearColor(0.1, 0.3, 0.9, 1.0);
  } else {
    gl.clearColor(0.2, 0.8, 0.4, 1.0);
  }
  gl.clear(gl.COLOR_BUFFER_BIT);

  // 2. The screen shows X.
  gl.bindFramebuffer(gl.FRAMEBUFFER, null);
  gl.viewport(0, 0, gl.drawingBufferWidth, gl.drawingBufferHeight);
  gl.drawArrays(gl.TRIANGLES, 0, 6);

  // 3. End holding X, RED, so the next frame's step 1 depends on the present
  //    keeping it and shows red if it did not.
  gl.bindFramebuffer(gl.FRAMEBUFFER, rtt);
  gl.clearColor(0.85, 0.1, 0.15, 1.0);
  gl.clear(gl.COLOR_BUFFER_BIT);

  frames += 1;
  if (frames === 1 || frames % 60 === 0) {
    console.error(
      `[rtt-boundary-probe] painted ${frames} frames, fbo_status=0x${status.toString(16)}, linked=${linked}, ` +
        `expect rgba(51,204,102,255) and never rgba(217,26,38,255)`
    );
  }
  requestAnimationFrame(paint);
}

requestAnimationFrame(paint);
