// Every WebGL resource call the Performance+ producer answers on its stream
// lane, made through the engine's own WebGL 2 facade.
//
// Run twice: in the embedded runtime, where the facade's calls reach the Rust
// ops, and on the producer, where they become records the host decodes. The
// commands the two produce -- and the errors they record -- must be the same,
// command for command. See
// engine/crates/runtime-v8/src/rendering/webgl/resource_parity.rs.
//
// The arguments include the values that tell implementations apart: a negative
// offset, a null upload, typed arrays with an element offset, a pixel-unpack
// offset, text outside ASCII, an empty varying name, and ids of zero and null.

const gl = new WebGL2RenderingContext({ _rid: 1, width: 64, height: 64 }, {});

const buffer = gl.createBuffer();
gl.bindBuffer(0x8892, buffer); // ARRAY_BUFFER
gl.bufferData(0x8892, new Float32Array([0, 1, 2, 3]), 0x88e4); // STATIC_DRAW
gl.bufferData(0x8893, 24, 0x88e8); // ELEMENT_ARRAY_BUFFER, size only, DYNAMIC_DRAW
gl.bufferSubData(0x8892, 4, new Uint8Array([9, 8, 7]));
gl.bufferSubData(0x8892, -1, new Uint8Array([1])); // INVALID_VALUE on both paths

const vertex = gl.createShader(0x8b31);
gl.shaderSource(vertex, "attribute vec4 p; void main() { gl_Position = p; } // ünïcödé ✓ 😀");
gl.compileShader(vertex);
const fragment = gl.createShader(0x8b30);
gl.shaderSource(fragment, "precision mediump float; void main() { gl_FragColor = vec4(1.0); }");
gl.compileShader(fragment);
const program = gl.createProgram();
gl.attachShader(program, vertex);
gl.attachShader(program, fragment);
gl.bindAttribLocation(program, 0, "p");
gl.transformFeedbackVaryings(program, ["a", "", "bé"], 0x8c8c); // INTERLEAVED_ATTRIBS
gl.linkProgram(program);
gl.uniformBlockBinding(program, 0, 2);

const texture = gl.createTexture();
gl.bindTexture(0x0de1, texture); // TEXTURE_2D
gl.texImage2D(0x0de1, 0, 0x1908, 2, 1, 0, 0x1908, 0x1401, new Uint8Array([1, 2, 3, 4, 5, 6, 7, 8]));
gl.texImage2D(0x0de1, 1, 0x1908, 1, 1, 0, 0x1908, 0x1401, null);
gl.texSubImage2D(0x0de1, 0, 1, 0, 1, 1, 0x1908, 0x1401, new Uint8Array([9, 9, 9, 9]));
gl.texStorage2D(0x0de1, 1, 0x8058, 4, 4); // RGBA8

const volume = gl.createTexture();
gl.bindTexture(0x806f, volume); // TEXTURE_3D
gl.texImage3D(0x806f, 0, 0x1908, 1, 1, 2, 0, 0x1908, 0x1401, new Uint8Array([1, 2, 3, 4, 5, 6, 7, 8, 9, 10]), 2);
gl.texSubImage3D(0x806f, 0, 0, 0, 0, 1, 1, 1, 0x1908, 0x1401, new Uint16Array([1, 2, 3, 4]), 1);
gl.texStorage3D(0x806f, 1, 0x8058, 2, 2, 2);

const framebuffer = gl.createFramebuffer();
gl.bindFramebuffer(0x8d40, framebuffer);
const renderbuffer = gl.createRenderbuffer();
gl.bindRenderbuffer(0x8d41, renderbuffer);
gl.renderbufferStorage(0x8d41, 0x81a5, 4, 4); // DEPTH_COMPONENT16
gl.renderbufferStorageMultisample(0x8d41, 4, 0x8058, 4, 4);
gl.framebufferRenderbuffer(0x8d40, 0x8d00, 0x8d41, renderbuffer); // DEPTH_ATTACHMENT
gl.framebufferTexture2D(0x8d40, 0x8ce0, 0x0de1, texture, 0); // COLOR_ATTACHMENT0
gl.framebufferTexture2D(0x8d40, 0x8ce1, 0x0de1, null, 0);
gl.drawBuffers([0x8ce0, 0]); // COLOR_ATTACHMENT0, NONE
gl.invalidateFramebuffer(0x8d40, [0x8ce0]);
gl.blitFramebuffer(0, 0, 4, 4, 4, 4, -4, -4, 0x4000, 0x2600); // COLOR_BUFFER_BIT, NEAREST

const query = gl.createQuery();
gl.beginQuery(0x8c2f, query); // ANY_SAMPLES_PASSED
gl.endQuery(0x8c2f);
const sampler = gl.createSampler();
const vertexArray = gl.createVertexArray();
const feedback = gl.createTransformFeedback();
gl.bindTransformFeedback(0x8e22, feedback); // TRANSFORM_FEEDBACK
gl.beginTransformFeedback(0x0000); // POINTS
gl.pauseTransformFeedback();
gl.resumeTransformFeedback();
gl.endTransformFeedback();
gl.bindTransformFeedback(0x8e22, null);
const sync = gl.fenceSync(0x9116, 0); // SYNC_GL_FENCE
gl.deleteSync(sync);

gl.deleteQuery(query);
gl.deleteSampler(sampler);
gl.deleteVertexArray(vertexArray);
gl.deleteTransformFeedback(feedback);
gl.deleteFramebuffer(framebuffer);
gl.deleteRenderbuffer(renderbuffer);
gl.deleteTexture(volume);
gl.deleteTexture(texture);
gl.deleteProgram(program);
gl.deleteShader(vertex);
gl.deleteShader(fragment);
gl.deleteBuffer(buffer);
