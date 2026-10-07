// Does removing PboPool::acquire's glClientWaitSync wait change upload
// throughput, and does it corrupt anything?
//
// Simulates a level-load / texture-streaming burst: 64 textures, each
// 512x512 RGBA (1 MiB), uploaded back-to-back via texImage2D with a fresh
// Uint8Array every time -- a full respecify on every call, the access
// pattern PboPool::acquire's doc comment reasons about (STREAM_DRAW +
// glBufferData orphans, so a probe-not-wait is safe regardless of whether the
// previous transfer finished). That is enough textures to push well past
// PboPool::DEFAULT_POOL_SIZE (4), so most of the burst has to decide what to
// do with no fence-ready buffer available -- old code: wait up to 5ms then
// take a fresh name; current code: take a fresh name immediately.
//
// Each texture gets a distinct, verifiable solid colour. After the burst,
// drawn as an 8x8 grid covering the whole canvas -- one draw per cell,
// sampling that cell's own texture. If a reused (still-in-flight, or wrongly
// aliased) buffer ever produced a corrupted upload, the corresponding cell
// reads a colour other than the one that texture was assigned, which
// scripts/measure-pbo-stream-burst.sh checks pixel-by-pixel rather than by a
// single dominant colour (a 64-colour frame has no single dominant colour to
// read).
const GRID = 8;
const N = GRID * GRID;
const TEX_SIZE = 512;

const canvas = migo.createCanvas();
const gl = canvas.getContext('webgl');

const VERT = `
attribute vec2 a_pos;
varying vec2 v_uv;
void main() {
  v_uv = a_pos * 0.5 + 0.5;
  gl_Position = vec4(a_pos, 0.0, 1.0);
}
`;
const FRAG = `
precision mediump float;
uniform sampler2D u_tex;
varying vec2 v_uv;
void main() {
  gl_FragColor = texture2D(u_tex, v_uv);
}
`;

function compile(type, src) {
  const s = gl.createShader(type);
  gl.shaderSource(s, src);
  gl.compileShader(s);
  if (!gl.getShaderParameter(s, gl.COMPILE_STATUS)) {
    console.error('[pbo-stream-burst] shader compile failed: ' + gl.getShaderInfoLog(s));
  }
  return s;
}
const program = gl.createProgram();
gl.attachShader(program, compile(gl.VERTEX_SHADER, VERT));
gl.attachShader(program, compile(gl.FRAGMENT_SHADER, FRAG));
gl.linkProgram(program);
gl.useProgram(program);

const quad = gl.createBuffer();
gl.bindBuffer(gl.ARRAY_BUFFER, quad);
gl.bufferData(
  gl.ARRAY_BUFFER,
  new Float32Array([-1, -1, 1, -1, -1, 1, -1, 1, 1, -1, 1, 1]),
  gl.STATIC_DRAW
);
const aPos = gl.getAttribLocation(program, 'a_pos');
gl.enableVertexAttribArray(aPos);
gl.vertexAttribPointer(aPos, 2, gl.FLOAT, false, 0, 0);

const uTex = gl.getUniformLocation(program, 'u_tex');
gl.uniform1i(uTex, 0);
gl.activeTexture(gl.TEXTURE0);

// Colours chosen the way state-shadow-probe's are: distinct per channel, so a
// wrong-cell readback says which texture actually landed there.
function colourFor(i) {
  return [(i * 41) % 256, (i * 89) % 256, (i * 157 + 30) % 256, 255];
}

const t0 = Date.now();
const textures = [];
for (let i = 0; i < N; i++) {
  const [r, g, b, a] = colourFor(i);
  const data = new Uint8Array(TEX_SIZE * TEX_SIZE * 4);
  for (let p = 0; p < data.length; p += 4) {
    data[p] = r; data[p + 1] = g; data[p + 2] = b; data[p + 3] = a;
  }
  const tex = gl.createTexture();
  gl.bindTexture(gl.TEXTURE_2D, tex);
  gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA, TEX_SIZE, TEX_SIZE, 0, gl.RGBA, gl.UNSIGNED_BYTE, data);
  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.NEAREST);
  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.NEAREST);
  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
  textures.push(tex);
}
const burstMs = Date.now() - t0;
console.error('[pbo-stream-burst] uploaded ' + N + ' ' + TEX_SIZE + 'x' + TEX_SIZE + ' textures in ' + burstMs + 'ms');

function drawGrid() {
  const w = canvas.width, h = canvas.height;
  const cw = Math.floor(w / GRID), ch = Math.floor(h / GRID);
  gl.enable(gl.SCISSOR_TEST);
  gl.bindBuffer(gl.ARRAY_BUFFER, quad);
  gl.vertexAttribPointer(aPos, 2, gl.FLOAT, false, 0, 0);
  for (let row = 0; row < GRID; row++) {
    for (let col = 0; col < GRID; col++) {
      const i = row * GRID + col;
      const x = col * cw, y = row * ch;
      gl.viewport(x, y, cw, ch);
      gl.scissor(x, y, cw, ch);
      gl.bindTexture(gl.TEXTURE_2D, textures[i]);
      gl.drawArrays(gl.TRIANGLES, 0, 6);
    }
  }
  gl.disable(gl.SCISSOR_TEST);
  gl.viewport(0, 0, w, h);
}

let frames = 0;
function paint() {
  drawGrid();
  frames += 1;
  if (frames === 2 || frames % 60 === 0) {
    console.error('[pbo-stream-burst] painted ' + frames + ' frames, grid=' + GRID + 'x' + GRID + ' cellPx~' + Math.floor(TEX_SIZE));
  }
  requestAnimationFrame(paint);
}
requestAnimationFrame(paint);
