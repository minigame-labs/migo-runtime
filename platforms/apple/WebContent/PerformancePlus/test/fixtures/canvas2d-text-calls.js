// Every Canvas2D text and image call the Performance+ producer answers on its
// stream lane, made through the engine's own 2D facade.
//
// Run twice, like the WebGL fixture beside it: in the embedded runtime, where
// the facade's calls reach the Rust ops, and on the producer, where they become
// records the host decodes. The Canvas2D commands of both, in order, must be
// equal -- see engine/crates/runtime-v8/src/rendering/webgl/canvas2d_parity.rs.
//
// The arguments are the ones that tell two implementations apart: a font
// shorthand in every accepted form and one that must be refused, text outside
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
// Refused by both parsers: no size. The font stays what it was, and neither
// side records anything.
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

// Images: a loaded image as the facade reads one -- `loaded`, its shared id,
// its size. The ids sit above 2^30, where consecutive f32 values are 128 apart:
// the batch used to carry its ids as floats and name another image.
const image = { loaded: true, rid: 0x40000001, width: 32, height: 16 };
const other = { loaded: true, rid: 0x40000002, width: 8, height: 8 };
ctx.drawImage(image, 4, 8);
ctx.drawImage(image, 0, 0, 64, 32);
ctx.drawImage(other, 1, 2, 3, 4, 5, 6, 7, 8);
ctx.drawImageBatch([
  { image, dx: 1, dy: 2 },
  { image: other, sx: 0, sy: 0, sw: 8, sh: 8, dx: 3, dy: 4, dw: 16, dh: 16 },
]);
