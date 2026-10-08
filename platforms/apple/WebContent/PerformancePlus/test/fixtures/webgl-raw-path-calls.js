// The WebGL calls the engine's facade cannot encode into its own stream, made
// through the facade so that both executions take that path.
//
// The facade encodes a call whose arguments are all numbers and falls back to
// the op otherwise: a BigInt argument is the smallest way to make that happen
// without changing what the call means, because `#[smi]` and `f32` accept a
// BigInt exactly as they accept a Number (see fixtures/op-arg-answers.json).
// A uniform array past the stream's 512-word bound takes the same path for the
// reason it exists.
//
// Run twice: in the embedded runtime, where these reach the Rust ops, and on
// the producer, where they become records the host decodes. The commands must
// be the same, command for command. See
// engine/crates/runtime-v8/src/rendering/webgl/resource_parity.rs.

const gl = new WebGL2RenderingContext({ _rid: 1, width: 64, height: 64 }, {});

// -- state, all through the raw path --
gl.viewport(0n, 0n, 64n, 64n);
gl.scissor(1n, 2n, 3n, 4n);
gl.clearColor(0.25, 0.5, 0.75, 1n);
gl.clearDepth(1n);
gl.clearStencil(3n);
gl.enable(0x0b71n); // DEPTH_TEST
gl.disable(0x0b44n); // CULL_FACE
gl.colorMask(true, false, true, 1n);
gl.depthMask(0n);
gl.depthFunc(0x0203n); // LEQUAL
gl.depthRange(0n, 1n);
gl.cullFace(0x0405n); // BACK
gl.frontFace(0x0901n); // CW
gl.lineWidth(2n);
gl.polygonOffset(1n, 2n);
gl.blendColor(0.1, 0.2, 0.3, 1n);
gl.blendEquation(0x8006n); // FUNC_ADD
gl.blendEquationSeparate(0x8006n, 0x800an); // FUNC_ADD, FUNC_SUBTRACT
gl.blendFunc(1n, 0n);
gl.blendFuncSeparate(1n, 0n, 1n, 0n);
gl.stencilFunc(0x0207n, 1n, 0xffn); // ALWAYS
gl.stencilFuncSeparate(0x0404n, 0x0207n, 1n, 0xffn); // FRONT
gl.stencilMask(0xf0n);
gl.stencilMaskSeparate(0x0405n, 0x0fn); // BACK
gl.stencilOp(0x1e00n, 0x1e01n, 0x1e02n); // KEEP, REPLACE, INCR
gl.stencilOpSeparate(0x0404n, 0x1e00n, 0x1e00n, 0x1e00n);
gl.hint(0x8192n, 0x1101n); // GENERATE_MIPMAP_HINT, NICEST
gl.pixelStorei(0x0cf5n, 4n); // UNPACK_ALIGNMENT
gl.activeTexture(0x84c1n); // TEXTURE1

// -- binding --
const buffer = gl.createBuffer();
gl.bindBuffer(0x8892n, buffer);
gl.bindBufferBase(0x8c8en, 0n, buffer); // TRANSFORM_FEEDBACK_BUFFER
gl.bindBufferRange(0x8c8en, 1n, buffer, 0n, 16n);
const texture = gl.createTexture();
gl.bindTexture(0x0de1n, texture);
gl.generateMipmap(0x0de1n);
gl.texParameteri(0x0de1n, 0x2801n, 0x2601n); // MIN_FILTER, LINEAR
gl.texParameterf(0x0de1n, 0x813an, 1.5); // TEXTURE_MIN_LOD
const framebuffer = gl.createFramebuffer();
gl.bindFramebuffer(0x8d40n, framebuffer);
const renderbuffer = gl.createRenderbuffer();
gl.bindRenderbuffer(0x8d41n, renderbuffer);
const vertexArray = gl.createVertexArray();
gl.bindVertexArray(vertexArray);
const sampler = gl.createSampler();
gl.bindSampler(0n, sampler);
gl.samplerParameteri(sampler, 0x2801n, 0x2601n);
gl.samplerParameterf(sampler, 0x813an, 0.5);

// -- attributes and drawing --
gl.enableVertexAttribArray(0n);
gl.vertexAttribPointer(0n, 3n, 0x1406n, false, 12n, 0n); // FLOAT
gl.vertexAttribDivisor(0n, 1n);
// constant attribute values, and the integer variants (the BigInt makes each take the op)
gl.vertexAttrib4f(1n, 0.1, 0.2, 0.3, 1n);
gl.vertexAttrib2f(1n, 0.5, 0.25);            // an op argument is a BigInt only where the call has one: this one is the encoded path
gl.vertexAttribI4i(2n, 1n, -2n, 3n, 4n);
gl.vertexAttribI4ui(2n, 1n, 2n, 3n, 4n);
gl.vertexAttribIPointer(2n, 2n, 0x1404n, 0n, 0n); // INT
gl.disableVertexAttribArray(1n);
// A draw needs a program in use and, for indices, an element array buffer the draw stays inside; the facade refuses
// one without, before the op. The link status is what the renderer would answer: neither execution has one here.
const program = gl.createProgram();
gl.linkProgram(program);
gl._programParameterCache.set(program.id, new Map([[0x8b82, 1]])); // LINK_STATUS
gl.useProgram(program);
gl.bindBuffer(0x8893, gl.createBuffer()); // ELEMENT_ARRAY_BUFFER
gl.bufferData(0x8893, new Uint16Array([0, 1, 2]), 0x88e4);
gl.drawArrays(0x0004n, 0n, 3n); // TRIANGLES
gl.drawArraysInstanced(0x0004n, 0n, 3n, 2n);
gl.drawElements(0x0004n, 3n, 0x1403n, 0n); // UNSIGNED_SHORT
gl.drawElementsInstanced(0x0004n, 3n, 0x1403n, 0n, 2n);

// -- uniforms --
// `uniform1i`, `uniform1f` and the scalar vector forms are always encodable --
// the facade coerces them itself -- so the raw path is unreachable for them
// from here; their records are the same shape as the ones below.
const location = { id: 7 };

// Past the encoder's 512-word inline bound, which is the only way to reach the
// raw uniform path: 600 words is 37 `mat4`s, the size a skinned mesh's bone
// array sits at, and the record carries it because the record's ceiling is its
// own (64 Ki words) and not the encoder's.
const large = 600;
gl.uniform1fv(location, new Float32Array(large).fill(0.5));
gl.uniform2fv(location, new Float32Array(large).fill(1.5));
gl.uniform3fv(location, new Float32Array(large).fill(2.5));
gl.uniform4fv(location, new Float32Array(large).fill(3.5));
gl.uniform1iv(location, new Int32Array(large).fill(-1));
gl.uniform2iv(location, new Int32Array(large).fill(2));
gl.uniform3iv(location, new Int32Array(large).fill(3));
gl.uniform4iv(location, new Int32Array(large).fill(4));
gl.uniformMatrix2fv(location, false, new Float32Array(large).fill(0.25));
gl.uniformMatrix3fv(location, false, new Float32Array(large).fill(0.5));
gl.uniformMatrix4fv(location, true, new Float32Array(large).fill(0.75));
// WebGL 2: the unsigned vectors and the non-square matrices (600 is a whole number of every width)
gl.uniform1uiv(location, new Uint32Array(large).fill(5));
gl.uniform2uiv(location, new Uint32Array(large).fill(6));
gl.uniform3uiv(location, new Uint32Array(large).fill(7));
gl.uniform4uiv(location, new Uint32Array(large).fill(8));
gl.uniformMatrix2x3fv(location, false, new Float32Array(large).fill(0.125));
gl.uniformMatrix2x4fv(location, true, new Float32Array(large).fill(0.25));
gl.uniformMatrix3x2fv(location, false, new Float32Array(large).fill(0.375));
gl.uniformMatrix3x4fv(location, true, new Float32Array(large).fill(0.5));
gl.uniformMatrix4x2fv(location, false, new Float32Array(large).fill(0.625));
gl.uniformMatrix4x3fv(location, true, new Float32Array(large).fill(0.75));
