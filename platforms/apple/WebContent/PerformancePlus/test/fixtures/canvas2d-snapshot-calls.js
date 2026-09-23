// `getImageData`, which in this engine is a capture rather than a read.
//
// The 2D facade does not bring pixels back to JavaScript for a read it can
// capture: it queues a snapshot beside the draws it follows and hands content a
// lazy `ImageData` whose bytes are fetched only if something asks for them. That
// is what makes `fillText` into a texture cost no round trip at all, and it is
// the path this fixture takes -- in the embedded runtime the capture is an op
// into the frame collector, and on the producer it is a record in the same run.
//
// The other half -- the read that does come back, and the fallback for a
// rectangle the facade cannot capture -- is synchronous, and a synchronous call
// needs a host endpoint this harness does not have. It is checked in
// test/canvas2d-pixels.test.mjs instead, against the arguments and the answers.

const ctx = new CanvasRenderingContext2D({ _rid: 1, width: 64, height: 64 });

// Draws first, so the capture has something to follow: what is being compared
// is the order as much as the record.
ctx.fillStyle = "#123456";
ctx.fillRect(0, 0, 64, 64);

// The whole canvas, which is what cocos reads after a single fillText.
ctx.getImageData(0, 0, 64, 64);

// A sub-rectangle, and one with a negative extent -- the facade normalises the
// rectangle before it captures, so both lanes must capture the same one.
ctx.getImageData(8, 16, 32, 16);
ctx.getImageData(40, 40, -8, -8);

// Between two more draws, so a capture that drifted out of the run would show
// as a different order rather than a different record.
ctx.fillRect(1, 1, 2, 2);
ctx.getImageData(0, 0, 4, 4);
ctx.fillRect(3, 3, 4, 4);
