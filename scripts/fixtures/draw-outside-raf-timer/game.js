// Draws from a timer, never through requestAnimationFrame. The red channel is the draw count
// times 16, so the frame on screen at the end says whether later draws were presented, or only
// the first.
const canvas = migo.createCanvas();
const context = canvas.getContext("2d");
let drawn = 0;
setInterval(() => {
    drawn++;
    context.fillStyle = "rgb(" + Math.min(255, drawn * 16) + ", 0, 0)";
    context.fillRect(0, 0, canvas.width, canvas.height);
}, 300);
