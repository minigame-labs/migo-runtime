// The two Canvas2D styles a colour cannot express: gradients and patterns.
//
// Run twice, like the fixtures beside it: in the embedded runtime, where the 2D
// facade's calls reach the Rust ops, and on the producer, where they become
// records the host decodes. The Canvas2D commands of both, in order, must be
// equal -- see engine/crates/runtime-v8/src/rendering/webgl/canvas2d_parity.rs.
//
// The arguments are the ones that tell two implementations apart: all three
// gradient kinds (the conic one puts its start angle where `x1` is, which the
// facade decided and neither side may reinterpret), stops whose colours come
// from every form the facade's colour reader accepts, an offset that is not a
// round number, a gradient with one stop (which the facade refuses to apply at
// all), and the four repetition keywords, which are two bools by the time they
// reach the op.

const ctx = new CanvasRenderingContext2D({ _rid: 1, width: 64, height: 64 });

const linear = ctx.createLinearGradient(0, 0, 100, 50);
linear.addColorStop(0, "#ff0000");
linear.addColorStop(0.35, "rgba(0, 128, 255, 0.5)");
linear.addColorStop(1, "lime");
ctx.fillStyle = linear;
ctx.strokeStyle = linear;

const radial = ctx.createRadialGradient(10, 20, 5, 30, 40, 25);
radial.addColorStop(0, "white");
radial.addColorStop(1, "transparent");
ctx.fillStyle = radial;

// `createConicGradient(startAngle, cx, cy)`: the facade passes the centre as the
// first circle and the angle where `x1` belongs, so a reader that assumed the
// linear layout would rotate the gradient instead of failing.
const conic = ctx.createConicGradient(1.25, 32, 32);
conic.addColorStop(0, "#000");
conic.addColorStop(1, "#fff");
ctx.strokeStyle = conic;

// Fewer than two stops: the facade does not apply it, so neither lane records
// anything and the style stays what it was.
const bare = ctx.createLinearGradient(0, 0, 1, 1);
bare.addColorStop(0.5, "red");
ctx.fillStyle = bare;

// Patterns. The image is a loaded one as the facade reads it -- its shared id is
// what crosses, and no pixel does.
const image = { loaded: true, rid: 0x40000003, width: 16, height: 16 };
ctx.fillStyle = ctx.createPattern(image, "repeat");
ctx.strokeStyle = ctx.createPattern(image, "repeat-x");
ctx.fillStyle = ctx.createPattern(image, "repeat-y");
ctx.strokeStyle = ctx.createPattern(image, "no-repeat");
// No repetition given is `repeat`, which is what a browser does.
ctx.fillStyle = ctx.createPattern(image);
