// Proves createVideoDecoder decodes on a platform's own codec, end to end:
// clip.mp4 is ten 64 x 48 H.264 frames at ten a second, frame i flat
// (i * 25, 255 - i * 25, 128). Every frame must arrive, in order, with its
// presentation time and its colour; then a seek to 450 ms must continue from
// the frame shown there, the one at 400 ms. The verdict is one line on the
// console; a failure names the step.

function report(line) {
  console.error("migo-video-probe: " + line);
}

function expected(i) {
  return [i * 25, 255 - i * 25, 128];
}

// The middle pixel, against the frame's flat colour, allowing for 4:2:0 and
// the encoder's loss.
function colourOf(frame) {
  const pixels = new Uint8Array(frame.data);
  const at = ((frame.height >> 1) * frame.width + (frame.width >> 1)) * 4;
  return [pixels[at], pixels[at + 1], pixels[at + 2]];
}

function near(a, b) {
  return a.every(function (value, k) { return Math.abs(value - b[k]) <= 12; });
}

const decoder = migo.createVideoDecoder();
const frames = [];
let ended = false;
let phase = "decode";
let seekFrame = null;

decoder.on("ended", function () { ended = true; });

function take() {
  let frame;
  while ((frame = decoder.getFrameData()) !== null) {
    if (phase === "decode") frames.push({ pts: frame.pkPts, size: frame.width + "x" + frame.height, colour: colourOf(frame) });
    else if (seekFrame === null) seekFrame = { pts: frame.pkPts, colour: colourOf(frame) };
  }
}

function verify() {
  const pts = frames.map(function (f) { return f.pts; });
  if (frames.length !== 10) return "frames " + frames.length + " pts " + pts.join(",");
  for (let i = 0; i < 10; i++) {
    if (frames[i].size !== "64x48") return "frame " + i + " size " + frames[i].size;
    if (Math.abs(frames[i].pts - i * 100) > 1) return "frame " + i + " pts " + frames[i].pts;
    if (!near(frames[i].colour, expected(i))) return "frame " + i + " colour " + frames[i].colour.join(",");
  }
  return null;
}

function tick() {
  take();
  if (phase === "decode" && ended) {
    const problem = verify();
    if (problem !== null) {
      report("FAIL " + problem);
      return;
    }
    phase = "seek";
    ended = false;
    decoder.seek(450).then(undefined, function (e) { report("FAIL seek " + JSON.stringify(e)); });
  } else if (phase === "seek" && seekFrame !== null) {
    if (Math.abs(seekFrame.pts - 400) > 1 || !near(seekFrame.colour, expected(4))) {
      report("FAIL seek landed on pts " + seekFrame.pts + " colour " + seekFrame.colour.join(","));
      return;
    }
    report("PASS frames=10 size=64x48 pts=0..900 seek=400");
    decoder.remove();
    return;
  }
  requestAnimationFrame(tick);
}

decoder.start({ source: "clip.mp4", mode: 1 }).then(
  function (res) {
    report("started " + res.width + "x" + res.height);
    requestAnimationFrame(tick);
  },
  function (e) { report("FAIL start " + JSON.stringify(e)); }
);
