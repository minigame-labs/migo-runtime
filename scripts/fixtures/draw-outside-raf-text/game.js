// Text drawn by a task that writes nothing else. The first frame sets everything through the
// command stream -- a blue background, a white fill colour, a large font -- and ends; a timer
// then calls only fillText, which reaches the renderer through its own op and not the stream. So
// the text is on screen only if the engine also sees content that arrives that way.
const canvas = migo.createCanvas();
const context = canvas.getContext("2d");
context.fillStyle = "#2040c0";
context.fillRect(0, 0, canvas.width, canvas.height);
context.fillStyle = "#ffffff";
context.font = "160px sans-serif";
setTimeout(() => {
    context.fillText("MIGO", 40, canvas.height / 2);
}, 700);
