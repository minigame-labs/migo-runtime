// Rounded rectangles and paths as values: `roundRect`, and `Path2D` filled, stroked and clipped.
//
// Run twice, like the fixtures beside it: in the embedded runtime, where the 2D facade's stream records and its path
// op reach the renderer, and on the producer, where they become records the host decodes. The Canvas2D commands of
// both, in order, must be equal -- see engine/crates/runtime-v8/src/rendering/webgl/canvas2d_parity.rs.
//
// The calls are the ones that tell two implementations apart: radii in each form the facade converts (a number, a
// point, a list of three), a path built by every `CanvasPath` call, one parsed from SVG path data with relative,
// implicit, reflected and arc segments, one added to another under a transform, both fill rules, an empty path
// clipped (which clips everything) -- and a path longer than the stream buffer holds, which the embedded facade sends
// by op behind a flush and the producer writes as the record the encoder could not.

const ctx = new CanvasRenderingContext2D({ _rid: 1, width: 64, height: 64 });

ctx.beginPath();
ctx.roundRect(1, 2, 30, 20, 4);
ctx.roundRect(40, 2, -20, 10, { x: 3, y: 5 });
ctx.roundRect(1, 30, 30, 20, [1, { x: 2, y: 3 }, 4]);
ctx.fill();

const built = new Path2D();
built.moveTo(1, 1);
built.lineTo(10, 1);
built.quadraticCurveTo(12, 3, 10, 6);
built.bezierCurveTo(8, 8, 4, 8, 2, 6);
built.arcTo(1, 5, 1, 3, 1.5);
built.arc(5, 5, 2, 0, Math.PI, true);
built.ellipse(20, 20, 6, 3, 0.25, 0, 2 * Math.PI, false);
built.rect(30, 30, 5, 5);
built.roundRect(40, 40, 10, 10, [2, 3]);
built.closePath();
ctx.fill(built);
ctx.fill(built, "evenodd");

const svg = new Path2D("M10 10 l5 0 0 5 h-5 z m20 0 c1 2 3 4 5 6 s7 8 9 10 Q40 40 45 35 T50 30 a5 4 30 1 0 5 5");
ctx.translate(3, 4);
ctx.stroke(svg);

const combined = new Path2D(svg);
combined.addPath(built, { a: 2, d: 0.5, e: 7, f: -3 });
ctx.clip(combined, "evenodd");
ctx.clip(new Path2D());

// Some 1,600 curves: more words than one stream buffer carries.
const long = new Path2D("M0 0");
for (let i = 0; i < 1600; i++) long.bezierCurveTo(i, i + 1, i + 2, i + 3, i + 4, i + 5);
ctx.fill(long);
ctx.lineWidth = 3;
ctx.stroke(long);
