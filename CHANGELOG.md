# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
- WebGL: `KHR_parallel_shader_compile`, where the driver compiles and links in parallel (`GL_KHR_parallel_shader_compile`):
  COMPLETION_STATUS_KHR of a shader or program is asked of the renderer without waiting for the compile or link -- the
  renderer's deferred link read stays queued -- and is kept once true, or once LINK_STATUS was read, so polling content
  (three.js, Babylon) keeps its frames while shaders build. True for one never compiled or linked and on a lost context;
  INVALID_ENUM and null before the extension is enabled.
- WebGL 1: `OES_texture_float` and `OES_texture_half_float` -- FLOAT and HALF_FLOAT_OES uploads of each unsized format,
  held as ES 3.0's 32- and 16-bit float formats, luminance and alpha through a swizzle so they sample as (l, l, l, 1)
  and (0, 0, 0, a) -- with `OES_texture_half_float_linear`, and `WEBGL_color_buffer_float` and
  `EXT_color_buffer_half_float` enabled with them where the renderer renders to floats, as browsers enable them: RGBA
  float attachments and RGBA32F_EXT / RGBA16F_EXT renderbuffers, read as FLOAT, HALF_FLOAT_OES or the implementation's
  pair and never as bytes, blended into as EXT_float_blend has it. `WEBGL_depth_texture`: DEPTH_COMPONENT and
  DEPTH_STENCIL images of TEXTURE_2D's level 0 made without data, never sub-uploaded, copied or mipmapped, sampled as
  luminance through any filter (the driver is given NEAREST, the filtering ES 3.0 has for depth). `EXT_sRGB`: sRGB
  textures, attachments and SRGB8_ALPHA8 renderbuffers. `EXT_blend_minmax`, `OES_fbo_render_mipmap`, and
  `OES_standard_derivatives`, `EXT_shader_texture_lod` and `EXT_frag_depth` where the driver compiles them. Each adds
  nothing before it is enabled: its formats, types, renderbuffer formats, attachment queries (COMPONENT_TYPE,
  COLOR_ENCODING), equations, hint and levels are refused as a browser refuses them.
- WebGL: `OES_texture_float_linear`, where the driver filters 32-bit float textures. Without it such a texture is
  incomplete -- sampled as (0, 0, 0, 1) -- while a filter of it is not NEAREST, as WebGL specifies: the driver
  underneath filtered it regardless wherever it could. The filters judged are those of the sampler bound to the unit
  where WebGL 2 has one, else the texture's; the image judged is the base level's; every texture target is judged,
  as WebGL 1's rule for textures whose size is not a power of two now is too. With the extension and
  EXT_color_buffer_float, a 32-bit float texture may be mipmapped.
- WebGL: `EXT_texture_filter_anisotropic`, where the driver filters anisotropically: TEXTURE_MAX_ANISOTROPY_EXT on a
  texture or a sampler, from 1 to MAX_TEXTURE_MAX_ANISOTROPY_EXT (INVALID_VALUE outside it, INVALID_ENUM before the
  extension is enabled). `WEBGL_debug_renderer_info`, whose UNMASKED_VENDOR_WEBGL and UNMASKED_RENDERER_WEBGL are the
  driver's strings, answered whether or not it is enabled, as browsers now answer them; VENDOR and RENDERER are a
  browser's masked "WebKit" and "WebKit WebGL". WebGL 2's `EXT_color_buffer_half_float`, where the driver renders to
  the 16-bit float formats: R16F, RG16F and RGBA16F become colour-renderable, and EXT_color_buffer_float's other
  formats do not.
- WebGL 2: `EXT_color_buffer_float`, offered where the renderer's driver renders to float colour buffers (ES 3.2, or
  `GL_EXT_color_buffer_float`). Enabled, R16F, RG16F, RGBA16F, R32F, RG32F, RGBA32F and R11F_G11F_B10F are
  colour-renderable: a framebuffer with such an attachment is complete, a renderbuffer and a copy may have such a
  format, and a 16-bit float or R11F_G11F_B10F texture may be mipmapped. three.js's half-float render targets -- PMREM
  environment maps among them -- rely on it; without it, the framebuffer completeness the facade now judges refused
  them. The renderer's WebGL capabilities reach the facade as one bitfield, `op_webgl_query_gpu_caps` (bit 2
  for float colour buffers), on both lanes.
- Performance+: `readPixels` reads the pair the call names -- RGBA/FLOAT from a float colour buffer, RGBA_INTEGER from
  an integer one, the driver's own IMPLEMENTATION_COLOR_READ pair -- as the in-process runtime does; it read
  RGBA/UNSIGNED_BYTE only and refused every other pair as INVALID_OPERATION, so a float or integer framebuffer could not
  be read at all. The synchronous readback's reply is sized by the pair (`frame_wire::sync::readback_bytes_per_pixel`,
  the one table both lanes size reads by; the producer's copy is held to it pair by pair).
- WebGL: `WEBGL_compressed_texture_etc1`, with an object of its own carrying `COMPRESSED_RGB_ETC1_WEBGL`. The name used
  to answer with the ETC2/EAC object, so content asking for ETC1 found ten other formats and not its own, and an ETC1
  upload was INVALID_ENUM. Its blocks are uploaded as ETC2 RGB8, which decodes every ETC1 block to the same texels and
  which every GLES 3.0 driver has. An ETC1 image is a 2D one defined whole, by `compressedTexImage2D` or 2D immutable
  storage; a sub-image upload, a 3D call or 3D storage of it is INVALID_OPERATION, as a browser has it.
- Canvas 2D: `Path2D`, `roundRect`, and `isPointInPath` / `isPointInStroke`. A `Path2D` is built by the `CanvasPath`
  calls, from another path, or from SVG path data (every command, relative and implicit forms, reflected control
  points and arcs, read up to the command holding the first error), and `addPath` adds another under a
  `DOMMatrix2DInit`. `fill`, `stroke` and `clip` take one, resolved as WebIDL resolves their overloads; a fill rule that
  is not `nonzero` or `evenodd` is now the TypeError a browser raises rather than nonzero. The path travels with each
  use as the segments its calls made, is drawn through the transform current at the use, and leaves the current
  default path alone; one longer than a stream buffer goes by op behind a flush. `roundRect` converts its radii -- a
  number, a point, or a list of one to four -- and checks them in the specification's order, and the renderer scales
  radii that would overlap and mirrors a negative size. The hit tests are answered by the renderer, which holds the
  path, the transform and the line styles, after everything recorded before them: points on an edge are inside, a
  stroke's outline is traced under the line width, caps, joins and dashes, and a singular transform answers false.
  They were stubs that answered false, `roundRect` threw, and `fill(path)` filled the current default path. On
  Performance+ the records are new in the 2D block (573 to 576) and the hit test is synchronous operation 14, whose
  body is bounded by the longest path a record carries rather than the 4096 bytes of a fixed-argument call.
- WebGL 2: `texImage2D` and `texSubImage2D` from an offset into the bound PIXEL_UNPACK_BUFFER (`..., format, type,
  offset)`). The number used to be a TypeError, as WebGL 1 has it. The four `tex*Image*` uploads now name one source
  -- the call's bytes or the buffer from an offset -- in the command, the ops and the Performance+ records alike, and
  the renderer hands the driver the offset only with a buffer bound. Refused before anything is sent, in a browser's
  order: no buffer bound, or UNPACK_FLIP_Y_WEBGL / UNPACK_PREMULTIPLY_ALPHA_WEBGL set, is INVALID_OPERATION before
  the other arguments are judged; then a negative offset is INVALID_VALUE, and an offset past 2^31 - 1, not a multiple
  of the type's size or reading past the buffer is INVALID_OPERATION.
- WebGL 2: `getBufferSubData`. The bound buffer's bytes, read into a range of the destination view (`dstOffset` and
  `length` in its elements), after the GPU work that writes them -- a copy, transform feedback, a `readPixels` into a
  pack buffer, which is how three.js reads render targets back asynchronously. The renderer maps the range for reading
  (GLES has no glGetBufferSubData); the embedded op copies the bytes straight into the destination, and the
  Performance+ lane asks for them through the new synchronous operation 13, in parts of at most one reply (16 MiB)
  each. Everything the specification refuses is refused before anything is asked, against the buffer state the facade
  keeps: no buffer bound or transform feedback active on its target (INVALID_OPERATION), a negative offset or a range
  past the view or the buffer (INVALID_VALUE).
- Canvas: `toDataURL(type, quality)` on every canvas, 2D and WebGL. It writes a PNG, or a baseline JPEG for
  `image/jpeg` (`quality` in [0, 1], default 0.92); any other type is answered with a PNG, an empty canvas with
  `data:,`, and a canvas that was never given a context is a transparent image. The encoders are JavaScript
  in the engine's own web layer, so the embedded runtime and the iOS Performance+ producer run one
  implementation and no native encoder is linked in. A WebGL canvas is read from its drawing buffer, right
  way up, with alpha un-premultiplied unless the context asked for premultiplied. PNG is written as RGB
  when nothing is transparent, with per-row filters and LZ77 matching; JPEG is 4:4:4 with Huffman tables
  built for the image. `scripts/test-canvas-image-encode.sh` decodes what they write (PNG through Node's
  inflate, JPEG through a decoder that shares no code with the encoder) and is a CI step.
- WebGL: `vertexAttrib{1,2,3,4}f`, `vertexAttrib{1,2,3,4}fv`, `getVertexAttrib` and `getVertexAttribOffset` on WebGL 1 and 2,
  and `vertexAttribI4i`, `vertexAttribI4iv`, `vertexAttribI4ui`, `vertexAttribI4uiv`, `vertexAttribIPointer` and
  `isVertexArray` on WebGL 2. They were `TypeError: not a function`: three.js writes the default value of an attribute a
  geometry lacks (a `ShaderMaterial` with `defaultAttributeValues`) with `vertexAttrib*fv`, and engines read an
  attribute's array state back with `getVertexAttrib`. All four arities of the constant-value call cross as one record
  (the components a call leaves out are 0, 0, 0, 1), the integer forms as two, and the integer pointer as a fourth; the
  render side keeps `vertexAttribPointer` and `vertexAttribIPointer` apart when it skips a repeated call. The queries are
  answered from a per-vertex-array-object shadow of what the calls set. A list that is too short is INVALID_VALUE and
  changes nothing; an integer pointer with FLOAT is INVALID_ENUM. Opcodes 59..62 (the GL block's fixed range is now
  1..=62); in the embedded runtime, the Performance+ producer and the frame decoder alike.
- WebGL 2: `uniform{1,2,3,4}ui`, `uniform{1,2,3,4}uiv` and the non-square matrices `uniformMatrix{2x3,2x4,3x2,3x4,4x2,4x3}fv`.
  They were `TypeError: not a function`; an engine that uploads an unsigned-integer uniform (a `uvec` / `uint` the shader
  takes, an instance or bone index) or a `mat3x4` skinning palette had no way to do it. The unsigned vectors cross as
  records of their own with the words kept unsigned (a value above 2^31 is not negative), the matrices keep their
  `transpose` flag, and a list is taken with WebGL 2's `srcOffset` / `srcLength`. A list that is empty, not a whole
  number of elements, or shorter than `srcOffset` / `srcLength` ask for is INVALID_VALUE and sends nothing; a value
  that is not a list is a `TypeError`. Opcodes 267..=276 (the variable block is now 256..=276), in the embedded
  runtime, the Performance+ producer (including its decode-budget estimate) and the frame decoder alike.
- WebGL 2: `clearBufferfv`, `clearBufferiv`, `clearBufferuiv`, `clearBufferfi` and `drawRangeElements`. They were
  `TypeError: not a function`; three.js, Babylon.js and PlayCanvas clear a render target with an integer colour format
  with `clearBufferuiv` / `clearBufferiv` (`clear` writes floats, which an integer buffer cannot take), and PlayCanvas
  clears each colour attachment of a multiple-render-target framebuffer by its draw buffer. Each clear crosses as one
  fixed record (opcodes 63..=66; the GL block's fixed range is now 1..=66) with its values typed as the call types
  them, and it honours the scissor and the write masks as `clear` does; on the shown canvas it damages what the
  `clear` of the same buffers would (COLOR in draw buffer 0 only). A buffer the call does not take is INVALID_ENUM, a
  draw buffer that is negative, at or past `MAX_DRAW_BUFFERS` for COLOR, or other than 0 for DEPTH / STENCIL /
  DEPTH_STENCIL is INVALID_VALUE, and so is a list with fewer elements than the buffer needs after `srcOffset`; a value
  that is not a list is a `TypeError`, raised before any of them. `drawRangeElements` draws what `drawElements` does
  (its range is a hint), and an `end` below `start` is INVALID_VALUE. In the embedded runtime, the Performance+ producer
  and the frame decoder alike. Not yet: INVALID_OPERATION for a clear whose type does not match the draw buffer's
  format (an integer buffer cleared with `clearBufferfv`), which needs the facade to know each attachment's format;
  until then such a clear is what the driver makes of it (OpenGL ES leaves it undefined).
- WebGL: `copyTexImage2D` and `copyTexSubImage2D` (WebGL 1 and 2), and WebGL 2's `copyTexSubImage3D`,
  `copyBufferSubData`, `framebufferTextureLayer` and `invalidateSubFramebuffer`. They were `TypeError: not a function`:
  three.js copies the framebuffer into a texture for its transmission pass every frame, and renders into the layers of
  array textures. The copies are fixed records of the GL block (opcodes 67..=70; its fixed range is now 1..=70) -- only
  their arguments cross, the GPU reads the read framebuffer or the buffer -- and `copyTexImage2D` charges the level it
  defines to the GPU budget as `texImage2D` does. `framebufferTextureLayer` and `invalidateSubFramebuffer` are ops in
  process and records 175 and 206 of the resource block on the Performance+ lane, with the layer answered back by
  `getFramebufferAttachmentParameter(..., FRAMEBUFFER_ATTACHMENT_TEXTURE_LAYER)`. A target or format a call does not
  take is INVALID_ENUM (a depth or stencil format for `copyTexImage2D` INVALID_OPERATION; WebGL 1 takes only the five
  unsized formats), a negative level, offset, size or layer, a nonzero border and a cube face that is not square
  INVALID_VALUE; `copyBufferSubData` converts its offsets and size as WebIDL's `long long` and refuses one past 2^31,
  which no buffer reaches, rather than wrapping it into range.
- WebGL 2: `compressedTexImage3D`, `compressedTexSubImage3D`, `waitSync` and `isSync`, and WebGL 2's overloads of
  `compressedTexImage2D` / `compressedTexSubImage2D`. A compressed upload now takes either a view -- its elements from
  `srcOffset`, `srcLengthOverride` of them unless that is 0, counted in the view's own element size -- or an
  `imageSize` / `offset` pair naming a range of the bound PIXEL_UNPACK_BUFFER; the 2D records carry that range too
  (resource records 198/199 grew two words, 207/208 are new). glow takes `glCompressedTexImage2D/3D`'s data only as a
  slice, so for the buffer form the renderer resolves those two entry points once and passes the offset where GL reads
  it. A view range past its end, a negative `imageSize` and a buffer offset that is negative or past 2^31 are
  INVALID_VALUE. A compressed 3D level is charged to the GPU budget at its image size. `fenceSync` returns a real
  `WebGLSync` object: `isSync` answers for it, `deleteSync` marks it, and `waitSync` / `clientWaitSync` on a deleted one
  are INVALID_OPERATION; `waitSync` takes only flags 0 and `TIMEOUT_IGNORED` (INVALID_VALUE otherwise) and a value that
  is not a `WebGLSync` is a TypeError. Resource record 176 on the Performance+ lane.
- WebGL: `getAttachedShaders`, `detachShader`, `validateProgram` and `sampleCoverage`, and WebGL 2's `isSampler`,
  `getSamplerParameter`, `getIndexedParameter` and `getSyncParameter`. The queries answer from what the calls did, as
  `getVertexAttrib` does, so none crosses: what a program has attached (ATTACHED_SHADERS too -- a cached count used to
  go stale on the next attach), what `samplerParameter*` set (from ES 3.0's initial values), what `bindBufferBase` /
  `bindBufferRange` bound (uniform buffers on the context, transform feedback buffers on the bound transform feedback
  object, a deleted buffer unbound from both), and a fence's type, condition and flags (its status is a poll of the
  fence, as `clientWaitSync` answers). To keep those answers true the calls refuse what GL would: attaching a shader
  already attached or of a type already attached, detaching one that is not, a deleted program, shader or sampler
  (INVALID_OPERATION); a sampler parameter it does not have or a value it does not take (INVALID_ENUM; an enum set
  through `samplerParameterf` takes the nearest integer); an indexed binding point past the maximum and a uniform
  buffer offset off UNIFORM_BUFFER_OFFSET_ALIGNMENT (INVALID_VALUE -- the driver's error would not have reached
  `getError`). `createShader` of a type that is not one is INVALID_ENUM and null, where it returned a shader the
  render side had silently dropped. Samplers are `WebGLSampler` objects. `sampleCoverage` is GL record 71;
  `detachShader` and `validateProgram` are resource records 177 and 178, and `validateProgram` drops the cached
  VALIDATE_STATUS. The render side keeps its list of attached shaders (the shader-cache key at link time) in step
  with a detach.
- WebGL: `getUniform`, and WebGL 2's `getFragDataLocation`. Each asks the driver through the generic state query
  (`frame_wire::sync::gl_state` 7 and 8; `program_state.rs` answers), so nothing new crosses on either lane. A
  uniform's value is typed as WebGL types it -- a number or a boolean for a scalar, a Float32Array, Int32Array,
  Uint32Array or Array of booleans for a vector or a matrix -- and a float crosses as its bits, so it is the float
  the uniform holds (NaN and the infinities included). A uniform location now belongs to its program and the name
  it was looked up by until the program links again: `getUniform` with another program's location, or one from before
  a relink, is INVALID_OPERATION and null, as are a deleted program and one that did not link.

### Fixed
- WebGL: `texImage2D`/`texImage3D` with no data zero-fills the image's storage instead of handing the driver a null
  pointer and trusting it to initialize to transparent black (ES 3.0 3.7.2, WebGL 1.0/2.0): reusing a texture object's
  storage at a level it previously held a smaller image at left that image's bytes visible at the equivalent offsets
  of the new one.
- Workers: a message crossing between the main thread and a worker is no longer logged -- on either side, in Rust or
  JavaScript. A game posting every frame paid a log line per message, and the worker side logged the whole message
  body, content's data. A worker's lifecycle (created, loaded, exited, failed) is still logged, once.
- WebGL 2: `blitFramebuffer` refuses what ES 3.0 4.3.3 and WebGL 2.0 5.38 refuse, in a browser's order, before anything
  reaches the driver, whose errors never reach `getError`: a filter of none (INVALID_ENUM), a mask of other bits
  (INVALID_VALUE), LINEAR with depth or stencil (INVALID_OPERATION); then, framebuffers complete, the same image read
  and written -- one framebuffer, two framebuffers sharing an image, or the drawing buffer onto itself --, integer
  data with LINEAR or against other data, depth or stencil of another format or that the read framebuffer lacks, colour
  from no read image, a multisampled draw framebuffer, and from a multisampled read framebuffer another format or
  rectangle (INVALID_OPERATION). Only the framebuffers' completeness was judged.
- WebGL: a program or shader is deleted as GL deletes it, once nothing uses it, as browsers have it: the current program
  stays and answers every call until another is made current -- though it is not made current again nor takes a shader
  (INVALID_OPERATION) -- and a shader stays while attached to a program that is there; `isProgram` and `isShader`
  answer whether it is. Once gone, every program or shader call that takes it is INVALID_VALUE (it was
  INVALID_OPERATION, or no error at all, or a stale answer); another context's is INVALID_OPERATION; a value that is
  no program or shader, null among them, is a TypeError, where calls took it silently. Deleting another context's
  object is INVALID_OPERATION. The renderer deletes a program or shader when it goes, and not before: it dropped the
  program in use at once, so the draws until another was made current, and every query of it, named a program it no
  longer had (on Performance+ the queries threw).
- WebGL: the calls a browser refuses are refused, each with its error, where they reached the driver or were taken: an
  attribute index past MAX_VERTEX_ATTRIBS on every attribute call (INVALID_VALUE); WebGL 2's attribute types in WebGL 1
  (INVALID_ENUM); an attribute offset or stride off its type's size, a packed type not of size 4, an offset into no
  buffer (INVALID_OPERATION; WebGL 2's packed attribute types are taken, where the decoder refused them); a gl_ name
  bound (INVALID_OPERATION); an attribute or uniform location looked up in a program never linked, or whose link
  failed (INVALID_OPERATION); a program or shader parameter WebGL does not have (INVALID_ENUM and null); a fence of
  another condition (INVALID_ENUM) or flags (INVALID_VALUE); more storage levels than the size has (INVALID_OPERATION,
  was INVALID_VALUE); a transform feedback buffer mode of none (INVALID_ENUM) or more separate varyings than there are
  bindings (INVALID_VALUE). A detach ignores the textarget, and in WebGL 2 the level, as ES 3.0 has it, and gives the
  driver TEXTURE_2D's level 0.
- WebGL: every state setter refuses what WebGL refuses, and the state stays as it was: a comparison function, face,
  stencil operation or blend factor that is none (INVALID_ENUM), a constant colour factor with a constant alpha one
  among the colour factors (INVALID_OPERATION), SRC_ALPHA_SATURATE as a WebGL 1 destination factor (INVALID_ENUM), a
  line width not above 0 (INVALID_VALUE), a depth range from far to near (INVALID_OPERATION). These reached the driver,
  whose error never reaches `getError`. The rules are the decoder's, for the stream and the raw call alike.
- WebGL: `getParameter` answers its version's parameters and the enabled extensions' only -- WebGL 2's in a WebGL 1
  context, MAX_DRAW_BUFFERS and MAX_COLOR_ATTACHMENTS without WEBGL_draw_buffers, and any parameter of no WebGL are
  INVALID_ENUM and null, where the driver answered a number -- and as WebGL types each: Float32Array, Int32Array and
  booleans where it answered plain arrays, MAX_VIEWPORT_DIMS as both dimensions where it answered one, the GLint64
  limits (MAX_ELEMENT_INDEX and the rest) whole, the stencil masks as the GLuint set where a driver clamps them, and
  COMPRESSED_TEXTURE_FORMATS as the formats of the compressed-texture extensions enabled.
- WebGL: an attachment of a texture level is complete only as ES 3.0 4.4.4.2 has it -- from the base level to the last
  a full chain has, a level other than the base only of a mipmap-complete texture, a cube map's face only of a
  cube-complete one -- and setting TEXTURE_BASE_LEVEL or TEXTURE_MAX_LEVEL judges framebuffers again; the facade had
  called such framebuffers complete. A copy into a depth or depth-stencil image is INVALID_OPERATION; it was let
  through to the driver. IMPLEMENTATION_COLOR_READ_FORMAT and _TYPE of a read framebuffer that cannot be read are
  INVALID_OPERATION and null, as they are in browsers.
- WebGL 1: `blendEquation`, `blendEquationSeparate` and `hint` refuse what they do not take (INVALID_ENUM): MIN and MAX
  before EXT_blend_minmax, an unknown equation, target or mode reached the driver, which took MIN and MAX and logged the
  rest. `readPixels` takes WebGL 1's formats and types only, and a view of another type than the type is
  INVALID_OPERATION before the read buffer is judged, as browsers order them. `getFramebufferAttachmentParameter`
  answers WebGL 1's targets, attachment points and parameters only (READ_FRAMEBUFFER, the attachment sizes and
  TEXTURE_LAYER are WebGL 2's; a colour attachment past the context's is INVALID_ENUM), refuses the default framebuffer
  (INVALID_OPERATION), and COMPONENT_TYPE of DEPTH_STENCIL_ATTACHMENT is INVALID_OPERATION in either version.
- WebGL: every extension a context offers is one table's -- the registry's names, the WebGL versions that have each and
  the renderer capability each needs -- which both `getExtension` and `getSupportedExtensions` read. A WebGL 2 context
  offered WebGL 1's ANGLE_instanced_arrays, OES_vertex_array_object, WEBGL_draw_buffers and OES_element_index_uint,
  which are WebGL 2 core and which no browser offers it; every context answered `EXT_instanced_arrays` and
  `WEBGL_instanced_arrays`, names the registry does not have. Names now compare case-insensitively, as WebGL specifies,
  and a lost context answers null to both calls. A driver's VENDOR, RENDERER, VERSION or SHADING_LANGUAGE_VERSION
  string is escaped in the renderer's answer: a quote or a backslash in one made the answer unreadable.
- WebGL: the default framebuffer's draw and read buffers. Where the engine's drawing buffer stands in for the default
  framebuffer -- an FBO, which takes no BACK --, `drawBuffers([BACK])` and `readBuffer(BACK)` reached the driver as BACK
  and were refused there, unseen: once content had turned either to NONE it could not turn it back, and drew or read
  nothing until the context was lost. BACK now names the drawing buffer's colour attachment. The engine's own use of
  that buffer no longer depends on the content's choice: the present reads the colour attachment, and the clear a
  present owes clears it, whatever read or draw buffer the content set. `drawBuffers` (and WEBGL_draw_buffers'
  `drawBuffersWEBGL`) is judged before anything is sent, in a browser's order -- a value that is not NONE, BACK or a
  colour attachment the context has INVALID_ENUM, more than MAX_DRAW_BUFFERS INVALID_VALUE, anything but BACK or NONE
  alone for the default framebuffer and anything but COLOR_ATTACHMENTi or NONE at place i for an object
  INVALID_OPERATION -- and recorded per framebuffer, which is what `DRAW_BUFFERi` answers without crossing (the driver
  answered COLOR_ATTACHMENT0 for the default framebuffer's, which is BACK).
- WebGL 2: `EXT_float_blend`, offered where the renderer's driver blends into 32-bit float colour buffers (desktop GL,
  or GL ES with `GL_EXT_float_blend`) and enabled with EXT_color_buffer_float there, as a browser enables it. Where it
  cannot, a draw with blending on and a 32-bit float image among its draw buffers is INVALID_OPERATION, as the
  extension specifies; the driver refused it unseen, or blended undefined values.
- WebGL: an upload whose pixels are a TexImageSource -- a decoded image or `ImageBitmap`, a canvas, `getImageData`'s
  snapshot, `ImageData` -- is converted to the call's format and type as WebGL defines it, on both lanes and for every
  source alike. The renderer copied an 8-bit RGB(A) image of a source it held on the GPU and handed every other
  combination the source's RGBA8 bytes as though they were already the asked-for format: an RGB, a packed
  (`UNSIGNED_SHORT_5_6_5`, `4_4_4_4`, `5_5_5_1`, `2_10_10_10_REV`, `10F_11F_11F_REV`), a half-float or float, a
  LUMINANCE / ALPHA / RG / RED / integer upload came out garbled, an sRGB or float destination of a GPU-held source was
  refused by the copy, and `ImageData` was read with the content's UNPACK_ALIGNMENT and UNPACK_ROW_LENGTH. Now the
  pixels are packed as a browser packs them -- a packed type truncated to its bits, `RGB10_A2` as `v * 1023 / 255`
  rounded down, 8-bit and packed destinations premultiplied in 8 bits, float ones in floating point --, a GPU copy is
  kept for a source the renderer holds whenever it reproduces that exactly (no flip, no alpha change, a destination of
  the source's own 8-bit components), and a GPU-held source is read back otherwise, only the rows the call selects. WebGL
  2's selection applies: `UNPACK_SKIP_PIXELS` / `UNPACK_SKIP_ROWS` and the call's size select a rectangle of the source
  as `UNPACK_FLIP_Y_WEBGL` leaves it, and a selection past the source is INVALID_OPERATION. `texImage3D` and
  `texSubImage3D` take a TexImageSource, sliced by `UNPACK_IMAGE_HEIGHT` from `UNPACK_SKIP_IMAGES`; they uploaded
  nothing. The WebGL 2 table of TexImageSource formats (3.7.6) is enforced in a browser's order -- an internal format
  outside it INVALID_VALUE, a format or type outside it INVALID_ENUM, a combination it does not list
  INVALID_OPERATION --, and a value that is no TexImageSource is a TypeError, as is one in WebGL 1's 9-argument form.
  One record and one op carry every such upload (`TEX_IMAGE_SOURCE`, `op_tex_image_source`), replacing six.
- WebGL: a framebuffer object's completeness is judged by the facade, from the attachments, texture images and
  renderbuffer storage it records (ES 3.0 4.4.4, WebGL 1.0 6.6): no attachment, an attachment that is not an image its
  point can render to (a LUMINANCE texture, a float format while EXT_color_buffer_float is not enabled, a renderbuffer without
  storage), WebGL 1's attachments of different sizes or a depth and a stencil attachment that are not one image, and
  renderbuffers of different sample counts. `checkFramebufferStatus` answers those without crossing, and asks the
  driver once a configuration the rules allow, for the combinations an implementation may still refuse. A draw, clear,
  `clearBuffer*`, `readPixels`, copy or blit with a framebuffer that is not complete is INVALID_FRAMEBUFFER_OPERATION
  and nothing is sent: the driver refused such a call too, but its error never reached `getError`, and the facade had
  recorded what the call defined as if it had run. INVALID_FRAMEBUFFER_OPERATION is now recorded as itself; the
  facade's error queue turned it into INVALID_OPERATION. A copy takes only a read buffer it can convert (ES 3.0 table
  3.15, as a browser enforces it) -- a component the destination has and the source lacks, another component type,
  colour encoding or component size is INVALID_OPERATION -- and a float destination is INVALID_ENUM without
  EXT_color_buffer_float enabled. `readBuffer` is recorded and checked (BACK or NONE for the
  default framebuffer, NONE or a colour attachment for an object) and READ_BUFFER answers from it; a read from NONE is
  INVALID_OPERATION. The attachment calls are judged before they are recorded: the target and attachment point, a
  texture of the textarget's kind, a level of 0 in WebGL 1, a renderbuffer that has been bound, a 3D or 2D-array
  texture for a layer, and the default framebuffer, which takes none.
- Canvas 2D: the current default path keeps each point where the transform current when it was added put it, as the
  specification has it. A path built and then filled after a `translate` was drawn through the later transform. The
  renderer keeps the path in the space it was built in and moves it once, when it is next used, so a `save()` /
  `translate()` / `restore()` around a path costs it nothing.
- Canvas 2D: `ellipse` continues the subpath the line to its start is in. Its arc began a contour of its own, so
  `moveTo(centre); ellipse(...); closePath(); fill()` filled the chord, not the pie.
- WebGL: a drawing buffer is cleared after it has been presented unless the context asked for it to be preserved
  (`preserveDrawingBuffer`, WebGL 1.0 2.2) -- colour to transparent black, depth to 1, stencil to 0. It was never
  cleared: a frame that did not clear began on the frame before, a read after the present returned the presented frame,
  and under bypass (drawing straight into the window) a swap left the window's buffer undefined and content drew over
  whatever it held. The clear is owed from the end of a frame that drew into the screen -- when a browser hands the
  buffer to its compositor, so what content finds in its buffer depends on its own frames and never on when the render
  thread swapped -- until the content next draws into, clears or reads the default framebuffer, and made then, with the
  content's state put back: a frame that begins with a clear of its own pays only for the buffers that clear does not
  overwrite (one with no scissor, write mask or rasterizer discard in its way), and a frame that only reads leaves the
  screen as it was. A preserved buffer is never bypassed. This replaces the latch that turned bypass off for good at the
  first `readPixels` of the screen's default framebuffer (and the external-frame sessions' version of it, set from the
  start), which kept a frame's contents across its present so that a read arriving after it saw them: such a read now
  sees the cleared buffer, as the specification has it, and an in-frame read no longer costs a full-screen copy every
  frame after it. `clear` with a bit that names no buffer is INVALID_VALUE and clears nothing; it was sent. The bypass
  presentation probe asks for the window's buffers (`stencil: true`), which bypass has required since the drawing
  buffer took the context's attributes.
- Presentation: a frame is presented whole. A barrier -- the packet a synchronous call sends mid-frame so that it sees
  what was recorded before it -- presented the GL work it carried at the next tick, half a frame, whenever content asked
  a question between draws; Canvas2D barriers already did not. The frame's end presents what its barriers drew, in
  Canvas2D and WebGL alike, and when everything it drew went ahead in barriers its end is still sent (an empty
  presenting packet, in the embedded runtime and the Performance+ producer, where a frame with nothing left to carry
  sent none and was never presented). And a finished frame is presented before anything that arrives behind it runs: a
  task that drew between a frame's end and the tick that swaps it put its draws into the frame being shown. In the
  steady state nothing arrives in between, and frames are still presented on the frame clock.
- WebGL: `drawArraysInstancedANGLE` and `drawElementsInstancedANGLE` draw only with an attribute array enabled at
  divisor 0, and are INVALID_OPERATION without one, as ANGLE_instanced_arrays has it in WebGL 1 and a browser enforces
  it; a plain draw is not held to it. `vertexAttribDivisor` and `vertexAttribDivisorANGLE` refuse an index past
  MAX_VERTEX_ATTRIBS (INVALID_VALUE) instead of sending it.
- WebGL 2: sampler, transform feedback and query objects are the facade's, like every other object. SAMPLER_BINDING
  and TRANSFORM_FEEDBACK_BINDING answer with the object bound, not the driver's integer name, and a deleted sampler is
  unbound from every unit; `bindSampler` refuses a unit past the limit (INVALID_VALUE) and `bindTransformFeedback`
  another target (INVALID_ENUM) or a change while the bound object is active and not paused (INVALID_OPERATION). A
  transform feedback or query object was a plain object any `{_id}` could pass for, and an object of another kind is
  now WebIDL's TypeError. A query is begun on one target for good, one at a time a slot -- the two occlusion targets
  share one -- and `endQuery` with nothing active is INVALID_OPERATION, as is asking an active or never-begun query for
  its result. QUERY_RESULT_AVAILABLE is a boolean, false in the task that ended the query and the same every time it is
  asked within a task (WebGL 2.0 5.38); QUERY_RESULT is 0 until it is available, and the renderer asks the driver for a
  result only once it is, where it used to wait for the GPU to finish the query on the render thread.
- Canvas2D / WebGL: Skia's GL work runs in its own context. Skia does its GL work in whatever EGL context is current,
  and a cleanup is GL work -- it deletes textures and framebuffers -- but the periodic purge of every 2D context's
  unused resources (every 250 ms), the low-memory trim, and the re-capping of every context's share of the resource
  cache on each canvas create and destroy were all run with whatever context happened to be current, often a WebGL
  canvas's. A framebuffer name means a different object in every context, so the purge deleted that context's
  framebuffer of the same name: on the iPhone a WebGL canvas lost its DrawingBuffer (its context's framebuffer 1)
  mid-frame and drew into its 1x1 pbuffer from then on, a draw that read back as nothing. Purges and trims now go
  through one sweep that makes each `GrDirectContext`'s own context current -- the shared offscreen-2D context once,
  each other canvas's its own -- and a context installs its share of the cache when it is current for its own work
  (its flush, or the sweep), which also makes a canvas create or destroy cost nothing per live context. The image-copy
  framebuffer is keyed by the context that owns it, and a destroyed canvas's framebuffers, vertex arrays, queries and
  transform feedbacks go with its context instead of being deleted by name from the resource context, as teardown's
  are.
- WebGL: the drawing buffer has exactly the buffers the context's attributes ask for (WebGL 1.0 5.2), and
  `getContextAttributes()` says what it has. It used to be RGBA8 with a 24-bit depth and an 8-bit stencil buffer
  whatever was asked: a context without stencil (the default) had one, so a stencil test that cannot fail without a
  stencil buffer failed; `depth: false` kept a depth buffer; `alpha: false` read back the alpha it drew; the stencil
  attribute was reported true by default and antialiasing on, though the buffer is single-sampled. Each context's
  constructor now encodes its attributes ahead of its first command (frame-wire GL record 73, `WEBGL_CONTEXT`: alpha,
  depth, stencil, preserveDrawingBuffer bits), and the renderer allocates the colour buffer RGBA8 or RGB8 and the
  depth/stencil buffer as DEPTH24_STENCIL8, DEPTH_COMPONENT24, STENCIL_INDEX8 or none. The screen canvas's buffer takes
  the format (and keeps it across a recreated surface and a GPU reset); an offscreen WebGL canvas, which drew into its
  pbuffer's own framebuffer -- the EGL config's buffers, at the canvas's size -- gets a DrawingBuffer as its default
  framebuffer and its pbuffer shrinks to one pixel. Bypass (drawing straight into the window) is taken only for a
  context whose buffers are the window's own. Antialiasing, which the implementation may decline, is declined and
  reported off; stencil defaults to false. `WebGL2RenderingContext`'s constructor dropped its options entirely, so a
  WebGL 2 context ignored every attribute: it passes them on. A DrawingBuffer's same-size `canvas.width` assignment
  now clears it, as the specification resets the buffer on every assignment (the screen canvas's did not).
- WebGL: a draw reads no vertex outside the buffers of the attributes the program in use consumes, as the
  specification has it (WebGL 1.0 6.4-6.6, kept by WebGL 2): a consumed attribute enabled as an array with no buffer, or
  whose buffer does not hold every vertex the draw reads at the attribute's offset and stride -- or every instance an
  instanced attribute reads by its divisor -- is INVALID_OPERATION and nothing is drawn. It was sent, and a driver
  without robust access read past the buffer. An indexed draw is judged by its largest index: the facade keeps the
  bytes of every element-array buffer (WebGL lets one take data only through `bufferData`, `bufferSubData` and copies
  between element-array buffers) and caches the largest index of each range drawn until they change; in WebGL 2 the
  primitive-restart index reads no vertex. Which locations a program consumes comes with LINK_STATUS, asked once per
  link as the link's whole result (`gl_state::LINK_RESULT`), so it costs no crossing of its own. The draw's own
  arguments are judged first, in a browser's order: a mode that is no primitive, or an index type the context does
  not take (UNSIGNED_INT needs OES_element_index_uint in WebGL 1), is INVALID_ENUM; a negative first, count, instance
  count or offset INVALID_VALUE; an offset off the index size INVALID_OPERATION.
- WebGL 1: a texture whose level 0 is not a power of two each way samples as incomplete -- (0, 0, 0, 1) -- unless both
  its wraps are CLAMP_TO_EDGE and its minification filter reads no mipmap, as the specification has it (ES 2.0 3.8.2).
  The driver underneath is OpenGL ES 3.0, for which such a texture is complete, so it used to be sampled. The facade
  keeps which textures are incomplete and where each is bound, after every call that can change either (a level 0
  defined, a wrap or filter set, a bind, a delete); a draw holds no texture at those bindings -- texture 0, which WebGL
  never gives an image, so the driver samples it as incomplete -- and puts them back after it. A draw with none
  pays one size test; WebGL 2, whose rules are ES 3.0's, withholds nothing.
- WebGL: the facade keeps a record of every texture image -- each level of a 2D, 3D or 2D-array texture and of each
  face of a cube map, with its internal format, the format and type its data came in, its size and whether it is
  compressed or immutable -- and judges by it, before anything is sent, what used to reach the driver, whose errors
  never reach `getError`: an upload, copy or compressed upload into an image that is not there (INVALID_OPERATION),
  past its edge (INVALID_VALUE), from a (format, type) ES 3.0 tables 3.2/3.3 do not have with the image's internal
  format -- in WebGL 1 another format or type than the image was defined with -- (INVALID_OPERATION); immutable
  storage defined again by `texImage*`, `copyTexImage2D`, a compressed image or `texStorage*` (INVALID_OPERATION); a
  WebGL 1 mipmap level of a size that is not a power of two (INVALID_VALUE); `generateMipmap` of a base that is not
  there, empty, compressed, not colour-renderable and filterable, a WebGL 1 non-power-of-two, or a cube map whose faces
  are not all there and alike (INVALID_OPERATION), which starts at TEXTURE_BASE_LEVEL in WebGL 2 and whose levels are
  recorded. An image is recorded when the call that defines it is sent, so `copyTexImage2D` judges the decoder's
  rules here too, in the decoder's order, and the compressed uploads judge theirs: a format is one only while its
  extension is enabled (INVALID_ENUM), its data is whole blocks over the size (INVALID_VALUE), an upload into it
  starts on a block, ends on one or at the edge and is of its format (INVALID_OPERATION), and it is no 3D texture's
  (INVALID_OPERATION).
- WebGL 2: `texStorage2D` / `texStorage3D` take the compressed formats of an enabled extension, as three.js allocates a
  KTX2 texture before `compressedTexSubImage2D` fills it: they were INVALID_ENUM in the facade and refused by the GPU
  budget, so such a texture stayed black. The budget charges compressed storage in whole blocks, from one table of the
  WebGL compressed formats (`compressed_upload::compressed_block`) that the KTX2 path now reads too.
  `WEBGL_compressed_texture_astc` names all 28 formats of the LDR profile and has `getSupportedProfiles()`.
- WebGL: `texParameteri` / `texParameterf` refuse a parameter the context's textures do not have, or a value an enum one
  does not take (INVALID_ENUM; WebGL 1 has the filters and the two wraps only), and a negative TEXTURE_BASE_LEVEL or
  TEXTURE_MAX_LEVEL (INVALID_VALUE); `getTexParameter` answers every WebGL 2 parameter (the LODs as the floats they were
  set to, TEXTURE_IMMUTABLE_FORMAT and TEXTURE_IMMUTABLE_LEVELS from `texStorage*`). A value is converted as WebIDL
  converts it and always encoded in the stream: `op_tex_parameteri`, `op_tex_parameterf` and `op_generate_mipmap`,
  reached only by a non-number argument (and then a TypeError), are gone.
- WebGL: a renderbuffer takes RGB565, which was INVALID_ENUM, and the formats are the specification's per interface
  (WebGL 1's six; in WebGL 2 the colour-renderable sized formats that need no extension, depth and stencil);
  `renderbufferStorage*` with no renderbuffer bound is INVALID_OPERATION. `texStorage*` refuses DEPTH_STENCIL, an
  unsized format the old list took for a sized one, and a cube map that is not square (INVALID_VALUE).
- WebGL 2: a 3D upload from a view while UNPACK_FLIP_Y_WEBGL or UNPACK_PREMULTIPLY_ALPHA_WEBGL is set is
  INVALID_OPERATION, as the specification has it (the flags are defined for 2D images); it was uploaded unflipped. An
  upload offset past 2^31 - 1 is INVALID_OPERATION, as a browser answers it; it was INVALID_VALUE.
- Performance+: a sub-image upload's record always names its source. A `texSubImage3D` record could name none, and the
  renderer then handed the driver a null pointer to read from.
- WebGL: texture uploads are checked as the specification checks them, before anything is sent; the driver's own
  error never reached `getError`, and an upload from a view shorter than its rows had the driver read past the view.
  In the order a browser judges them: an internal format the context does not have is INVALID_VALUE, a format or a
  type INVALID_ENUM (WebGL 1 has the unsized formats and their four types only), and a combination ES 3.0 tables 3.2
  / 3.3 do not have INVALID_OPERATION (a format other than the internal format in WebGL 1; RGBA8 from RGB in WebGL 2;
  a depth image in a TEXTURE_3D). Pixels from a view are INVALID_OPERATION when the view is not of the type's kind
  (a DataView, a Uint16Array for UNSIGNED_BYTE), or holds fewer bytes from its `srcOffset` than the pixel-store state
  lays the upload over -- rows padded to UNPACK_ALIGNMENT, UNPACK_ROW_LENGTH and UNPACK_IMAGE_HEIGHT in place of the
  size, the skips in front -- and only those bytes are copied. A sub-rectangle outside its data store (WebGL 2.0 5.35)
  is INVALID_OPERATION, `texSubImage2D` / `texSubImage3D` of null pixels INVALID_VALUE, an array or another value
  that is not a view a TypeError. `texSubImage2D` and `texSubImage3D` take the format and type pairs of either table.
- WebGL 2: `texImage2D` / `texSubImage2D(..., srcData, srcOffset)` upload from `srcOffset`; they uploaded from the
  start of the view. A `srcOffset` past the data of a 3D upload is INVALID_OPERATION ("not enough data"), as the
  specification has it; it was INVALID_VALUE.
- WebGL 2: an upload names its pixels or a PIXEL_UNPACK_BUFFER, never both. With a buffer bound, an upload from a view
  (or of null pixels) is INVALID_OPERATION; an upload from an offset needs a buffer bound and its range inside the
  buffer, at a multiple of the type's size (INVALID_OPERATION), for the 3D and the compressed uploads alike. The
  renderer refuses an offset with no buffer bound as well, asking the driver: GL would read the offset as an address,
  and on the Performance+ lane it comes from another process. It refuses bytes fewer than the upload's rows read, by
  the same layout.
- WebGL: `pixelStorei` refuses what GL does not have -- a pname the context lacks (INVALID_ENUM; the row lengths and
  skips are WebGL 2's), an alignment other than 1, 2, 4 or 8 or a negative length or skip (INVALID_VALUE), a
  colour-space conversion other than NONE or BROWSER_DEFAULT_WEBGL (INVALID_ENUM) -- and changes nothing when it does.
  `getParameter` answers the pixel-store state as it was set, without asking the driver.
- Renderer: the pixel-store shadow keeps what content set apart from which values the driver is known to hold. A Skia
  boundary forgot both, so an upload after it would have read its rows -- and its flip and premultiply flags -- as the
  defaults; today only 2D canvases cross that boundary, so no upload was affected.
- WebGL: texture, framebuffer and renderbuffer objects behave as GL's do. A texture keeps the target it was first
  bound to (another is INVALID_OPERATION), a call on a texture -- every `tex*` upload, copy, storage and parameter
  call, `generateMipmap`, `getTexParameter` -- needs one bound (INVALID_OPERATION) to a target the call takes
  (INVALID_ENUM: a cube map's face for an image, the cube map for a parameter), and `activeTexture` past
  MAX_COMBINED_TEXTURE_IMAGE_UNITS is INVALID_ENUM. `getParameter` answers TEXTURE_BINDING_3D and
  TEXTURE_BINDING_2D_ARRAY with the objects (they were the driver's integer names). `bindFramebuffer` takes
  READ_FRAMEBUFFER / DRAW_FRAMEBUFFER in WebGL 2 only and `bindRenderbuffer` RENDERBUFFER only (INVALID_ENUM), and
  none of the three binds a deleted object or another context's (INVALID_OPERATION) or a value that is not one
  (TypeError). A deleted texture leaves every unit and the framebuffers bound, a deleted renderbuffer RENDERBUFFER and
  the framebuffers bound, a deleted framebuffer its bindings (ES 3.0 D.1.2). `isTexture`, `isFramebuffer` and
  `isRenderbuffer` are false for an object never bound, as GL answers.
- WebGL: deleting the framebuffer a context has bound left the draws after it on name 0, not the canvas's drawing
  buffer, until content bound a framebuffer again: GL reverts a deleted binding to 0, which is not the default
  framebuffer of a canvas that draws into a DrawingBuffer. The renderer binds the default -- the DrawingBuffer where
  there is one -- before it deletes the object.
- WebGL: calls the specification refuses are refused before they reach the driver, whose error never reached
  `getError`. `enable` / `disable` / `isEnabled` of an enum that is not a capability -- RASTERIZER_DISCARD in WebGL 1
  among them -- are INVALID_ENUM; a negative `viewport` size is INVALID_VALUE, as a negative `scissor` size already
  was (the decoder reads the size as the signed GLsizei it is, for both lanes, and so does the op); `useProgram` of a program
  that did not link, was deleted or is another context's is INVALID_OPERATION and leaves the program in use as it was
  (whether a program linked is asked once per link and kept, as `getProgramParameter` keeps it), and a value that is
  not a program is a TypeError; a draw with no program in use is INVALID_OPERATION.
- WebGL: errors are flags, one per code (ES 3.0 2.5), as Chrome holds its own: a code raised by several calls before
  `getError` reads it is read once. The queue used to hold every one, so a single `getError` left the same error behind
  for the next. The facade's own refusals and the decoder's go into that one queue per context (the facade kept a
  second, ahead of it). Held as flags, the queue cannot grow past the codes there are, so its 256-entry cap, the
  overflow counter and the OUT_OF_MEMORY it planted are gone; the overflow slot of the debug statistics stays, at 0,
  so the layout the platform overlays read is unchanged.
- WebGL 2: `bufferData(target, view, usage, srcOffset, length)` and `bufferSubData(target, dstByteOffset, view,
  srcOffset, length)` upload the elements they name. Both used to ignore `srcOffset` and `length` and upload the whole
  view, so a partial attribute update -- three.js updates ranges of an attribute this way -- wrote the wrong bytes, or
  ran past the buffer and was dropped by the driver.
- WebGL: buffer state is kept for every target. `getParameter` answers COPY_READ / COPY_WRITE / PIXEL_PACK /
  PIXEL_UNPACK / UNIFORM / TRANSFORM_FEEDBACK_BUFFER_BINDING and VERTEX_ARRAY_BINDING with the objects (they were the
  driver's integer names), `getBufferParameter` answers on every WebGL 2 target (it was INVALID_ENUM there), the
  element array buffer is the bound vertex array object's and the generic transform feedback buffer the bound transform
  feedback object's (ES 3.0 tables 6.2 and 6.24). From that state the calls refuse what the specification refuses
  before anything is sent: a target the context does not have and a usage it does not take (INVALID_ENUM); nothing
  bound, a deleted buffer, an index buffer bound as other data or the reverse, a draw that would read past its index
  buffer or has none, and a `copyBufferSubData` between index and other data (INVALID_OPERATION); a negative size or
  offset, null data, a `bufferSubData` or `copyBufferSubData` range past its buffer and two overlapping ranges of one
  buffer (INVALID_VALUE). Deleting a buffer unbinds it from this context and from the vertex array and transform
  feedback objects bound, and an object not bound keeps it (ES 3.0 D.1.2). `isBuffer` and `isVertexArray` are false
  for an object never bound, as GL answers.
- WebGL 1: `OES_vertex_array_object` works. Its `bindVertexArrayOES` and `deleteVertexArrayOES` called WebGL 2
  methods a WebGL 1 context does not have, and threw. Vertex array objects are `WebGLVertexArrayObject` objects of the
  context that made them.
- WebGL: a name passed to `getUniformLocation`, `getAttribLocation`, `bindAttribLocation` or `getFragDataLocation`
  is checked as WebGL 1.0 6.20 and 6.21 say before GL is asked: longer than 256 characters (1024 in WebGL 2) or
  holding a character outside the GLSL ES source character set is INVALID_VALUE, and a name WebGL reserves (`webgl_`,
  `_webgl_`) finds nothing (`bindAttribLocation`: INVALID_OPERATION). They used to reach the driver.
- WebGL 2: a fence signals. Nothing submitted the commands of a context that only draws offscreen -- a frame flushes
  the canvas it presents -- so a fence made there never reached the GPU and `clientWaitSync` / SYNC_STATUS answered
  TIMEOUT_EXPIRED / UNSIGNALED for good. A browser flushes at the end of every task; here the task that makes a fence
  ends (its microtask checkpoint) by flushing each context that made one, once. Within that task the fence answers
  unsignalled without asking the render side, as the specification requires, so a loop cannot spin on it there.
  `flush()` and `finish()` now submit the context's commands (GL record 72) as well as sending what was recorded; on a
  lost context they do nothing. On the Performance+ lane Promise's statics in the engine's primordials are bound to
  Promise, as deno binds them: `PromiseResolve` threw there.
- WebGL: a `srcOffset` is converted as WebIDL's `unsigned long long` (`clearBuffer*`, the uniform lists, `texImage3D`
  / `texSubImage3D`, the compressed uploads). `>>> 0` wrapped it modulo 2^32, so an offset of 2^32 read from the start
  of the list instead of being INVALID_VALUE. `texImage3D` / `texSubImage3D` with a `srcOffset` past the end of the view
  are INVALID_VALUE, where they uploaded nothing in silence, and their PIXEL_UNPACK_BUFFER offset is refused when it is
  negative or past 2^31 rather than truncated to 32 bits.
- WebGL: a device limit the facade asks for (MAX_VERTEX_ATTRIBS, MAX_DRAW_BUFFERS, MAX_COLOR_ATTACHMENTS) is kept only
  once the context answers: `getVertexAttrib` used to keep the fallback it took from a lost context for good. An index
  below the minimum every implementation has (8 or 16 attributes, 4 draw buffers, 4 colour attachments) asks nothing.
- WebGL: `invalidateFramebuffer` checks the attachments it names against the framebuffer bound: the default one
  takes COLOR / DEPTH / STENCIL, a framebuffer object its attachment points, and a colour attachment past
  `MAX_COLOR_ATTACHMENTS` is INVALID_OPERATION; anything else is INVALID_ENUM, where the call used to reach the driver.
  On a canvas that draws into a DrawingBuffer (the framebuffer object that stands in for the default framebuffer),
  COLOR / DEPTH / STENCIL are now passed as that object's attachments: GL takes only attachment points on an object,
  so the invalidation the content asked for was an error the driver dropped.
- iOS (Performance+): the producer's decode-budget estimate charges a gradient's stops. `OP2D_SET_FILL_STYLE_GRADIENT`
  and `OP2D_SET_STROKE_STYLE_GRADIENT` carry a byte payload the host charges for, and the producer's table of payload
  records did not list them, so a frame of gradients was estimated below what the host admits it at.
  `scripts/test-render-opcode-agreement.sh` now holds the producer's payload-prefix tables to the Rust record specs,
  opcode by opcode, which is how this was found.
- iOS (Performance+): `getError()` returns the errors the producer found itself. A call the facade refuses before it is
  encoded -- an upload over the budget, `texImage2D` with a nonzero border, a `readPixels` into a short buffer -- is an
  error only the producer can know; it was recorded, and
  `op_webgl_get_error` never drained that queue, so none was ever returned. It answers from the producer's queue first
  (one per call, oldest first, without crossing) and then the host's.
- Storage: the last `setStorage` / `removeStorage` / `clearStorage` for a key is the one that wins. Async
  mutations ran as blocking SQLite writes on the scheduler's file-system pool, which has several workers, so two
  writes to one key could run at once and finish in either order: a burst of saves left an earlier value on
  disk (an iPhone 12 lost the last write for one or two of twenty keys every run; a Mac, whose writes finish in
  microseconds, never showed it). Mutations of one store now apply in the order they were asked for, by a chain
  per storage directory that holds no lock while it waits and that a cancelled write does not stop. The place in
  line is taken when the call is made -- in the order the host handled the requests -- and not when the task that
  runs it is first polled, so the spawn order of a multi-threaded runtime cannot reorder them. Both executions
  share the one path, so the embedded runtime gets the same guarantee.
- Canvas: a WebGL canvas can be the source of `drawImage(canvas, ...)` on a 2D context and of
  `texImage2D` / `texSubImage2D` on another WebGL context (or its own), where both read nothing:
  the renderer copies 2D canvases only, and a WebGL canvas's pixels are a drawing buffer. What the
  canvas shows now is read back (`readPixels`, ordered after the context's own commands) onto a 2D
  canvas the Canvas keeps for the purpose, and that is what is drawn or uploaded, right way up.
  p5.js's `filter()` was the real engine that hit it (it copies its WebGL layer back with
  `drawImage` and copies its own canvas into a framebuffer texture); its INVERT test is now
  green. This costs a readback and an upload per use -- right for a screenshot or a filter, not
  for a per-frame composite; a GPU-side copy is the optimisation if one is needed.
- iOS (Performance+): a synchronous call (`readPixels`, `getImageData`, `toDataURL`, a WebGL query) no longer
  waits out its 60-second deadline at random, and records are no longer lost. A finished packet shared its
  buffer with the writer until it was sent, and a barrier that waits for the frame window blocks in a
  synchronous request; a `FinalizationRegistry` callback that frees a collected canvas runs inside that
  request's `send`, and the destroy op it calls appended a record to the same buffer. It overwrote the
  packet's last word -- the host refused the packet for a pad that was not zero, every later packet was held
  behind the gap, and the call waited out its deadline -- or, when the packet ended on the alignment, was
  discarded by the next `reset()`, so the host never freed the canvas. The packet now owns its buffer from
  `finish`, the writer moves onto another (two alternate, so the frame loop still allocates nothing), and a
  record appended meanwhile is the next packet's. Found by `webgl-spec` dying on an iPhone 12 at a different
  place on every change to the test order; the test now appends a record from inside a blocking call and
  requires every packet well formed and the record delivered once.
- iOS (Performance+): `downloadFile` works. Every download failed with "Invalid mix of BigInt and other type
  in division": the host wrote the response's content length as a 64-bit integer, which the producer reads
  as a BigInt, where the embedded runtime hands the facade a Number (serde_v8's `u64` in a struct) and the
  download's progress divides by it. `uploadFile` reported the bytes sent as a BigInt for the same reason.
  The host now writes both as the file-system answers already were -- a Number while it is a safe integer --
  from one helper they all use, and a test holds each field.
- Canvas2D: colour strings are read as the specification has them, by one parser. `fillStyle`,
  `strokeStyle` and `shadowColor` read back the serialised colour (`#ff0000`, or
  `rgba(255, 0, 0, 0.5)`) instead of the string that was assigned; a string that is not a
  colour is ignored and leaves the previous style, where it used to be stored (and drew
  black); `addColorStop` with one throws a SyntaxError. `hsl()`, `hsla()`, `hwb()`, the
  modern space syntax (`rgb(10 20 30 / 50%)`), percentages, decimals and `none` are read;
  `lab()`, `lch()`, `oklab()`, `oklch()` and `color()` are not (ignored). The facade is now
  the only reader: there were three (the facade's, the host's `parse_color_string` behind
  `op_set_fill_style`, and the Performance+ producer's port), kept in step by a corpus test,
  and they read a narrower language than the one content writes. The renderer is only ever
  sent the colour, so the Rust parser, its named-colour table, the three string ops and the
  producer's port are gone.
- Canvas2D: freeing the copy a `createPattern(canvas)` pattern holds, and allocating its id,
  now go after everything the stream holds (the rule every op in the 2D facade follows; the
  test that holds it had been missed when the pattern change merged).
- iOS (Performance+): `decodeAudioData` works. It threw "audioData is detached or cannot be detached"
  for every buffer: the producer looked `structuredClone` up when it ran, and by then the engine's
  namespace handling had retired the Worker's own names. The platform primitive is now captured at
  load like the others, and a test removes the global and requires the transfer to still work. This
  was the whole audio path on iOS: Howler's WebAudio, Phaser's audio loader and three.js's AudioLoader.
- iOS (Performance+): `requestAnimationFrame`'s timestamp is on `performance.now()`'s timeline. The host
  stamped frames with its own clock, 115 ms apart from content's after a loading screen, so a
  frame's timestamp came out before the moment it was asked for and elapsed times went negative.
  Both clocks now count from the producer's start, and the host's stamps are put on that timeline
  with the smallest transport delay seen taken out of the gap (never later than the clock, never
  backwards, vsync spacing kept).
- Canvas2D: `createPattern` accepts a canvas, and a pattern does not paint past its tile
  on an axis it does not repeat. It returned null for a canvas -- so a tiled background
  built from an offscreen tile (Phaser's Canvas TileSprite, every hand-rolled 2D game)
  painted nothing -- and `no-repeat`, `repeat-x` and `repeat-y` stretched the tile's edge
  pixels over the rest of the canvas instead of leaving it transparent. A new 2D record
  (`CAPTURE_IMAGE`, 572, two words; `contracts/frame-wire/wire-v1.md`, amendment of
  2026-10-02) keeps a copy of the canvas as it is when the pattern is made -- what the
  canvas draws later does not reach the pattern -- in the image store under an id the
  facade allocates and destroys when the pattern is collected. The repetition is
  validated (SyntaxError), a canvas with no pixels is an InvalidStateError, `""` and
  `null` mean `repeat`.
- WebGL: a present no longer takes the content's framebuffer binding with it. The
  engine presents on the display clock, which can land between two of the content's
  GL batches, and the swap-time blit rebinds the read and draw framebuffers; it then
  re-pointed `FRAMEBUFFER` at the default one. Content that had a render target bound
  and keeps its own cache of what is bound (three.js, like most engines) did not
  rebind, and its next passes drew onto the canvas: `PMREMGenerator` returned a black
  environment map about one run in twelve, and a multiple-render-target scene
  intermittently read black. The present now puts back exactly the read and draw
  bindings it found. Found by running migo-conformance's engine bundles repeatedly;
  `engine-three-advanced` gains a scene that runs the generator 40 times with the
  frame clock ticking (24-28 black maps before, none after).
- Canvas2D: a `getImageData` whose pixels were read while the renderer presented could
  come back all zeros -- about one read in 400 on an iPhone 12 (Performance+), where
  the capture and the read of `.data` are a socket round trip apart. The renderer
  dropped its snapshot pool at every present; it now drops only the snapshots of
  content frames that have ended (a presenting packet), and keeps the frame being
  built. Found by running migo-conformance's suites on a device.
- Canvas2D: an `ImageData` from `getImageData` read after its frame has ended no longer
  reads as zeros. The facade captures a GPU snapshot and reads it back only on `.data`,
  and the renderer keeps a snapshot for one frame: a stored `ImageData` used for
  hit-testing was all transparent a frame later, on every platform (a 100 ms delay was
  enough). The frame-end pass now reads back every `ImageData` nobody has read and no
  `texImage2D`/`texSubImage2D` has consumed -- the readback a browser does at the call,
  taken when it can no longer be avoided; the text-label pattern the snapshots exist
  for (`texImage2D(imageData)` in the same frame) still costs no readback. The facade
  also counts snapshot bytes (32 MiB, half the render pool's 64 MiB) and reads eagerly
  past them: a capture the pool refuses reached nobody, and sixty 1 MiB captures read
  later would have been all zeros from the 64th megabyte on.
- iOS (Performance+): the on-screen canvas reports the size the renderer gives it
  (the surface in CSS pixels: 390 x 844 on an iPhone 12), not the physical surface
  (1170 x 2532). Content that sized its drawing from `canvas.width` drew at three
  times the canvas it was drawing on, and a `getImageData` past the real canvas
  came back empty. A size the content sets is still its own.
- iOS: an image that finished decoding before the on-screen 2D canvas had been
  drawn on or read left that canvas unwritable -- every later `fillRect`,
  `drawImage` and `putImageData` read back as transparent black -- until a WebGL
  context existed (measured on an iPhone 12, ANGLE Metal). The render thread now
  flushes its own queue after taking an upload from the upload thread. Found by
  running migo-conformance's `image-decode` suite on a device.
- Canvas2D: `drawImage` accepts a canvas as its source, in all three forms and
  including the canvas itself and the on-screen canvas. It silently drew nothing:
  the facade only knew images the host had decoded. A new 2D record
  (`DRAW_CANVAS`, 570) carries it; the renderer copies the source into a texture
  the destination can see, keeps that one copy for every later draw of the same
  unchanged canvas (200 draws of a 256x256 canvas took 264 ms with a copy per
  draw and 51 ms with the cache), and drops it when the source is painted on,
  resized or destroyed. Not yet: a WebGL canvas as a source, and `createPattern`
  with a canvas.
- Canvas2D: `fill("evenodd")` and `clip("evenodd")` honour the rule. The argument
  was dropped and every fill and clip was nonzero, so the holes of an even-odd
  shape (icons, rings, cut-outs) came out solid. Two new 2D records
  (`FILL_EVEN_ODD` 568, `CLIP_EVEN_ODD` 569) carry it
  (`contracts/frame-wire/wire-v1.md`, amendment of 2026-10-01).
- Canvas2D attributes now keep their previous value when assigned what the
  specification rejects, instead of storing it: `lineWidth`/`miterLimit` that is
  zero, negative or not finite, `globalAlpha` outside 0..1 or NaN (it was
  clamped), an unknown `lineCap`/`lineJoin`/`textAlign`/`textBaseline`, a negative
  `shadowBlur`, a non-finite shadow offset or `lineDashOffset`, a `setLineDash`
  list with a negative or non-finite entry. `lineWidth = NaN` used to read NaN and
  draw nothing. `getLineDash()` reports an odd list repeated to an even one
  (`[5]` is `[5, 5]`) and `shadowColor` reads `rgba(0, 0, 0, 0)` by default.
- Canvas2D methods that take numbers return without doing anything when an
  argument is NaN or infinite (`translate`, `scale`, `rotate`, `transform`,
  `setTransform`, the path methods, `fillText`/`strokeText`), as in a browser: a
  single `translate(NaN, 0)` used to poison the matrix and blank every later
  draw. `arc`, `arcTo` and `ellipse` with a negative radius throw
  `IndexSizeError`.
- `canvas.width` / `canvas.height` convert what they are given as an
  `unsigned long` does: `canvas.width = 1023.75` reads back 1023 (it read back
  1023.75 while the renderer used 1023), a numeric string works (it threw a
  `TypeError`), a negative number takes the default of 300 (it threw an engine
  error). `getContext` answers `null` for a different kind of context than the
  canvas already has, and `createImageData(imageData)` is accepted.
- `getImageData(0, 0, 0, 1)` threw a `ReferenceError` (`DOMException is not
  defined`) instead of `IndexSizeError`: the 2D context now carries its own
  `DOMException` where the host provides none.
- Canvas2D: `imageSmoothingEnabled` now does what it says. The property was never
  sent to the renderer -- assigning it left a plain value on the JavaScript object
  -- so every scaled `drawImage` was sampled bilinearly and pixel art came out
  blurred on every platform. `imageSmoothingEnabled = false` samples the nearest
  texel; it is saved and restored with the rest of the drawing state, survives a
  readback, and is reset by a canvas resize. `imageSmoothingQuality` is accepted
  and validated; all three levels draw the same. Found by the first run of
  migo-conformance on macOS, where a 2x display put the old assertion's sample a
  device pixel off a texel centre. A new 2D record (`SET_IMAGE_SMOOTHING`, 567) carries it
  (`contracts/frame-wire/wire-v1.md`, amendment of 2026-10-01).
- `putImageData` writes pixels. It was an empty function (`// Not implemented`): p5.js's `updatePixels`, EaselJS's filters
  and every game that edits pixels on the CPU changed nothing, found by running both libraries on the real host. The
  specification's algorithm (the dirty-rectangle form, negative extents, WebIDL `long` arguments) is in the 2D facade, and
  the rectangle -- cut to the ImageData's dirty part and the canvas's bounds, so nothing outside is sent -- goes to the
  renderer as one command that Skia's `writePixels` executes: it ignores the transform, the clip, `globalAlpha`, the
  composite operation and the shadow, as the specification says, and converts from `ImageData`'s unpremultiplied RGBA.
  In process it is `op_put_image_data` (a stream op); on the Performance+ lane it is the new record
  `OP2D_PUT_IMAGE_DATA` (571: `H x y width height byte_length | rgba`), written as bands of at most 1 MiB so a record stays
  inside a packet, decoded with the byte count checked against `width * height * 4`. The damage of a put is its exact device
  rectangle, whatever the drawing state. Tests: the renderer under a hostile state (transform, clip, alpha, composite),
  edge clipping, a short buffer (red before, green after), the decoder (order, pixels, wrong byte counts), the
  producer's banding (tiles the rectangle exactly), and the conformance `canvas2d-spec` assertions. Runtime-v8 JavaScript
  changed: joins the snapshot backlog.
- `onended` of `AudioBufferSourceNode`, `OscillatorNode` and `ConstantSourceNode` fires. The nodes accepted the handler and
  told the console that "onended callbacks are not dispatched by the native graph", so Phaser's `complete`, three.js's
  `onEnded` and every sound library that chains one sound after another never heard a sound end. The graph knew all along
  (`prune` already collects the finished source): setting a handler now asks the audio thread to watch that node
  (`op_audio_watch_source_ended`, service op 181, a command in every lane), and when a watched source finishes -- runs out of
  buffer, or `stop()` comes due -- the audio thread sends `HostCommand::AudioSourceEnded`, delivered through the host-bridge
  hook `_internalTriggerAudioSourceEnded` (in process and in the embedded runtime alike). A game that never sets `onended`
  costs the audio thread nothing; a source collected while it still had audio to play reports nothing; an end is reported
  once. Tests: the context (runs to its end, stopped, collected, unwatched), the embedded dispatch, and the runtime with a
  fake audio thread. Runtime-v8 JavaScript changed: joins the snapshot backlog.
- `InnerAudioContext`'s `canplay` listeners run after the duration is known. The context fetched `duration` from the
  audio thread with an async op when the native `canPlay` arrived and fired the listeners without waiting for the answer,
  so every `onCanplay` callback read `duration === 0` (it was right 50 ms later); Howler.js's HTML5 path computes its end
  timer from `node.duration` in that callback, got a zero-length sound and ended it before it started. Events that arrive
  while the state is being fetched now wait behind it, so listeners still see canplay before the play of an autoplaying
  source (a test plays the audio thread: red before, green after). Runtime-v8 JavaScript changed: joins the snapshot backlog.
- Uniform-block introspection: `getActiveUniformBlockName`, `getActiveUniformBlockParameter`,
  `getUniformIndices` and `getActiveUniforms` exist (PlayCanvas 2.x reads every block's name while it links
  a shader, and stopped there with a TypeError). The driver answers, through four more numbers of the generic state
  query (`gl_state::ACTIVE_UNIFORM_BLOCK_NAME` .. `ACTIVE_UNIFORMS_PARAMETER`; no new wire kind, op or host
  dispatch arm), in an envelope that carries either the value or the specification's error, so an unlinked program,
  a block or uniform index past the count and a pname WebGL leaves out (`UNIFORM_NAME_LENGTH`,
  `UNIFORM_BLOCK_NAME_LENGTH`) raise INVALID_OPERATION / INVALID_VALUE / INVALID_ENUM on the context instead of
  reaching the driver. A list longer than the wire's name limit goes in pieces. ANGLE answers a `getActiveUniforms`
  list longer than the program's active uniform count with zeros; the driver is asked about each uniform once and
  every requested index is answered from that. 43 new conformance assertions pin std140 sizes, offsets and strides,
  the long lists, and each error; with PlayCanvas 2.22.6, Babylon.js 9.29.0, Pixi, Phaser and three.js now all run
  their scenes.
- `getFramebufferAttachmentParameter` exists (Phaser 3.90 asks it while it boots: a TypeError that
  stopped the game before its first frame). The facade now records what is attached to each framebuffer, so
  the object (the wrapper the content holds, not a GL name), its type and its level are answered from
  that, and the default framebuffer follows the specification (`BACK`/`DEPTH`/`STENCIL`, present as the
  context attributes say, anything else INVALID_ENUM). What it cannot know -- an attached texture's component
  sizes, type and colour encoding -- is the driver's, through the second number of the generic state query
  (`gl_state::FRAMEBUFFER_ATTACHMENT_PARAMETER`; no new wire kind, op or contract line). With it Phaser 3.90.0,
  Pixi 7.4.3 and three.js 0.186.1 all run their scenes on real pixels (conformance `engines/`).
- Phaser 3 renders: dynamic text, `useProgram(null)`, integer uniforms. Found running Phaser on the runtime
  (graphics, a canvas texture, `Text`; 5 of 6 checks passed, and then 6 of 6):
  - **A canvas drawn and uploaded in the same frame became an empty texture.** `texImage2D(canvas)` of a
    canvas with pending draws goes through a snapshot record whose `format`/`type` are zeros (the facade has
    none to forward for the six-argument form); the GPU budget refuses an unsized internal format that does
    not equal its format, so the upload was rejected -- logged once as "WebGL GPU storage rejected" -- and the
    texture was never allocated. A texture without storage samples (0,0,0,1): Phaser's `Text` was an opaque black
    box. Every text sprite built this way (Phaser, Cocos Labels) was affected; a canvas that had been read
    back first went through the other path and worked, which is why no test saw it. The snapshot is RGBA8
    pixels, so the budget now accounts it as such (`canvas_source_upload_format`).
  - **`useProgram(null)` failed** with "program not found: 0" and left the previous program bound; it unbinds.
  - **`uniform2i`, `uniform3i`, `uniform4i` did not exist** (WebGL 1; Phaser sets its integer vector uniforms
    with them): now defined, over the array forms' records.
  - `MIGO_GL_TRACE_FAILURES=1` makes the render thread log the command that failed beside its error. The error
    named what went wrong, not which of the thousand calls of a frame asked for it, and finding that took bisecting
    the content; it is how the first two items were found.
- Pixi v7 renders: `getInternalformatParameter` exists. Pixi asks `gl.getInternalformatParameter(RENDERBUFFER,
  ..., SAMPLES)` while it creates its renderer and three.js when it makes a multisampled target, and the
  facade did not define it (a TypeError at construction). It is the driver's answer, descending as the
  WebGL 2 specification promises, empty for a format that cannot be multisampled (ES 3.0 cannot multisample
  an integer renderbuffer, which ANGLE's ES 3.1 reports anyway), `null` with INVALID_ENUM for a target other
  than RENDERBUFFER or a pname other than SAMPLES. With it a Pixi scene -- graphics, a canvas sprite, Text,
  alpha blending, a render texture, the ticker -- draws the right pixels (10 checks on a real device; they
  crashed the process three times before this: a zero-sized canvas, `Intl`, and this).
  Built on a new synchronous query kind, `STATE` (`gl_query::STATE`, 16): JSON text, one kind for every
  query that reads a value or a list back and has no reply shape of its own, selected by a number in
  `frame_wire::sync::gl_state`. The 40-odd methods of the specification that are still missing
  (`getUniform`, `getVertexAttrib`, `getIndexedParameter`, the uniform-block family, ...) need no new
  wire kind, producer op, contract entry or host dispatch each: a number and a handler arm.
- `Intl`, `toLocaleString`, `localeCompare` and `normalize` work, and `new Intl.DateTimeFormat()`
  no longer aborts the process. V8 does not carry its own locale data: the embedder hands it
  ICU's data before the first isolate, and `deno_core` does that only with its
  `include_icu_data` feature, which this runtime built without. So `new
  Intl.NumberFormat('en-US')` threw `TypeError: Internal error. Icu error.`, and
  `new Intl.DateTimeFormat()` or `new Intl.Segmenter()` crashed the process (a V8 CHECK,
  not an exception; a segfault in the unit tests). Nothing had ever formatted a number
  in a test, and a game that calls `score.toLocaleString()`, sorts names with
  `localeCompare`, or runs Pixi's `Text` (which builds an `Intl.Segmenter`) was one line
  from it. The full data is 10.7 MB, a quarter of the Android library, so the new
  `migo-icu-data` crate reads deno_core's pinned ICU 77 file at build time and links only
  what `policy.rs` keeps: 6.8 MB. Kept, because what is wrong when it is missing is a
  wrong answer: the number/date/plural data of every locale (without a locale's file a
  German player's `1234.5` is `1,234.5`), the time zone rules, the break iterators and
  their dictionaries (`Intl.Segmenter`, Chinese word breaking), Chinese collation
  (pinyin/stroke order for a leaderboard), and the display names of 16 languages. Dropped:
  character converters, transliteration, number spell-out, and the display names and
  collation tailorings of the other locales (they fall back to the root). Installed once,
  before the first runtime of any kind (the main one, the prewarmed one, snapshot
  generation). The data is stored uncompressed so ICU reads it in place and only touched
  pages become resident. The filter is code (a 150-line reader/writer of ICU's package
  format with tests, no tool and no committed blob) and the Android size budgets move by
  its size, with the reason written next to them. Found running Pixi on the runtime;
  migo-conformance's `intl-spec` (27 assertions) crashes the released v0.9.19 and passes
  here.
- Asking for WebGL on a canvas nobody has sized yet no longer takes the process down.
  `document.createElement('canvas')` is zero-sized until something sizes it, and
  Pixi, Phaser and three.js all test for WebGL on exactly such a canvas before they
  make their own. The offscreen canvas's EGL pbuffer was created at the canvas's size,
  zero by zero; on a Mac ANGLE turned that into a zero-width Metal texture descriptor,
  Metal's validation asserted, and the whole process aborted -- no JavaScript
  exception, no log line -- the first time a game merely looked for WebGL (on drivers
  that refuse a zero pbuffer, `createCanvas()` failed instead). The pbuffer is only
  the surface `eglMakeCurrent` needs and is now never smaller than one pixel
  (`pbuffer_extent`, at both places one is made), and `drawingBufferWidth/Height`
  say what the WebGL specification says for a zero-sized canvas: "A 0x0 canvas will
  yield a 1x1 drawingBufferWidth/Height". Found running Pixi on the runtime;
  migo-conformance's `webgl-zero-size` aborts the released v0.9.19 and passes here.
- three.js can create a WebGLRenderer, and a canvas texture is the right way up.
  Found by running three.js (r1xx) on the runtime for the first time, which no test
  had done: `new THREE.WebGLRenderer()` threw `TypeError: expected i32` out of
  `getParameter(gl.MAX_SAMPLES)`. The WebGL 2 facade had never declared 160 of the
  specification's constants (`MAX_SAMPLES`, `TEXTURE_3D`, `COLOR_ATTACHMENT1..15`,
  `DRAW_BUFFER0..15`, `UNIFORM_BLOCK_*`, `READ_BUFFER`, `RED`, `RG`, ...), so `gl.X`
  was `undefined` and went to a native op as an argument. All 559 constants of
  Khronos's WebGL 1 and 2 IDL are now declared with the specification's values.
  With that, a three.js scene (basic and PBR materials, a sphere, a data texture, an
  animation loop) renders correctly, with one more fix: `texSubImage2D(canvas)` ignored
  `UNPACK_FLIP_Y_WEBGL` and `UNPACK_PREMULTIPLY_ALPHA_WEBGL` (only the whole-image
  `texImage2D` honoured them), and three.js, Pixi and Cocos all allocate with
  `texImage2D`/`texStorage2D` and stream every canvas texture in with `texSubImage2D`,
  so every `CanvasTexture` (every text sprite, every canvas atlas) came out upside down.
  New gate, `scripts/test-webgl-spec-surface-contract.sh`: Khronos's IDL is vendored
  (`contracts/runtime/webgl-idl/`), every constant must be declared with its value,
  and every method must be defined or listed with its reason in
  `contracts/runtime/webgl-spec-surface.json`. The list is the honest size of the gap:
  66 methods are not implemented yet (`uniform2i`..`4i` and the `ui`/non-square-matrix
  uniforms, `vertexAttrib*f`/`I4*`/`IPointer`, `getUniform`, `getVertexAttrib`,
  `clearBuffer*`, `copyTexImage2D`, `getBufferSubData`, ...). Proven red for a wrong
  value, a missing constant, a method neither defined nor listed, a stale entry and a
  reasonless group.
- Three permission checks work again, and a failed call names its API once. Found by
  probing every callback-style `migo.*` API once on a desktop host
  (migo-conformance's new `api-surface-spec`, found at run time from the namespace so
  an API added later is covered; 272 assertions, released v0.9.19 fails 18):
  - `checkUserLocation`, `checkWritePhotosAlbum` and `getWritePhotosAlbum` answered
    `fail: _authSetting is not defined` on every call since the host-owns-the-answer
    refactor (2026-08-01) removed the local map and left these three naming it. The
    checks now read the host's current answer; `getWritePhotosAlbum`, which used to set
    its own entry to `true` (granting itself the permission), asks the host the way
    `authorize` does and answers under its own name.
  - An API a host does not serve (`getClipboardData`, `vibrateShort`, `showToast`,
    ... on a host without that service) failed with its name twice,
    `getClipboardData:fail getClipboardData:fail not supported`: the op answers a whole
    message and the caller put the prefix on it again. One composer
    (`failMessage`, in `02_async.js`) now builds every `<api>:fail <why>`, and the
    open-coded sites in the login, update, payment, camera, audio, websocket and
    download/upload modules use it.
- Input events carry a time on the clock content reads. A host stamps input in its
  own clock -- the system uptime on Apple and Android, which the C ABI does not name --
  and the engine handed that number to content untouched: on macOS a touch arrived
  with `timeStamp` 62,067,560 ms ahead of `performance.now()`. Content compares
  `event.timeStamp` with `performance.now()` and with the requestAnimationFrame
  timestamp (input smoothing, double-tap windows against the frame clock, latency
  probes; every ported web game), so every such comparison was wrong on exactly the
  platforms whose clock disagrees. Touch, key, mouse, wheel and gamepad timestamps now
  go through one converter (`input/00_input_clock.js`): the host's clock is anchored to
  the page's at the first event of a burst of input (a quarter-second gap starts a new
  burst, so the clocks cannot drift apart over a long session or a sleep), inside a
  burst the host's spacing is kept, and the result is never in the future. The header
  documents the contract (`include/migo/input.h`: use one clock per host; do not
  convert). Found by migo-conformance's new `input-touch-spec`, the first bundle that
  sends the game input (a macOS host replays real mouse events through the view's own
  handlers; 20 assertions; released v0.9.19 fails the timestamp one and passes the
  other 19, which is the evidence the coordinate and lifecycle path is right).
  Also: a touch listener that throws is now reported (`console.error`) instead of
  swallowed silently, so a handler that throws on every touch is not a game that
  mysteriously "ignores input"; the listeners after it still hear the event.
- `migo.request` works again for any response that has a body. Since v0.9.10 every
  request that received a non-empty body failed with `request:fail ... read data
  failed: TypeError: expected typed ArrayBufferView` (errno 500): the perf pass that
  made the body read bounded (#225) merged the chunks and handed the result on as an
  ArrayBuffer to the code that decodes a typed array, so `text`, JSON and
  `arraybuffer` responses all failed, and `readAll`'s old Uint8Array contract was
  nowhere asserted. It went unseen because nothing ran the engine's own `request()`
  over a body: the SSRF filter refuses every address a test could listen on, and
  the one fetch test read the body with `core.read` itself. A `data:` URL is answered
  through the same ops, reader and callbacks without a connection, so the new
  `request_through_the_engine` tests (and migo-conformance's `network-spec`) run the
  real code. They also pin what the same pass left wrong:
  - A response with no body (empty, 204, 304, HEAD) arrives as `""` or an empty
    ArrayBuffer, not `null`, so `res.data.length` is not a TypeError.
  - A `success` callback that throws no longer makes the request a failure: the
    delivery sat inside the `try` that reports a failed read, so the app's own bug
    ran its `fail` and `complete` after its `success`. The same shape was in
    `downloadFile` (the download was kept and reported failed) and `uploadFile`. One
    settler now delivers a request's outcome once, each callback isolated, and a
    `fail` that throws no longer escapes into the event loop.
  - `request`, `downloadFile` and `uploadFile` with arguments that cannot make a
    request -- an unknown or non-string `method`, a `null` options object or
    `header` -- fail through `fail`/`complete` with `request:fail ...` instead of
    throwing out of the caller's helper; callbacks that are not functions are ignored.
  - A body that fits one chunk (most JSON) is no longer copied a second time.
- A proxy on the machine no longer breaks every network request. With an HTTP or
  SOCKS proxy configured -- the `HTTP_PROXY`/`HTTPS_PROXY` environment or the
  system setting, which the engine's client honours as the system WebView does --
  every `request`, image and streamed audio fetch failed with `connection to
  127.0.0.1 is not allowed (private/loopback address)` (errno 500) *after the
  server had answered it*: the SSRF check on the address a response came from was
  the proxy's address, 127.0.0.1 for a local proxy and a private one for a
  corporate proxy. That check guarded nothing the client's own resolver and the
  gate before the send do not already guard (a name resolving to a blocked
  address is refused before any connection; an IP-literal URL is refused when the
  request is built, and on every redirect hop), so it is gone, and tests pin both:
  a response a loopback proxy delivers is delivered, and a loopback destination is
  still refused without the listener behind it seeing a connection. Found by
  trying `migo.request` from the macOS conformance host on a machine behind a
  local proxy.
- Starting many reads at once no longer fails most of them. A game does not read
  its assets one at a time: an asset loader starts dozens of `readFile` calls in
  one tick. Each whole-file read was charged the largest read a read can be
  (100 MiB) against the scheduler's pending-byte budget instead of what the file
  is, so sixty reads of a few hundred bytes spent the budget twice over and 37 of
  them failed with `IO pending-byte budget exhausted: requested 104857600 bytes`.
  The same went for the digest (`getFileInfo`) and Brotli read of a package
  entry. A read is now charged the size of the file (one `stat`) or of the
  package entry, bounded by the caller's `length`; one nobody can size keeps the
  cautious estimate, which is what the budget is for. Found by migo-conformance's
  new `io-spec` bundle (released v0.9.19: 23 of 60 succeed), and by a unit test
  that failed one run in twenty for the same reason.
- Workers: `terminate()` is quiet. The worker's event loop ends with V8's
  "execution terminated", which is the answer to the request, and it was reported
  to the content's `onError` (and logged at ERROR) as if the worker had crashed,
  every time. Also: `terminate()` followed at once by `createWorker()` -- restarting
  a worker -- no longer fails or hands back a worker that never starts: the old
  thread needs a moment to leave its isolate, and `createWorker()` now waits for it
  (up to two seconds) instead of racing it. And a worker may post a burst of
  messages: the queue held 64, so a worker that posted a result per item in a loop
  threw `Worker message queue full` at the 65th and the rest never existed (68 of
  200 arrived); it holds 4096 per direction, still inside the same byte budget that
  actually bounds the memory. Found by migo-conformance's new `worker-spec` bundle
  (17 assertions: the released v0.9.19 fails 3).
- `performance.now()`'s origin is clamped to the clock it is read against, which
  matters only to tests that advance a paused clock by hand: the worker timer
  lifecycle tests failed when run on their own, since the shared process origin
  introduced with the frame-timestamp fix could be later than a paused clock's
  "now" and time then stood still at zero until the clock caught up. They passed in
  the full run by accident of ordering.
- The V8 code cache's directory has one name however many sessions start at once.
  Two sessions asking for the cache while its worker was clearing and recreating
  the directory (it does that whenever the stored V8 version differs, which a
  fresh directory always does) could find it missing, fail to resolve it, and key
  the shared registry on the unresolved spelling -- `/var/...` against
  `/private/var/...` -- which gave one directory two caches, two counters and a
  32 MiB budget each. The parent directory is resolved instead, which nothing in
  the module touches. The unit test that guards the shared budget failed one run
  in four on this and was recorded as a flake; the cause was real.
- The first `createCanvas()` of a game no longer fails with
  `[Timeout] get_canvas_info timed out` when the render thread is slow to start.
  The call waited one second for the render thread's answer, and the render
  thread answers only after its own start-up (creating the EGL context, probing
  the GPU, a cold driver): on a slow or loaded machine the call timed out and the
  unhandled rejection ended the game before its first frame. The wait is five
  seconds -- it bounds a hung render thread, not a busy one (a dead one is seen at
  once, and a stuck one is the watchdog's) -- and the presentation-paths CI gate,
  which failed one run in several with "blit-probe never painted" after a cold
  runner spent 3.3 s building the runtime and 5.8 s probing the GPU, stops
  failing for that reason.
- `getSystemInfoSync().platform` and `getDeviceInfo().platform` named the
  platform the engine was built for instead of `"android"` on everything. A host
  that attached no device services (the in-process Apple host, and the desktop
  hosts that implement window info only) was answered with an `"android"` default
  with an empty `system` string, so content branching on the platform -- which
  audio format, which layout, which input model -- decided as if it ran on
  Android on a Mac, a Windows PC or a Linux box. The answer is now `"mac"`,
  `"windows"`, `"linux"`, `"ios"`, `"ohos"` or `"android"` from the build target,
  with `system` leading with the OS name (`"macOS"`), `abi` from the build, and
  nothing invented: `model` stays `"unknown"`, `brand` empty, `benchmarkLevel`
  `-1`. A host that can say more (a model, an OS version) overrides it, as the
  Android host and the iOS Performance+ profile already do. Found by
  migo-conformance's new `system-info-spec` bundle (37 assertions: the released
  v0.9.19 fails the one that checks the platform against the OS string).
- File system: `readFileSync(path, "base64")` (and `readFile`, `unzip`'s readers)
  threw `btoa is not defined`: the encoder called the page's `btoa`, which this
  runtime does not have without an adapter. It uses the native codec the other
  encodings use. `writeSync` / `write` returned `{ bytesWritten }` as a BigInt, so
  `bytesWritten + 1` was a `TypeError` and `JSON.stringify` of the result threw;
  it is a Number now, as `readSync`'s `bytesRead` already was.
- File system: a failed `renameSync` / `copyFileSync` (and the async forms) told
  the content the host paths the virtual ones resolved to -- `rename
  /Users/<name>/Library/Application Support/Migo/files/.../user_data/x -> ...` --
  which is the user's home directory and the game's sandbox location, in the text
  of an error a game may well log or report. It names the paths the content gave.
  The same for a `readdirSync` that meets a non-UTF-8 file name. Found by
  migo-conformance's new `fs-spec` bundle (81 assertions: the released v0.9.19
  fails 8, and two groups of its assertions never ran because the base64 read
  threw first).
- The timestamp handed to a `requestAnimationFrame` callback is on the same
  timeline as `performance.now()`, as in a browser. They were two clocks:
  `performance.now()` counted from the creation of the JavaScript runtime, the
  frame timestamp from the first vsync (or from the render thread's start where
  there is no display clock). A game that had loaded for two seconds read
  `ts = 0` on its first frame against a `performance.now()` of 2000, so a loop
  starting with `dt = ts - performance.now()` had a negative first step and an
  animation timeline anchored to `performance.now()` began two seconds behind.
  Both now count from one process-wide origin (`shared::time_origin`), and the
  vsync clock's timestamps are put on it by anchoring the first frame
  (`RafTimeline`) and keeping the host clock's spacing after that.
  `performance.timeOrigin` exists (it was `undefined`, so
  `performance.timeOrigin + performance.now()` was `NaN`).
- `requestAnimationFrame` after an idle screen or a long frame no longer receives
  a timestamp from before the stall. The render thread signals every vsync while
  animating so a consumer that keeps up never blocks; the signal went through a
  bounded channel that dropped the NEWEST signal when full, so a consumer that
  fell behind was handed the oldest two timestamps the render thread had written
  (on Android's eventfd path the newest, which was always right). The first
  callback after 300 ms of idle got a time 267 ms old, its successor a step of
  that size. Every platform now keeps only the newest unconsumed signal, and a
  signal more than 50 ms older than the request is passed over for the next one.
  Found by migo-conformance's new `timers-spec` bundle (25 assertions: the
  released v0.9.19 fails 3).
- Web Audio: the errors the engine's own audio, `ImageData` and Canvas 2D code
  throw are the ones the specification names. They were written as
  `new DOMException(...)`, and `DOMException` is the host page's to provide: with
  no adapter installed, `createBuffer(1, 10, 1)`, a second `start()` on an
  oscillator, `connect()` to a missing input and `createImageData(0, 1)` threw a
  `ReferenceError` instead of `NotSupportedError`, `InvalidStateError` and
  `IndexSizeError`. One shared factory (`base/06_dom_exception.js`) makes them: it
  uses the adapter's `DOMException` when there is one, so `instanceof` holds in
  content, and its own class (same `name`, `message` and legacy `code`) otherwise,
  and installs nothing on the global object. Also: `createBuffer` / `new
  AudioBuffer` with zero channels or zero length are `NotSupportedError` (they
  were `RangeError`); `getChannelData`, `copyFromChannel` and `copyToChannel` with
  a channel that does not exist are `IndexSizeError` (they were `RangeError`);
  `decodeAudioData` rejects bytes it cannot read with `EncodingError` (it rejected
  with an `AudioError` carrying the engine's text), through the promise and the
  error callback alike.
- Web Audio: `decodeAudioData` makes a buffer at the context's sample rate. It
  resampled to the device's, so a context at 44100 Hz on a 48000 Hz device handed
  content buffers whose `sampleRate` was not its own, and every sample index
  computed from the context's rate was 8.8% off; playback also paid to resample
  them back at run time. Found by migo-conformance's new `audio-spec` bundle (53
  assertions: the released v0.9.19 fails 19 of them).
- WebGL: `pixelStorei(UNPACK_FLIP_Y_WEBGL)` and
  `pixelStorei(UNPACK_PREMULTIPLY_ALPHA_WEBGL)` were recorded and never applied,
  so every upload ignored them: an engine that flips its textures (three.js and
  Babylon do by default) drew them upside down, and one that uploads premultiplied
  (Pixi, Phaser, Egret) got straight alpha and bright fringes. The renderer now
  applies them to `texImage2D` and `texSubImage2D` from bytes (a typed array,
  `ImageData`, a decoded image), reading the rows the way the driver would
  (`UNPACK_ALIGNMENT`, `UNPACK_ROW_LENGTH`, the skips), and to `texImage2D` from a
  canvas. A canvas also keeps WebGL's default of straight alpha: it holds
  premultiplied colour, and was uploaded as it was whatever the flag said, so a
  translucent edge came out too dark. The flags are no longer sent to the driver,
  which has no such parameter and answers `INVALID_ENUM`; `getParameter` reports
  them as booleans (and `UNPACK_COLORSPACE_CONVERSION_WEBGL` as
  `BROWSER_DEFAULT_WEBGL`). Not yet: `texSubImage2D` from a canvas ignores them.
- WebGL: `getParameter(VERSION)` and `getParameter(SHADING_LANGUAGE_VERSION)` begin
  with `WebGL 1.0` / `WebGL GLSL ES 1.00` (`WebGL 2.0` / `WebGL GLSL ES 3.00` on a
  WebGL 2 context) followed by the driver's string in parentheses, which is what
  content and libraries test; they returned the driver's `OpenGL ES 3.0 ...`.
- WebGL: `invalidateFramebuffer(target, 4294967295)`, `drawBuffers(4294967295)` and
  `drawBuffersWEBGL(4294967295)` -- a number where a sequence of enums belongs --
  built a 16 GiB typed array, and the isolate stopped answering until the watchdog
  ended it. They throw `TypeError`, as WebIDL says; `null` and `undefined` remain
  the empty list. Found by a hostile-call test (`robustness-hostile-calls` in
  migo-conformance: 1.8 M random calls with NaN, infinities, 2^32, BigInt, odd
  typed arrays at the Canvas2D, WebGL and WebGL 2 contexts) which otherwise found
  no crash and no other hang.
- WebGL 2: `gl.HALF_FLOAT` was `undefined` (the constant was never declared), so a
  half-float upload -- the `type` of every `RGBA16F` texture -- went to the driver
  with no type.
- WebGL: `isBuffer`, `isFramebuffer`, `isProgram`, `isRenderbuffer`, `isShader`
  and `isTexture` exist (calling one was a `TypeError`), and `getShaderSource`,
  `getBufferParameter`, `getTexParameter` and `getRenderbufferParameter` answer
  from what the content set, without a round trip to the render thread. Not yet:
  `getVertexAttrib`, `getVertexAttribOffset`, `getUniform`.
- WebGL: a context on an offscreen canvas began with a 1x1 viewport and scissor
  box. A GL context takes the size of the surface it is first made current with,
  and an offscreen canvas is created as a 1x1 pbuffer and sized by the content
  afterwards, so every draw was clipped to one pixel until the content called
  `viewport` itself, and `enable(SCISSOR_TEST)` without a `scissor` call clipped
  everything for good. `getParameter(VIEWPORT)` and `getParameter(SCISSOR_BOX)`
  now start at the drawing buffer's size, set when the first GL command reaches
  the canvas.
- Canvas2D compositing: `source-in`, `source-out`, `destination-in`,
  `destination-atop` and `copy` only changed the pixels of the shape being drawn.
  The specification composites against a bitmap that is transparent beyond the
  shape, so for these five everything else the clip allows is cleared (or kept,
  per operator): drawing a circle with `destination-in` is how a picture is
  cropped to a circle, and it cropped nothing. These draws now go through a layer
  composited over the whole clip.
- WebGL: a new offscreen canvas's drawing buffer could begin holding an earlier
  canvas's pixels, the same defect as the Canvas2D one below (measured on macOS:
  77-93 of 360 after the first ~90). A drawing buffer is specified as transparent
  black, depth 1, stencil 0 when created and again when resized; the DrawingBuffer
  is now cleared to that where it is allocated, and a pbuffer canvas at its first
  use after it was created or resized, with the content's own clear values, write
  masks, scissor and rasterizer-discard put back afterwards. Assigning a WebGL
  canvas the size it already has now clears it too.
- Canvas2D: a new canvas could begin holding an earlier canvas's pixels. The
  framebuffer under a fresh surface is whatever the driver returns, and ANGLE's
  Metal backend recycles the storage of destroyed surfaces without clearing it:
  after a few hundred canvases had been created and collected, every new one
  started with the pixels of one that had been freed (measured on macOS: 162 of
  480). Every Canvas2D surface is now cleared when it is created, which also makes
  assigning a canvas the size it already has clear it by construction.
- Canvas2D: `ctx.ellipse(x, y, rx, ry, 0, 0, 2 * Math.PI)` -- a full turn, the
  usual way to draw an ellipse -- drew nothing. Skia's `arc_to` treats a sweep of
  360 degrees as degenerate; `arc` already split a full turn in two and
  `ellipse` did not.
- Canvas2D `getImageData` returned premultiplied colour for translucent pixels:
  half-transparent red read back as `(127, 0, 0, 127)` where `ImageData` holds
  straight alpha, `(255, 0, 0, 128)`. Every translucent edge was darker than it
  is, and a read followed by a write would have darkened it again. The CPU read
  of the snapshot behind `getImageData` now unpremultiplies.
- Canvas2D shadows: `shadowOffsetX/Y` and `shadowBlur` are in device pixels and
  not affected by the current transform, as the specification says; they were
  carried through the matrix, so a context under `scale(2, 2)` -- every
  device-pixel-ratio game -- drew its shadow twice as far and twice as soft as a
  browser. And `globalAlpha` was applied to the shadow twice (once in the
  silhouette, once in the shadow colour): at `globalAlpha = 0.5` the shadow was a
  quarter transparent instead of half.
- Canvas2D text on macOS and iOS: `serif`, `monospace`, `cursive`, `fantasy`,
  `system-ui` (and `-apple-system`, `ui-monospace`, ...) now reach a system face.
  Skia's CoreText font manager answers `Helvetica` and `Menlo` but none of the CSS
  generic keywords, so each used to fall through to the bundled Noto Sans: a
  `monospace` overlay came out proportional, a `serif` heading came out sans, and
  text that named no installed family was drawn in a face no Apple browser picks.
  The keywords now map to the faces Safari and Chrome use (`Helvetica`, `Times`,
  `Menlo`, the system UI font, `Apple Chancery`/`Snell Roundhand`, `Papyrus`);
  Android and Linux keep resolving them natively.
- Canvas2D `measureText` and `getTextLineHeight`, every platform: the JS-thread
  measurement resolved only the first name of the `font` list (plus `sans-serif`)
  while `fillText` resolved the whole list, so `"Microsoft YaHei", serif`
  measured one face and painted another and text laid out from the measurement
  did not fit what was drawn. Both now resolve the same list.
- Canvas2D: assigning `canvas.width` or `canvas.height` the value it already has
  now clears the canvas and resets the context, as the specification says
  (`canvas.width = canvas.width` is the old way to clear a canvas). It cleared
  nothing and reset nothing on the renderer while the JavaScript half reset its
  shadow of the state, so `fillStyle = "#000"` afterwards drew the previous
  colour. A draw still queued when the canvas is resized, to any size, is
  dropped with the old bitmap instead of landing on the new one. The engine's
  own resizes to the size a canvas already has stay a no-op.
- Canvas2D: a `getImageData` on an offscreen canvas could stop the on-screen
  canvas from drawing or reading back anything afterwards. The snapshot code
  kept one temporary framebuffer for every canvas, created in whichever EGL
  context needed it first, and used that name in all the others; a framebuffer
  is not shared between contexts, so in the on-screen canvas's context the name
  was its DrawingBuffer, and each snapshot attached a texture to it and detached
  it again. Each canvas now has its own temporary. Found by the first run of
  migo-conformance on macOS; the same sequence on any platform reaches it.

## v0.9.19 (2026-09-29)

### Fixed
- macOS: on a Mac with two GPUs held on its integrated one (`pmset gpuswitch 0`),
  a game's frame loop ran and its window stayed black; even a Canvas2D read
  back empty. ANGLE took the
  system default GPU -- the discrete one -- whatever drove the display.
  `MigoGameView` now sets its layer's device to the GPU that drives the
  window's display and the engine renders on the layer's device, named to
  ANGLE through `EGL_ANGLE_platform_angle_device_id`. A host attaching its own
  `CAMetalLayer` through the C ABI does the same (`include/migo/platform/macos.h`).
  Measured on a MacBookPro16,1 under each `gpuswitch` setting: the view renders
  on the GPU that drives its display, integrated or discrete.

## v0.9.18 (2026-09-29)

### Removed
- C ABI: the resource-lane declarations in `external_frames.h` --
  `MigoResourceReservationDescriptor`, `MigoResourceOutcome`,
  `MigoResourceState`, `MigoResourceError`. No entry point ever took them and
  nothing implemented the lane; uploads larger than a frame packet are staged
  in the command stream instead (below).

### Fixed
- iOS (Performance+): a WebGL upload larger than one frame packet -- 4 MiB --
  was refused `OUT_OF_MEMORY` on the producer, so it drew nothing: a
  2048-square RGBA texture (16 MiB), a 2048-square ASTC 4x4 one (exactly
  4 MiB), a large vertex buffer. Its bytes now cross ahead of the call as
  staged chunks in the same command stream, over as many packets as they
  need, and the host moves them into the upload whole; up to the 64 MiB one
  upload may carry on every lane. Uploads that fit a packet are sent exactly as
  before. A simulator acceptance test draws with an 8 MiB texture and a 5 MiB
  vertex buffer and reads back both halves of the texture.
- iOS (Performance+): a record that filled the last 8 bytes of a frame packet
  threw instead of being sent -- the writer asked for 8 bytes more than the
  check before it had allowed for.
- Docs: the developer docs describe iOS and macOS as shipped -- an Apple SDK
  reference for `MigoGameView`, six platforms on the home and architecture
  pages -- Linux as the released C ABI SDK it is, and the English architecture
  diagrams render.

## v0.9.17 (2026-09-28)

### Fixed
- Android: the engine's frame-rate request never reached the display.
  `ANativeWindow_setFrameRate` (Android 11+) was not called at all, from
  v0.9.5 on, because the wrapper every window surface passes through did not
  forward the request, so the display picked its refresh rate from its own
  heuristics rather than from the rate the game presents at. On an Android 14
  emulator the game's layer now carries a 60 Hz vote with Default
  compatibility; before the fix it carried none.

## v0.9.16 (2026-09-28)

### Fixed
- iOS (Performance+): a game could start to a black screen, its frame loop
  running and nothing drawn, in about one launch in four on an iPhone XS Max
  under load. WebKit keeps firing a Worker's timers while the Worker is
  blocked in a synchronous request -- the channel the engine's synchronous
  calls (`getImageData`, `getStorageSync`, WebGL queries) take -- so a timer
  could run in the middle of the script that made the call. The web adapter's
  `load` event, queued on a zero-delay timer, then fired while a Phaser bundle
  was still being evaluated, before the game registered for it, and the game
  never started. The producer now holds every timer and socket event that
  arrives during a synchronous call until the script that made it has
  finished, and runs them in arrival order, each as a task of its own. The
  engine's synchronous channel was designed on `Atomics.wait`, which
  dispatches nothing; this restores what it assumed. A device acceptance test
  arms a timer before 300 ms of synchronous calls: without the fix it ran
  inside them on every run. The black screen reproduced on v0.9.14 and v0.9.15
  alike.
- Docs: the Apple guide says the game's orientation has to hold before
  `MigoGameView`'s first layout, not only eventually.

## v0.9.15 (2026-09-28)

### Added
- C ABI (external-frames product): `migo_session_start_frame_endpoint`,
  `migo_session_stop_frame_endpoint` and
  `migo_session_get_frame_transport_statistics` with its
  `MigoFrameTransportStatistics` record. While the endpoint runs it owns the
  downlink, so `migo_session_take_downlink`,
  `migo_session_take_service_message` and `migo_session_set_downlink_waker`
  return `MIGO_ERROR_INVALID_STATE`; a host that brings its own transport keeps
  using them as before.

### Changed
- iOS (Performance+): the engine terminates the producer's socket itself. The
  loopback WebSocket the WebContent producer sends frames on and hears its
  verdicts, frame-clock ticks and answers from used to be the host's --
  Network.framework, every message copied across the C boundary and the
  downlink pumped from a GCD queue -- and on devices that was about a quarter
  of the App process's CPU samples while a game ran (iPhone 15 Pro: 23%, ~3.4%
  of a core), with three queue hops between the frame clock and the socket. Now
  an uplink message is one read and a submit on the engine thread that read it,
  and a downlink message one unpark and one write, with Nagle's algorithm off
  and `SO_NOSIGPIPE` on every connection. Measured on an iPhone XS Max against
  v0.9.14 built the same way, interleaved, 30-second windows, one message each
  way per frame in both: the App process's CPU fell by 4.7 and 5.2 points
  (bunnymark, endless-runner) and was unchanged on canvasmark, and frames
  presented late fell from 13 in 9 runs to 4 in 8. The WebKit helper processes'
  CPU time rose by about as much as the App's fell -- every process ran on the
  efficiency cores, whose clock they share -- so the device's total CPU time is
  about the same; an energy measurement needs a phone that runs Power Profiler.
  `MigoFrameChannel` keeps the content origin's half and its `Statistics`, now
  counted by the engine for both uplinks alike.

### Removed
- Swift: `MigoFrameTransport`, the host-side Network.framework socket the
  engine's frame endpoint replaces.

## v0.9.14 (2026-09-28)

### Fixed
- iOS (Performance+): the web view that hosts the producer is window-sized
  again (still off-screen), undoing v0.9.13's one-point shape. At one point the
  Canvas2D bench game presented more late frames in every one of six
  interleaved 30-second pairs on an iPhone 15 Pro -- 6.3 against 4.0 per 30 s,
  with both builds made the same way and differing only in that frame -- while
  the WebGL game showed no difference. Frame pacing outranks the 11 MiB of
  WebContent memory the smaller view saved, so the window's size stays until
  the cause is understood. v0.9.13's canvas-sized drawable is unaffected.

## v0.9.13 (2026-09-27)

### Changed
- iOS and macOS: when the onscreen canvas is smaller than the window -- as the
  canvas of a game that sizes it in logical pixels is -- the drawable now takes
  the canvas's size and Core Animation scales it to the view, the way a browser
  composites a canvas, instead of the engine upscaling into three window-sized
  drawables every frame. On an iPhone 15 Pro the Performance+ lane's footprint
  fell 41-47 MiB per bench game (bunnymark 184 to 137 MiB, endless-runner 229
  to 188, canvasmark 164 to 119) with CPU unchanged, and frames still present
  at 60 per second with a 16.67 ms p99 interval. It needs fixed-size window
  surfaces, which ANGLE implements only for D3D, so the Apple ANGLE build now
  carries them for Metal (`angle-apple-52f59428-p2`). The drawable's size is
  the engine's: a host must not set `CAMetalLayer.drawableSize`, and
  `MigoGameView` no longer does (`include/migo/platform/ios.h`, `macos.h`).
- iOS (Performance+): the web view that hosts the producer is one point square
  just off the window's edge instead of window-sized. WebKit keeps a backing
  store for the page at the view's size even though the page draws nothing;
  at the window's size that was 11 MiB of WebContent's memory on an iPhone 15
  Pro, with frame rate and CPU unchanged at one point.
- Developer documentation: the Apple page is an integration guide for the
  released SwiftPM package -- the two products and their minimum OS, content
  signing, platform notes, the device capabilities `MigoGameView` answers and
  the ones it refuses, and what has been verified where -- in place of a
  description of the skeleton.

## v0.9.12 (2026-09-27)

### Changed
- iOS and macOS (Performance+): a frame now crosses the WebContent boundary as
  one message each way instead of two. A presenting packet is also the request
  for the next frame-clock tick, and its verdict travels with that tick unless
  the producer cannot wait for it (a refusal, an empty window, or no tick
  owed). On an iPhone XS Max at 60 fps the Performance+ lane's CPU time fell
  7-20% across the bench games, most of it in WebKit's network process, with
  frame rate and memory unchanged. No wire-format change: a host that drains
  after every submit, or a producer that still sends every request, stays
  correct (`contracts/frame-wire/wire-v1.md`, amendment of 2026-09-27).

## v0.9.11 (2026-09-27)

### Fixed
- Every platform: `WEBGL_lose_context.loseContext()` reset the whole GPU share
  group instead of losing the one context it was called on. Pixi calls it on a
  probe context while choosing a renderer, and a reset that landed after the
  game had built its shaders took them away: on an iPhone, Pixi games stayed
  black on their first launch after install. It now loses that context only, as
  the extension specifies; the reset is still reachable for verification as
  `MIGO_debug_gpu_reset`.
- Every platform: `console.error(error)` logged an `Error` as `{}`. It now logs
  its name, message and stack; `undefined`, functions and symbols, which logged
  as nothing, log as their text.
- iOS and macOS (Performance+): a game that draws only with Canvas2D showed a
  black screen -- its batches were never marked for presentation.
- iOS and macOS (Performance+): after a GL context loss every frame was refused
  and the game froze on its last picture, because the producer was never told
  the new resource epoch. The host now sends it (`DOWN_CONTEXT_STATE`, a
  downlink-only addition to frame-wire v1), and content gets
  `webglcontextlost`/`webglcontextrestored` as on every other platform.
- iOS and macOS (Performance+): a web adapter's `XMLHttpRequest` shim broke the
  engine's synchronous calls, and its timers threw `Illegal invocation`; the
  producer now uses the platform objects it captured before content ran.
- iOS and macOS (Performance+): content saw the WebKit Worker's own globals
  (`importScripts`, `self`, `navigator`, ...). Phaser read `importScripts`,
  decided it was in a Web Worker and refused to start. Content now sees the
  same global names the engine publishes on every other platform.

## v0.9.10 (2026-09-26)

### Fixed
- Android C ABI: JPEG images still failed to decode after v0.9.9's fix -- Skia,
  as this engine links it, decodes PNG but not JPEG, and the Java SDK's
  BitmapFactory fallback is out of a C host's reach. The C ABI package now
  carries the Rust image decoders every other platform's C ABI already has; the
  Java SDK's AAR is unchanged. Found by migo-conformance's new image-decode
  bundle, which runs every decode path on both Android embeddings.

## v0.9.9 (2026-09-26)

### Fixed
- Android C ABI: images never decoded, so every WebGL game rendered black while
  its frame loop kept running. Android builds leave the Rust image decoders out
  and rely on the Java SDK to register BitmapFactory at load time, which a C
  host never runs. The C ABI now registers Skia's decoders -- straight into an
  `AHardwareBuffer` where the renderer can import one, into RGBA memory where an
  image must be CPU-backed -- at no binary cost, since Skia is already linked
  for rendering. The Android C host's on-device check now paints its first frame
  from decoded images on both paths, so this cannot pass unnoticed again.

## v0.9.8 (2026-09-26)

### Changed
- The engine now waits up to ten seconds, not two, for the renderer to report
  its GPU capabilities before it runs content. The wait only guards against a
  renderer that has stopped: one that fails reports it immediately, and one
  that comes up is waited for only as long as it takes. At two seconds, a
  slow but healthy first start -- a cold driver shader cache on a low-end
  device, or software rendering on a loaded machine -- was treated as a failure
  and the session refused its content for good.

### Fixed
- Windows: no session could start when a host's files, cache or code-cache
  directory was spelled with `..`, such as the examples' `<files>\..\cache`
  ("initialize sandbox filesystem: failed to pin sandbox roots"). Sandbox roots
  were opened in Win32's `\\?\` namespace, where `..` is a file name rather
  than a step up. A root is now given the meaning Win32 itself gives it before
  it is opened; relative roots are still refused. The Windows arms of the
  sandbox filesystem and the atomic writer now run in CI, which they had never
  done.
- Linux: the offscreen (headless) target needed a window server after all.
  It asked EGL for the default display, which Mesa resolves to X11, so it
  failed to start wherever `DISPLAY` was unset -- every CI runner and server.
  It now uses Mesa's surfaceless platform when the driver offers it, and asks
  only for the pbuffer configuration a headless target renders into.
- iOS and macOS: after `migo_surface_release_query` reported RELEASED, the
  host's `CAMetalLayer` could still be alive for a moment, released on the
  render thread just afterwards. Objects ANGLE autoreleased during the surface
  teardown lived until the render loop's pool drained, which came after
  RELEASED. Teardown now drains them before RELEASED is published, on detach,
  on surface replacement and at shutdown.

## v0.9.7 (2026-09-25)

### Added
- iOS and macOS SDK: `migo-<version>-apple-sdk.zip`, a Swift package with one
  view per platform. `MigoGameView` (`MigoApplePerformancePlus` on iOS,
  `MigoMacV8` on macOS) takes an installed game and owns everything between it
  and pixels -- engine session, `CAMetalLayer`, display clock, input, app
  lifecycle, and on iOS the audio session and WebContent crash recovery.
  `MigoGameInstaller` installs a package atomically and skips a version that is
  already there. Both products ship a privacy manifest that a gate checks
  against the engine's sources in both directions. The zip carries its own
  `.attestation.json`, like every other SDK archive, and a runnable app for
  each platform is `apple-swift/` in migo-examples.
- iOS and macOS: the soft keyboard. `migo.showKeyboard` opens the system
  keyboard on iOS and a text field along the game's bottom edge on macOS; the
  player's text comes back as `onKeyboardInput`/`Confirm`/`Complete`. On the
  iOS lane the three keyboard ops became host commands to the same keyboard
  service the embedded runtime calls, so a host with no keyboard refuses them
  in the same words.
- iOS and macOS: the device. `vibrateShort`/`vibrateLong` drive the Taptic
  Engine on iOS; `setKeepScreenOn` holds the display awake (the idle timer on
  iOS, a power assertion on macOS) and gives it back when the game ends;
  `getGameLogManager().log` entries reach the app as `MigoGameView.Event.gameLog`;
  `getNetworkType`/`onNetworkStatusChange` follow `NWPathMonitor`, and on iOS
  `getBatteryInfo` reports the battery and Low Power Mode. A Mac has nothing to
  vibrate and no battery API a game should need, so those answer "not
  supported" there, as on a device without the hardware.
- C ABI: device capabilities. Three optional, independent callbacks appended to
  `MigoHostCallbacks` -- `on_vibrate`, `on_keep_screen_on`, `on_game_log` -- each
  offered to content exactly when installed, and two report functions,
  `migo_session_set_network_status` and `migo_session_set_battery_status`,
  whose last report the engine answers content's synchronous reads from and
  forwards a network change while content listens. An older host is
  zero-extended to none of them, which is the previous behaviour.
- C ABI: `MigoEngineConfig.code_signing_public_key`, the Ed25519 key signed
  content is verified against. Until now a C ABI host could not supply one, so
  its only configuration that loaded content was
  `MIGO_ENGINE_FLAG_ALLOW_UNSIGNED_CONTENT`. Appended to the record: a host
  passing the old size is zero-extended to "no key" and behaves as before.
  Setting the key and the unsigned flag together is refused.

- macOS: a host-owned `CAMetalLayer` can be attached through the C ABI.
  `MIGO_PLATFORM_MACOS_CA_METAL_LAYER` now appears in the library's advertised
  attachable kinds, and it appears because an attach ran, not because a backend
  compiled: `apple-sdk.yml` run 34017950809 built a real `CAMetalLayer` on a
  hosted macOS runner, carried it through `migo_session_attach_surface` as
  generation 1, and completed the whole retirement handshake — `begin_detach`,
  poll to `MIGO_SURFACE_RELEASE_RELEASED`, `migo_surface_release_destroy`,
  `migo_session_destroy`, `migo_engine_destroy` — before releasing the layer.
  An `NSView` is refused rather than resolved to a layer, so the host keeps
  ownership of its own drawable. The same run loaded the pinned ANGLE under the
  name the recipe declares and got an EGL display back from it. iOS is
  deliberately not included: its arm of the same module compiles everywhere and
  has attached nothing anywhere.
- Images with transparency are transcoded to ASTC at package ingest on devices
  whose GPU decodes it, and stay on ETC2 elsewhere. The block footprint is
  chosen per image by what it reconstructs: the encoder grades its own output
  against the source and takes the largest block whose worst channel error stays
  within budget, so a smooth image lands at 0.25 bytes per pixel — a quarter of
  ETC2 RGBA, at higher fidelity — while a sprite with a hard alpha edge stays at
  one byte per pixel rather than losing the edge. Opaque images stay on ETC2 RGB
  everywhere, because at half a byte per pixel it is smaller than any ASTC
  footprint. The choice is per image and per device, which is only possible
  because ingest runs on the device.
- Apple: ANGLE over Metal is built from source and pinned
  (`contracts/artifact-manifest/apple-angle.lock.json` +
  `scripts/build-angle-apple.sh` + `scripts/fetch-apple-angle.sh`) for every
  slice group the engine is built for — iOS, the iOS simulator and macOS, five
  configurations in all. ANGLE publishes no official prebuilt binaries for any
  platform, so these are self-hosted and hash-verified before use, the same
  model the Windows ANGLE runtime and the V8 archives already follow. It exists
  because there is no GL framework on iOS: rustc's own link line for the Apple
  slices asks for `-framework OpenGL` on macOS and for no GL framework at all on
  iOS, while Skia is configured for its GL backend. Metal is the only backend
  built; the desktop GL backend, which defaults to on for macOS, is pinned off
  so both Apple platforms resolve the same renderer. The published shape differs
  per platform because upstream's does — a framework bundle on iOS, where Apple
  accepts an embedded framework and rejects a bare dylib, and a shared library
  on macOS — and it is deliberately not evened out: `libEGL` locates `libGLESv2`
  at run time by composing a name against a directory, and that name and
  directory are platform-specific, so repackaging either side would leave ANGLE
  searching where its dispatch library is not. `--print-loader-layout` is what
  answers where each library has to sit; the xcframeworks are split along the
  same axis instead, one per library per platform family.
- The runtime reports how many times a frame's drawing crossed from JavaScript
  into native, on a five-second window, at `info` level: `[boundary] frames=…
  crossings/frame=… worst=… commands/frame=… commands/crossing=…`. The ratio is
  what a command stream exists to move, and it was previously only observable
  from inside a test.

### Changed
- Canvas2D commands cross the JavaScript/native boundary as a binary command
  stream rather than one op per call, sharing one buffer and one opcode space
  with the WebGL stream so the order between the two survives the crossing
  without a barrier between every pair of commands. A frame that issues three
  hundred 2D calls now crosses once.

### Fixed
- iOS: installing an update over a game that had already run under code
  signing failed on an iPhone ("you don't have permission"): Darwin refuses to
  rename a directory its caller cannot write, and the engine seals the package
  read-only. The installer now restores the owner's write bit on the sealed
  root alone before moving it aside. The simulator's host file system had let
  the rename through.
- iOS and macOS: the engine's render and session threads ran at the default
  quality-of-service class, so the main thread waiting on the session's
  startup was a priority inversion and the frame work could land on efficiency
  cores. They are now user-interactive; decode and IO threads are utility.

- iOS: signed content was not verified. The Performance+ lane mounted the
  installed package as it was, so a host that configured a signing key got no
  verification at all. It now runs the embedded execution's launch sequence
  before anything is mounted -- sealed-receipt check, else a full manifest and
  file-hash verification that seals the tree -- and refuses the load on any
  mismatch, or on signing with no key.
- `MigoGameInstaller` could not update a game that had run under signing: the
  engine seals a verified tree read-only, and the installer replaced it in
  place. An update now renames the sealed tree aside, renames the new one in
  and removes the old one as a trusted uninstall.
- iOS: a game that called `migo.exitMiniProgram()` stopped drawing and the app
  was never told. The external-frame session ended on the request without the
  exit notification the in-process runtime sends; it now sends it.
- The Apple SDK could not put its iOS and macOS engines in one xcframework:
  assembly required byte-identical header directories, and each group's module
  map lists the frameworks its own archive links. Only the C headers are now
  compared across groups.
- SBOMs listed crates the shipped build never compiles. `cargo metadata`
  resolves features for the whole workspace, so a crate any member enabled was
  in every artifact's graph -- the iOS SDK, built without a JavaScript engine,
  would have listed `v8`. Each SBOM is now cut to its build's own `cargo tree`.
- ASTC 8x8 textures upload again. The engine mapped
  `VK_FORMAT_ASTC_8x8_UNORM_BLOCK` to `0x93B9`, which is the token for a 10x6
  block, so `glCompressedTexImage2D` rejected every such texture with
  `GL_INVALID_VALUE`. It had never fired because nothing produced ASTC 8x8, and
  the test that covered it asserted the same wrong number, copied from the
  constant it was checking; the tokens are now derived from the block size the
  extension assigns them to.
- `ctx.fillStyle = "transparent"` no longer paints opaque black. The keyword was
  missing from the engine's named-colour table, so it fell through to the
  unknown-name branch, which reads black -- the loudest possible wrong answer for
  a keyword whose meaning is "do not paint". `strokeStyle` and `shadowColor` had
  the same fault, and gradient colour stops were unaffected.

---

## v0.9.6 (2026-08-30)

A hotfix for a WebGL regression in v0.9.5.

### Fixed
- `bufferData(target, ArrayBuffer, usage)` uploads its data again. v0.9.5 added
  a `size < 0` guard to `bufferData` that ran before the code checked whether a
  payload was supplied, and the JS binding passes `size = -1` on the data path
  because the field is unused there. So the most common WebGL upload -- vertex
  and index buffers -- became a silent no-op that also recorded a spurious
  `INVALID_VALUE`, and every WebGL draw painted black. The op had no test;
  `migo-conformance`'s `webgl-basics` bundle caught it on the first run against
  the v0.9.5 release AAR. The negative-size check now runs only on the
  size-only form, and `op_buffer_data` has a regression test on both forms.

---

## v0.9.5 (2026-08-29)

A correctness and efficiency pass over io, graphics and audio. Redundant GL
calls that slipped past the state shadow are deduplicated and the shadow itself
stopped hashing to decide; `measureText` and per-sample `AudioParam` automation
stop allocating and re-walking on every call; `AnalyserNode`'s scalar
parameters take effect; `compressImage` shares a two-thread pool instead of
spawning one thread per image. A symlink race in sub-package and ZIP extraction
is closed, a WebGL context loss no longer strands a game between frames, and
`getUpdateManager()` stops inventing updates. Host frame-timestamp jitter no
longer drops frames for the rest of a session, and `MigoRuntime` reports the
real engine version.

### Added
- `AnalyserNode`'s scalar parameters take effect. `minDecibels`, `maxDecibels`
  and `smoothingTimeConstant` were accepted and then ignored, so
  `getByteFrequencyData` / `getFloatFrequencyData` returned unsmoothed data over
  a fixed range regardless of what content set. They now reach the audio thread
  through a dedicated op and apply.
- `scripts/build-aar.sh --jitless` builds an engine that asks V8 to stop
  generating machine code (`--jitless`). HarmonyOS 5.0.0(12) forbids a
  third-party VM from making memory executable, so Migo runs interpreted on
  NEXT whether it asks to or not; this is the build that produces the size of
  that penalty rather than guessing at it. Off by default, selected by no
  release path, and the artifact is named so it cannot be mistaken for one. The
  measured numbers are in `migo-bench/JITLESS.md`.

### Changed
- `migo.compressImage` no longer starts one OS thread per call. A batch — a
  screenshot sheet, an avatar pipeline — got a thread per image, each with a
  1 MB stack, all competing with the render thread. It now uses a shared,
  daemon pool of two.
- `PRESCREEN.md` and the prescreen report now say which published names are
  no-op stubs and which fail loudly, rather than covering all of them with one
  sentence true of only some. The two stubs that were failing silently now
  answer `"<api>:fail not supported"` like the other 86.
- On Android, `requestVsync` resolves its Java method ID once per process
  instead of hashing the method name and signature against the JNI method cache
  on every frame. The lookup is off the frame-scheduling path now; the id is a
  process-lifetime constant.
- The GitHub Release notes now lead with this file's `## v<version>` section.
  They previously carried only the asset table, verification steps, and GitHub's
  raw commit list — the curated record of what changed was in the repo but not
  on the release page. `write-release-notes.sh` refuses to write notes for a
  version whose section is missing or empty.
- Redundant GL calls stop reaching the driver on two more paths. `glScissor`
  could not be deduplicated before, because the dirty-region Canvas2D batcher
  and the DrawingBuffer blit both moved the scissor box outside the state
  shadow; both now route through it. The texture-unit, `glEnable`/`glDisable`
  and vertex-attribute shadows were rebuilt from hash maps and sets keyed by GL
  enum or `(vao, index)` into arrays and bitmasks, so deciding that a call is
  redundant no longer hashes.
- `AudioParam` automation is evaluated once per 128-sample block instead of once
  per sample. A parameter driven by `setValueAtTime`, a ramp, `setTargetAtTime`
  or `setValueCurveAtTime` walked its whole event timeline for every sample in
  the block; it now makes one forward pass.
- `measureText` and the Canvas2D font ops no longer allocate on every call. They
  cloned the whole text string to keep a character count for a render-thread
  timeout branch they rarely reach; they now take the count first and move the
  original.
- WebGL error reporting under queue pressure follows the spec. Past the
  per-context cap the queue used to discard the oldest un-retrieved error; it
  now keeps the oldest, drops the newest, reserves the last slot for a sticky
  `OUT_OF_MEMORY`, and counts the drops in the render diagnostics.
- `chacha20` (pulled in transitively by `rand`) is updated off a yanked
  release. No advisory attached; `cargo audit` stays clean.

### Fixed
- A re-linked WebGL program's uniforms reach the driver again. `glLinkProgram`
  gives a program fresh, zeroed uniform storage; the per-`(program, location)`
  dedup cache was not cleared across it, so content that re-linked a program and
  then re-uploaded an unchanged uniform value had that upload silently dropped —
  the draw used the reset value, with no GL error and nothing in a log.
- Sub-package and ZIP extraction is no longer open to a symlink race. An entry's
  path was validated as a string and then resolved again by a separate syscall;
  a path component could turn into a symlink in between and let the entry write
  outside its destination directory. Every component is now reached with
  `openat` under `O_NOFOLLOW` from a held directory descriptor, so a swapped
  component fails with `ELOOP` at the moment of use rather than after a passed
  check.
- `fetch` no longer puts the sandbox's internal path layout in a request to a
  remote server.
- Terminating a Worker while it is still starting up is no longer a silent
  no-op. The handle the host holds was published only after the worker runtime
  finished initialising, so `terminate()` called in that window did nothing and
  the worker ran to completion.
- A game is no longer stranded after a WebGL context loss and restore. The
  render drain dispatches `webglcontextlost` / `webglcontextrestored` into JS;
  a handler that resolves a promise or requests a frame leaves a pending task,
  and the event loop stayed parked on it until some unrelated host command
  arrived — which, for a game between frames, it never did.
- The C ABI's minimum `struct_size` for `MigoHostCallbacks` was wrong on ILP32.
  It used a literal `32`, correct for LP64's pointer width; a 32-bit host
  passing a struct that fully contained `dispatch` (minimum `20` there) was
  refused with `MIGO_ERROR_INVALID_ARGUMENT`. The minimum is now derived from
  the field offset.
- `migo.getUpdateManager()` no longer invents updates. It was deciding with
  `Math.random() < 0.3` at construction whether to fire
  `onCheckForUpdate({hasUpdate: true})`, so roughly a quarter of launches
  showed the game's own "new version — restart?" prompt and then `applyUpdate()`
  restarted nothing. This runtime has no update channel; the manager now says so
  instead of pretending.
- Non-Android hosts (Linux, Windows, OpenHarmony, bare C embedders) now remove
  a session's sandbox `/tmp` directory when the session ends. `GamePaths::clean_temp`
  had no caller outside Android's Java SDK, so every session left its
  `tmp/{id}` subtree under the cache root for the life of the install.
- Host timestamp jitter no longer drops frames for the rest of a session. The
  vsync decimator admitted a frame within a fixed 0.25 ms of its deadline, and
  every useful cadence puts that deadline exactly on a vsync — so a host whose
  frame-callback timestamps jittered by more than that dropped frames
  permanently: a replayed 60 Hz stream with 0.4 ms of jitter rendered 14 of 24
  vsyncs, a 60 fps request running at ~35 and staying there. The tolerance is
  now half the smaller of the last two delivered-vsync gaps, taking the frame
  nearest its deadline rather than the first past it. The frame rate the
  content asked for now also reaches Android's display-mode hint, so a request
  that is a whole divisor of the panel's rate gets an even cadence; and the
  requested-rate range and default, which four copies of the rule disagreed on
  (`2^31` and non-finite handled differently on each side, a 24 fps request
  silently raised to 30, a ceiling of 120 that left 144 Hz panels unreachable),
  now live in one module.
- `MigoRuntime.getNativeVersion()` and the public `MigoRuntime.SDK_VERSION`
  constant report the running engine's version. Both had frozen to an early
  release's number while the AAR's own `versionName` tracked `release/VERSION`,
  and since they were the same frozen string the SDK's skew check compared a
  value against itself and could never fire. The JNI `version()` now derives
  from `CARGO_PKG_VERSION` and `SDK_VERSION` from `BuildInfo.VERSION`, both of
  which `scripts/test-release-version-contract.sh` holds equal to
  `release/VERSION`.

---

## v0.9.4 (2026-08-24)

Android delivery gets more flexible — the engine can be kept out of the first
install and handed over at first launch — and Android cold start moves ahead of
the system WebView on every benchmark game. A sweep of the game-facing API by
behaviour rather than by reading turned up several that failed silently. The
`wx` namespace is gone.

### Added
- `MigoGameActivity.onCreateRuntimeConfig()`. A config handed to
  `buildLaunchIntent` travels through an in-process table keyed by a token in
  the intent, so it reaches the activity only when whatever started it was in
  the same process. A game opened from a deep link, a notification, a launcher
  shortcut or `am start` is not: the token is absent and the launch silently ran
  on a default config, whatever the host had configured everywhere else.
  Overriding this describes how the app runs games once, and it applies however
  the activity was reached. The default returns `null`, which is exactly the
  previous behaviour.
- `MigoNativeLoader.prepare(context, file)`, for hosts that deliver the engine
  themselves. It runs the same verification the load already does, on the
  thread the caller is on. It does not make the load safer -- what it changes
  is when a bad file is found: a truncated download or a mirror serving the
  previous release is otherwise discovered when a user opens a game, as a
  launch failure, rather than by the download code that still has the network
  connection. It also keeps the check off the main thread; verification is
  41 ms for a 45 MB release engine on a Mate 30 Pro, and the load afterwards
  finds the result recorded and hashes nothing.
- The first `readPixels` on a WebGL context no longer returns an empty buffer.
  While one canvas exists and nothing has read the default framebuffer, WebGL
  renders straight to the window surface and the intermediate DrawingBuffer is
  bypassed. The first readback ends that bypass -- a read needs a real FBO --
  and the engine bound the DrawingBuffer without putting anything in it, so the
  read returned `[0,0,0,0]` for pixels the game had just drawn. It happens once
  per context, at startup, which is the hardest kind of bug to notice and the
  easiest to blame on the content; content that builds textures by drawing and
  reading back gets one empty texture. `signal_default_fbo_readback` has
  documented this snapshot since the flag was introduced -- only the code was
  missing. Found by a new WebGL bundle in migo-conformance on its first run.
- Android hosts can keep the engine out of their first install. `libmigo.so`
  is ~17 MB of store download and ~45 MB installed per ABI, paid by every
  user whether or not they ever open a mini-game. Two new release assets let
  that cost move to the first game launch: `migo-<version>-android-nojni.aar`
  (the published AAR with `jni/**` deleted) and
  `migo-<version>-jni-android-<arch>.tar.gz` (the bytes it no longer carries).
  A host installs a `NativeLibraryProvider` through the new `MigoNativeLoader`
  and hands over the file; Migo verifies it against the artifact manifest the
  AAR already embeds before loading it, so a partial download or a mirror
  still serving the previous release fails with a readable reason instead of
  crashing inside the engine. Migo downloads nothing itself: on Google Play
  the only compliant source is Play Feature Delivery, and stores without it
  expect the host to serve the file, so one built-in downloader would be
  wrong for one of the two.
- `scripts/test-android-nojni-aar-contract.sh`, which holds the engine-less
  AAR to being a deletion rather than a second build -- identical
  `classes.jar`, identical embedded artifact identities, and every removed
  byte accounted for in exactly one engine archive.

### Changed
- The native library now loads on first use rather than in
  `MigoRuntime.getInstance()`. Every accessor that needs native calls loads
  first, so the packaged default behaves exactly as before; what changes is
  that merely obtaining the singleton no longer pulls the engine into the
  process.
- `LEGAL.md` states that hosting the engine binary to deliver it into your
  own app is covered by the Additional Use Grant and is not a Competitive
  Offering.
- Android cold start is materially faster, and now leads the system WebView on
  both metrics for all three benchmark games. Five costs sat on that path and
  none had to. Full-screen immersive mode was applied from `createSession` --
  after the window had been laid out with the system bars and the surface
  created at that smaller size -- so every launch resized the window and made
  the engine rebuild its GPU-side surface mid-startup; it is now applied in
  `MigoGameActivity.onCreate`, before the surface exists. And the session was
  created inside the `surfaceCreated` callback, holding the main thread for
  ~114 ms in the middle of the traversal that draws the window; it is now
  posted so the traversal can finish and draw first. The three engine-side
  costs are the entries below. Measured on a Mate 30 Pro against the device's
  Android System WebView, medians of 3 cold runs: first frame 385→274 ms
  (bunnymark), 384→258 (canvasmark), 523→344 (endless-runner); game-ready
  523→400, 378→336, 804→605. Steady-state fps and memory are unchanged.
- Session creation is ~38 ms cheaper. Building the Canvas2D text context
  costs 35-41 ms on an arm64 device -- `SkFontMgr_Android` parses
  `/system/etc/fonts.xml` and enumerates the system families, then the
  bundled fallback face is parsed on top -- and it was being done on the host
  thread inside `RenderThread::spawn`, before the render thread existed. Every
  session therefore delayed the start of EGL/Skia initialization by that much
  and then waited for it, whether or not the game ever drew a glyph. The
  context is now built by the render thread once its GPU capabilities are
  published: off the host's critical path, and still long before any game code
  can ask for a glyph, so no first `fillText` pays for it either. `Host::new`
  drops from 88-99 ms to 50-58 ms; game-ready improves by 16-53 ms across the
  three benchmark games.
- Creating a session no longer waits for the GPU. `Host::new` blocked on the
  render thread publishing its capabilities -- 30-44 ms with the caller's
  `createSession` blocked behind it -- although nothing between that point and
  the first line of launch JS reads them. The wait moved to just before any
  prelude or module runs, which is where the invariant it protects was always
  written down: no untrusted JS may observe the provisional all-false
  capability snapshot. A GPU failure is now reported from `startGame` rather
  than from `createSession`. Game-ready improves a further 10-21 ms.
- The dev player deploys a whole game bundle, not just `game.js` and
  `game.json`. It copied those two files by name, so it could not run any
  bundle carrying an asset -- a font, an image, a sub-package -- which made a
  whole category of conformance test impossible to write.
- The GLES dispatch table is built once per session instead of twice.
  `glow::Context::from_loader_function` resolves 709 symbols through
  `eglGetProcAddress`, which costs 41-50 ms on an arm64 device, and it was
  being paid twice: once by `CanvasManager`, with the host thread waiting on
  it, and again by the render thread immediately afterwards, delaying the
  first frame by the same amount. Both were built on the render thread from
  EGL contexts in one share group, so the manager's table now serves both.
  Game-ready drops a further 40-53 ms.
- The log level a host configures now takes effect. `tracing` caches what it
  thinks of a callsite the first time it sees one, and the dynamic level filter
  did not opt out of that: a callsite first reached while the process default
  was `Warn` was cached as disabled and stayed dead, so a host that asked for
  `Info` got silence -- including for the startup timings, which are emitted
  while the host is being built and so could never be seen.
- The host event loop no longer logs a warning when it parks with nothing
  pending. It does that three times on every single launch, in the window
  before the game registers its first `requestAnimationFrame`; a warning on
  the guaranteed path is how a log teaches its reader to skip warnings.
- On arm64, `libmigo.so` is about 1.85 MiB smaller. `.rela.dyn` was 2.17 MB,
  99.8% of it single-word `R_AARCH64_RELATIVE` entries at 24 bytes each;
  `--pack-dyn-relocs=android` group-encodes them, and bionic has decoded that
  format since API 23. `.text` is byte-for-byte identical. A first size-budget
  gate holds the engine to a ceiling from here on, with the unchanged previous
  release as its red case.
- The thirteen rustflags `engine/.cargo/config.toml` declares for each Android
  target now reach the AAR link. Cargo replaces rather than merges
  `[target.<triple>].rustflags` when `RUSTFLAGS` is set in the environment, and
  the AAR build sets it — so on the one build that ships, the config's flags
  were silently discarded.
- `createVideoDecoder`'s stub answers `"createVideoDecoder:fail not supported"`
  instead of accepting `start()` and then never delivering a frame or an event.
  Content that polled `getFrameData()` or waited on a listener waited forever,
  with nothing in the log.
- The BSL `Change Date` is re-stamped to publication + 4 years on every release
  and gated against decaying, exceeding the four-year ceiling BSL 1.1 itself
  imposes, or drifting from `LEGAL.md` and the READMEs.
- The C ABI header no longer implies that a production host can both clear
  `MIGO_ENGINE_FLAG_ALLOW_UNSIGNED_CONTENT` and load content: doing so without
  also supplying a code-signing public key fails with `CodeSignatureInvalid`.
  The two paths that actually work — signing enabled with a key, or the flag
  set — are now stated.

### Removed
- The `wx` namespace. The engine used to install both `migo` and `wx` at
  bootstrap; it now installs only `migo`. Content written for a mini-game
  platform gets that platform's global from an external compatibility adapter
  (`migo-wx-adapter`), which is where that surface belongs. `adapter/` left the
  repository with it — Migo is a pure engine.

### Fixed
- `test-android-host-api-contract.sh` froze nothing an embedder reaches by
  subclassing. It read the surface with `javap -public`, so
  `MigoGameActivity`'s `onCreateGameListener`, `onLaunchFailed`,
  `onSessionCreated` and `getGameSession` -- the documented, "zero-boilerplate"
  integration path -- were outside the freeze along with every other protected
  member. R8 once marked exactly those methods `final` in the release AAR and
  external subclasses stopped compiling; this gate could not have seen it. It
  now reads `-protected`, and the baseline grew by the 13 members that were
  never pinned.
- An unparseable `ctx.font` assignment is a no-op again, on both sides of the
  engine. WHATWG says an invalid value leaves the previous font in effect, and
  the render thread already did that -- it rejected the shorthand and kept its
  state. The JS thread did not: it stored the string and `measureText` answered
  from a best-effort parse of it. So a typo'd font string measured at one size
  and painted at another, silently. `ctx.font = "definitely not a font"` after
  `64px "..."` measured 36 px of text the engine painted 221 px of. The check
  now happens once, in `op_set_font`, ahead of both -- so an invalid value never
  reaches either side and `ctx.font` never reports one. The strict parser moved
  to `shared` to make that possible, which is also what finally makes the
  "one parser, one source of truth" comment in the 2D context true.
- `login`, `getUserInfo` and `getPhoneNumber` return a Promise, like every
  other API on that surface. They resolved immediately on `undefined`, so
  `await migo.login()` did not wait and content ran on as though sign-in had
  finished.
- Concurrent first `setStorage` calls no longer lose the SQLite WAL pragma
  race. Ten writes issued together intermittently failed one; the same ten
  issued sequentially never did.
- A throwing content callback no longer swallows the callbacks queued after it.
  A `success` handler that threw stopped `complete` from running, so content
  that hid a loading spinner there left it on screen.
- The image-cache cumulative trim-bytes counter saturates instead of wrapping.
- Three engine frame-loop internals are non-enumerable on `globalThis`, like
  their siblings — a `for...in` over the global no longer walks them.
- `libc++_shared.so` is no longer packed next to `libmigo.so`. Nothing loaded
  it: `librusty_v8.a` carries Chromium's libc++ statically and `libmigo.so`
  declares no `DT_NEEDED` for the shared one. Two megabytes per ABI, gone.
- `minimp3`'s safe wrapper is dropped, taking `slice-ring-buffer`
  (RUSTSEC-2025-0044, four double-frees reachable through safe APIs, no fixed
  version) out of the tree. Only the raw `mp3dec_*` C entry points were ever
  called. `cargo audit` is clean.
- Several WebGL state calls (`bindBuffer`, `blendColor`, `polygonOffset`,
  `bindTexture`) now go through the same redundant-call dedup cache as their
  peers.

---

## v0.9.3 (2026-08-15)

The last platform gap closes: Android, Linux, Windows, and OpenHarmony each
now build and publish both their x86_64 and arm64 release assets entirely in
CI. No platform's release depends on a local or native-machine build step
any more — release-windows-arm64 and Linux's arm64 addition are this cycle's
own new jobs, and this is the first tag either has ever actually run under.

### Added
- `release-linux` now builds and publishes `aarch64-unknown-linux-gnu`
  alongside `x86_64`, cross-compiled from the same x86_64 runner
- V8 built for `aarch64-pc-windows-msvc`
- ANGLE built from source for Windows arm64 -- the first ANGLE-from-source
  pipeline for any Windows arch this project has shipped
- `release-windows-arm64`, a native `windows-11-arm` CI job producing a real
  Windows arm64 SDK package, wired into `publish`
- Runtime generation fencing, callback correlation, and verification lanes
- Per-session isolate support
- BLE notification path and audio realtime gates
- OpenHarmony API floor declaration gate
- Session delivery and verification lanes (A12)

### Fixed
- `fetch-v8-archives.sh`'s Windows-target file naming matched the literal
  `x86_64-pc-windows-msvc` only, so `aarch64-pc-windows-msvc` fell through to
  a Unix-style name (`librusty_v8-aarch64-pc-windows-msvc.a`) that was never
  published, 404-ing every fetch
- The x86_64 V8 component manifest's recorded hash and toolchain provenance
  described a rebuild that was never actually uploaded to the archive
  release, so `release-windows` failed sha256 verification against the
  archive that was actually still there
- `engine/.cargo/config.toml` pinned `CC`/`CXX` to `clang-cl` for
  `x86_64-pc-windows-msvc` only; `aarch64-pc-windows-msvc` had no
  corresponding pin, so `cc-rs` fell back to probing versioned compiler names
  (`clang-18`, ...) that don't exist on a runner with only choco's LLVM on
  PATH, failing `ring`'s build script
- A canvas the content never sized now follows a surface that was destroyed and
  recreated at a different size, instead of keeping the size derived from the
  previous one. Rotating while the app is in the background takes that path on
  every Android device, and content came back to a `canvas.width`/`height`
  describing the window it was suspended on, stretched across the new one by the
  presentation blit while `migo.getSystemInfoSync()` reported the real extent.
- A canvas the content *did* size with `canvas.width` is no longer moved when the
  surface resizes. It had been rescaled in proportion to the surface, so a game
  that picked a fixed resolution kept drawing in coordinates its own backing
  store no longer had — into a corner of it.

### Changed
- `MigoSurfaceDescriptor.generation` now documents the rule the C ABI already
  enforced: every attach must carry a generation strictly greater than any the
  session has accepted, and a metrics update carries the live attachment's own.
  A host that stamps a constant is refused with `MIGO_ERROR_STALE_SURFACE` from
  its second attach onwards, which any platform that destroys and recreates its
  window — Android on every trip through the background — reaches on the first
  resume.

---

## v0.9.2 (2026-08-13)

One release, one asset naming scheme, one publisher. Every platform is now
built and staged the same way and named `migo-<version>[-capi]-<platform>[-<arch>].<ext>`,
replacing the three schemes v0.9.1 shipped side by side
(`migo-full-release-arm64-v8a.aar`, `migo-sdk-android-arm64-v8a.tar.gz`,
`migo-linux-x86_64.tar.gz`). This is also the first release where the Windows
and OpenHarmony packages are produced by the same reproducible path as
Android and Linux, rather than hand-tarred.

### Added
- `migo-<version>-android.aar` — the single Java/Kotlin AAR (universal, both
  ABIs); the slim and arm64-only AAR variants are no longer published (a
  consumer's own `abiFilters`/App Bundle already owns that choice)
- `migo-<version>-capi-<platform>-<arch>.tar.gz` for `android`, `linux`,
  `windows`, and `ohos`, each with a package manifest and reproducible
  packaging (`scripts/package-sdk.sh`)
- `migo-<version>-sbom.cdx.json`, `SHA256SUMS.txt`, `version.json`
- V8 startup snapshots now embed on Linux (host + worker, full profile), not
  only Android — `runtime-v8/build.rs` dispatches embedding by `(os, arch)`
  instead of an Android-only check. Snapshot filenames carry an OS segment
  (`SNAPSHOT-<profile>-<os>-<arch>.bin`) so android-x86_64 and linux-x86_64,
  previously colliding, are distinct files
- `scripts/test-capi-snapshot-embedding-contract.sh`: proves a shipped C-ABI
  static/shared library actually contains the snapshot bytes its package
  manifest claims, the same property the AAR contract already proved for
  `libmigo.so`
- Windows: `x86_64-pc-windows-msvc` V8 built and its component manifest
  sealed for the first time; the archive (`rusty_v8.lib` + `rusty_v8.dll` +
  import library) is published on the `v8-archives-e6a88b3` release and
  fetchable via `scripts/fetch-v8-archives.sh x86_64-pc-windows-msvc`
- Windows: ANGLE's runtime (`libEGL.dll`, `libGLESv2.dll`,
  `d3dcompiler_47.dll`) is pinned to a verified download
  (`contracts/artifact-manifest/windows-angle.lock.json` +
  `scripts/fetch-windows-angle.sh`) instead of an ad hoc local directory —
  ANGLE publishes no official prebuilt Windows binaries, so these are
  self-hosted on the same release tag the V8 archives use
- OpenHarmony: `librusty_v8-{aarch64,x86_64}-linux-ohos.a` published on
  `v8-archives-e6a88b3`, and OHOS builds in CI (`release-ohos`)
- `scripts/verify-release-assets.sh`: enforces that every published asset is
  covered either by `SHA256SUMS.txt` or its own `.attestation.json`, checked
  against the live GitHub release rather than build intent
- `scripts/test-release-asset-naming-contract.sh` and
  `test-release-asset-ordering-contract.sh` guard the naming scheme and the
  publish job's asset list against drift

### Changed
- `release.yml` restructured per platform: `release-android`, `release-linux`,
  and `release-ohos` build and stage in parallel; a single `publish` job merges
  every platform's staged output, generates one `SHA256SUMS.txt` covering the
  whole release (previously only the Android job's output), and performs one
  upload. Windows is not in CI yet (`build-windows-sdk.sh` needs WSL/`cmd.exe`
  interop a `windows-latest` runner does not have) and is built and uploaded
  by hand until a Windows-native job exists
- The Android V8 archive directories and release asset names moved from bare
  architecture words to full target triples (`aarch64` →
  `aarch64-linux-android`, `x86_64` → `x86_64-linux-android`), matching the
  vocabulary the Linux and OpenHarmony directories already used
- `scripts/publish-release.sh`, whose `required_files` still named v0.9.0-era
  assets and had fully diverged from `release.yml`, is removed

### Fixed
- `SHA256SUMS.txt` no longer silently covers only the Android job's output
  while implying whole-release coverage — see the `release.yml` restructuring
  above
- A release AAR could previously claim an embedded snapshot in its slice
  manifest without the shipped `.so` actually containing it (`build.rs` fails
  safe and only warns on a stale/invalid snapshot); `scripts/test-android-
  snapshot-embedding-contract.sh` and its new C-ABI sibling now read the
  shipped bytes to prove this rather than trusting the manifest

---

## Engine — v0.9.0 (2026-07-28)

First public engine release. Ships the Rust multi-crate engine with a C ABI
(`libmigo_capi`) on four platforms: Android, Linux, Windows, and OpenHarmony.

### Added
- WebAudio-style runtime with `AudioContext`, `AudioBuffer`, and
  `InnerAudioContext` APIs compatible with mini-game style
- Audio decoders for MP3, OGG, and WAV formats; streaming and caching pipeline
- Canvas 2D and WebGL rendering APIs
- File I/O (sync and async), network fetch, and touch input
- C ABI (`migo-capi`) with a documented, controlled export surface (`migo_*`) --
  still a candidate today, not yet frozen; see `include/migo/README.md`
- Android JNI bindings and AAR packaging (`migo-full-release.aar`,
  `migo-slim-release.aar`); Android demo project in
  [migo-examples](https://github.com/minigame-labs/migo-examples)
- Android C-API SDK tarballs (`migo-sdk-android-arm64-v8a.tar.gz`,
  `migo-sdk-android-x86_64.tar.gz`)
- Linux x86_64 C-API SDK (`migo-sdk-linux-x86_64.tar.gz`); see `linux-sdk-0.1.0`
  below
- Windows x86_64 C-API SDK (`migo-sdk-windows-x86_64.tar.gz`); see
  `windows-sdk-0.1.1` below
- OpenHarmony (aarch64 and x86_64) builds and contract gates; no published
  release yet (see known gaps in `dist/migo-ohos-x86_64/share/migo/ohos-x86_64-manifest.json`)
- Prebuilt V8 archives for Android and Linux distributed via release assets
  (release `v8-archives-e6a88b3`, 2026-07-25)
- `scripts/fetch-v8-archives.sh` fetches and verifies prebuilt V8 archives
  against committed component manifests
- `release/VERSION` as the single version source; `scripts/test-release-version-contract.sh`
  enforces that all build consumers derive from it

### Changed
- Renamed project from `minigame_host` to `migo`
- Renamed SO library from `libminigame_host.so` to `libmigo.so`
- V8 archives moved from Git LFS to release assets to avoid LFS quota exhaustion

---

## Linux SDK — linux-sdk-0.1.0 (2026-07-28)

Packaged alongside `v0.9.0`. The Linux SDK carries its own version series because
it is a separately consumable artifact with its own ABI and loader-floor contract,
distinct from the engine's feature version.

### Added
- `libmigo.so` (versioned, soname `libmigo.so.1`) and `libmigo.a` for
  `x86_64-unknown-linux-gnu`
- glibc 2.31 / GLIBCXX 3.4.28 loader floor, enforced by building against the
  Debian bullseye amd64 sysroot (Chromium's pinned sysroot)
- Export surface controlled by a version script; only the documented `migo_*`
  entry points are exported
- CMake `find_package(migo)` support, `pkg-config` `.pc`, and public headers
- Package manifest (`linux-x86_64-manifest.json`) with sha256 hashes and
  provenance; verified by `scripts/test-linux-sdk-contract.sh`
- Qt 6 host kit (`platforms/linux/host-kit/`) with X11 surface view and managed
  session; gated by `scripts/test-linux-qt-host-kit.sh`

---

## Windows SDK — windows-sdk-0.1.1 (2026-07-29)

The Windows SDK carries its own version series for the same reason as the Linux
SDK. `windows-sdk-0.1.1` supersedes `windows-sdk-0.1.0` (never publicly tagged):
`0.1.0` shipped a DLL that loaded and exported all entry points but could attach
no Win32 surface; `0.1.1` adds the Win32 HWND platform layer.

### Added
- `migo.dll` (x86_64, MSVC) with a `.def`-controlled export surface restricted to
  documented `migo_*` entry points
- `migo.lib` import library, `rusty_v8.dll`, and ANGLE runtime DLLs
  (`libEGL.dll`, `libGLESv2.dll`, `d3dcompiler_47.dll`) shipped alongside
- CMake `find_package(migo)` support targeting the MSVC toolchain
- Win32 HWND surface platform layer; the DLL loads and reports
  `MIGO_PLATFORM_WIN32_HWND` as an attachable kind
- Contract gate (`scripts/test-windows-sdk-contract.sh`) that loads the DLL and
  exercises `migo_query_capabilities` to verify surface support is present

---

## Versioning

This project uses [Semantic Versioning](https://semver.org/):

- **MAJOR**: Incompatible API changes
- **MINOR**: New functionality (backward compatible)
- **PATCH**: Bug fixes (backward compatible)

Per-platform SDKs (Linux, Windows) carry their own version series
(`linux-sdk-X.Y.Z`, `windows-sdk-X.Y.Z`) because each is a separately consumable
artifact with its own ABI contract. The engine version (`v0.9.0`) and the
per-platform SDK versions can move independently.

### Pre-1.0 Policy

While the version is below 1.0.0:
- MINOR version bumps may include breaking changes
- PATCH version bumps are backward compatible

[Unreleased]: https://github.com/minigame-labs/migo/compare/v0.9.3...HEAD
[v0.9.3]: https://github.com/minigame-labs/migo/releases/tag/v0.9.3
[v0.9.2]: https://github.com/minigame-labs/migo/releases/tag/v0.9.2
[v0.9.0]: https://github.com/minigame-labs/migo/releases/tag/v0.9.0
[linux-sdk-0.1.0]: https://github.com/minigame-labs/migo/releases/tag/linux-sdk-0.1.0
[windows-sdk-0.1.1]: https://github.com/minigame-labs/migo/releases/tag/windows-sdk-0.1.1
