// The texture uploads whose pixels are a TexImageSource.
//
// `gl.texImage2D(target, ..., canvas)`, the snapshot form of it and an upload of
// `ImageData` are how a game gets a 2D drawing onto the GPU: each is one
// `TexImageSource` upload, whose pixels the renderer converts as the call asks.
// What the record carries -- the call, its size, format and type, the image a
// sub call fills and which source it names -- is decided here, in JavaScript,
// from the shape of the source the facade was handed, so the fixture hands it
// each of those shapes in each call and requires both lanes to build the same
// command.
//
// The sources are plain objects with the fields the facade actually reads, for
// the reason the WebGL fixtures beside this one build their contexts directly:
// a real canvas would need a render thread, and what is under test is the
// upload, not the canvas. A decoded image is not here: the host's image table
// resolves its id, and this lane's fixture has no table.

const gl = new WebGL2RenderingContext({ _rid: 130, width: 4, height: 4 }, {});
gl.bindTexture(gl.TEXTURE_2D, gl.createTexture()); // an upload needs a texture bound

// A canvas element, as cocos hands one over: a numeric `_rid` and a
// `getContext`. The full form takes its size from the arguments; the short form
// from the source itself.
const canvas = { _rid: 9, width: 16, height: 16, getContext: () => null };
gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA, 16, 16, 0, gl.RGBA, gl.UNSIGNED_BYTE, canvas);
gl.texImage2D(gl.TEXTURE_2D, 1, gl.RGBA, gl.RGBA, gl.UNSIGNED_BYTE, canvas);
gl.texSubImage2D(gl.TEXTURE_2D, 0, 0, 0, gl.RGBA, gl.UNSIGNED_BYTE, canvas);

// A snapshot-backed `ImageData`, as `getImageData` returns one: the id is what
// names the pixels.
const snapshot = { __migo_snapshot_id__: 4242, width: 8, height: 4 };
gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA, 8, 4, 0, gl.RGBA, gl.UNSIGNED_BYTE, snapshot);
gl.texImage2D(gl.TEXTURE_2D, 2, gl.RGBA, gl.RGBA, gl.UNSIGNED_BYTE, snapshot);
gl.texSubImage2D(gl.TEXTURE_2D, 0, 0, 0, gl.RGBA, gl.UNSIGNED_BYTE, snapshot);

// `ImageData` of its own: its rows cross, into a half-float image.
const rows = new Uint8ClampedArray(4 * 2 * 4);
for (let i = 0; i < rows.length; i += 1) rows[i] = i * 7;
const imageData = { width: 4, height: 2, data: rows };
gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA16F, gl.RGBA, gl.HALF_FLOAT, imageData);

// A WebGL 2 selection, and the 3D calls: two slices of a 2D array from it, one
// row apart, and one flipped into place.
gl.pixelStorei(gl.UNPACK_SKIP_PIXELS, 1);
gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA, 2, 2, 0, gl.RGBA, gl.UNSIGNED_BYTE, imageData);
gl.pixelStorei(gl.UNPACK_SKIP_PIXELS, 0);
gl.bindTexture(gl.TEXTURE_2D_ARRAY, gl.createTexture());
gl.texImage3D(gl.TEXTURE_2D_ARRAY, 0, gl.RGBA8, 4, 1, 2, 0, gl.RGBA, gl.UNSIGNED_BYTE, imageData);
gl.pixelStorei(gl.UNPACK_FLIP_Y_WEBGL, true);
gl.texSubImage3D(gl.TEXTURE_2D_ARRAY, 0, 0, 0, 1, 4, 1, 1, gl.RGBA, gl.UNSIGNED_BYTE, canvas);
gl.pixelStorei(gl.UNPACK_FLIP_Y_WEBGL, false);
