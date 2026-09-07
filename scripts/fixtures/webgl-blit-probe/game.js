// WebGL 2 framebuffer binding-point probe.
//
// `blitFramebuffer` is implemented, and it is reachable only by binding the two
// WebGL 2 framebuffer targets. Neither constant was declared, so
// `bindFramebuffer(gl.READ_FRAMEBUFFER, fb)` passed `undefined` as the target and
// the whole blit path was unusable -- the fourth instance of this defect class in
// 01_constants.js, and the first found by deriving the executor's constants and
// diffing them against the table rather than by reading code.
//
// Also asserts the two binding points are tracked apart. A single shadow answered
// the draw binding for a read bind and vice versa, which stays invisible only
// while READ_FRAMEBUFFER is unreachable -- so declaring the constant without
// splitting the shadow would have traded a dead path for a lying getParameter.
const canvas = migo.createCanvas();
const gl = canvas.getContext("webgl2");

let failures = 0;
function check(name, ok, detail) {
  if (ok) { console.error(`[webgl-blit] PASS ${name}`); }
  else { failures += 1; console.error(`[webgl-blit] FAIL ${name}: ${detail}`); }
}

function run() {
  if (!gl) { check("webgl2 context", false, "null"); return verdict(); }

  for (const name of ["READ_FRAMEBUFFER", "DRAW_FRAMEBUFFER", "READ_FRAMEBUFFER_BINDING"]) {
    check(`constant ${name}`, typeof gl[name] === "number", `is ${typeof gl[name]}`);
  }
  // GLES 3 gives FRAMEBUFFER_BINDING and DRAW_FRAMEBUFFER_BINDING one enum, so
  // there must be exactly one name for it here.
  check(
    "DRAW_FRAMEBUFFER_BINDING is not declared separately",
    gl.DRAW_FRAMEBUFFER_BINDING === undefined,
    "declaring it implies two queries where GLES has one enum",
  );

  const read = gl.createFramebuffer();
  const draw = gl.createFramebuffer();
  check("createFramebuffer twice", !!read && !!draw, "one was null");
  if (!read || !draw) return verdict();

  // Bind each point separately; each query must answer its own.
  gl.bindFramebuffer(gl.READ_FRAMEBUFFER, read);
  gl.bindFramebuffer(gl.DRAW_FRAMEBUFFER, draw);
  check(
    "READ_FRAMEBUFFER_BINDING answers the read bind",
    gl.getParameter(gl.READ_FRAMEBUFFER_BINDING) === read,
    "got the wrong object",
  );
  check(
    "FRAMEBUFFER_BINDING answers the draw bind",
    gl.getParameter(gl.FRAMEBUFFER_BINDING) === draw,
    "got the wrong object",
  );

  // FRAMEBUFFER binds both points.
  gl.bindFramebuffer(gl.FRAMEBUFFER, read);
  check(
    "FRAMEBUFFER binds both points",
    gl.getParameter(gl.FRAMEBUFFER_BINDING) === read
      && gl.getParameter(gl.READ_FRAMEBUFFER_BINDING) === read,
    "one point did not follow",
  );

  // And unbinding both leaves null on both.
  gl.bindFramebuffer(gl.FRAMEBUFFER, null);
  check(
    "unbinding FRAMEBUFFER clears both points",
    gl.getParameter(gl.FRAMEBUFFER_BINDING) === null
      && gl.getParameter(gl.READ_FRAMEBUFFER_BINDING) === null,
    "a point kept a stale binding",
  );

  // The blit call itself must reach the driver without an enum error. Sized
  // attachments are not set up, so an incomplete-framebuffer error is legal here;
  // what must NOT happen is INVALID_ENUM from an undefined target.
  while (gl.getError() !== gl.NO_ERROR) { /* drain */ }
  gl.bindFramebuffer(gl.READ_FRAMEBUFFER, read);
  gl.bindFramebuffer(gl.DRAW_FRAMEBUFFER, draw);
  gl.blitFramebuffer(0, 0, 8, 8, 0, 0, 8, 8, gl.COLOR_BUFFER_BIT, gl.NEAREST);
  let sawInvalidEnum = false;
  for (;;) {
    const e = gl.getError();
    if (e === gl.NO_ERROR) break;
    if (e === gl.INVALID_ENUM) sawInvalidEnum = true;
  }
  check("blitFramebuffer raises no INVALID_ENUM", !sawInvalidEnum, "an undefined target reached the driver");

  // A sized internal format, because 48 of them were absent and six implemented
  // methods take one. texStorage2D is the WebGL 2 way to allocate an immutable
  // texture; with gl.RGBA16F undefined it received `undefined` as the format.
  while (gl.getError() !== gl.NO_ERROR) { /* drain */ }
  check("constant RGBA16F", typeof gl.RGBA16F === "number", `is ${typeof gl.RGBA16F}`);
  check("constant DEPTH24_STENCIL8", typeof gl.DEPTH24_STENCIL8 === "number",
        `is ${typeof gl.DEPTH24_STENCIL8}`);
  check("constant TEXTURE_MAX_LEVEL", typeof gl.TEXTURE_MAX_LEVEL === "number",
        `is ${typeof gl.TEXTURE_MAX_LEVEL}`);
  const tex = gl.createTexture();
  gl.bindTexture(gl.TEXTURE_2D, tex);
  gl.texStorage2D(gl.TEXTURE_2D, 1, gl.RGBA16F, 8, 8);
  let sizedEnumError = false;
  for (;;) {
    const e = gl.getError();
    if (e === gl.NO_ERROR) break;
    if (e === gl.INVALID_ENUM) sizedEnumError = true;
  }
  check("texStorage2D with a sized format raises no INVALID_ENUM", !sizedEnumError,
        "an undefined internal format reached the driver");
  gl.deleteTexture(tex);

  // WebGL 2 pixel-store parameters. The sixth instance: the WebGL 1 pair
  // PACK_ALIGNMENT/UNPACK_ALIGNMENT was declared and these eight were not, so the
  // standard sub-rectangle upload set `undefined` as the parameter name. They were
  // reachable only from a file that had been classified engine-internal, which is why
  // nothing had checked them.
  while (gl.getError() !== gl.NO_ERROR) { /* drain */ }
  const store = ["PACK_ROW_LENGTH", "PACK_SKIP_PIXELS", "PACK_SKIP_ROWS",
                 "UNPACK_ROW_LENGTH", "UNPACK_SKIP_PIXELS", "UNPACK_SKIP_ROWS",
                 "UNPACK_IMAGE_HEIGHT", "UNPACK_SKIP_IMAGES"];
  for (const name of store) {
    check(`constant ${name}`, typeof gl[name] === "number", `is ${typeof gl[name]}`);
  }
  gl.pixelStorei(gl.UNPACK_ROW_LENGTH, 16);
  gl.pixelStorei(gl.UNPACK_SKIP_PIXELS, 2);
  let storeEnumError = false;
  for (;;) {
    const e = gl.getError();
    if (e === gl.NO_ERROR) break;
    if (e === gl.INVALID_ENUM) storeEnumError = true;
  }
  check("pixelStorei with WebGL 2 parameters raises no INVALID_ENUM", !storeEnumError,
        "an undefined parameter name reached the driver");
  check("UNPACK_ROW_LENGTH reads back what was set",
        gl.getParameter(gl.UNPACK_ROW_LENGTH) === 16,
        `got ${gl.getParameter(gl.UNPACK_ROW_LENGTH)}`);
  gl.pixelStorei(gl.UNPACK_ROW_LENGTH, 0);
  gl.pixelStorei(gl.UNPACK_SKIP_PIXELS, 0);

  gl.deleteFramebuffer(read);
  gl.deleteFramebuffer(draw);
  verdict();
}

function verdict() {
  console.error(failures === 0
    ? "[webgl-blit] VERDICT ok"
    : `[webgl-blit] VERDICT ${failures} check(s) failed`);
}

requestAnimationFrame(() => { run(); });
