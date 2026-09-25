// The texture uploads whose pixels the host already holds.
//
// `gl.texImage2D(target, ..., canvas)` and the snapshot form of it are how a
// game gets a 2D drawing onto the GPU without a pixel ever reaching JavaScript:
// the renderer copies GPU to GPU, in process and on this lane alike. Which of
// the two the facade picks is decided here, in JavaScript, from the shape of the
// source it was handed -- a snapshot-backed `ImageData` or a canvas element --
// so the fixture hands it each of those shapes and requires both lanes to build
// the same command.
//
// The sources are plain objects with the fields the facade actually reads, for
// the reason the WebGL fixtures beside this one build their contexts directly:
// a real canvas would need a render thread, and what is under test is the
// upload, not the canvas.

const gl = new WebGLRenderingContext({ _rid: 130, width: 4, height: 4 }, {});

// A canvas element, as cocos hands one over: a numeric `_rid` and a
// `getContext`. The full form takes its size from the arguments, so the
// facade only takes the direct path when they agree with the source's.
const canvas = { _rid: 9, width: 16, height: 16, getContext: () => null };
gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA, 16, 16, 0, gl.RGBA, gl.UNSIGNED_BYTE, canvas);
// The short form, whose size comes from the source itself.
gl.texImage2D(gl.TEXTURE_2D, 1, gl.RGBA, gl.RGBA, gl.UNSIGNED_BYTE, canvas);
gl.texSubImage2D(gl.TEXTURE_2D, 0, 2, 3, gl.RGBA, gl.UNSIGNED_BYTE, canvas);

// A snapshot-backed `ImageData`, as `getImageData` returns one: the id is what
// names the pixels, and the size has to agree with the arguments for the full
// form to take the direct path.
const snapshot = { __migo_snapshot_id__: 4242, width: 8, height: 4 };
gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA, 8, 4, 0, gl.RGBA, gl.UNSIGNED_BYTE, snapshot);
gl.texImage2D(gl.TEXTURE_2D, 2, gl.RGBA, gl.RGBA, gl.UNSIGNED_BYTE, snapshot);
gl.texSubImage2D(gl.TEXTURE_2D, 0, 5, 6, gl.RGBA, gl.UNSIGNED_BYTE, snapshot);

// Not here: a full-form call whose size disagrees with the source's. The facade
// then treats the source as bytes, and an object that is not a typed array is a
// `TypeError` out of the engine's own `toBoundedUploadBytes` -- in both runtimes,
// which is agreement of a kind, but it ends the fixture rather than recording
// anything. The disagreement is what the size fields above are for.
