// Every Canvas2D text and image call the Performance+ producer answers on its
// stream lane, made through the engine's own 2D facade.
//
// Run twice, like the WebGL fixture beside it: in the embedded runtime, where
// the facade's calls reach the Rust ops, and on the producer, where they become
// records the host decodes. The Canvas2D commands of both, in order, must be
// equal -- see engine/crates/runtime-v8/src/rendering/webgl/canvas2d_parity.rs.
//
// The arguments are the ones that tell two implementations apart: fonts the
// facade read from shorthands of every form and one it refused, text outside
// ASCII, a `maxWidth` left out (which is `Infinity`), alignment and baseline
// keywords at both ends of their tables, and a dash pattern.

// Through the real entry point, which is the only way to a 2D context in
// either runtime: `createCanvas` is a global in both, and `getContext("2d")` is
// what brings the context into existence -- an op in process, a record here.
// The context directly, as the WebGL fixture beside this one builds its own:
// `createCanvas` asks the render thread for the surface, which the embedded
// test runtime does not have. `getContext("2d")` reaches this constructor.
const ctx = new CanvasRenderingContext2D({ _rid: 1, width: 64, height: 64 });

ctx.font = "16px sans-serif";
ctx.font = "italic bold 24px 'Noto Sans CJK SC', serif";
ctx.font = "12pt Times";
// The family names cross joined by NUL: an empty one, one with an escaped comma
// in it, and a size the facade rounds to the six digits `font` reads back.
ctx.font = "13.333333px '', A\\, B, monospace";
// Not a font (no size): the facade ignores it, the font stays what it was, and
// neither side records anything.
ctx.font = "not-a-font";

ctx.textAlign = "start";
ctx.textAlign = "center";
ctx.textBaseline = "top";
ctx.textBaseline = "ideographic";
ctx.direction = "rtl";
ctx.direction = "inherit";

ctx.fillText("hello", 4, 8);
ctx.fillText("中文 text", 0.5, 63.25, 48);
ctx.strokeText("outline", -2, 16, 100);
ctx.fillText("", 1, 1);

ctx.setLineDash([5, 5]);
ctx.setLineDash([10, 3, 2, 3]);
ctx.setLineDash([]);

// Images: the engine's own, as a decode leaves one -- loaded, its shared id,
// its natural size -- without a decoder to run. (A look-alike object is not an
// image source: the context refuses it with a TypeError.) The ids sit above
// 2^30, where consecutive f32 values are 128 apart: the batch used to carry its
// ids as floats and name another image.
const decoded = (sharedId, width, height) => {
  const image = createImage();
  image._loaded = true;
  image.complete = true;
  image._shared_img_id = sharedId;
  image.width = image.naturalWidth = width;
  image.height = image.naturalHeight = height;
  return image;
};
const image = decoded(0x40000001, 32, 16);
const other = decoded(0x40000002, 8, 8);
ctx.drawImage(image, 4, 8);
ctx.drawImage(image, 0, 0, 64, 32);
ctx.drawImage(other, 1, 2, 3, 4, 5, 6, 7, 8);
ctx.drawImageBatch([
  { image, dx: 1, dy: 2 },
  { image: other, sx: 0, sy: 0, sw: 8, sh: 8, dx: 3, dy: 4, dw: 16, dh: 16 },
]);

// Colour strings in forms that used to leave the facade as text for the host to read: spaces around the channels, a
// name no table knows, a hex length that is not one. The facade reads every colour string itself now, so both lanes
// get the same records -- and a string that is not a colour sends none.
ctx.fillStyle = "rgb( 1 , 2 , 3 )";
ctx.fillRect(0, 0, 1, 1);
ctx.strokeStyle = "rgba( 10 , 20 , 30 , .5 )";
ctx.strokeRect(0, 0, 1, 1);
ctx.shadowColor = "not-a-colour";
ctx.fillStyle = "#12345";
ctx.fillRect(1, 1, 1, 1);
